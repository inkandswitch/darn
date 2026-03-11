//! Darn workspace management.
//!
//! A workspace is a directory containing a `.darn` JSON marker file. All
//! storage lives under `~/.config/darn/workspaces/<id>/`.

pub mod refresh_diff;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sedimentree_core::id::SedimentreeId;
use sedimentree_fs_storage::FsStorage;
use subduction_core::{connection::Connection, peer::id::PeerId};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::tokio::{TimeoutTokio, client::TokioWebSocketClient};
use thiserror::Error;
use tungstenite::http::Uri;

use crate::peer::PeerAddress;

use tokio_util::sync::CancellationToken;

use crate::{
    attributes::AttributeRules,
    config::{self, NoConfigDir},
    directory::{Directory, entry::EntryType},
    discover::{self, DiscoverProgress, DiscoverResult},
    dotfile::{DarnConfig, DotfileError},
    file::{File, file_type::FileType, state::FileState},
    manifest::{Manifest, ManifestError, content_hash, tracked::Tracked},
    peer::Peer,
    refresh::{self, RefreshError},
    sedimentree::{self, SedimentreeError},
    signer::{self, LoadSignerError, SignerError},
    staged_update::{StageError, StagedUpdate},
    subduction::{
        self, AuthenticatedDarnConnection, DarnAttachError, DarnIoError, DarnRegistrationError,
        DarnSubduction, SubductionInitError,
    },
    sync_progress::{ApplyResult, SyncProgressEvent, SyncSummary},
    workspace::{id::WorkspaceId, layout::WorkspaceLayout},
};
use refresh_diff::RefreshDiff;

/// A `darn` workspace rooted at a directory.
///
/// The `Darn` struct manages a workspace with:
/// - A `.darn` marker file in the project root
/// - Centralized storage under `~/.config/darn/workspaces/<id>/`
/// - Automerge document storage via Subduction
/// - Peer configuration for sync
/// - An Iroh endpoint for peer-to-peer QUIC transport (when the `iroh` feature is enabled)
#[derive(Debug)]
pub struct Darn {
    /// The root directory of the workspace (contains `.darn` file).
    root: PathBuf,

    /// Loaded configuration from the `.darn` file.
    config: DarnConfig,

    /// Paths into `~/.config/darn/workspaces/<id>/`.
    layout: WorkspaceLayout,

    /// The Subduction instance for this workspace.
    subduction: Arc<DarnSubduction>,

    /// Long-lived Iroh endpoint for QUIC transport.
    ///
    /// Created once at open time using the persistent Ed25519 signing key,
    /// giving this node a stable Iroh node ID. Used for both outgoing
    /// connections and accepting incoming ones.
    #[cfg(feature = "iroh")]
    iroh_endpoint: iroh::Endpoint,
}

impl Darn {
    /// Initialize a new workspace at the given path.
    ///
    /// Creates a `.darn` marker file, centralized storage under
    /// `~/.config/darn/workspaces/<id>/`, and ensures the global signer exists.
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

        // Check for existing workspace
        let dotfile_path = root.join(crate::dotfile::DOTFILE_NAME);
        if dotfile_path.exists() {
            return Err(InitWorkspaceError::AlreadyExists(root));
        }

        // Generate workspace ID from canonical path
        let id = WorkspaceId::from_path(&root);

        // Create initial manifest with random root directory ID
        let manifest = Manifest::new();
        let root_directory_id = manifest.root_directory_id();

        // Create centralized storage directory
        let layout = WorkspaceLayout::new(id)?;
        layout.create_dirs()?;

        // Save manifest to centralized location
        manifest.save(&layout.manifest_path())?;

        // Create .darn marker file with default ignore/attribute patterns
        let config = DarnConfig::create(&root, id, root_directory_id)?;

        // Ensure global signer exists
        let signer_dir = config::global_signer_dir()?;
        let signer = signer::load_or_generate(&signer_dir)?;

        let peer_id: PeerId = signer.verifying_key().into();
        tracing::info!(
            path = %root.display(),
            workspace_id = %id.to_hex(),
            peer_id = %hex::encode(peer_id.as_bytes()),
            "Initialized workspace"
        );

        Ok(InitializedDarn {
            root,
            config,
            layout,
        })
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

        // Check for existing workspace
        let dotfile_path = root.join(crate::dotfile::DOTFILE_NAME);
        if dotfile_path.exists() {
            return Err(InitWorkspaceError::AlreadyExists(root));
        }

        // Generate workspace ID from canonical path
        let id = WorkspaceId::from_path(&root);

        // Create centralized storage directory
        let layout = WorkspaceLayout::new(id)?;
        layout.create_dirs()?;

        // Save manifest with provided root directory ID to centralized location
        let manifest = Manifest::with_root_id(root_directory_id);
        manifest.save(&layout.manifest_path())?;

        // Create .darn marker file with default ignore/attribute patterns
        let config = DarnConfig::create(&root, id, root_directory_id)?;

        // Ensure global signer exists
        let signer_dir = config::global_signer_dir()?;
        let signer = signer::load_or_generate(&signer_dir)?;

        let peer_id: PeerId = signer.verifying_key().into();
        tracing::info!(
            path = %root.display(),
            workspace_id = %id.to_hex(),
            root_directory_id = %bs58::encode(root_directory_id.as_bytes()).into_string(),
            peer_id = %hex::encode(peer_id.as_bytes()),
            "Initialized workspace with root directory ID"
        );

        Ok(InitializedDarn {
            root,
            config,
            layout,
        })
    }

    /// Open an existing workspace and hydrate Subduction from storage.
    ///
    /// Searches for a `.darn` file starting from the given path and walking up
    /// to parent directories. Loads all existing sedimentrees from storage.
    ///
    /// # Errors
    ///
    /// Returns an error if no workspace is found or Subduction cannot be initialized.
    pub async fn open(path: &Path) -> Result<Self, OpenError> {
        let root = Self::find_root(path)?;
        let config = DarnConfig::load(&root)?;
        let layout = WorkspaceLayout::new(config.id)?;

        let signer = Self::load_signer_static()?;
        let storage = Self::storage_from_layout(&layout)?;
        let subduction = Box::pin(subduction::hydrate(signer, storage)).await?;

        #[cfg(feature = "iroh")]
        let iroh_endpoint = {
            let signer_dir = config::global_signer_dir()?;
            let key_bytes = signer::load_key_bytes(&signer_dir).map_err(SignerLoadError::from)?;
            let secret_key = iroh::SecretKey::from_bytes(&key_bytes);
            iroh::Endpoint::builder()
                .secret_key(secret_key)
                .alpns(vec![subduction_iroh::ALPN.to_vec()])
                .bind()
                .await
                .map_err(|e| OpenError::IrohBind(e.to_string()))?
        };

        Ok(Self {
            root,
            config,
            layout,
            subduction,
            #[cfg(feature = "iroh")]
            iroh_endpoint,
        })
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
        let config = DarnConfig::load(&root).map_err(|_| NotAWorkspace)?;
        let layout = WorkspaceLayout::new(config.id).map_err(|_| NotAWorkspace)?;
        Ok(UnopenedDarn {
            root,
            config,
            layout,
        })
    }

    /// Find the workspace root by walking up the directory tree.
    ///
    /// # Errors
    ///
    /// Returns an error if no `.darn` file is found.
    pub fn find_root(start: &Path) -> Result<PathBuf, NotAWorkspace> {
        DarnConfig::find_root(start).map_err(|_| NotAWorkspace)
    }

    /// Get the workspace root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the loaded `.darn` config.
    #[must_use]
    pub const fn config(&self) -> &DarnConfig {
        &self.config
    }

    /// Get the workspace layout (centralized storage paths).
    #[must_use]
    pub const fn layout(&self) -> &WorkspaceLayout {
        &self.layout
    }

    /// Get the storage directory path (`~/.config/darn/workspaces/<id>/storage/`).
    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.layout.storage_dir()
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
        Self::load_signer_static()
    }

    /// Load the global signer (static helper).
    fn load_signer_static() -> Result<MemorySigner, SignerLoadError> {
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

    /// Get a reference to the long-lived Iroh endpoint.
    ///
    /// This endpoint uses the persistent Ed25519 signing key, giving
    /// this node a stable Iroh node ID across restarts. Use it for
    /// both outgoing connections and accepting incoming ones.
    #[cfg(feature = "iroh")]
    #[must_use]
    pub const fn iroh_endpoint(&self) -> &iroh::Endpoint {
        &self.iroh_endpoint
    }

    /// Get the Iroh public key derived from the persistent signing key.
    ///
    /// This is the public identity other Iroh peers use to address
    /// this node.
    #[cfg(feature = "iroh")]
    #[must_use]
    pub fn iroh_public_key(&self) -> iroh::PublicKey {
        self.iroh_endpoint.secret_key().public()
    }

    /// Get the full Iroh endpoint address (public key + relay URL + direct addresses).
    #[cfg(feature = "iroh")]
    #[must_use]
    pub fn iroh_addr(&self) -> iroh::EndpointAddr {
        self.iroh_endpoint.addr()
    }

    /// Create storage from a workspace layout.
    fn storage_from_layout(layout: &WorkspaceLayout) -> Result<FsStorage, StorageError> {
        FsStorage::new(layout.storage_dir()).map_err(StorageError)
    }

    /// Get the manifest file path (`~/.config/darn/workspaces/<id>/manifest.json`).
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.layout.manifest_path()
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
        Self::storage_from_layout(&self.layout)
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

        // Load attribute rules for consistent file type detection
        let attributes = AttributeRules::from_workspace_root(&self.root).ok();

        // Read current file content, coercing to match the stored file type
        let current_fs_digest = content_hash::hash_file(&path)?;
        let mut new_file = File::from_path_with_attributes(&path, attributes.as_ref())?;
        new_file.content = new_file.content.coerce_to(entry.file_type);

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
    /// Modified files are processed in parallel (read, diff, store),
    /// then manifest entries are patched sequentially.
    ///
    /// Returns a summary of which files were updated, missing, or had errors.
    pub async fn refresh_all(&self, manifest: &mut Manifest) -> RefreshDiff {
        use futures::{StreamExt, stream};
        use std::sync::Mutex;

        let mut diff = RefreshDiff::default();

        // Phase 1: Classify files (fast — only compares hashes)
        let mut modified: Vec<RefreshCandidate> = Vec::new();

        for entry in manifest.iter() {
            let path = entry.relative_path.clone();
            match entry.state(&self.root) {
                FileState::Clean => {}
                FileState::Missing => diff.missing.push(path),
                FileState::Modified => modified.push(RefreshCandidate {
                    path,
                    sedimentree_id: entry.sedimentree_id,
                    file_type: entry.file_type,
                    current_fs_digest: entry.file_system_digest,
                }),
            }
        }

        if modified.is_empty() {
            return diff;
        }

        // Phase 2: Refresh files in parallel
        let concurrency = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);

        let results: Mutex<Vec<RefreshResult>> = Mutex::new(Vec::new());

        stream::iter(&modified)
            .for_each_concurrent(concurrency, |candidate| {
                let results = &results;

                async move {
                    let outcome = self.refresh_file_by_id(candidate).await;
                    if let Ok(mut r) = results.lock() {
                        r.push(RefreshResult {
                            path: candidate.path.clone(),
                            sedimentree_id: candidate.sedimentree_id,
                            outcome,
                        });
                    }
                }
            })
            .await;

        let results = results.into_inner().unwrap_or_default();

        // Phase 3: Patch manifest sequentially
        for result in results {
            match result.outcome {
                Ok(Some(digests)) => {
                    if let Some(entry) = manifest.get_by_id_mut(&result.sedimentree_id) {
                        entry.file_system_digest = digests.file_system;
                        entry.sedimentree_digest = digests.sedimentree;
                    }
                    diff.updated.push(result.path);
                }
                Ok(None) => {} // Not actually modified (race)
                Err(e) => diff.errors.push((result.path, e)),
            }
        }

        diff
    }

    /// Refresh a single file, returning updated digests.
    ///
    /// This is the parallelizable core of refresh: reads the file, loads the
    /// existing Automerge doc, applies the diff, stores changes, and computes
    /// new digests. Does not touch the manifest.
    async fn refresh_file_by_id(
        &self,
        candidate: &RefreshCandidate,
    ) -> Result<Option<RefreshedDigests>, RefreshError> {
        let path = self.root.join(&candidate.path);

        // Re-check: hash file to confirm it's still modified
        let new_fs_digest = content_hash::hash_file(&path)?;
        let sed_id = candidate.sedimentree_id;

        if new_fs_digest == candidate.current_fs_digest {
            return Ok(None); // Raced — file reverted
        }

        let attributes = AttributeRules::from_workspace_root(&self.root).ok();
        let mut new_file = File::from_path_with_attributes(&path, attributes.as_ref())?;
        new_file.content = new_file.content.coerce_to(candidate.file_type);

        // Load existing doc
        let mut am_doc = sedimentree::load_document(&self.subduction, sed_id)
            .await
            .map_err(|e| RefreshError::Storage(Box::new(e)))?
            .ok_or_else(|| RefreshError::Storage(Box::new(SedimentreeError::NotFound)))?;

        let old_heads = am_doc.get_heads();
        refresh::update_automerge_content(&mut am_doc, new_file.content)?;

        sedimentree::add_changes(&self.subduction, sed_id, &mut am_doc, &old_heads)
            .await
            .map_err(|e| RefreshError::Storage(Box::new(e)))?;

        let new_sed_digest = sedimentree::compute_digest(&self.subduction, sed_id)
            .await
            .map_err(|e| RefreshError::Storage(Box::new(e)))?;

        Ok(Some(RefreshedDigests {
            file_system: new_fs_digest,
            sedimentree: new_sed_digest,
        }))
    }

    /// Stage all remote changes for batch application to the workspace.
    ///
    /// This is the slow "prepare" phase: for each tracked file whose
    /// sedimentree digest changed (indicating remote updates), loads the
    /// merged CRDT document, serializes the content, and writes it to a
    /// staging directory. Also discovers new files from the remote
    /// directory tree that aren't in the local manifest. No workspace
    /// files are modified until [`StagedUpdate::commit`] is called.
    ///
    /// # Errors
    ///
    /// Individual file errors are collected in [`ApplyResult`]; the
    /// `StagedUpdate` contains only the successfully staged operations.
    #[allow(clippy::too_many_lines)]
    pub async fn stage_remote_changes(
        &self,
        manifest: &Manifest,
    ) -> Result<(StagedUpdate, ApplyResult), StageError> {
        let mut staged = StagedUpdate::new(&self.root)?;
        let mut errors = ApplyResult::new();

        tracing::debug!(
            manifest_entries = manifest.iter().count(),
            "stage_remote_changes: checking for changes"
        );

        // Step 1: Stage updates to existing tracked files
        for entry in manifest.iter() {
            let path = entry.relative_path.clone();

            let new_sed_digest =
                match sedimentree::compute_digest(&self.subduction, entry.sedimentree_id).await {
                    Ok(d) => d,
                    Err(e) => {
                        errors.errors.push((path, format!("compute digest: {e}")));
                        continue;
                    }
                };

            tracing::debug!(
                path = %path.display(),
                old_digest = %entry.sedimentree_digest,
                new_digest = %new_sed_digest,
                changed = new_sed_digest != entry.sedimentree_digest,
                "stage_remote_changes: checking file"
            );

            if new_sed_digest == entry.sedimentree_digest {
                continue; // No remote changes
            }

            let local_changed = entry.state(&self.root) == FileState::Modified;

            let am_doc =
                match sedimentree::load_document(&self.subduction, entry.sedimentree_id).await {
                    Ok(Some(doc)) => doc,
                    Ok(None) => {
                        errors
                            .errors
                            .push((path, "document not found after sync".into()));
                        continue;
                    }
                    Err(e) => {
                        errors.errors.push((path, format!("load document: {e}")));
                        continue;
                    }
                };

            let file = match File::from_automerge(&am_doc) {
                Ok(f) => f,
                Err(e) => {
                    errors.errors.push((path, format!("parse file: {e}")));
                    continue;
                }
            };

            let file_type = FileType::from(&file.content);

            if let Err(e) = staged.stage_write(
                &file,
                path.clone(),
                entry.sedimentree_id,
                file_type,
                new_sed_digest,
                local_changed,
            ) {
                errors.errors.push((path, format!("stage write: {e}")));
            }
        }

        // Step 2: Stage new files from remote directory tree
        if let Err(e) = self
            .stage_remote_files_recursive(
                manifest.root_directory_id(),
                PathBuf::new(),
                manifest,
                &mut staged,
                &mut errors,
            )
            .await
        {
            tracing::warn!("Error staging remote files: {e}");
        }

        // Step 3: Stage deletions (files not in remote tree)
        if let Err(e) = self
            .stage_deleted_files(manifest, &mut staged, &mut errors)
            .await
        {
            tracing::warn!("Error staging deleted files: {e}");
        }

        Ok((staged, errors))
    }

    /// Convenience wrapper: stage + commit in one call.
    ///
    /// Equivalent to calling [`stage_remote_changes`](Self::stage_remote_changes)
    /// followed by [`StagedUpdate::commit`].
    pub async fn apply_remote_changes(&self, manifest: &mut Manifest) -> ApplyResult {
        let (staged, mut stage_errors) = match self.stage_remote_changes(manifest).await {
            Ok(pair) => pair,
            Err(e) => {
                let mut result = ApplyResult::new();
                result
                    .errors
                    .push((PathBuf::new(), format!("staging failed: {e}")));
                return result;
            }
        };

        if staged.is_empty() {
            return stage_errors;
        }

        match staged.commit(manifest).await {
            Ok(mut result) => {
                // Merge any staging errors into the commit result
                result.errors.append(&mut stage_errors.errors);
                result
            }
            Err(e) => {
                stage_errors
                    .errors
                    .push((PathBuf::new(), format!("commit failed: {e}")));
                stage_errors
            }
        }
    }

    /// Recursively stage new files from a remote directory.
    #[allow(clippy::only_used_in_recursion)]
    #[allow(clippy::too_many_lines)]
    async fn stage_remote_files_recursive(
        &self,
        dir_id: sedimentree_core::id::SedimentreeId,
        current_path: PathBuf,
        manifest: &Manifest,
        staged: &mut StagedUpdate,
        errors: &mut ApplyResult,
    ) -> Result<(), SedimentreeError> {
        tracing::debug!(
            ?dir_id,
            path = %current_path.display(),
            "stage_remote_files_recursive: loading directory"
        );

        let Some(am_doc) = sedimentree::load_document(&self.subduction, dir_id).await? else {
            tracing::debug!(?dir_id, "stage_remote_files_recursive: empty directory");
            return Ok(());
        };

        let Ok(dir) = Directory::from_automerge(&am_doc) else {
            tracing::debug!(?dir_id, "stage_remote_files_recursive: not a directory");
            return Ok(());
        };

        tracing::debug!(
            ?dir_id,
            entry_count = dir.entries.len(),
            "stage_remote_files_recursive: found entries"
        );

        for entry in &dir.entries {
            let entry_path = current_path.join(&entry.name);

            tracing::debug!(
                name = %entry.name,
                entry_type = ?entry.entry_type,
                sed_id = ?entry.sedimentree_id,
                "stage_remote_files_recursive: processing entry"
            );

            match entry.entry_type {
                EntryType::File => {
                    if manifest.get_by_id(&entry.sedimentree_id).is_some() {
                        tracing::debug!(name = %entry.name, "already tracked, skipping");
                        continue;
                    }

                    tracing::info!(name = %entry.name, "discovered new remote file");

                    let am_doc =
                        match sedimentree::load_document(&self.subduction, entry.sedimentree_id)
                            .await
                        {
                            Ok(Some(doc)) => doc,
                            Ok(None) => continue,
                            Err(e) => {
                                errors
                                    .errors
                                    .push((entry_path.clone(), format!("load file: {e}")));
                                continue;
                            }
                        };

                    let file = match File::from_automerge(&am_doc) {
                        Ok(f) => f,
                        Err(e) => {
                            errors
                                .errors
                                .push((entry_path.clone(), format!("parse file: {e}")));
                            continue;
                        }
                    };

                    let file_type = FileType::from(&file.content);

                    let sed_digest =
                        match sedimentree::compute_digest(&self.subduction, entry.sedimentree_id)
                            .await
                        {
                            Ok(d) => d,
                            Err(e) => {
                                errors
                                    .errors
                                    .push((entry_path.clone(), format!("compute digest: {e}")));
                                continue;
                            }
                        };

                    if let Err(e) = staged.stage_create(
                        &file,
                        entry_path.clone(),
                        entry.sedimentree_id,
                        file_type,
                        sed_digest,
                    ) {
                        errors
                            .errors
                            .push((entry_path, format!("stage create: {e}")));
                    }
                }

                EntryType::Folder => {
                    Box::pin(self.stage_remote_files_recursive(
                        entry.sedimentree_id,
                        entry_path,
                        manifest,
                        staged,
                        errors,
                    ))
                    .await?;
                }
            }
        }

        Ok(())
    }

    /// Stage deletions for files no longer in the remote directory tree.
    async fn stage_deleted_files(
        &self,
        manifest: &Manifest,
        staged: &mut StagedUpdate,
        errors: &mut ApplyResult,
    ) -> Result<(), SedimentreeError> {
        use std::collections::HashSet;

        let mut remote_ids = HashSet::new();
        let root_dir_id = manifest.root_directory_id();
        self.collect_remote_sedimentree_ids(root_dir_id, &mut remote_ids)
            .await?;

        let to_delete: Vec<_> = manifest
            .iter()
            .filter(|entry| !remote_ids.contains(&entry.sedimentree_id))
            .map(|entry| (entry.sedimentree_id, entry.relative_path.clone()))
            .collect();

        for (sed_id, relative_path) in to_delete {
            let full_path = self.root.join(&relative_path);
            if full_path.exists() {
                staged.stage_delete(relative_path, sed_id);
            } else {
                // File already missing — just clean up manifest
                staged.stage_delete(relative_path.clone(), sed_id);
                tracing::debug!(path = %relative_path.display(), "already missing from disk");
            }
        }

        // Suppress unused variable warning — errors param is for consistency with
        // sibling methods and future use
        let _ = errors;

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
        let Some(am_doc) = sedimentree::load_document(&self.subduction, dir_id).await? else {
            tracing::debug!(?dir_id, "No directory document found (empty)");
            return Ok(()); // Empty directory
        };

        let Ok(dir) = Directory::from_automerge(&am_doc) else {
            tracing::debug!(?dir_id, "Failed to parse as directory");
            return Ok(()); // Not a directory document
        };

        tracing::debug!(
            ?dir_id,
            entries = dir.entries.len(),
            "Loaded directory with entries"
        );

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
    ///
    /// # Errors
    ///
    /// Returns `SyncError` if the remote directory cannot be loaded or
    /// if individual sedimentree syncs fail.
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
        tracing::debug!(
            known_count = known_ids.len(),
            "sync_missing_sedimentrees: known IDs from manifest"
        );

        // Collect all IDs from the remote directory tree
        let mut remote_ids = HashSet::new();
        if let Err(e) = self
            .collect_all_sedimentree_ids(manifest.root_directory_id(), &mut remote_ids)
            .await
        {
            tracing::warn!("Error collecting remote sedimentree IDs: {e}");
        }
        tracing::debug!(
            remote_count = remote_ids.len(),
            "sync_missing_sedimentrees: remote IDs from directory tree"
        );

        // Find IDs we don't have
        let missing: Vec<_> = remote_ids.difference(&known_ids).copied().collect();
        tracing::debug!(
            missing_count = missing.len(),
            "sync_missing_sedimentrees: missing IDs"
        );

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
        let Some(am_doc) = sedimentree::load_document(&self.subduction, dir_id).await? else {
            tracing::debug!(?dir_id, "collect_all_sedimentree_ids: no document found");
            return Ok(()); // Empty directory
        };

        let Ok(dir) = Directory::from_automerge(&am_doc) else {
            tracing::debug!(
                ?dir_id,
                "collect_all_sedimentree_ids: not a directory document"
            );
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

    /// Discover and track new (untracked, non-ignored) files.
    ///
    /// Scan for new untracked files without ingesting them.
    ///
    /// Returns paths of files that could be tracked. This is fast and has no
    /// side effects—use `ingest_files()` to actually process and store them.
    ///
    /// # Errors
    ///
    /// Returns an error if ignore rules cannot be loaded.
    pub fn scan_new_files(&self, manifest: &Manifest) -> Result<Vec<PathBuf>, DiscoverError> {
        Ok(discover::scan_new_files(&self.root, manifest)?)
    }

    /// Ingest files into storage and add to manifest.
    ///
    /// Takes a list of paths (from `scan_new_files()`) and processes them:
    /// reads content, converts to Automerge, stores in sedimentree, updates
    /// directory tree, and adds to manifest.
    ///
    /// Files are processed in parallel for performance.
    ///
    /// # Arguments
    ///
    /// * `paths` - Paths to ingest (from `scan_new_files()`)
    /// * `manifest` - The manifest to update with new files
    /// * `on_progress` - Callback for progress updates
    /// * `cancel` - Cancellation token; if cancelled, returns immediately with partial results
    ///
    /// # Errors
    ///
    /// Returns an error if ingestion fails fatally.
    /// Individual file errors are collected in the result.
    pub async fn ingest_files<F>(
        &self,
        paths: Vec<PathBuf>,
        manifest: &mut Manifest,
        force_immutable: bool,
        on_progress: F,
        cancel: &CancellationToken,
    ) -> Result<DiscoverResult, DiscoverError>
    where
        F: Fn(DiscoverProgress<'_>) + Send + Sync,
    {
        let (discovered, errors, cancelled) = discover::ingest_files_parallel(
            paths,
            &self.root,
            &self.subduction,
            manifest,
            force_immutable,
            on_progress,
            cancel,
        )
        .await;

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
    /// Dispatches to the appropriate transport based on the peer's address:
    /// - `PeerAddress::WebSocket` → `TokioWebSocketClient`
    /// - `PeerAddress::Iroh` → `subduction_iroh::client::connect`
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established.
    pub async fn connect_peer(
        &self,
        peer: &Peer,
    ) -> Result<(AuthenticatedDarnConnection, PeerId), SyncError> {
        match &peer.address {
            PeerAddress::WebSocket { url } => self.connect_peer_ws(url, peer.audience).await,
            #[cfg(feature = "iroh")]
            PeerAddress::Iroh { node_id, relay_url } => {
                self.connect_peer_iroh(node_id, relay_url.as_deref(), peer.audience)
                    .await
            }
        }
    }

    /// Connect to a peer via WebSocket.
    async fn connect_peer_ws(
        &self,
        ws_url: &str,
        audience: subduction_core::connection::handshake::Audience,
    ) -> Result<(AuthenticatedDarnConnection, PeerId), SyncError> {
        let uri: Uri = ws_url.parse()?;
        let signer = self.load_signer()?;

        let (authenticated, listener_fut, sender_fut) =
            TokioWebSocketClient::new(uri, TimeoutTokio, Self::DEFAULT_TIMEOUT, signer, audience)
                .await?;

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

        let actual_peer_id = authenticated.peer_id();
        let authenticated =
            authenticated.map(|c| crate::subduction::DarnConnection::WebSocket(Box::new(c)));

        Ok((authenticated, actual_peer_id))
    }

    /// Connect to a peer via Iroh (QUIC).
    #[cfg(feature = "iroh")]
    async fn connect_peer_iroh(
        &self,
        node_id: &str,
        relay_url: Option<&str>,
        audience: subduction_core::connection::handshake::Audience,
    ) -> Result<(AuthenticatedDarnConnection, PeerId), SyncError> {
        let public_key: iroh::PublicKey = node_id.parse().map_err(SyncError::IrohNodeId)?;

        let mut addr = iroh::EndpointAddr::new(public_key);
        if let Some(relay) = relay_url {
            let parsed: iroh::RelayUrl = relay.parse().map_err(SyncError::IrohRelayUrl)?;
            addr = addr.with_relay_url(parsed);
        }

        let signer = self.load_signer()?;

        let result = subduction_iroh::client::connect(
            &self.iroh_endpoint,
            addr,
            Self::DEFAULT_TIMEOUT,
            TimeoutTokio,
            &signer,
            audience,
        )
        .await?;

        tokio::spawn(async move {
            if let Err(e) = result.listener_task.await {
                tracing::error!("Iroh listener error: {e:?}");
            }
        });
        tokio::spawn(async move {
            if let Err(e) = result.sender_task.await {
                tracing::error!("Iroh sender error: {e:?}");
            }
        });

        let actual_peer_id = result.authenticated.peer_id();
        let authenticated = result
            .authenticated
            .map(crate::subduction::DarnConnection::Iroh);

        Ok((authenticated, actual_peer_id))
    }

    /// Accept incoming Iroh connections in a loop until cancelled.
    ///
    /// Each accepted connection is authenticated via the Subduction handshake
    /// and registered with the Subduction instance, enabling bidirectional sync.
    /// Background listener/sender tasks are spawned for each connection.
    ///
    /// The loop runs until the `cancel` token is triggered (e.g., on Ctrl+C).
    #[cfg(feature = "iroh")]
    pub async fn accept_iroh_connections(&self, cancel: CancellationToken) {
        use subduction_core::connection::{handshake::Audience, nonce_cache::NonceCache};

        let signer = match self.load_signer() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to load signer for Iroh accept loop: {e}");
                return;
            }
        };

        let our_peer_id: PeerId = signer.verifying_key().into();
        let nonce_cache = NonceCache::default();
        let handshake_max_drift = Duration::from_secs(600);

        // Clients in discovery mode derive their audience from the Iroh node ID
        // string (the service name for PeerAddress::Iroh). We must accept that
        // same audience on the server side.
        let iroh_public_key_str = self.iroh_endpoint.secret_key().public().to_string();
        let discovery_audience = Audience::discover(iroh_public_key_str.as_bytes());

        tracing::info!(
            iroh_public_key = %iroh_public_key_str,
            "Iroh accept loop started"
        );

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    tracing::info!("Iroh accept loop cancelled");
                    break;
                }
                result = subduction_iroh::server::accept_one(
                    &self.iroh_endpoint,
                    Self::DEFAULT_TIMEOUT,
                    TimeoutTokio,
                    &signer,
                    &nonce_cache,
                    our_peer_id,
                    Some(discovery_audience),
                    handshake_max_drift,
                ) => {
                    match result {
                        Ok(accept_result) => {
                            let peer_id = accept_result.peer_id;
                            tracing::info!(%peer_id, "Accepted incoming Iroh connection");

                            tokio::spawn(async move {
                                if let Err(e) = accept_result.listener_task.await {
                                    tracing::error!(%peer_id, "Iroh listener error: {e:?}");
                                }
                            });
                            tokio::spawn(async move {
                                if let Err(e) = accept_result.sender_task.await {
                                    tracing::error!(%peer_id, "Iroh sender error: {e:?}");
                                }
                            });

                            let authenticated = accept_result
                                .authenticated
                                .map(crate::subduction::DarnConnection::Iroh);

                            if let Err(e) = self.subduction.register(authenticated).await {
                                tracing::error!(%peer_id, "Failed to register Iroh connection: {e:?}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Iroh accept error: {e:?}");
                        }
                    }
                }
            }
        }
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
        let (success, stats, call_errors, io_errors) =
            self.subduction.full_sync(Some(Self::DEFAULT_TIMEOUT)).await;

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
            address: peer.address.display_addr(),
        });

        let (authenticated_connection, peer_id) = self.connect_peer(peer).await?;
        tracing::info!("Connected to peer {}", peer_id);

        on_progress(SyncProgressEvent::Connected { peer_id });

        // Register the authenticated connection (without auto-syncing - we handle sync manually below)
        self.subduction.register(authenticated_connection).await?;

        // Collect ALL sedimentree IDs to sync (root, all directories, and all files)
        let root_dir_id = manifest.root_directory_id();
        let mut all_ids = std::collections::HashSet::new();
        all_ids.insert(root_dir_id);

        // Traverse directory tree to find all subdirectories
        if let Err(e) = self
            .collect_all_sedimentree_ids(root_dir_id, &mut all_ids)
            .await
        {
            tracing::warn!("Error collecting directory tree IDs: {e}");
        }

        // Also include all file IDs from manifest (in case tree traversal missed any)
        all_ids.extend(manifest.iter().map(|e| e.sedimentree_id));

        let sedimentree_ids: Vec<_> = all_ids.into_iter().collect();

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
        if let Ok(new_count) = self.sync_missing_sedimentrees(manifest, &peer_id).await
            && new_count > 0
        {
            tracing::info!(
                "Synced {} new sedimentrees from remote directory tree",
                new_count
            );
            summary.sedimentrees_synced += new_count;
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
    config: DarnConfig,
    layout: WorkspaceLayout,
}

impl InitializedDarn {
    /// Get the workspace root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the loaded `.darn` config.
    #[must_use]
    pub const fn config(&self) -> &DarnConfig {
        &self.config
    }

    /// Get the workspace layout (centralized storage paths).
    #[must_use]
    pub const fn layout(&self) -> &WorkspaceLayout {
        &self.layout
    }

    /// Get the manifest file path (`~/.config/darn/workspaces/<id>/manifest.json`).
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.layout.manifest_path()
    }

    /// Set `force_immutable` in the `.darn` config and save it.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file cannot be written.
    pub fn set_force_immutable(
        &mut self,
        force_immutable: bool,
    ) -> Result<(), crate::dotfile::DotfileError> {
        self.config.force_immutable = force_immutable;
        self.config.save(&self.root)
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
    config: DarnConfig,
    layout: WorkspaceLayout,
}

impl UnopenedDarn {
    /// Get the workspace root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the loaded `.darn` config.
    #[must_use]
    pub const fn config(&self) -> &DarnConfig {
        &self.config
    }

    /// Get the workspace layout (centralized storage paths).
    #[must_use]
    pub const fn layout(&self) -> &WorkspaceLayout {
        &self.layout
    }

    /// Get the storage directory path (`~/.config/darn/workspaces/<id>/storage/`).
    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.layout.storage_dir()
    }

    /// Get the manifest file path (`~/.config/darn/workspaces/<id>/manifest.json`).
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.layout.manifest_path()
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
        let signer = Darn::load_signer_static()?;
        let storage = Darn::storage_from_layout(&self.layout)?;
        Ok(subduction::spawn(signer, storage))
    }

    /// Convert to a full `Darn` instance by hydrating Subduction.
    ///
    /// # Errors
    ///
    /// Returns an error if hydration fails.
    pub async fn hydrate(self) -> Result<Darn, OpenError> {
        let signer = Darn::load_signer_static()?;
        let storage = Darn::storage_from_layout(&self.layout)?;
        let subduction = Box::pin(subduction::hydrate(signer, storage)).await?;

        #[cfg(feature = "iroh")]
        let iroh_endpoint = {
            let signer_dir = config::global_signer_dir()?;
            let key_bytes = signer::load_key_bytes(&signer_dir).map_err(SignerLoadError::from)?;
            let secret_key = iroh::SecretKey::from_bytes(&key_bytes);
            iroh::Endpoint::builder()
                .secret_key(secret_key)
                .alpns(vec![subduction_iroh::ALPN.to_vec()])
                .bind()
                .await
                .map_err(|e| OpenError::IrohBind(e.to_string()))?
        };

        Ok(Darn {
            root: self.root,
            config: self.config,
            layout: self.layout,
            subduction,
            #[cfg(feature = "iroh")]
            iroh_endpoint,
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
        Darn::load_signer_static()
    }
}

/// A file identified as modified during Phase 1 of refresh.
struct RefreshCandidate {
    path: PathBuf,
    sedimentree_id: SedimentreeId,
    file_type: crate::file::file_type::FileType,
    current_fs_digest: sedimentree_core::crypto::digest::Digest<content_hash::FileSystemContent>,
}

/// Updated digests from a parallel file refresh.
struct RefreshedDigests {
    file_system: sedimentree_core::crypto::digest::Digest<content_hash::FileSystemContent>,
    sedimentree:
        sedimentree_core::crypto::digest::Digest<sedimentree_core::sedimentree::Sedimentree>,
}

/// Result from refreshing a single file in parallel.
struct RefreshResult {
    path: PathBuf,
    sedimentree_id: SedimentreeId,
    outcome: Result<Option<RefreshedDigests>, RefreshError>,
}

/// Error opening a workspace.
#[derive(Debug, Error)]
pub enum OpenError {
    /// Not a workspace.
    #[error(transparent)]
    NotAWorkspace(#[from] NotAWorkspace),

    /// Config directory error.
    #[error(transparent)]
    Config(#[from] NoConfigDir),

    /// Dotfile error.
    #[error(transparent)]
    Dotfile(#[from] DotfileError),

    /// Signer error.
    #[error(transparent)]
    Signer(#[from] SignerLoadError),

    /// Storage error.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// Subduction initialization error.
    #[error(transparent)]
    Init(#[from] SubductionInitError),

    /// Iroh endpoint failed to bind.
    #[cfg(feature = "iroh")]
    #[error("iroh endpoint bind failed: {0}")]
    IrohBind(String),
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

/// No workspace found (no `.darn` file).
#[derive(Debug, Clone, Copy, Error)]
#[error("not a darn workspace (or any parent): .darn file not found")]
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

    /// Dotfile error.
    #[error(transparent)]
    Dotfile(#[from] DotfileError),

    /// Registry error.
    #[error(transparent)]
    Registry(#[from] crate::workspace::registry::RegistryError),

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

    /// WebSocket connection error.
    #[error(transparent)]
    WebSocketConnection(#[from] subduction_websocket::tokio::client::ClientConnectError),

    /// Invalid iroh node ID.
    #[cfg(feature = "iroh")]
    #[error("invalid iroh node ID: {0}")]
    IrohNodeId(iroh::KeyParsingError),

    /// Invalid iroh relay URL.
    #[cfg(feature = "iroh")]
    #[error("invalid iroh relay URL: {0}")]
    IrohRelayUrl(iroh::RelayUrlParseError),

    /// Failed to bind iroh endpoint.
    #[cfg(feature = "iroh")]
    #[error("failed to bind iroh endpoint: {0}")]
    IrohBind(iroh::endpoint::BindError),

    /// Iroh connection error.
    #[cfg(feature = "iroh")]
    #[error(transparent)]
    IrohConnection(#[from] subduction_iroh::error::ConnectError),

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
