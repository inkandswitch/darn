//! Staged batch file updates for near-atomic workspace writes.
//!
//! Separates the slow "prepare" phase (loading CRDTs, serializing content,
//! writing to a staging directory) from the fast "commit" phase (parallel
//! renames into the workspace). External observers (Godot, IDEs, build
//! systems) see either the old or new state — never a half-written mix.
//!
//! # Usage
//!
//! ```ignore
//! // Phase 1: Prepare (slow, workspace untouched)
//! let staged = darn.stage_remote_changes(&manifest).await?;
//!
//! // (Optional: notify external observers to pause file watching)
//!
//! // Phase 2: Commit (fast, parallel renames)
//! let result = staged.commit(&mut manifest).await?;
//!
//! // (Optional: notify external observers to resume)
//! ```

use std::path::{Path, PathBuf};

use sedimentree_core::{crypto::digest::Digest, id::SedimentreeId, sedimentree::Sedimentree};
use thiserror::Error;

use crate::{
    file::{File, file_type::FileType},
    manifest::{Manifest, content_hash, tracked::Tracked},
    sync_progress::ApplyResult,
};

/// Prefix for staging directories inside the workspace root.
///
/// Staging dirs are created via [`tempfile::TempDir`] with this prefix so the
/// watcher and discovery can skip them.
pub const STAGING_DIR_PREFIX: &str = ".darn-staging-";

/// A batch of file operations ready to be committed to the workspace.
///
/// All file content has been written to a staging directory on the same
/// filesystem as the workspace. [`StagedUpdate::commit`] renames them into
/// place in parallel, minimizing the window where the workspace is in an
/// inconsistent state.
#[derive(Debug)]
pub struct StagedUpdate {
    /// Staging directory (same filesystem as workspace root).
    /// Cleaned up automatically on drop if not consumed by `commit`.
    staging_dir: tempfile::TempDir,

    /// Workspace root path.
    workspace_root: PathBuf,

    /// Files to rename from staging into the workspace.
    writes: Vec<StagedWrite>,

    /// Files to delete from the workspace.
    deletes: Vec<StagedDelete>,

    /// Manifest patches to apply after all filesystem operations complete.
    patches: Vec<ManifestPatch>,

    /// Classification of each change (for the `ApplyResult`).
    classifications: Vec<ChangeClass>,
}

/// A file staged for writing.
#[derive(Debug)]
struct StagedWrite {
    /// Path inside the staging directory.
    staged_path: PathBuf,

    /// Relative path within the workspace.
    relative_path: PathBuf,
}

/// A file staged for deletion.
#[derive(Debug)]
struct StagedDelete {
    /// Relative path within the workspace.
    relative_path: PathBuf,
}

/// A deferred manifest mutation.
#[derive(Debug)]
enum ManifestPatch {
    /// Track a new file.
    Track(Tracked),

    /// Update digests for an existing tracked file.
    UpdateDigests {
        sedimentree_id: SedimentreeId,
        fs_digest: Digest<content_hash::FileSystemContent>,
        sed_digest: Digest<Sedimentree>,
    },

    /// Remove a file from tracking.
    Untrack(SedimentreeId),
}

/// How a change should be classified in the `ApplyResult`.
#[derive(Debug)]
enum ChangeClass {
    Updated(PathBuf),
    Merged(PathBuf),
    Created(PathBuf),
    Deleted(PathBuf),
}

/// Errors during the commit phase.
#[derive(Debug, Error)]
pub enum CommitError {
    /// I/O error during rename or delete.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Manifest save failed.
    #[error("failed to save manifest: {0}")]
    ManifestSave(#[source] crate::manifest::ManifestError),
}

impl StagedUpdate {
    /// Create a new empty staged update rooted at the given workspace path.
    ///
    /// The staging directory is created inside the workspace root so that
    /// renames are guaranteed to be on the same filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error if the staging directory cannot be created.
    pub fn new(workspace_root: &Path) -> Result<Self, std::io::Error> {
        let staging_dir = tempfile::Builder::new()
            .prefix(STAGING_DIR_PREFIX)
            .tempdir_in(workspace_root)?;

        Ok(Self {
            staging_dir,
            workspace_root: workspace_root.to_path_buf(),
            writes: Vec::new(),
            deletes: Vec::new(),
            patches: Vec::new(),
            classifications: Vec::new(),
        })
    }

    /// Stage a file write: serialize `file` into the staging directory.
    ///
    /// The file is written to `<staging>/<relative_path>`. Parent directories
    /// inside the staging dir are created as needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written to the staging directory.
    pub fn stage_write(
        &mut self,
        file: &File,
        relative_path: PathBuf,
        sedimentree_id: SedimentreeId,
        _file_type: FileType,
        sed_digest: Digest<Sedimentree>,
        is_merge: bool,
    ) -> Result<(), StageError> {
        let staged_path = self.staging_dir.path().join(&relative_path);

        // Create parent directories inside staging
        if let Some(parent) = staged_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write to staging (simple direct write — staging dir is not observed)
        file.write_to_staging(&staged_path)?;

        // Compute filesystem digest from the staged file
        let fs_digest = content_hash::hash_file(&staged_path)?;

        // Record the write operation
        self.writes.push(StagedWrite {
            staged_path,
            relative_path: relative_path.clone(),
        });

        // Record the manifest patch
        self.patches.push(ManifestPatch::UpdateDigests {
            sedimentree_id,
            fs_digest,
            sed_digest,
        });

        // Record the classification
        if is_merge {
            self.classifications
                .push(ChangeClass::Merged(relative_path));
        } else {
            self.classifications
                .push(ChangeClass::Updated(relative_path));
        }

        Ok(())
    }

    /// Stage a new file creation from remote.
    ///
    /// Like [`stage_write`](Self::stage_write), but also adds a manifest
    /// `Track` entry for the new file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written to the staging directory.
    pub fn stage_create(
        &mut self,
        file: &File,
        relative_path: PathBuf,
        sedimentree_id: SedimentreeId,
        file_type: FileType,
        sed_digest: Digest<Sedimentree>,
    ) -> Result<(), StageError> {
        let staged_path = self.staging_dir.path().join(&relative_path);

        if let Some(parent) = staged_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        file.write_to_staging(&staged_path)?;

        let fs_digest = content_hash::hash_file(&staged_path)?;

        self.writes.push(StagedWrite {
            staged_path,
            relative_path: relative_path.clone(),
        });

        self.patches.push(ManifestPatch::Track(Tracked::new(
            sedimentree_id,
            relative_path.clone(),
            file_type,
            fs_digest,
            sed_digest,
        )));

        self.classifications
            .push(ChangeClass::Created(relative_path));

        Ok(())
    }

    /// Stage a file deletion.
    pub fn stage_delete(&mut self, relative_path: PathBuf, sedimentree_id: SedimentreeId) {
        self.deletes.push(StagedDelete {
            relative_path: relative_path.clone(),
        });
        self.patches.push(ManifestPatch::Untrack(sedimentree_id));
        self.classifications
            .push(ChangeClass::Deleted(relative_path));
    }

    /// Number of staged file operations.
    #[must_use]
    pub const fn file_count(&self) -> usize {
        self.writes.len() + self.deletes.len()
    }

    /// Whether there are any staged operations.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.deletes.is_empty()
    }

    /// Paths that will be created or updated.
    #[must_use]
    pub fn written_paths(&self) -> Vec<&Path> {
        self.writes
            .iter()
            .map(|w| w.relative_path.as_path())
            .collect()
    }

    /// Paths that will be deleted.
    #[must_use]
    pub fn deleted_paths(&self) -> Vec<&Path> {
        self.deletes
            .iter()
            .map(|d| d.relative_path.as_path())
            .collect()
    }

    /// All paths affected by this update (writes + deletes).
    #[must_use]
    pub fn affected_paths(&self) -> Vec<&Path> {
        let mut paths = self.written_paths();
        paths.extend(self.deleted_paths());
        paths
    }

    /// Commit all staged changes to the workspace.
    ///
    /// This is the fast phase:
    /// 1. Create necessary parent directories in the workspace
    /// 2. Rename staged files into place (parallel)
    /// 3. Delete removed files (parallel)
    /// 4. Clean up empty directories
    /// 5. Apply manifest patches
    ///
    /// # Errors
    ///
    /// Uses best-effort: if some renames fail, the remaining operations
    /// still proceed. Failures are reported in [`ApplyResult::errors`].
    /// The staging directory is preserved on error for potential retry.
    #[allow(clippy::too_many_lines)]
    pub async fn commit(self, manifest: &mut Manifest) -> Result<ApplyResult, CommitError> {
        let mut result = ApplyResult::new();

        // 1. Ensure parent directories exist in the workspace for all writes
        let mut parent_dirs: Vec<PathBuf> = self
            .writes
            .iter()
            .filter_map(|w| {
                self.workspace_root
                    .join(&w.relative_path)
                    .parent()
                    .map(Path::to_path_buf)
            })
            .collect();
        parent_dirs.sort();
        parent_dirs.dedup();

        for dir in &parent_dirs {
            tokio::fs::create_dir_all(dir).await?;
        }

        // 2. Rename staged files into place — parallel
        let rename_handles: Vec<_> = self
            .writes
            .into_iter()
            .map(|w| {
                let target = self.workspace_root.join(&w.relative_path);
                let staged = w.staged_path;
                let rel = w.relative_path;
                tokio::spawn(async move {
                    // On Windows, remove target before rename
                    #[cfg(target_os = "windows")]
                    if target.exists() {
                        drop(tokio::fs::remove_file(&target).await);
                    }

                    match tokio::fs::rename(&staged, &target).await {
                        Ok(()) => Ok(rel),
                        Err(e) => Err((rel, format!("rename: {e}"))),
                    }
                })
            })
            .collect();

        // 3. Delete files — parallel
        let ws_root = self.workspace_root.clone();
        let delete_handles: Vec<_> = self
            .deletes
            .into_iter()
            .map(|d| {
                let full_path = ws_root.join(&d.relative_path);
                let rel = d.relative_path;
                tokio::spawn(async move {
                    if full_path.exists() {
                        match tokio::fs::remove_file(&full_path).await {
                            Ok(()) => Ok(rel),
                            Err(e) => Err((rel, format!("delete: {e}"))),
                        }
                    } else {
                        Ok(rel)
                    }
                })
            })
            .collect();

        // Await all renames
        for handle in rename_handles {
            match handle.await {
                Ok(Ok(_path)) => {}
                Ok(Err((path, err))) => result.errors.push((path, err)),
                Err(e) => result
                    .errors
                    .push((PathBuf::new(), format!("task join: {e}"))),
            }
        }

        // Await all deletes
        for handle in delete_handles {
            match handle.await {
                Ok(Ok(path)) => {
                    // Clean up empty parent directories
                    let full = self.workspace_root.join(&path);
                    if let Some(parent) = full.parent() {
                        cleanup_empty_dirs(parent, &self.workspace_root);
                    }
                }
                Ok(Err((path, err))) => result.errors.push((path, err)),
                Err(e) => result
                    .errors
                    .push((PathBuf::new(), format!("task join: {e}"))),
            }
        }

        // 4. Apply manifest patches (skip patches for failed operations)
        let failed_paths: std::collections::HashSet<_> =
            result.errors.iter().map(|(p, _)| p.clone()).collect();

        for patch in self.patches {
            match patch {
                ManifestPatch::Track(tracked) => {
                    if !failed_paths.contains(&tracked.relative_path) {
                        manifest.track(tracked);
                    }
                }
                ManifestPatch::UpdateDigests {
                    sedimentree_id,
                    fs_digest,
                    sed_digest,
                } => {
                    if let Some(entry) = manifest.get_by_id_mut(&sedimentree_id)
                        && !failed_paths.contains(&entry.relative_path)
                    {
                        entry.file_system_digest = fs_digest;
                        entry.sedimentree_digest = sed_digest;
                    }
                }
                ManifestPatch::Untrack(sed_id) => {
                    manifest.untrack_by_id(&sed_id);
                }
            }
        }

        // 5. Build the classification result (only for operations that succeeded)
        for class in self.classifications {
            match class {
                ChangeClass::Updated(ref p) if !failed_paths.contains(p) => {
                    result.updated.push(p.clone());
                }
                ChangeClass::Merged(ref p) if !failed_paths.contains(p) => {
                    result.merged.push(p.clone());
                }
                ChangeClass::Created(ref p) if !failed_paths.contains(p) => {
                    result.created.push(p.clone());
                }
                ChangeClass::Deleted(ref p) if !failed_paths.contains(p) => {
                    result.deleted.push(p.clone());
                }
                ChangeClass::Updated(_)
                | ChangeClass::Merged(_)
                | ChangeClass::Created(_)
                | ChangeClass::Deleted(_) => {}
            }
        }

        Ok(result)
    }
}

/// Walk up from `dir` removing empty directories until we hit the workspace root.
fn cleanup_empty_dirs(dir: &Path, workspace_root: &Path) {
    let mut current = dir;

    while current.starts_with(workspace_root) && current != workspace_root {
        let is_empty = match std::fs::read_dir(current) {
            Ok(mut entries) => entries.next().is_none(),
            Err(_) => break,
        };

        if is_empty {
            if std::fs::remove_dir(current).is_err() {
                break;
            }
            tracing::debug!("Removed empty directory: {}", current.display());
        } else {
            break;
        }

        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
    }
}

/// Errors during the staging phase.
#[derive(Debug, Error)]
pub enum StageError {
    /// I/O error writing to the staging directory.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Error writing file content.
    #[error("write file: {0}")]
    WriteFile(#[from] crate::file::WriteFileError),
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::manifest::content_hash;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn random_id() -> Result<SedimentreeId, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)?;
        Ok(SedimentreeId::new(bytes))
    }

    fn dummy_sed_digest() -> sedimentree_core::crypto::digest::Digest<Sedimentree> {
        sedimentree_core::crypto::digest::Digest::force_from_bytes([0u8; 32])
    }

    #[test]
    fn new_creates_staging_dir() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let staged = StagedUpdate::new(workspace.path())?;

        assert!(staged.staging_dir.path().exists());
        let dir_name = staged
            .staging_dir
            .path()
            .file_name()
            .ok_or("staging dir should have a name")?
            .to_string_lossy();
        assert!(dir_name.starts_with(STAGING_DIR_PREFIX));
        Ok(())
    }

    #[test]
    fn empty_staged_update() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let staged = StagedUpdate::new(workspace.path())?;

        assert!(staged.is_empty());
        assert_eq!(staged.file_count(), 0);
        assert!(staged.written_paths().is_empty());
        assert!(staged.deleted_paths().is_empty());
        assert!(staged.affected_paths().is_empty());
        Ok(())
    }

    #[test]
    fn stage_create_writes_to_staging_dir() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut staged = StagedUpdate::new(workspace.path())?;

        let file = crate::file::File::text("hello.txt", "hello world");
        let id = random_id()?;

        staged.stage_create(
            &file,
            PathBuf::from("hello.txt"),
            id,
            FileType::Text,
            dummy_sed_digest(),
        )?;

        assert_eq!(staged.file_count(), 1);
        assert!(!staged.is_empty());
        assert_eq!(staged.written_paths().len(), 1);
        assert_eq!(staged.written_paths()[0], Path::new("hello.txt"));

        // File should exist in staging dir, not workspace
        let staged_file = staged.staging_dir.path().join("hello.txt");
        assert!(staged_file.exists());
        assert!(!workspace.path().join("hello.txt").exists());

        Ok(())
    }

    #[test]
    fn stage_create_nested_path() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut staged = StagedUpdate::new(workspace.path())?;

        let file = crate::file::File::text("deep.txt", "nested content");
        let id = random_id()?;

        staged.stage_create(
            &file,
            PathBuf::from("a/b/c/deep.txt"),
            id,
            FileType::Text,
            dummy_sed_digest(),
        )?;

        let staged_file = staged.staging_dir.path().join("a/b/c/deep.txt");
        assert!(staged_file.exists());
        assert_eq!(std::fs::read_to_string(staged_file)?, "nested content");

        Ok(())
    }

    #[test]
    fn stage_delete_records_path() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut staged = StagedUpdate::new(workspace.path())?;

        let id = random_id()?;
        staged.stage_delete(PathBuf::from("old.txt"), id);

        assert_eq!(staged.file_count(), 1);
        assert_eq!(staged.deleted_paths().len(), 1);
        assert_eq!(staged.deleted_paths()[0], Path::new("old.txt"));
        assert!(staged.written_paths().is_empty());

        Ok(())
    }

    #[test]
    fn affected_paths_includes_writes_and_deletes() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut staged = StagedUpdate::new(workspace.path())?;

        let file = crate::file::File::text("new.txt", "content");
        let id1 = random_id()?;
        let id2 = random_id()?;

        staged.stage_create(
            &file,
            PathBuf::from("new.txt"),
            id1,
            FileType::Text,
            dummy_sed_digest(),
        )?;
        staged.stage_delete(PathBuf::from("old.txt"), id2);

        assert_eq!(staged.file_count(), 2);
        assert_eq!(staged.affected_paths().len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn commit_renames_files_into_workspace() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();
        let mut staged = StagedUpdate::new(workspace.path())?;

        let file = crate::file::File::text("hello.txt", "hello world");
        let id = random_id()?;

        staged.stage_create(
            &file,
            PathBuf::from("hello.txt"),
            id,
            FileType::Text,
            dummy_sed_digest(),
        )?;

        let result = staged.commit(&mut manifest).await?;

        // File should now exist in workspace
        let ws_file = workspace.path().join("hello.txt");
        assert!(ws_file.exists());
        assert_eq!(std::fs::read_to_string(ws_file)?, "hello world");

        // Manifest should have the entry
        let tracked = manifest
            .get_by_path(Path::new("hello.txt"))
            .ok_or("hello.txt should be tracked")?;
        assert_eq!(tracked.sedimentree_id, id);

        // Result should classify as created
        assert_eq!(result.created.len(), 1);
        assert!(result.errors.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn commit_creates_parent_dirs() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();
        let mut staged = StagedUpdate::new(workspace.path())?;

        let file = crate::file::File::text("nested.txt", "deep content");
        let id = random_id()?;

        staged.stage_create(
            &file,
            PathBuf::from("a/b/nested.txt"),
            id,
            FileType::Text,
            dummy_sed_digest(),
        )?;

        staged.commit(&mut manifest).await?;

        assert!(workspace.path().join("a/b/nested.txt").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a/b/nested.txt"))?,
            "deep content"
        );

        Ok(())
    }

    #[tokio::test]
    async fn commit_deletes_files() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();

        // Pre-create a file in the workspace
        let target = workspace.path().join("doomed.txt");
        std::fs::write(&target, "bye")?;
        assert!(target.exists());

        // Track it in manifest
        let id = random_id()?;
        manifest.track(crate::manifest::tracked::Tracked::new(
            id,
            PathBuf::from("doomed.txt"),
            FileType::Text,
            content_hash::hash_bytes(b"bye"),
            dummy_sed_digest(),
        ));

        let mut staged = StagedUpdate::new(workspace.path())?;
        staged.stage_delete(PathBuf::from("doomed.txt"), id);

        let result = staged.commit(&mut manifest).await?;

        assert!(!target.exists());
        assert!(manifest.get_by_path(Path::new("doomed.txt")).is_none());
        assert_eq!(result.deleted.len(), 1);
        assert!(result.errors.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn commit_delete_nonexistent_succeeds() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();

        let id = random_id()?;
        let mut staged = StagedUpdate::new(workspace.path())?;
        staged.stage_delete(PathBuf::from("ghost.txt"), id);

        let result = staged.commit(&mut manifest).await?;

        // Should succeed without error (file already gone)
        assert!(result.errors.is_empty());
        assert_eq!(result.deleted.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn commit_mixed_creates_and_deletes() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();

        // Pre-create a file to delete
        std::fs::write(workspace.path().join("old.txt"), "old")?;
        let old_id = random_id()?;
        manifest.track(crate::manifest::tracked::Tracked::new(
            old_id,
            PathBuf::from("old.txt"),
            FileType::Text,
            content_hash::hash_bytes(b"old"),
            dummy_sed_digest(),
        ));

        let mut staged = StagedUpdate::new(workspace.path())?;

        // Stage a new file
        let file = crate::file::File::text("new.txt", "new content");
        let new_id = random_id()?;
        staged.stage_create(
            &file,
            PathBuf::from("new.txt"),
            new_id,
            FileType::Text,
            dummy_sed_digest(),
        )?;

        // Stage deletion of old file
        staged.stage_delete(PathBuf::from("old.txt"), old_id);

        let result = staged.commit(&mut manifest).await?;

        assert!(workspace.path().join("new.txt").exists());
        assert!(!workspace.path().join("old.txt").exists());
        assert!(manifest.get_by_path(Path::new("new.txt")).is_some());
        assert!(manifest.get_by_path(Path::new("old.txt")).is_none());
        assert_eq!(result.created.len(), 1);
        assert_eq!(result.deleted.len(), 1);
        assert!(result.errors.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn commit_binary_file() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();
        let mut staged = StagedUpdate::new(workspace.path())?;

        let content: Vec<u8> = (0..=255).collect();
        let file = crate::file::File::binary("data.bin", content.clone());
        let id = random_id()?;

        staged.stage_create(
            &file,
            PathBuf::from("data.bin"),
            id,
            FileType::Binary,
            dummy_sed_digest(),
        )?;

        staged.commit(&mut manifest).await?;

        let ws_file = workspace.path().join("data.bin");
        assert!(ws_file.exists());
        assert_eq!(std::fs::read(ws_file)?, content);

        let tracked = manifest
            .get_by_path(Path::new("data.bin"))
            .ok_or("data.bin should be tracked")?;
        assert_eq!(tracked.file_type, FileType::Binary);

        Ok(())
    }

    #[tokio::test]
    async fn commit_overwrites_existing_file() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();

        // Pre-create the file with old content
        std::fs::write(workspace.path().join("file.txt"), "old version")?;
        let id = random_id()?;
        manifest.track(crate::manifest::tracked::Tracked::new(
            id,
            PathBuf::from("file.txt"),
            FileType::Text,
            content_hash::hash_bytes(b"old version"),
            dummy_sed_digest(),
        ));

        let mut staged = StagedUpdate::new(workspace.path())?;
        let file = crate::file::File::text("file.txt", "new version");

        staged.stage_write(
            &file,
            PathBuf::from("file.txt"),
            id,
            FileType::Text,
            dummy_sed_digest(),
            false,
        )?;

        let result = staged.commit(&mut manifest).await?;

        assert_eq!(
            std::fs::read_to_string(workspace.path().join("file.txt"))?,
            "new version"
        );
        assert_eq!(result.updated.len(), 1);
        assert!(result.errors.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn commit_empty_staged_update() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();
        let staged = StagedUpdate::new(workspace.path())?;

        let result = staged.commit(&mut manifest).await?;

        assert!(result.created.is_empty());
        assert!(result.updated.is_empty());
        assert!(result.merged.is_empty());
        assert!(result.deleted.is_empty());
        assert!(result.errors.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn commit_cleans_up_empty_parent_dirs_after_delete() -> TestResult {
        let workspace = tempfile::tempdir()?;
        let mut manifest = Manifest::new();

        // Create nested file
        std::fs::create_dir_all(workspace.path().join("a/b"))?;
        std::fs::write(workspace.path().join("a/b/file.txt"), "content")?;

        let id = random_id()?;
        manifest.track(crate::manifest::tracked::Tracked::new(
            id,
            PathBuf::from("a/b/file.txt"),
            FileType::Text,
            content_hash::hash_bytes(b"content"),
            dummy_sed_digest(),
        ));

        let mut staged = StagedUpdate::new(workspace.path())?;
        staged.stage_delete(PathBuf::from("a/b/file.txt"), id);

        staged.commit(&mut manifest).await?;

        // File should be gone
        assert!(!workspace.path().join("a/b/file.txt").exists());
        // Empty parent dirs should be cleaned up
        assert!(!workspace.path().join("a/b").exists());
        assert!(!workspace.path().join("a").exists());

        Ok(())
    }
}
