//! Tracked file entry in the manifest.

use std::path::{Path, PathBuf};

use sedimentree_core::{crypto::digest::Digest, id::SedimentreeId};
use serde::{Deserialize, Serialize};

pub use sedimentree_core::sedimentree::Sedimentree;

use super::content_hash::{self, FileSystemContent};
use crate::{
    file::{file_type::FileType, state::FileState},
    serde_base58,
    unix_timestamp::UnixTimestamp,
};

/// A tracked file entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tracked {
    /// The unique Sedimentree ID for this file's CRDT history.
    #[serde(with = "serde_base58::sedimentree_id")]
    pub sedimentree_id: SedimentreeId,

    /// Path relative to the workspace root.
    pub relative_path: PathBuf,

    /// Content kind determines merge strategy (text CRDT vs binary LWW).
    pub file_type: FileType,

    /// When the file was first tracked.
    pub tracked_at: UnixTimestamp,

    /// Blake3 digest of raw file content for change detection.
    #[serde(with = "serde_base58::digest")]
    pub file_system_digest: Digest<FileSystemContent>,

    /// Hash of sorted blob digests in the sedimentree.
    ///
    /// Enables change detection: if computed digest != stored, sedimentree has changed.
    /// Deterministic: same blobs = same digest.
    #[serde(with = "serde_base58::digest")]
    pub sedimentree_digest: Digest<Sedimentree>,
}

impl Tracked {
    /// Creates a new tracked file entry.
    #[must_use]
    pub fn new(
        sedimentree_id: SedimentreeId,
        relative_path: PathBuf,
        file_type: FileType,
        file_system_digest: Digest<FileSystemContent>,
        sedimentree_digest: Digest<Sedimentree>,
    ) -> Self {
        Self {
            sedimentree_id,
            relative_path,
            file_type,
            tracked_at: UnixTimestamp::now(),
            file_system_digest,
            sedimentree_digest,
        }
    }

    /// Determines the file's state by comparing stored digest with current disk content.
    #[must_use]
    pub fn state(&self, workspace_root: &Path) -> FileState {
        let path = workspace_root.join(&self.relative_path);
        match content_hash::hash_file(&path) {
            Ok(digest) if digest == self.file_system_digest => FileState::Clean,
            Ok(_) => FileState::Modified,
            Err(_) => FileState::Missing,
        }
    }
}
