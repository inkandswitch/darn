//! Parallel file discovery.
//!
//! Discovers new files in a workspace and processes them in parallel for performance.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use futures::{stream, StreamExt};
use sedimentree_core::id::SedimentreeId;
use tokio_util::sync::CancellationToken;

use crate::{
    attributes::AttributeRules,
    file::{file_type::FileType, File, SerializeError},
    ignore::IgnoreRules,
    manifest::{content_hash::{self, FileSystemContent}, tracked::Tracked, Manifest},
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
    fn shard_for(&self, path: &Path) -> &Mutex<HashMap<PathBuf, SedimentreeId>> {
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
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
    pub sedimentree_digest: sedimentree_core::crypto::digest::Digest<sedimentree_core::sedimentree::Sedimentree>,
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

    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden directories and .darn, but allow config files
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.')
                || e.depth() == 0
                || name == ".darnignore"
                || name == ".darnattributes"
        })
    {
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

/// Process a single file for discovery.
///
/// Reads the file, converts to Automerge, stores in sedimentree, and updates directory tree.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_single_file(
    path: &Path,
    root: &Path,
    subduction: &DarnSubduction,
    root_dir_id: SedimentreeId,
    dir_cache: &ShardedDirCache,
    attributes: &AttributeRules,
) -> Result<DiscoveredFile, FileProcessError> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| FileProcessError::InvalidPath)?
        .to_path_buf();

    // CPU-intensive work: file read + Automerge conversion
    // Run on blocking threadpool to avoid starving the async runtime
    let path_owned = path.to_path_buf();
    let attributes_default = attributes.clone();
    let (file_type, am_doc) = tokio::task::spawn_blocking(move || {
        let doc = File::from_path_with_attributes(&path_owned, Some(&attributes_default))
            .map_err(FileProcessError::Read)?;

        let file_type = if doc.content.is_text() {
            FileType::Text
        } else {
            FileType::Binary
        };

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
        .map_err(FileProcessError::Sedimentree)?;

    // Add file to directory tree (with caching)
    let parent_dir_id =
        ensure_parent_directories_cached(subduction, root_dir_id, &relative_path, dir_cache)
            .await
            .map_err(FileProcessError::Sedimentree)?;

    let file_name = relative_path
        .file_name()
        .ok_or(FileProcessError::InvalidPath)?
        .to_string_lossy();

    sedimentree::add_file_to_directory(subduction, parent_dir_id, &file_name, sedimentree_id)
        .await
        .map_err(FileProcessError::Sedimentree)?;

    // Compute digests (hash is CPU-bound, run on blocking pool)
    let path_for_hash = path.to_path_buf();
    let file_system_digest = tokio::task::spawn_blocking(move || {
        content_hash::hash_file(&path_for_hash)
    })
    .await
    .map_err(|e| FileProcessError::Spawn(e.to_string()))?
    .map_err(FileProcessError::Hash)?;
    let sedimentree_digest = sedimentree::compute_digest(subduction, sedimentree_id)
        .await
        .map_err(FileProcessError::Sedimentree)?;

    Ok(DiscoveredFile {
        relative_path,
        sedimentree_id,
        file_type,
        file_system_digest,
        sedimentree_digest,
    })
}

/// Ensure parent directories exist, using the cache to avoid redundant operations.
async fn ensure_parent_directories_cached(
    subduction: &DarnSubduction,
    root_id: SedimentreeId,
    relative_path: &Path,
    cache: &ShardedDirCache,
) -> Result<SedimentreeId, SedimentreeError> {
    let parent = relative_path.parent();

    // No parent means file is in root
    if parent.is_none() || parent == Some(Path::new("")) {
        return Ok(root_id);
    }

    let parent_path = parent.expect("checked above");

    // Check cache first
    if let Some(id) = cache.get(parent_path) {
        return Ok(id);
    }

    // Cache miss - need to ensure directories exist
    let parent_id = sedimentree::ensure_parent_directories(subduction, root_id, relative_path).await?;

    // Cache the result
    cache.insert(parent_path.to_path_buf(), parent_id);

    Ok(parent_id)
}

/// Generate a random `SedimentreeId` compatible with automerge-repo.
///
/// Uses 16 random bytes (zero-padded to 32) for automerge URL compatibility.
fn generate_sedimentree_id() -> Result<SedimentreeId, FileProcessError> {
    let mut id_bytes = [0u8; 32];
    // Only fill first 16 bytes; rest stays zero for automerge-repo compatibility
    getrandom::getrandom(&mut id_bytes[..16]).map_err(|e| FileProcessError::Random(e.to_string()))?;
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
    Sedimentree(#[from] SedimentreeError),

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
pub(crate) async fn ingest_files_parallel<F>(
    paths: Vec<PathBuf>,
    root: &Path,
    subduction: &DarnSubduction,
    manifest: &Manifest,
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

    // Process in parallel
    let concurrency = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let dir_cache = ShardedDirCache::new();
    let results: Arc<Mutex<Vec<DiscoveredFile>>> = Arc::new(Mutex::new(Vec::new()));
    let errors: Arc<Mutex<Vec<(PathBuf, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let completed = AtomicUsize::new(0);
    let in_flight = AtomicUsize::new(0);
    let last_completed: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

    // Process files concurrently
    stream::iter(paths)
        .for_each_concurrent(concurrency, |path| {
            let dir_cache = &dir_cache;
            let results = Arc::clone(&results);
            let errors = Arc::clone(&errors);
            let last_completed = Arc::clone(&last_completed);
            let completed = &completed;
            let in_flight = &in_flight;
            let on_progress = &on_progress;
            let attributes = &attributes;

            async move {
                // Check cancellation before processing
                if cancel.is_cancelled() {
                    return;
                }

                // Track that we're starting this file
                in_flight.fetch_add(1, Ordering::Relaxed);

                // Process the file
                let result =
                    process_single_file(&path, root, subduction, root_dir_id, dir_cache, attributes).await;

                // Update counters and last_completed
                in_flight.fetch_sub(1, Ordering::Relaxed);
                completed.fetch_add(1, Ordering::Relaxed);

                match result {
                    Ok(file) => {
                        // Update last_completed with this file's path
                        if let Ok(mut lc) = last_completed.lock() {
                            *lc = Some(file.relative_path.clone());
                        }
                        if let Ok(mut r) = results.lock() {
                            r.push(file);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to process {}: {e}", path.display());
                        // Still update last_completed for failed files
                        if let Ok(mut lc) = last_completed.lock() {
                            if let Ok(rel) = path.strip_prefix(root) {
                                *lc = Some(rel.to_path_buf());
                            }
                        }
                        if let Ok(mut errs) = errors.lock() {
                            errs.push((path, e.to_string()));
                        }
                    }
                }

                // Report progress after completion
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

    // Final progress update
    let last_path = last_completed.lock().ok().and_then(|g| g.clone());
    on_progress(DiscoverProgress {
        completed: completed.load(Ordering::Relaxed),
        total,
        last_completed: last_path.as_deref(),
        in_flight: 0,
    });

    // Extract results - at this point the stream is done so we're the only holder
    let final_results = match Arc::try_unwrap(results) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default(),
    };

    let final_errors = match Arc::try_unwrap(errors) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default(),
    };

    (final_results, final_errors, cancel.is_cancelled())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharded_cache_insert_and_get() {
        let cache = ShardedDirCache::new();
        let path = PathBuf::from("src/foo/bar");
        let id = SedimentreeId::new([42; 32]);

        assert!(cache.get(&path).is_none());

        cache.insert(path.clone(), id);

        assert_eq!(cache.get(&path), Some(id));
    }

    #[test]
    fn sharded_cache_different_paths_different_shards() {
        let cache = ShardedDirCache::new();

        // Insert many paths to exercise multiple shards
        for i in 0..100 {
            let path = PathBuf::from(format!("dir{i}/file.txt"));
            let id = SedimentreeId::new([i as u8; 32]);
            cache.insert(path.clone(), id);
            assert_eq!(cache.get(&path), Some(id));
        }
    }
}
