//! Helper functions for sedimentree storage operations via Subduction.
//!
//! This module provides functions to store and load Automerge documents using
//! sedimentree's commit/fragment model. All operations go through Subduction,
//! which handles signing, storage, and peer synchronization.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │                    Automerge ↔ Sedimentree                           │
//! ├──────────────────────────────────────────────────────────────────────┤
//! │                                                                      │
//! │  LooseCommits (individual changes):                                  │
//! │    change.hash()        →  Digest<LooseCommit>                       │
//! │    change.deps()        →  parents                                   │
//! │    change.raw_bytes()   →  Blob                                      │
//! │                                                                      │
//! │  Fragments (compressed runs via Subduction):                         │
//! │    Subduction handles fragment building automatically                │
//! │    when add_commit() returns FragmentRequested                       │
//! │                                                                      │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```

use std::{collections::BTreeSet, convert::Infallible, path::Path};

use automerge::{Automerge, Change, ChangeHash};
use future_form::Sendable;
use sedimentree_fs_storage::FsStorage;
use subduction_core::subduction::error::WriteError;
use sedimentree_core::{
    blob::Blob, crypto::digest::Digest, id::SedimentreeId, sedimentree::Sedimentree,
};
use thiserror::Error;

use crate::{
    directory::{entry::EntryType, DeserializeError, Directory, SerializeError},
    subduction::{DarnConnection, DarnSubduction}
};

/// Store all changes from an Automerge document as sedimentree commits.
///
/// Each Automerge change is stored as a `LooseCommit` with its associated blob.
/// Subduction handles signing and propagation to peers.
///
/// # Errors
///
/// Returns an error if storage fails.
pub async fn store_document(
    subduction: &DarnSubduction,
    id: SedimentreeId,
    doc: &mut Automerge,
) -> Result<(), SedimentreeError> {
    let changes = doc.get_changes(&[]);
    for change in &changes {
        store_change(subduction, id, change).await?;
    }
    Ok(())
}

/// Store a single Automerge change as a `LooseCommit` + `Blob`.
///
/// Subduction handles signing internally and propagates to connected peers.
///
/// # Errors
///
/// Returns an error if storage fails.
pub async fn store_change(
    subduction: &DarnSubduction,
    id: SedimentreeId,
    change: &Change,
) -> Result<(), SedimentreeError> {
    let blob = Blob::new(change.raw_bytes().to_vec());

    let parents: BTreeSet<_> = change
        .deps()
        .iter()
        .map(|h| Digest::force_from_bytes(h.0))
        .collect();

    subduction
        .add_commit(id, parents, blob)
        .await
        .map_err(|e| SedimentreeError::SubductionWrite(Box::new(e)))?;

    Ok(())
}

/// Store only new changes since the given heads.
///
/// Returns the number of changes stored.
///
/// # Errors
///
/// Returns an error if storage fails.
pub async fn add_changes(
    subduction: &DarnSubduction,
    id: SedimentreeId,
    doc: &mut Automerge,
    since_heads: &[ChangeHash],
) -> Result<usize, SedimentreeError> {
    let new_changes = doc.get_changes(since_heads);
    let count = new_changes.len();
    for change in &new_changes {
        store_change(subduction, id, change).await?;
    }
    Ok(count)
}

/// Load an Automerge document from sedimentree blobs.
///
/// Returns `None` if no blobs exist for the given sedimentree ID.
///
/// # Errors
///
/// Returns an error if storage operations fail or blobs cannot be loaded.
pub async fn load_document(
    subduction: &DarnSubduction,
    id: SedimentreeId,
) -> Result<Option<Automerge>, SedimentreeError> {
    let Some(blobs) = subduction
        .get_blobs(id)
        .await
        .map_err(SedimentreeError::StorageRead)?
    else {
        tracing::debug!(?id, "load_document: no blobs found");
        return Ok(None);
    };

    // NOTE: Sort by size descending (larger = likely older = better load order)
    let mut blobs_vec: Vec<_> = blobs.into_iter().collect();
    blobs_vec.sort_by_key(|b| std::cmp::Reverse(b.as_slice().len()));

    tracing::debug!(?id, blob_count = blobs_vec.len(), "load_document: loading blobs");

    let mut doc = Automerge::new();
    for blob in blobs_vec {
        doc.load_incremental(blob.as_slice())
            .map_err(SedimentreeError::AutomergeLoad)?;
    }

    Ok(Some(doc))
}

/// Compute sedimentree digest from sorted commit digests.
///
/// The digest is computed by hashing all commit digests in sorted order,
/// enabling deterministic change detection.
///
/// # Errors
///
/// This function is infallible for the current implementation.
pub async fn compute_digest(
    subduction: &DarnSubduction,
    id: SedimentreeId,
) -> Result<Digest<Sedimentree>, SedimentreeError> {
    let commits = subduction.get_commits(id).await.unwrap_or_default();

    let mut digests: Vec<[u8; 32]> = commits
        .iter()
        .map(|c| *Digest::hash(c).as_bytes())
        .collect();
    digests.sort_unstable();

    let mut hasher = blake3::Hasher::new();
    for digest_bytes in &digests {
        hasher.update(digest_bytes);
    }

    Ok(Digest::force_from_bytes(*hasher.finalize().as_bytes()))
}

// ============================================================================
// Directory operations
// ============================================================================

/// Ensure all parent directories exist in the directory tree.
///
/// Given a relative path like `src/foo/bar.rs`, ensures:
/// - Root has "src" folder entry
/// - "src" has "foo" folder entry
///
/// Returns the sedimentree ID of the direct parent directory.
///
/// # Arguments
///
/// * `subduction` - The Subduction instance for storage
/// * `root_id` - The root directory's sedimentree ID
/// * `relative_path` - The relative path of the file being tracked
///
/// # Errors
///
/// Returns an error if storage operations fail.
pub async fn ensure_parent_directories(
    subduction: &DarnSubduction,
    root_id: SedimentreeId,
    relative_path: &Path,
) -> Result<SedimentreeId, SedimentreeError> {
    let components: Vec<_> = relative_path
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();

    let mut current_id = root_id;
    let mut current_name = String::new();

    for component in &components {
        let name = component.as_os_str().to_string_lossy().to_string();

        let mut doc = load_document(subduction, current_id)
            .await?
            .unwrap_or_else(Automerge::new);

        Directory::init_doc(&mut doc, &current_name).map_err(SedimentreeError::Serialize)?;

        let dir = Directory::from_automerge(&doc).map_err(SedimentreeError::Deserialize)?;

        if let Some(entry) = dir.get(&name) {
            if entry.entry_type == EntryType::Folder {
                current_id = entry.sedimentree_id;
                current_name = name;
                continue;
            }

            return Err(SedimentreeError::PathConflict(format!(
                "path component '{name}' already exists as a file"
            )));
        }

        let subdir_id = generate_sedimentree_id()?;

        let heads_before: Vec<_> = doc.get_heads().into_iter().collect();
        Directory::add_folder_to_doc(&mut doc, &name, subdir_id)
            .map_err(SedimentreeError::Serialize)?;
        add_changes(subduction, current_id, &mut doc, &heads_before).await?;

        let mut subdir_doc = Automerge::new();
        Directory::init_doc(&mut subdir_doc, &name).map_err(SedimentreeError::Serialize)?;
        store_document(subduction, subdir_id, &mut subdir_doc).await?;

        current_id = subdir_id;
        current_name = name;
    }

    if components.is_empty() {
        let mut doc = load_document(subduction, current_id)
            .await?
            .unwrap_or_else(Automerge::new);

        let heads_before: Vec<_> = doc.get_heads().into_iter().collect();
        Directory::init_doc(&mut doc, "").map_err(SedimentreeError::Serialize)?;

        if doc.get_heads() != heads_before {
            add_changes(subduction, current_id, &mut doc, &heads_before).await?;
        }
    }

    Ok(current_id)
}

/// Add a file entry to its parent directory.
///
/// This loads the existing directory document, adds the file entry in place,
/// and stores only the new changes. This preserves the Automerge change history
/// and ensures proper CRDT merging.
///
/// # Arguments
///
/// * `subduction` - The Subduction instance
/// * `parent_id` - Sedimentree ID of the parent directory
/// * `file_name` - Name of the file (not full path)
/// * `file_sedimentree_id` - Sedimentree ID of the file
///
/// # Errors
///
/// Returns an error if storage operations fail.
pub async fn add_file_to_directory(
    subduction: &DarnSubduction,
    parent_id: SedimentreeId,
    file_name: &str,
    file_sedimentree_id: SedimentreeId,
) -> Result<(), SedimentreeError> {
    let mut doc = load_document(subduction, parent_id)
        .await?
        .unwrap_or_else(Automerge::new);

    let heads_before: Vec<_> = doc.get_heads().into_iter().collect();

    Directory::init_doc(&mut doc, "").map_err(SedimentreeError::Serialize)?;
    Directory::add_file_to_doc(&mut doc, file_name, file_sedimentree_id)
        .map_err(SedimentreeError::Serialize)?;

    // Store only new changes
    let new_changes = add_changes(subduction, parent_id, &mut doc, &heads_before).await?;
    tracing::debug!(
        ?parent_id,
        file_name,
        new_changes,
        "add_file_to_directory: stored changes"
    );

    Ok(())
}

/// Remove a file entry from its parent directory.
///
/// This loads the existing directory document, removes the entry in place,
/// and stores only the new changes.
///
/// # Arguments
///
/// * `subduction` - The Subduction instance
/// * `parent_id` - Sedimentree ID of the parent directory
/// * `file_name` - Name of the file to remove
///
/// # Errors
///
/// Returns an error if storage operations fail.
pub async fn remove_file_from_directory(
    subduction: &DarnSubduction,
    parent_id: SedimentreeId,
    file_name: &str,
) -> Result<bool, SedimentreeError> {
    let Some(mut doc) = load_document(subduction, parent_id).await? else {
        return Ok(false);
    };

    let heads_before: Vec<_> = doc.get_heads().into_iter().collect();
    let removed =
        Directory::remove_entry_from_doc(&mut doc, file_name).map_err(SedimentreeError::Serialize)?;

    if removed {
        add_changes(subduction, parent_id, &mut doc, &heads_before).await?;
    }

    Ok(removed)
}

/// Find the sedimentree ID of a directory at the given path.
///
/// Returns `None` if the directory doesn't exist.
///
/// # Arguments
///
/// * `subduction` - The Subduction instance for storage
/// * `root_id` - The root directory's sedimentree ID
/// * `dir_path` - The relative path of the directory to find
///
/// # Errors
///
/// Returns an error if storage operations fail.
pub async fn find_directory_id(
    subduction: &DarnSubduction,
    root_id: SedimentreeId,
    dir_path: &Path,
) -> Result<Option<SedimentreeId>, SedimentreeError> {
    let components: Vec<_> = dir_path.components().collect();

    let mut current_id = root_id;

    for component in components {
        let name = component.as_os_str().to_string_lossy();

        let dir = match load_document(subduction, current_id).await? {
            Some(am_doc) => {
                Directory::from_automerge(&am_doc).map_err(SedimentreeError::Deserialize)?
            }
            None => return Ok(None),
        };

        match dir.get(&name) {
            Some(entry) if entry.entry_type == EntryType::Folder => {
                current_id = entry.sedimentree_id;
            }
            _ => return Ok(None),
        }
    }

    Ok(Some(current_id))
}

/// Generate a random sedimentree ID.
///
/// Only the first 16 bytes are randomized; the remaining 16 bytes are zero.
/// This ensures compatibility with automerge-repo's 16-byte document IDs
/// when converting to/from automerge URLs.
fn generate_sedimentree_id() -> Result<SedimentreeId, SedimentreeError> {
    let mut id_bytes = [0u8; 32];
    // Only randomize the first 16 bytes for automerge-repo compatibility
    getrandom::getrandom(&mut id_bytes[..16]).map_err(SedimentreeError::Random)?;
    Ok(SedimentreeId::new(id_bytes))
}

/// Errors from sedimentree operations.
#[derive(Debug, Error)]
pub enum SedimentreeError {
    /// Subduction write error.
    #[error("subduction write error: {0}")]
    SubductionWrite(Box<WriteError<Sendable, FsStorage, DarnConnection, Infallible>>),

    /// Storage read error.
    #[error("storage read error: {0}")]
    StorageRead(sedimentree_fs_storage::FsStorageError),

    /// Automerge load error.
    #[error("automerge error: {0}")]
    AutomergeLoad(automerge::AutomergeError),

    /// Directory serialization error.
    #[error("serialize error: {0}")]
    Serialize(SerializeError),

    /// Directory deserialization error.
    #[error("deserialize error: {0}")]
    Deserialize(DeserializeError),

    /// Random generation error.
    #[error("random generation error: {0}")]
    Random(getrandom::Error),

    /// Path conflict (file exists where folder expected).
    #[error("path conflict: {0}")]
    PathConflict(String),

    /// Document not found in storage.
    #[error("document not found")]
    NotFound,
}
