//! File system content and digest computation.
//!
//! Uses [`Digest`] from sedimentree for consistency with Automerge blob digests.

use std::{
    fs::File,
    io::{self, BufReader},
    path::Path,
};

use sedimentree_core::crypto::digest::Digest;

/// Raw file content from the file system (before Automerge serialization).
///
/// This is a newtype wrapper around the file bytes, distinguishing file system
/// content from [`sedimentree_core::blob::Blob`] which holds serialized Automerge docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemContent(Vec<u8>);

impl FileSystemContent {
    /// Creates new file system content from bytes.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the content as a byte slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Consumes self and returns the inner bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Computes the Blake3 digest of this content.
    #[must_use]
    pub fn digest(&self) -> Digest<Self> {
        let hash = blake3::hash(&self.0);
        Digest::force_from_bytes(*hash.as_bytes())
    }

    /// Reads file content from the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read(path)?;
        Ok(Self(data))
    }
}

/// Computes a digest directly from a byte slice.
///
/// Convenience function for when you don't need the intermediate [`FileSystemContent`].
#[must_use]
pub fn hash_bytes(data: &[u8]) -> Digest<FileSystemContent> {
    let hash = blake3::hash(data);
    Digest::force_from_bytes(*hash.as_bytes())
}

/// Computes a digest of a file at the given path using streaming I/O.
///
/// This is zero-copy: the file is streamed through a buffer and hashed
/// incrementally without loading the entire contents into memory.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read.
pub fn hash_file(path: &Path) -> io::Result<Digest<FileSystemContent>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut reader, &mut hasher)?;
    Ok(Digest::force_from_bytes(*hasher.finalize().as_bytes()))
}
