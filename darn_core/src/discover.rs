//! Parallel file discovery.
//!
//! Discovers new files in a workspace and processes them in parallel for performance.

use std::{
    collections::{BTreeSet, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::{StreamExt, stream};
use sedimentree_core::id::SedimentreeId;
use tokio_util::sync::CancellationToken;

use crate::{
    attributes::AttributeRules,
    file::{File, SerializeError, file_type::FileType},
    ignore::IgnoreRules,
    manifest::{
        Manifest,
        content_hash::{self, FileSystemContent},
        tracked::Tracked,
    },
    sedimentree::{self, SedimentreeError},
    subduction::DarnSubduction,
};

/// Number of shards for the directory cache.
const NUM_SHARDS: usize = 16;

/// Result of file discovery.
#[derive(Debug)]
pub struct DiscoverResult {
    /// Successfully discovered and stored files.
    pub new_files: Vec<PathBuf>,

    /// Files that failed to process: (path, error message).
    pub errors: Vec<(PathBuf, String)>,

    /// Whether discovery was cancelled before completion.
    pub cancelled: bool,
}

/// Progress update during discovery.
#[derive(Debug, Clone)]
pub struct DiscoverProgress<'a> {
    /// Number of files processed so far.
    pub completed: usize,

    /// Total number of files to process.
    pub total: usize,

    /// Most recently completed file (if any).
    pub last_completed: Option<&'a Path>,

    /// Number of files currently being processed.
    pub in_flight: usize,
}

/// Sharded cache for directory `SedimentreeId`s.
///
/// Uses multiple shards to reduce lock contention during parallel processing.
/// Each shard is a `Mutex<HashMap>`, and the shard is selected by hashing the path.
#[derive(Debug)]
pub struct ShardedDirCache {
    shards: [Mutex<HashMap<PathBuf, SedimentreeId>>; NUM_SHARDS],
}

impl Default for ShardedDirCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ShardedDirCache {
    /// Create a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
        }
    }

    /// Get a cached directory ID.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<SedimentreeId> {
        let shard = self.shard_for(path);
        shard.lock().ok()?.get(path).copied()
    }

    /// Insert a directory ID into the cache.
    pub fn insert(&self, path: PathBuf, id: SedimentreeId) {
        if let Ok(mut shard) = self.shard_for(&path).lock() {
            shard.insert(path, id);
        }
    }

    /// Get the shard for a given path.
    #[allow(clippy::indexing_slicing)] // modulo NUM_SHARDS guarantees bounds
    fn shard_for(&self, path: &Path) -> &Mutex<HashMap<PathBuf, SedimentreeId>> {
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        #[allow(clippy::cast_possible_truncation)] // truncation is fine; only used for shard index
        let index = (hasher.finish() as usize) % NUM_SHARDS;
        &self.shards[index]
    }
}

/// Information about a successfully discovered file.
#[derive(Debug)]
pub(crate) struct DiscoveredFile {
    pub relative_path: PathBuf,
    pub sedimentree_id: SedimentreeId,
    pub file_type: FileType,
    pub file_system_digest: sedimentree_core::crypto::digest::Digest<FileSystemContent>,
    pub sedimentree_digest:
        sedimentree_core::crypto::digest::Digest<sedimentree_core::sedimentree::Sedimentree>,
}

impl DiscoveredFile {
    /// Convert to a manifest `Tracked` entry.
    #[must_use]
    pub(crate) fn into_tracked(self) -> Tracked {
        Tracked::new(
            self.sedimentree_id,
            self.relative_path,
            self.file_type,
            self.file_system_digest,
            self.sedimentree_digest,
        )
    }
}

/// Scan for new untracked files (fast, no side effects).
///
/// Returns paths of files that could be tracked. Does NOT read file contents
/// or store anything. Call `ingest_files_parallel()` to actually process them.
///
/// # Errors
///
/// Returns an error if ignore patterns cannot be loaded.
pub(crate) fn scan_new_files(
    root: &Path,
    manifest: &Manifest,
) -> Result<Vec<PathBuf>, crate::ignore::IgnorePatternError> {
    let ignore_rules = IgnoreRules::from_workspace_root(root)?;
    Ok(collect_discovery_candidates(root, manifest, &ignore_rules))
}

/// Collect candidate files for discovery (fast, synchronous).
///
/// Walks the directory tree, filters out ignored and already-tracked files,
/// and returns a list of paths to process.
fn collect_discovery_candidates(
    root: &Path,
    manifest: &Manifest,
    ignore_rules: &IgnoreRules,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for entry in walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
        // Skip hidden directories and files (the .darn file is handled by ignore rules)
        let name = e.file_name().to_string_lossy();
        !name.starts_with('.') || e.depth() == 0
    }) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read directory entry: {e}");
                continue;
            }
        };

        // Skip directories
        if entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();

        // Get relative path
        let relative_path = match path.strip_prefix(root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };

        // Check if ignored
        if ignore_rules.is_ignored(&relative_path, false) {
            continue;
        }

        // Check if already tracked
        if manifest.get_by_path(&relative_path).is_some() {
            continue;
        }

        candidates.push(path.to_path_buf());
    }

    candidates
}

/// Intermediate result from Phase 1: the file has been read, converted to
/// Automerge, and stored as a sedimentree, but the directory tree has _not_
/// been updated yet.
struct StoredFile {
    discovered: DiscoveredFile,
}

/// Phase 1 of file ingestion (parallelizable).
///
/// Reads the file, converts to Automerge, stores as sedimentree, and computes
/// digests. Does _not_ touch the directory tree — all directory mutations are
/// deferred to Phase 2 to avoid concurrent CRDT map-key conflicts.
async fn store_single_file(
    path: &Path,
    root: &Path,
    subduction: &DarnSubduction,
    attributes: &AttributeRules,
    force_immutable: bool,
) -> Result<StoredFile, FileProcessError> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| FileProcessError::InvalidPath)?
        .to_path_buf();

    // CPU-intensive work: file read + Automerge conversion
    // Run on blocking threadpool to avoid starving the async runtime
    let path_owned = path.to_path_buf();
    let rel_path = relative_path.clone();
    let attributes_default = attributes.clone();
    let (file_type, am_doc) = tokio::task::spawn_blocking(move || {
        let doc = File::from_path_full(
            &path_owned,
            Some(rel_path.as_path()),
            Some(&attributes_default),
            force_immutable,
        )
        .map_err(FileProcessError::Read)?;

        let file_type = FileType::from(&doc.content);

        let am_doc = doc.into_automerge().map_err(FileProcessError::Automerge)?;
        Ok::<_, FileProcessError>((file_type, am_doc))
    })
    .await
    .map_err(|e| FileProcessError::Spawn(e.to_string()))??;

    let mut am_doc = am_doc;

    // Generate random SedimentreeId
    let sedimentree_id = generate_sedimentree_id()?;

    // Store as sedimentree commits
    sedimentree::store_document(subduction, sedimentree_id, &mut am_doc)
        .await
        .map_err(|e| FileProcessError::Sedimentree(Box::new(e)))?;

    // Compute digests (hash is CPU-bound, run on blocking pool)
    let path_for_hash = path.to_path_buf();
    let file_system_digest =
        tokio::task::spawn_blocking(move || content_hash::hash_file(&path_for_hash))
            .await
            .map_err(|e| FileProcessError::Spawn(e.to_string()))?
            .map_err(FileProcessError::Hash)?;
    let sedimentree_digest = sedimentree::compute_digest(subduction, sedimentree_id)
        .await
        .map_err(|e| FileProcessError::Sedimentree(Box::new(e)))?;

    Ok(StoredFile {
        discovered: DiscoveredFile {
            relative_path,
            sedimentree_id,
            file_type,
            file_system_digest,
            sedimentree_digest,
        },
    })
}

/// Phase 2 of file ingestion (sequential).
///
/// Ensures parent directories exist and adds the file entry to its parent
/// directory document. Must be called sequentially to avoid concurrent CRDT
/// map-key conflicts on the Automerge "docs" list.
async fn register_file_in_directory(
    subduction: &DarnSubduction,
    root_dir_id: SedimentreeId,
    stored: &StoredFile,
) -> Result<(), FileProcessError> {
    let relative_path = &stored.discovered.relative_path;

    // Ensure parent directories exist (creates intermediate dirs if needed)
    let parent_dir_id =
        sedimentree::ensure_parent_directories(subduction, root_dir_id, relative_path)
            .await
            .map_err(|e| FileProcessError::Sedimentree(Box::new(e)))?;

    let file_name = relative_path
        .file_name()
        .ok_or(FileProcessError::InvalidPath)?
        .to_string_lossy();

    sedimentree::add_file_to_directory(
        subduction,
        parent_dir_id,
        &file_name,
        stored.discovered.sedimentree_id,
    )
    .await
    .map_err(|e| FileProcessError::Sedimentree(Box::new(e)))?;

    Ok(())
}

/// Generate a random `SedimentreeId` compatible with automerge-repo.
///
/// Uses 16 random bytes (zero-padded to 32) for automerge URL compatibility.
#[allow(clippy::result_large_err)] // SedimentreeError is large but only used internally
fn generate_sedimentree_id() -> Result<SedimentreeId, FileProcessError> {
    let mut id_bytes = [0u8; 32];
    // Only fill first 16 bytes; rest stays zero for automerge-repo compatibility
    getrandom::getrandom(&mut id_bytes[..16])
        .map_err(|e| FileProcessError::Random(e.to_string()))?;
    Ok(SedimentreeId::new(id_bytes))
}

/// Error processing a single file during discovery.
#[derive(Debug, thiserror::Error)]
pub enum FileProcessError {
    /// Invalid file path.
    #[error("invalid file path")]
    InvalidPath,

    /// Failed to read file.
    #[error("failed to read file: {0}")]
    Read(#[from] crate::file::ReadFileError),

    /// Failed to convert to Automerge.
    #[error("automerge error: {0}")]
    Automerge(#[from] SerializeError),

    /// Sedimentree storage error.
    #[error("storage error: {0}")]
    Sedimentree(Box<SedimentreeError>),

    /// Failed to hash file.
    #[error("hash error: {0}")]
    Hash(#[from] std::io::Error),

    /// Random number generation failed.
    #[error("random generation failed: {0}")]
    Random(String),

    /// Failed to spawn blocking task.
    #[error("spawn error: {0}")]
    Spawn(String),
}

/// Ingest files into storage in parallel.
///
/// Takes a list of paths (from `scan_new_files()`) and processes them:
/// reads content, converts to Automerge, stores in sedimentree, and updates
/// directory tree.
///
/// # Arguments
///
/// * `paths` - Paths to ingest (from `scan_new_files()`)
/// * `root` - Workspace root directory
/// * `subduction` - Subduction instance for storage
/// * `manifest` - Current manifest (for root directory ID)
/// * `on_progress` - Callback for progress updates
/// * `cancel` - Cancellation token
///
/// # Returns
///
/// Returns ingested files, errors, and cancellation status.
#[allow(clippy::too_many_lines)]
pub(crate) async fn ingest_files_parallel<F>(
    paths: Vec<PathBuf>,
    root: &Path,
    subduction: &DarnSubduction,
    manifest: &Manifest,
    force_immutable: bool,
    on_progress: F,
    cancel: &CancellationToken,
) -> (Vec<DiscoveredFile>, Vec<(PathBuf, String)>, bool)
where
    F: Fn(DiscoverProgress<'_>) + Send + Sync,
{
    let root_dir_id = manifest.root_directory_id();
    let total = paths.len();

    if total == 0 {
        return (Vec::new(), Vec::new(), false);
    }

    // Load attribute rules for file type detection
    let attributes = AttributeRules::from_workspace_root(root).unwrap_or_default();

    let concurrency = crate::concurrency::io_bound();

    let stored_files: Arc<Mutex<Vec<StoredFile>>> = Arc::new(Mutex::new(Vec::new()));
    let errors: Arc<Mutex<Vec<(PathBuf, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let completed = AtomicUsize::new(0);
    let in_flight = AtomicUsize::new(0);
    let last_completed: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

    // ── Phase 1: store files in parallel (no directory updates) ──────────
    stream::iter(paths)
        .for_each_concurrent(concurrency, |path| {
            let stored_files = Arc::clone(&stored_files);
            let errors = Arc::clone(&errors);
            let last_completed = Arc::clone(&last_completed);
            let completed = &completed;
            let in_flight = &in_flight;
            let on_progress = &on_progress;
            let attributes = &attributes;

            async move {
                if cancel.is_cancelled() {
                    return;
                }

                in_flight.fetch_add(1, Ordering::Relaxed);

                let result =
                    store_single_file(&path, root, subduction, attributes, force_immutable).await;

                in_flight.fetch_sub(1, Ordering::Relaxed);
                completed.fetch_add(1, Ordering::Relaxed);

                match result {
                    Ok(stored) => {
                        if let Ok(mut lc) = last_completed.lock() {
                            *lc = Some(stored.discovered.relative_path.clone());
                        }
                        if let Ok(mut r) = stored_files.lock() {
                            r.push(stored);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to process {}: {e}", path.display());
                        if let Ok(mut lc) = last_completed.lock()
                            && let Ok(rel) = path.strip_prefix(root)
                        {
                            *lc = Some(rel.to_path_buf());
                        }
                        if let Ok(mut errs) = errors.lock() {
                            errs.push((path, e.to_string()));
                        }
                    }
                }

                let last_path = last_completed.lock().ok().and_then(|g| g.clone());
                on_progress(DiscoverProgress {
                    completed: completed.load(Ordering::Relaxed),
                    total,
                    last_completed: last_path.as_deref(),
                    in_flight: in_flight.load(Ordering::Relaxed),
                });
            }
        })
        .await;

    // Final progress update for phase 1
    let last_path = last_completed.lock().ok().and_then(|g| g.clone());
    on_progress(DiscoverProgress {
        completed: completed.load(Ordering::Relaxed),
        total,
        last_completed: last_path.as_deref(),
        in_flight: 0,
    });

    // Extract stored files
    let all_stored = match Arc::try_unwrap(stored_files) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default(),
    };

    let mut final_errors = match Arc::try_unwrap(errors) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default(),
    };

    // ── Phase 2a: ensure all parent directories exist (sequential) ─────
    //
    // Walk the set of unique parent paths top-down so intermediate
    // directories are created before their children. This is lightweight
    // (just creating empty Automerge directory docs) and must be
    // sequential because nested directories share ancestor docs.
    let unique_parents: BTreeSet<PathBuf> = all_stored
        .iter()
        .filter_map(|s| s.discovered.relative_path.parent().map(Path::to_path_buf))
        .collect();

    // BTreeSet iteration is sorted, so "src" comes before "src/sub".
    for parent in &unique_parents {
        if cancel.is_cancelled() {
            break;
        }

        // Build a dummy relative path with a placeholder filename so
        // ensure_parent_directories creates all components of `parent`.
        let probe = parent.join("__probe__");
        if let Err(e) =
            sedimentree::ensure_parent_directories(subduction, root_dir_id, &probe).await
        {
            tracing::warn!("Failed to create directory {}: {e}", parent.display());
            // Errors here will surface per-file in Phase 2b.
        }
    }

    // ── Phase 2b: add files to directories (parallel per parent) ────────
    //
    // Files sharing the same parent directory must be registered
    // sequentially (they mutate the same Automerge document). But files
    // in *different* directories can be processed concurrently.
    let mut by_parent: HashMap<PathBuf, Vec<&StoredFile>> = HashMap::new();
    for stored in &all_stored {
        let parent = stored
            .discovered
            .relative_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        by_parent.entry(parent).or_default().push(stored);
    }

    let final_results: Arc<Mutex<Vec<DiscoveredFile>>> = Arc::new(Mutex::new(Vec::new()));
    let shared_errors: Arc<Mutex<Vec<(PathBuf, String)>>> =
        Arc::new(Mutex::new(std::mem::take(&mut final_errors)));

    stream::iter(by_parent)
        .for_each_concurrent(concurrency, |(_, files)| {
            let results = Arc::clone(&final_results);
            let errors = Arc::clone(&shared_errors);

            async move {
                for stored in files {
                    if cancel.is_cancelled() {
                        break;
                    }

                    // Parent already exists from Phase 2a, so this only
                    // looks up the parent ID and adds the file entry.
                    if let Err(e) =
                        register_file_in_directory(subduction, root_dir_id, stored).await
                    {
                        tracing::warn!(
                            "Failed to register {} in directory: {e}",
                            stored.discovered.relative_path.display()
                        );
                        if let Ok(mut errs) = errors.lock() {
                            errs.push((stored.discovered.relative_path.clone(), e.to_string()));
                        }
                        continue;
                    }

                    if let Ok(mut r) = results.lock() {
                        r.push(DiscoveredFile {
                            relative_path: stored.discovered.relative_path.clone(),
                            sedimentree_id: stored.discovered.sedimentree_id,
                            file_type: stored.discovered.file_type,
                            file_system_digest: stored.discovered.file_system_digest,
                            sedimentree_digest: stored.discovered.sedimentree_digest,
                        });
                    }
                }
            }
        })
        .await;

    let final_results = match Arc::try_unwrap(final_results) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default(),
    };

    let final_errors = match Arc::try_unwrap(shared_errors) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default(),
    };

    (final_results, final_errors, cancel.is_cancelled())
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    #[test]
    fn sharded_cache_insert_then_get() {
        check!().with_type::<Vec<(String, [u8; 32])>>().for_each(
            |entries: &Vec<(String, [u8; 32])>| {
                let cache = ShardedDirCache::new();

                for (path_str, id_bytes) in entries {
                    let path = PathBuf::from(path_str);
                    let id = SedimentreeId::new(*id_bytes);
                    cache.insert(path.clone(), id);
                    assert_eq!(
                        cache.get(&path),
                        Some(id),
                        "get after insert should return the value"
                    );
                }
            },
        );
    }
}
