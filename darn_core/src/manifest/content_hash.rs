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
    pub const fn new(bytes: Vec<u8>) -> Self {
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

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    /// Hardcoded known-answer vectors for blake3.
    ///
    /// If these break, it means the hashing algorithm changed — which would
    /// silently invalidate every existing manifest on disk.
    #[test]
    fn hash_bytes_known_answer() {
        // blake3("") — from the BLAKE3 spec / reference implementation
        let expected_empty: [u8; 32] = [
            0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d, 0xea, 0x36, 0xdc,
            0xc9, 0x49, 0x9b, 0xcb, 0x25, 0xc9, 0xad, 0xc1, 0x12, 0xb7, 0xcc, 0x9a, 0x93, 0xca,
            0xe4, 0x1f, 0x32, 0x62,
        ];
        assert_eq!(
            hash_bytes(b"").as_bytes(),
            &expected_empty,
            "blake3 empty hash must match known vector"
        );

        // blake3("hello")
        let expected_hello: [u8; 32] = [
            0xea, 0x8f, 0x16, 0x3d, 0xb3, 0x86, 0x82, 0x92, 0x5e, 0x44, 0x91, 0xc5, 0xe5, 0x8d,
            0x4b, 0xb3, 0x50, 0x6e, 0xf8, 0xc1, 0x4e, 0xb7, 0x8a, 0x86, 0xe9, 0x08, 0xc5, 0x62,
            0x4a, 0x67, 0x20, 0x0f,
        ];
        assert_eq!(
            hash_bytes(b"hello").as_bytes(),
            &expected_hello,
            "blake3 'hello' hash must match known vector"
        );
    }

    /// The streaming hash (via `io::copy`) must agree with the in-memory hash.
    #[test]
    fn hash_file_matches_hash_bytes() {
        check!().with_type::<Vec<u8>>().for_each(|data: &Vec<u8>| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("test.bin");
            std::fs::write(&path, data).expect("write");

            let file_digest = hash_file(&path).expect("hash_file");
            let bytes_digest = hash_bytes(data);
            assert_eq!(
                file_digest, bytes_digest,
                "hash_file must agree with hash_bytes"
            );
        });
    }
}
