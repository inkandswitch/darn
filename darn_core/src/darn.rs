//! Darn workspace management.
//!
//! A workspace is a directory containing a `.darn/` subdirectory that tracks
//! files as Automerge CRDT documents.

pub mod refresh_diff;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration
};

use sedimentree_fs_storage::FsStorage;
use subduction_core::{connection::Connection, peer::id::PeerId};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::tokio::{TimeoutTokio, client::TokioWebSocketClient};
use thiserror::Error;
use tungstenite::http::Uri;

use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, NoConfigDir},
    directory::{Directory, entry::EntryType},
    discover::{self, DiscoverProgress, DiscoverResult},
    file::{File, file_type::FileType, state::FileState},
    manifest::{Manifest, ManifestError, content_hash, tracked::Tracked},
    peer::Peer,
    refresh::{self, RefreshError},
    sedimentree::{self, SedimentreeError},
    signer::{self, LoadSignerError, SignerError},
    subduction::{
        self, AuthenticatedDarnConnection, DarnAttachError, DarnIoError, DarnRegistrationError,
        DarnSubduction, SubductionInitError,
    },
    sync_progress::{ApplyResult, SyncProgressEvent, SyncSummary},
};
use refresh_diff::RefreshDiff;

/// Manifest filename within `.darn/`.
const MANIFEST_FILE: &str = "manifest.json";

/// The name of the `.darn` directory.
const DARN_DIR: &str = ".darn";

/// Storage subdirectory within `.darn/`.
const STORAGE_DIR: &str = "storage";

/// A `darn` workspace rooted at a directory.
///
/// The `Darn` struct manages a workspace with:
/// - File tracking via manifest
/// - Automerge document storage via Subduction
/// - Peer configuration for sync
#[derive(Debug)]
pub struct Darn {
    /// The root directory of the workspace (parent of `.darn/`).
    root: PathBuf,

    /// The Subduction instance for this workspace.
    subduction: Arc<DarnSubduction>,
}

impl Darn {
    /// Initialize a new workspace at the given path.
    ///
    /// Creates the `.darn/` directory structure and ensures the global signer exists.
    /// This is a synchronous operation that doesn't initialize Subduction.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A workspace already exists at the path
    /// - The directories cannot be created
    /// - Signer generation fails
    pub fn init(path: &Path) -> Result<InitializedDarn, InitWorkspaceError> {
        let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let darn_dir = root.join(DARN_DIR);

        if darn_dir.exists() {
            return Err(InitWorkspaceError::AlreadyExists(root));
        }

        // Create workspace directory structure
        std::fs::create_dir_all(&darn_dir)?;
        std::fs::create_dir_all(darn_dir.join(STORAGE_DIR))?;

        // Create initial manifest with random root directory ID
        let manifest = crate::manifest::Manifest::new();
        manifest.save(&darn_dir.join(MANIFEST_FILE))?;

        // Create default .darnignore file
        crate::ignore::create_default(&root)?;

        // Ensure global signer exists
        let signer_dir = config::global_signer_dir()?;
        let signer = signer::load_or_generate(&signer_dir)?;

        let peer_id: PeerId = signer.verifying_key().into();
        tracing::info!(
            path = %root.display(),
            peer_id = %hex::encode(peer_id.as_bytes()),
            "Initialized workspace"
        );

        Ok(InitializedDarn { root })
    }

    /// Initialize a workspace with a specific root directory ID.
    ///
    /// Use this when cloning a workspace — the root directory ID comes from
    /// the source workspace and is used to sync the directory tree.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A workspace already exists at the path
    /// - The directories cannot be created
    /// - Signer generation fails
    pub fn init_with_root_id(
        path: &Path,
        root_directory_id: sedimentree_core::id::SedimentreeId,
    ) -> Result<InitializedDarn, InitWorkspaceError> {
        let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let darn_dir = root.join(DARN_DIR);

        if darn_dir.exists() {
            return Err(InitWorkspaceError::AlreadyExists(root));
        }

        // Create workspace directory structure
        std::fs::create_dir_all(&darn_dir)?;
        std::fs::create_dir_all(darn_dir.join(STORAGE_DIR))?;

        // Create manifest with the provided root directory ID
        let manifest = crate::manifest::Manifest::with_root_id(root_directory_id);
        manifest.save(&darn_dir.join(MANIFEST_FILE))?;

        // Ensure global signer exists
        let signer_dir = config::global_signer_dir()?;
        let signer = signer::load_or_generate(&signer_dir)?;

        let peer_id: PeerId = signer.verifying_key().into();
        tracing::info!(
            path = %root.display(),
            root_directory_id = %bs58::encode(root_directory_id.as_bytes()).into_string(),
            peer_id = %hex::encode(peer_id.as_bytes()),
            "Initialized workspace with root directory ID"
        );

        Ok(InitializedDarn { root })
    }

    /// Open an existing workspace and hydrate Subduction from storage.
    ///
    /// Searches for a `.darn/` directory starting from the given path and
    /// walking up to parent directories. Loads all existing sedimentrees
    /// from storage.
    ///
    /// # Errors
    ///
    /// Returns an error if no workspace is found or Subduction cannot be initialized.
    pub async fn open(path: &Path) -> Result<Self, OpenError> {
        let root = Self::find_root(path)?;
        let signer = Self::load_signer_from(&root)?;
        let storage = Self::storage_from(&root)?;
        let subduction = Box::pin(subduction::hydrate(signer, storage)).await?;

        Ok(Self { root, subduction })
    }

    /// Open an existing workspace without initializing Subduction.
    ///
    /// Use this for operations that don't need Subduction (like peer management).
    ///
    /// # Errors
    ///
    /// Returns an error if no workspace is found.
    pub fn open_without_subduction(path: &Path) -> Result<UnopenedDarn, NotAWorkspace> {
        let root = Self::find_root(path)?;
        Ok(UnopenedDarn { root })
    }

    /// Find the workspace root by walking up the directory tree.
    ///
    /// # Errors
    ///
    /// Returns an error if no `.darn/` directory is found.
    pub fn find_root(start: &Path) -> Result<PathBuf, NotAWorkspace> {
        let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

        loop {
            let darn_dir = current.join(DARN_DIR);
            if darn_dir.is_dir() {
                return Ok(current);
            }

            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => return Err(NotAWorkspace),
            }
        }
    }

    /// Get the workspace root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the `.darn/` directory path.
    #[must_use]
    pub fn darn_dir(&self) -> PathBuf {
        self.root.join(DARN_DIR)
    }

    /// Get the storage directory path.
    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.darn_dir().join(STORAGE_DIR)
    }

    /// Get the Subduction instance.
    #[must_use]
    pub const fn subduction(&self) -> &Arc<DarnSubduction> {
        &self.subduction
    }

    /// Load the global signer.
    ///
    /// # Errors
    ///
    /// Returns an error if the signer cannot be loaded.
    pub fn load_signer(&self) -> Result<MemorySigner, SignerLoadError> {
        Self::load_signer_from(&self.root)
    }

    /// Load the global signer (static helper).
    fn load_signer_from(_root: &Path) -> Result<MemorySigner, SignerLoadError> {
        let signer_dir = config::global_signer_dir()?;
        Ok(signer::load(&signer_dir)?)
    }

    /// Get the peer ID from the global signer.
    ///
    /// # Errors
    ///
    /// Returns an error if the signer cannot be loaded.
    pub fn peer_id(&self) -> Result<PeerId, SignerLoadError> {
        let signer_dir = config::global_signer_dir()?;
        Ok(signer::peer_id(&signer_dir)?)
    }

    /// Create storage from root path.
    fn storage_from(root: &Path) -> Result<FsStorage, StorageError> {
        let storage_dir = root.join(DARN_DIR).join(STORAGE_DIR);
        FsStorage::new(storage_dir).map_err(StorageError)
    }

    /// Get the manifest file path.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.darn_dir().join(MANIFEST_FILE)
    }

    /// Load the manifest from disk.
    ///
    /// Returns an empty manifest if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded.
    pub fn load_manifest(&self) -> Result<Manifest, ManifestError> {
        Manifest::load(&self.manifest_path())
    }

    /// Save the manifest to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be saved.
    pub fn save_manifest(&self, manifest: &Manifest) -> Result<(), ManifestError> {
        manifest.save(&self.manifest_path())
    }

    /// Create an `FsStorage` instance for this workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if the storage cannot be initialized.
    pub fn storage(&self) -> Result<FsStorage, StorageError> {
        FsStorage::new(self.storage_dir()).map_err(StorageError)
    }

    /// Refreshes a single tracked file if it has changed.
    ///
    /// Loads the existing Automerge document from storage, applies incremental
    /// changes from the disk file, and saves the updated document back.
    ///
    /// Returns `true` if the file was updated, `false` if unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, the document cannot be loaded,
    /// or the changes cannot be saved.
    pub async fn refresh_file(&self, entry: &mut Tracked) -> Result<bool, RefreshError> {
        let path = self.root.join(&entry.relative_path);

        // Check current state
        let state = entry.state(&self.root);
        match state {
            FileState::Clean | FileState::Missing => return Ok(false),
            FileState::Modified => {}
        }

        // Read current file content
        let current_fs_digest = content_hash::hash_file(&path)?;
        let new_file = File::from_path(&path)?;

        // Load existing Automerge doc from sedimentree
        let mut am_doc = sedimentree::load_document(&self.subduction, entry.sedimentree_id)
            .await
            .map_err(|e| RefreshError::Storage(Box::new(e)))?
            .ok_or_else(|| RefreshError::Storage(Box::new(SedimentreeError::NotFound)))?;

        // Capture heads before applying changes (for incremental storage)
        let old_heads = am_doc.get_heads();

        // Apply incremental changes (consumes content, avoids clone for binary)
        refresh::update_automerge_content(&mut am_doc, new_file.content)?;

        // Store only the new changes
        sedimentree::add_changes(
            &self.subduction,
            entry.sedimentree_id,
            &mut am_doc,
            &old_heads,
        )
        .await
        .map_err(|e| RefreshError::Storage(Box::new(e)))?;

        // Update digests
        entry.file_system_digest = current_fs_digest;
        entry.sedimentree_digest =
            sedimentree::compute_digest(&self.subduction, entry.sedimentree_id)
                .await
                .map_err(|e| RefreshError::Storage(Box::new(e)))?;

        Ok(true)
    }

    /// Refreshes all tracked files that have changed.
    ///
    /// Returns a summary of which files were updated, missing, or had errors.
    pub async fn refresh_all(&self, manifest: &mut Manifest) -> RefreshDiff {
        let mut diff = RefreshDiff::default();

        for entry in manifest.iter_mut() {
            let path = entry.relative_path.clone();
            let state = entry.state(&self.root);

            match state {
                FileState::Clean => continue,
                FileState::Missing => {
                    diff.missing.push(path);
                    continue;
                }
                FileState::Modified => {}
            }

            match self.refresh_file(entry).await {
                Ok(true) => diff.updated.push(path),
                Ok(false) => {}
                Err(e) => diff.errors.push((path, e)),
            }
        }

        diff
    }

    /// Apply remote changes to local files after sync.
    ///
    /// For each tracked file, checks if the sedimentree digest changed (indicating
    /// remote changes were received). If so, loads the merged CRDT document and
    /// writes it to disk.
    ///
    /// Also discovers new files from the remote directory tree that aren't in
    /// the local manifest.
    ///
    /// # Errors
    ///
    /// Individual file errors are collected in the result; this method doesn't
    /// fail on individual file errors.
    pub async fn apply_remote_changes(&self, manifest: &mut Manifest) -> ApplyResult {
        let mut result = ApplyResult::new();

        tracing::debug!(
            manifest_entries = manifest.iter().count(),
            "apply_remote_changes: checking for changes"
        );

        for entry in manifest.iter_mut() {
            let path = entry.relative_path.clone();

            let new_sed_digest = match sedimentree::compute_digest(
                &self.subduction,
                entry.sedimentree_id,
            )
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    result.errors.push((path, format!("compute digest: {e}")));
                    continue;
                }
            };

            tracing::debug!(
                path = %path.display(),
                old_digest = %entry.sedimentree_digest,
                new_digest = %new_sed_digest,
                changed = new_sed_digest != entry.sedimentree_digest,
                "apply_remote_changes: checking file"
            );

            if new_sed_digest == entry.sedimentree_digest {
                continue; // No remote changes
            }

            let local_changed = entry.state(&self.root) == FileState::Modified;

            let am_doc = match sedimentree::load_document(&self.subduction, entry.sedimentree_id)
                .await
            {
                Ok(Some(doc)) => doc,
                Ok(None) => {
                    result.errors.push((path, "document not found after sync".into()));
                    continue;
                }
                Err(e) => {
                    result.errors.push((path, format!("load document: {e}")));
                    continue;
                }
            };

            // Parse as File
            let file = match File::from_automerge(&am_doc) {
                Ok(f) => f,
                Err(e) => {
                    result.errors.push((path, format!("parse file: {e}")));
                    continue;
                }
            };

            // Write to disk
            let full_path = self.root.join(&entry.relative_path);
            if let Err(e) = file.write_to_path(&full_path) {
                result.errors.push((path, format!("write file: {e}")));
                continue;
            }

            // Update digests
            match content_hash::hash_file(&full_path) {
                Ok(fs_digest) => {
                    entry.file_system_digest = fs_digest;
                    entry.sedimentree_digest = new_sed_digest;
                }
                Err(e) => {
                    result.errors.push((path.clone(), format!("hash file: {e}")));
                    // Still mark as updated since we wrote it
                }
            }

            if local_changed {
                result.merged.push(path);
            } else {
                result.updated.push(path);
            }
        }

        // Step 2: Discover new files from remote directory tree
        if let Err(e) = self
            .discover_remote_files(manifest, &mut result)
            .await
        {
            tracing::warn!("Error discovering remote files: {e}");
        }

        // Step 3: Detect and remove files deleted from remote
        if let Err(e) = self
            .remove_deleted_files(manifest, &mut result)
            .await
        {
            tracing::warn!("Error detecting deleted files: {e}");
        }

        result
    }

    /// Discover new files from the remote directory tree that aren't in the local manifest.
    async fn discover_remote_files(
        &self,
        manifest: &mut Manifest,
        result: &mut ApplyResult,
    ) -> Result<(), SedimentreeError> {
        let root_dir_id = manifest.root_directory_id();
        self.discover_remote_files_recursive(
            root_dir_id,
            PathBuf::new(),
            manifest,
            result,
        )
        .await
    }

    /// Recursively discover files from a remote directory.
    #[allow(clippy::only_used_in_recursion)] // manifest is used, clippy is confused by async
    #[allow(clippy::too_many_lines)] // Complex but coherent file discovery logic
    async fn discover_remote_files_recursive(
        &self,
        dir_id: sedimentree_core::id::SedimentreeId,
        current_path: PathBuf,
        manifest: &mut Manifest,
        result: &mut ApplyResult,
    ) -> Result<(), SedimentreeError> {
        tracing::debug!(
            ?dir_id,
            path = %current_path.display(),
            "discover_remote_files_recursive: loading directory"
        );

        // Load directory document
        let Some(am_doc) = sedimentree::load_document(&self.subduction, dir_id).await?
        else {
            tracing::debug!(?dir_id, "discover_remote_files_recursive: empty directory");
            return Ok(()); // Empty directory
        };

        let Ok(dir) = Directory::from_automerge(&am_doc) else {
            tracing::debug!(?dir_id, "discover_remote_files_recursive: not a directory");
            return Ok(()); // Not a directory document
        };

        tracing::debug!(
            ?dir_id,
            entry_count = dir.entries.len(),
            "discover_remote_files_recursive: found entries"
        );

        for entry in &dir.entries {
            let entry_path = current_path.join(&entry.name);

            tracing::debug!(
                name = %entry.name,
                entry_type = ?entry.entry_type,
                sed_id = ?entry.sedimentree_id,
                "discover_remote_files_recursive: processing entry"
            );

            match entry.entry_type {
                EntryType::File => {
                    if manifest.get_by_id(&entry.sedimentree_id).is_some() {
                        tracing::debug!(name = %entry.name, "already tracked, skipping");
                        continue;
                    }

                    tracing::info!(name = %entry.name, "discovered new remote file");
                    // This is a new file from remote
                    let am_doc = match sedimentree::load_document(
                        &self.subduction,
                        entry.sedimentree_id,
                    )
                    .await
                    {
                        Ok(Some(doc)) => doc,
                        Ok(None) => continue,
                        Err(e) => {
                            result
                                .errors
                                .push((entry_path.clone(), format!("load file: {e}")));
                            continue;
                        }
                    };

                    let file = match File::from_automerge(&am_doc) {
                        Ok(f) => f,
                        Err(e) => {
                            result
                                .errors
                                .push((entry_path.clone(), format!("parse file: {e}")));
                            continue;
                        }
                    };

                    let full_path = self.root.join(&entry_path);

                    if let Some(parent) = full_path.parent()
                        && let Err(e) = std::fs::create_dir_all(parent)
                    {
                        result
                            .errors
                            .push((entry_path.clone(), format!("create dir: {e}")));
                        continue;
                    }

                    if let Err(e) = file.write_to_path(&full_path) {
                        result
                            .errors
                            .push((entry_path.clone(), format!("write file: {e}")));
                        continue;
                    }

                    let file_type = if file.content.is_text() {
                        FileType::Text
                    } else {
                        FileType::Binary
                    };

                    // Compute digests
                    let file_system_digest = match content_hash::hash_file(&full_path) {
                        Ok(d) => d,
                        Err(e) => {
                            result
                                .errors
                                .push((entry_path.clone(), format!("hash file: {e}")));
                            continue;
                        }
                    };

                    let sedimentree_digest = match sedimentree::compute_digest(
                        &self.subduction,
                        entry.sedimentree_id,
                    )
                    .await
                    {
                        Ok(d) => d,
                        Err(e) => {
                            result
                                .errors
                                .push((entry_path.clone(), format!("compute digest: {e}")));
                            continue;
                        }
                    };

                    // Add to manifest
                    let tracked = Tracked::new(
                        entry.sedimentree_id,
                        entry_path.clone(),
                        file_type,
                        file_system_digest,
                        sedimentree_digest,
                    );
                    manifest.track(tracked);

                    result.created.push(entry_path);
                }

                EntryType::Folder => {
                    // Recurse into subdirectory
                    Box::pin(self.discover_remote_files_recursive(
                        entry.sedimentree_id,
                        entry_path,
                        manifest,
                        result,
                    ))
                    .await?;
                }
            }
        }

        Ok(())
    }

    /// Remove files that have been deleted from the remote directory tree.
    ///
    /// Compares the local manifest against the remote directory tree and removes
    /// any files that no longer exist in the remote.
    async fn remove_deleted_files(
        &self,
        manifest: &mut Manifest,
        result: &mut ApplyResult,
    ) -> Result<(), SedimentreeError> {
        use std::collections::HashSet;

        // Collect all sedimentree IDs from the remote directory tree
        let mut remote_ids = HashSet::new();
        let root_dir_id = manifest.root_directory_id();
        self.collect_remote_sedimentree_ids(root_dir_id, &mut remote_ids)
            .await?;

        // Find files in manifest that are not in the remote tree
        let to_delete: Vec<_> = manifest
            .iter()
            .filter(|entry| !remote_ids.contains(&entry.sedimentree_id))
            .map(|entry| (entry.sedimentree_id, entry.relative_path.clone()))
            .collect();

        // Delete each file
        for (sed_id, relative_path) in to_delete {
            let full_path = self.root.join(&relative_path);

            // Delete file from filesystem
            if full_path.exists() && let Err(e) = std::fs::remove_file(&full_path) {
                result
                    .errors
                    .push((relative_path.clone(), format!("delete file: {e}")));
                continue;
            }

            // Remove from manifest
            manifest.untrack_by_id(&sed_id);

            // Clean up empty parent directories
            if let Some(parent) = full_path.parent() {
                Self::cleanup_empty_dirs(parent, &self.root);
            }

            result.deleted.push(relative_path);
        }

        Ok(())
    }

    /// Recursively collect all file sedimentree IDs from the remote directory tree.
    async fn collect_remote_sedimentree_ids(
        &self,
        dir_id: sedimentree_core::id::SedimentreeId,
        ids: &mut std::collections::HashSet<sedimentree_core::id::SedimentreeId>,
    ) -> Result<(), SedimentreeError> {
        tracing::debug!(?dir_id, "Loading directory document for deletion check");

        // Load directory document
        let Some(am_doc) = sedimentree::load_document(&self.subduction, dir_id).await?
        else {
            tracing::debug!(?dir_id, "No directory document found (empty)");
            return Ok(()); // Empty directory
        };

        let Ok(dir) = Directory::from_automerge(&am_doc) else {
            tracing::debug!(?dir_id, "Failed to parse as directory");
            return Ok(()); // Not a directory document
        };

        tracing::debug!(?dir_id, entries = dir.entries.len(), "Loaded directory with entries");

        for entry in &dir.entries {
            match entry.entry_type {
                EntryType::File => {
                    ids.insert(entry.sedimentree_id);
                }
                EntryType::Folder => {
                    // Recurse into subdirectory
                    Box::pin(self.collect_remote_sedimentree_ids(entry.sedimentree_id, ids))
                        .await?;
                }
            }
        }

        Ok(())
    }

    /// Sync any sedimentrees from the remote directory tree that we don't have locally.
    ///
    /// This is called after the initial sync to fetch new files that were added
    /// to the remote but aren't in our manifest yet.
    ///
    /// Returns the number of new sedimentrees synced.
    pub async fn sync_missing_sedimentrees(
        &self,
        manifest: &Manifest,
        peer_id: &PeerId,
    ) -> Result<usize, SyncError> {
        use std::collections::HashSet;

        tracing::debug!("sync_missing_sedimentrees: starting");

        // Collect IDs we already have
        let mut known_ids: HashSet<_> = manifest.iter().map(|e| e.sedimentree_id).collect();
        known_ids.insert(manifest.root_directory_id());
        tracing::debug!(known_count = known_ids.len(), "sync_missing_sedimentrees: known IDs from manifest");

        // Collect all IDs from the remote directory tree
        let mut remote_ids = HashSet::new();
        if let Err(e) = self
            .collect_all_sedimentree_ids(manifest.root_directory_id(), &mut remote_ids)
            .await
        {
            tracing::warn!("Error collecting remote sedimentree IDs: {e}");
        }
        tracing::debug!(remote_count = remote_ids.len(), "sync_missing_sedimentrees: remote IDs from directory tree");

        // Find IDs we don't have
        let missing: Vec<_> = remote_ids.difference(&known_ids).copied().collect();
        tracing::debug!(missing_count = missing.len(), "sync_missing_sedimentrees: missing IDs");

        if missing.is_empty() {
            tracing::debug!("sync_missing_sedimentrees: no missing sedimentrees");
            return Ok(0);
        }

        tracing::info!("Syncing {} missing sedimentrees from remote", missing.len());

        let mut synced = 0;
        for sed_id in missing {
            match self
                .subduction
                .sync_with_peer(peer_id, sed_id, true, Some(Self::DEFAULT_TIMEOUT))
                .await
            {
                Ok((success, stats, _)) => {
                    if success && stats.total_received() > 0 {
                        synced += 1;
                        tracing::debug!(?sed_id, "Synced missing sedimentree");
                    }
                }
                Err(e) => {
                    tracing::warn!(?sed_id, "Failed to sync missing sedimentree: {e}");
                }
            }
        }

        Ok(synced)
    }

    /// Recursively collect all sedimentree IDs (files and folders) from the directory tree.
    async fn collect_all_sedimentree_ids(
        &self,
        dir_id: sedimentree_core::id::SedimentreeId,
        ids: &mut std::collections::HashSet<sedimentree_core::id::SedimentreeId>,
    ) -> Result<(), SedimentreeError> {
        tracing::debug!(?dir_id, "collect_all_sedimentree_ids: loading directory");

        // Load directory document
        let Some(am_doc) = sedimentree::load_document(&self.subduction, dir_id).await?
        else {
            tracing::debug!(?dir_id, "collect_all_sedimentree_ids: no document found");
            return Ok(()); // Empty directory
        };

        let Ok(dir) = Directory::from_automerge(&am_doc) else {
            tracing::debug!(?dir_id, "collect_all_sedimentree_ids: not a directory document");
            return Ok(()); // Not a directory document
        };

        tracing::debug!(
            ?dir_id,
            entry_count = dir.entries.len(),
            "collect_all_sedimentree_ids: found directory entries"
        );

        for entry in &dir.entries {
            tracing::debug!(
                name = %entry.name,
                sed_id = ?entry.sedimentree_id,
                entry_type = ?entry.entry_type,
                "collect_all_sedimentree_ids: found entry"
            );
            ids.insert(entry.sedimentree_id);

            if entry.entry_type == EntryType::Folder {
                // Recurse into subdirectory
                Box::pin(self.collect_all_sedimentree_ids(entry.sedimentree_id, ids)).await?;
            }
        }

        Ok(())
    }

    /// Clean up empty directories from leaf to root, stopping at workspace root.
    fn cleanup_empty_dirs(dir: &Path, workspace_root: &Path) {
        let mut current = dir;

        while current.starts_with(workspace_root) && current != workspace_root {
            // Check if directory is empty
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
                break; // Directory not empty, stop
            }

            // Move to parent
            current = match current.parent() {
                Some(p) => p,
                None => break,
            };
        }
    }

    /// Discover and track new (untracked, non-ignored) files.
    ///
    /// Walks the workspace and finds files that are not in the manifest
    /// and not ignored by `.darnignore`. For each new file, it creates
    /// a sedimentree document and adds it to the manifest.
    ///
    /// Files are processed in parallel for performance.
    ///
    /// # Arguments
    ///
    /// * `manifest` - The manifest to update with new files
    /// * `on_progress` - Callback for progress updates
    /// * `cancel` - Cancellation token; if cancelled, returns immediately with partial results
    ///
    /// # Errors
    ///
    /// Returns an error if file discovery fails fatally (e.g., can't read ignore rules).
    /// Individual file errors are collected in the result.
    pub async fn discover_new_files<F>(
        &self,
        manifest: &mut Manifest,
        on_progress: F,
        cancel: &CancellationToken,
    ) -> Result<DiscoverResult, DiscoverError>
    where
        F: Fn(DiscoverProgress<'_>) + Send + Sync,
    {
        let (discovered, errors, cancelled) = discover::discover_files_parallel(
            &self.root,
            &self.subduction,
            manifest,
            on_progress,
            cancel,
        )
        .await?;

        // Add discovered files to manifest
        let new_files: Vec<PathBuf> = discovered
            .into_iter()
            .map(|file| {
                let path = file.relative_path.clone();
                manifest.track(file.into_tracked());
                path
            })
            .collect();

        Ok(DiscoverResult {
            new_files,
            errors,
            cancelled,
        })
    }

    // ============================================================================
    // Sync operations
    // ============================================================================

    /// Default connection timeout for peer connections.
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Connect to a peer and return the authenticated connection.
    ///
    /// The connection can then be registered with Subduction for syncing.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established.
    pub async fn connect_peer(
        &self,
        peer: &Peer,
    ) -> Result<(AuthenticatedDarnConnection, PeerId), SyncError> {
        let uri: Uri = peer.url.parse()?;
        let signer = self.load_signer()?;

        let (authenticated_client, listener_fut, sender_fut) = TokioWebSocketClient::new(
            uri,
            TimeoutTokio,
            Self::DEFAULT_TIMEOUT,
            signer,
            peer.audience,
        )
        .await?;

        // Spawn background tasks for incoming and outgoing messages
        tokio::spawn(async move {
            if let Err(e) = listener_fut.await {
                tracing::error!("WebSocket listener error: {e:?}");
            }
        });
        tokio::spawn(async move {
            if let Err(e) = sender_fut.await {
                tracing::error!("WebSocket sender error: {e:?}");
            }
        });

        // Get the actual peer ID from the connection (may differ from expectation in discovery mode)
        let actual_peer_id = Connection::peer_id(authenticated_client.inner());

        Ok((authenticated_client, actual_peer_id))
    }

    /// Connect to a peer, attach to Subduction, and perform a full sync.
    ///
    /// # Errors
    ///
    /// Returns an error if connection or sync fails.
    pub async fn sync_with_peer(&self, peer: &Peer) -> Result<SyncResult, SyncError> {
        let (authenticated_connection, peer_id) = self.connect_peer(peer).await?;

        tracing::info!("Connected to peer {}", peer_id);

        // Register the authenticated connection (without auto-syncing - we use full_sync below)
        self.subduction.register(authenticated_connection).await?;

        // Perform full sync with all connected peers
        let (success, stats, call_errors, io_errors) = self
            .subduction
            .full_sync(Some(Self::DEFAULT_TIMEOUT))
            .await;

        for (conn, err) in &call_errors {
            tracing::warn!("Sync error with {:?}: {err:?}", Connection::peer_id(conn));
        }

        for (id, err) in &io_errors {
            tracing::warn!("I/O error syncing sedimentree {id:?}: {err}");
        }

        Ok(SyncResult {
            peer_id,
            success,
            stats,
        })
    }

    /// Sync with a peer, reporting progress via callback.
    ///
    /// This provides more granular progress than `sync_with_peer` by syncing
    /// each sedimentree individually and reporting progress.
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails. Individual sedimentree sync errors
    /// are reported in the callback and summary, not as errors.
    pub async fn sync_with_peer_progress<F>(
        &self,
        peer: &Peer,
        manifest: &Manifest,
        mut on_progress: F,
    ) -> Result<SyncSummary, SyncError>
    where
        F: FnMut(SyncProgressEvent),
    {
        on_progress(SyncProgressEvent::ConnectingToPeer {
            peer_name: peer.name.to_string(),
            url: peer.url.clone(),
        });

        let (authenticated_connection, peer_id) = self.connect_peer(peer).await?;
        tracing::info!("Connected to peer {}", peer_id);

        on_progress(SyncProgressEvent::Connected { peer_id });

        // Register the authenticated connection (without auto-syncing - we handle sync manually below)
        self.subduction.register(authenticated_connection).await?;

        // Collect sedimentree IDs to sync:
        // - Root directory
        // - All tracked files
        let mut sedimentree_ids: Vec<_> = vec![manifest.root_directory_id()];
        sedimentree_ids.extend(manifest.iter().map(|e| e.sedimentree_id));

        // Build path lookup for progress reporting
        let path_lookup: std::collections::HashMap<_, _> = manifest
            .iter()
            .map(|e| (e.sedimentree_id, e.relative_path.clone()))
            .collect();

        let total = sedimentree_ids.len();
        on_progress(SyncProgressEvent::StartingSync {
            total_sedimentrees: total,
        });

        let mut summary = SyncSummary::new();
        summary.peer_id = Some(peer_id);

        for (index, sed_id) in sedimentree_ids.into_iter().enumerate() {
            let file_path = path_lookup.get(&sed_id).cloned();

            on_progress(SyncProgressEvent::SedimentreeStarted {
                sedimentree_id: sed_id,
                file_path: file_path.clone(),
                index,
                total,
            });

            // Sync this sedimentree with the peer
            match self
                .subduction
                .sync_with_peer(&peer_id, sed_id, true, Some(Self::DEFAULT_TIMEOUT))
                .await
            {
                Ok((success, stats, errors)) => {
                    tracing::debug!(
                        "sync_with_peer returned: success={}, stats=(recv={}, sent={})",
                        success,
                        stats.total_received(),
                        stats.total_sent()
                    );
                    if success {
                        summary.add_sync_stats(&stats);
                    }

                    for (conn, err) in &errors {
                        tracing::warn!(
                            "Sync error for {:?} with {:?}: {err:?}",
                            sed_id,
                            Connection::peer_id(conn)
                        );
                    }

                    on_progress(SyncProgressEvent::SedimentreeCompleted {
                        sedimentree_id: sed_id,
                        items_received: stats.total_received(),
                        items_sent: stats.total_sent(),
                        index,
                        total,
                    });
                }
                Err(e) => {
                    let err_msg = format!("{e}");
                    tracing::error!("Failed to sync {:?}: {}", sed_id, err_msg);
                    summary.add_error(sed_id, err_msg);

                    // Still report completion (with 0 items)
                    on_progress(SyncProgressEvent::SedimentreeCompleted {
                        sedimentree_id: sed_id,
                        items_received: 0,
                        items_sent: 0,
                        index,
                        total,
                    });
                }
            }
        }

        // After syncing known files, check for new files in the remote directory tree
        // and sync their sedimentrees
        if let Ok(new_count) = self.sync_missing_sedimentrees(manifest, &peer_id).await {
            if new_count > 0 {
                tracing::info!("Synced {} new sedimentrees from remote directory tree", new_count);
                summary.sedimentrees_synced += new_count;
            }
        }

        on_progress(SyncProgressEvent::Completed(summary.clone()));

        Ok(summary)
    }
}

/// A newly initialized workspace that hasn't opened Subduction yet.
///
/// Returned by `Darn::init()`. Use `peer_id()` to get the identity,
/// then call `open()` to get a full `Darn` instance.
#[derive(Debug)]
pub struct InitializedDarn {
    root: PathBuf,
}

impl InitializedDarn {
    /// Get the workspace root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the `.darn/` directory path.
    #[must_use]
    pub fn darn_dir(&self) -> PathBuf {
        self.root.join(DARN_DIR)
    }

    /// Get the peer ID from the global signer.
    ///
    /// # Errors
    ///
    /// Returns an error if the signer cannot be loaded.
    pub fn peer_id(&self) -> Result<PeerId, SignerLoadError> {
        let signer_dir = config::global_signer_dir()?;
        Ok(signer::peer_id(&signer_dir)?)
    }
}

/// A workspace opened without Subduction.
///
/// Returned by `Darn::open_without_subduction()`. Use for operations
/// that don't require the full Subduction instance (like peer management).
#[derive(Debug)]
pub struct UnopenedDarn {
    root: PathBuf,
}

impl UnopenedDarn {
    /// Get the workspace root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the `.darn/` directory path.
    #[must_use]
    pub fn darn_dir(&self) -> PathBuf {
        self.root.join(DARN_DIR)
    }

    /// Get the storage directory path.
    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.darn_dir().join(STORAGE_DIR)
    }

    /// Get the manifest file path.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.darn_dir().join(MANIFEST_FILE)
    }

    /// Load the manifest from disk.
    ///
    /// Returns an empty manifest if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded.
    pub fn load_manifest(&self) -> Result<Manifest, ManifestError> {
        Manifest::load(&self.manifest_path())
    }

    /// Save the manifest to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be saved.
    pub fn save_manifest(&self, manifest: &Manifest) -> Result<(), ManifestError> {
        manifest.save(&self.manifest_path())
    }

    /// Get the peer ID from the global signer.
    ///
    /// # Errors
    ///
    /// Returns an error if the signer cannot be loaded.
    pub fn peer_id(&self) -> Result<PeerId, SignerLoadError> {
        let signer_dir = config::global_signer_dir()?;
        Ok(signer::peer_id(&signer_dir)?)
    }

    /// Create a fresh Subduction instance (without hydrating from storage).
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    pub fn subduction(&self) -> Result<Arc<DarnSubduction>, SubductionError> {
        let signer = Darn::load_signer_from(&self.root)?;
        let storage = Darn::storage_from(&self.root)?;
        Ok(subduction::spawn(signer, storage))
    }

    /// Convert to a full `Darn` instance by hydrating Subduction.
    ///
    /// # Errors
    ///
    /// Returns an error if hydration fails.
    pub async fn hydrate(self) -> Result<Darn, OpenError> {
        let signer = Darn::load_signer_from(&self.root)?;
        let storage = Darn::storage_from(&self.root)?;
        let subduction = Box::pin(subduction::hydrate(signer, storage)).await?;
        Ok(Darn {
            root: self.root,
            subduction,
        })
    }

    /// List all globally configured peers.
    ///
    /// Peers are stored in `~/.config/darn/peers/`.
    ///
    /// # Errors
    ///
    /// Returns an error if the peers directory cannot be read.
    pub fn list_peers(&self) -> Result<Vec<crate::peer::Peer>, crate::peer::PeerError> {
        crate::peer::list_peers()
    }

    /// Get a globally configured peer by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the peer file cannot be read.
    pub fn get_peer(
        &self,
        name: &crate::peer::PeerName,
    ) -> Result<Option<crate::peer::Peer>, crate::peer::PeerError> {
        crate::peer::get_peer(name)
    }

    /// Add or update a globally configured peer.
    ///
    /// # Errors
    ///
    /// Returns an error if the peer file cannot be written.
    pub fn add_peer(&self, peer: &crate::peer::Peer) -> Result<(), crate::peer::PeerError> {
        crate::peer::add_peer(peer)
    }

    /// Remove a globally configured peer by name.
    ///
    /// Returns `true` if the peer was removed, `false` if it didn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the peer file cannot be removed.
    pub fn remove_peer(
        &self,
        name: &crate::peer::PeerName,
    ) -> Result<bool, crate::peer::PeerError> {
        crate::peer::remove_peer(name)
    }

    /// Load the global signer.
    ///
    /// # Errors
    ///
    /// Returns an error if the signer cannot be loaded.
    pub fn load_signer(&self) -> Result<MemorySigner, SignerLoadError> {
        Darn::load_signer_from(&self.root)
    }
}

/// Error opening a workspace.
#[derive(Debug, Error)]
pub enum OpenError {
    /// Not a workspace.
    #[error(transparent)]
    NotAWorkspace(#[from] NotAWorkspace),

    /// Signer error.
    #[error(transparent)]
    Signer(#[from] SignerLoadError),

    /// Storage error.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// Subduction initialization error.
    #[error(transparent)]
    Init(#[from] SubductionInitError),
}

/// Error creating Subduction instance.
#[derive(Debug, Error)]
pub enum SubductionError {
    /// Signer error.
    #[error(transparent)]
    Signer(#[from] SignerLoadError),

    /// Storage error.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// Subduction initialization error.
    #[error(transparent)]
    Init(#[from] SubductionInitError),
}

/// Error loading the signer.
#[derive(Debug, Error)]
pub enum SignerLoadError {
    /// Config directory error.
    #[error(transparent)]
    Config(#[from] NoConfigDir),

    /// Signer loading error.
    #[error(transparent)]
    Signer(#[from] LoadSignerError),
}

/// No workspace found (no `.darn` directory).
#[derive(Debug, Clone, Copy, Error)]
#[error("not a darn workspace (or any parent): .darn directory not found")]
pub struct NotAWorkspace;

/// Error initializing a workspace.
#[derive(Debug, Error)]
pub enum InitWorkspaceError {
    /// Workspace already exists.
    #[error("workspace already exists at {0}")]
    AlreadyExists(PathBuf),

    /// Config directory error.
    #[error(transparent)]
    Config(#[from] NoConfigDir),

    /// Signer error.
    #[error(transparent)]
    Signer(#[from] SignerError),

    /// Manifest error.
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Error creating `FsStorage`.
#[derive(Debug, Error)]
#[error("storage error: {0}")]
pub struct StorageError(#[from] sedimentree_fs_storage::FsStorageError);

/// File is outside the workspace root.
#[derive(Debug, Error)]
#[error("file is outside workspace: {0}")]
pub struct FileOutsideWorkspace(PathBuf);

impl FileOutsideWorkspace {
    /// Creates a new error for a path outside the workspace.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

/// Result of syncing with a peer.
#[derive(Debug, Clone, Copy)]
pub struct SyncResult {
    /// The peer ID we connected to.
    pub peer_id: PeerId,

    /// Whether any sync operations succeeded.
    pub success: bool,

    /// Statistics about the sync operation.
    pub stats: subduction_core::connection::stats::SyncStats,
}

/// Errors discovering new files.
#[derive(Debug, Error)]
pub enum DiscoverError {
    /// Failed to build ignore rules.
    #[error(transparent)]
    Ignore(#[from] crate::ignore::IgnorePatternError),
}

/// Errors from sync operations.
#[derive(Debug, Error)]
pub enum SyncError {
    /// Invalid WebSocket URI.
    #[error(transparent)]
    InvalidUri(#[from] tungstenite::http::uri::InvalidUri),

    /// Error loading signer.
    #[error(transparent)]
    Signer(#[from] SignerLoadError),

    /// Connection error.
    #[error(transparent)]
    Connection(#[from] subduction_websocket::tokio::client::ClientConnectError),

    /// Error attaching connection.
    #[error(transparent)]
    Attach(#[from] DarnAttachError),

    /// Error registering connection.
    #[error(transparent)]
    Registration(#[from] DarnRegistrationError),

    /// Sync error.
    #[error(transparent)]
    Sync(#[from] DarnIoError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_home<F, R>(f: F) -> R
    where
        F: FnOnce(&Path) -> R,
    {
        let dir = tempfile::tempdir().expect("create tempdir");
        // Note: We can't easily override HOME in tests, so these tests
        // will use the real global signer. Integration tests would be
        // better for testing with isolated HOME.
        f(dir.path())
    }

    #[test]
    fn init_creates_darn_directory_structure() {
        with_temp_home(|temp_dir| {
            let ws = Darn::init(temp_dir).expect("init workspace");

            let darn_dir = temp_dir.join(DARN_DIR);
            assert!(darn_dir.is_dir(), ".darn directory should exist");

            let storage_dir = darn_dir.join(STORAGE_DIR);
            assert!(storage_dir.is_dir(), "storage directory should exist");

            assert!(
                ws.root()
                    .ends_with(temp_dir.file_name().unwrap_or_default())
            );
        });
    }

    #[test]
    fn init_twice_fails() {
        with_temp_home(|temp_dir| {
            Darn::init(temp_dir).expect("first init");

            let result = Darn::init(temp_dir);
            assert!(result.is_err(), "second init should fail");

            match result {
                Err(InitWorkspaceError::AlreadyExists(_)) => {}
                other => panic!("expected AlreadyExists, got {other:?}"),
            }
        });
    }

    #[test]
    fn open_without_subduction_existing_workspace() {
        with_temp_home(|temp_dir| {
            Darn::init(temp_dir).expect("init");

            let ws = Darn::open_without_subduction(temp_dir).expect("open");
            assert!(ws.darn_dir().is_dir());
        });
    }

    #[test]
    fn open_nonexistent_fails() {
        with_temp_home(|temp_dir| {
            let result = Darn::open_without_subduction(temp_dir);
            assert!(result.is_err(), "open without init should fail");

            match result {
                Err(NotAWorkspace) => {}
                other => panic!("expected NotAWorkspace, got {other:?}"),
            }
        });
    }

    #[test]
    fn open_from_subdirectory() {
        with_temp_home(|temp_dir| {
            Darn::init(temp_dir).expect("init");

            let subdir = temp_dir.join("src").join("deep").join("nested");
            std::fs::create_dir_all(&subdir).expect("create subdirs");

            let ws = Darn::open_without_subduction(&subdir).expect("open from nested subdir");
            assert!(ws.darn_dir().is_dir());
        });
    }

    #[test]
    fn find_root_from_nested_directory() {
        with_temp_home(|temp_dir| {
            Darn::init(temp_dir).expect("init");

            let subdir = temp_dir.join("a").join("b").join("c");
            std::fs::create_dir_all(&subdir).expect("create subdirs");

            let root = Darn::find_root(&subdir).expect("find_root");
            assert!(root.join(DARN_DIR).is_dir());
        });
    }

    #[test]
    fn find_root_not_found() {
        with_temp_home(|temp_dir| {
            let result = Darn::find_root(temp_dir);
            assert!(result.is_err());

            match result {
                Err(NotAWorkspace) => {}
                other => panic!("expected NotAWorkspace, got {other:?}"),
            }
        });
    }

    #[test]
    fn unopened_darn_paths() {
        with_temp_home(|temp_dir| {
            Darn::init(temp_dir).expect("init");
            let ws = Darn::open_without_subduction(temp_dir).expect("open");

            assert_eq!(ws.darn_dir(), ws.root().join(DARN_DIR));
            assert_eq!(ws.storage_dir(), ws.darn_dir().join(STORAGE_DIR));
        });
    }

    #[test]
    fn root_accessor() {
        with_temp_home(|temp_dir| {
            let ws = Darn::init(temp_dir).expect("init");

            // Root should be the canonicalized temp_dir
            assert!(ws.root().is_absolute());
            assert!(ws.root().is_dir());
        });
    }
}
