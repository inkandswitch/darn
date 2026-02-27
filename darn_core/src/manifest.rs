//! Manifest for tracking files in a `darn` workspace.
//!
//! The manifest maintains a mapping between [`SedimentreeId`]s and file metadata,
//! persisted as JSON at `~/.config/darn/workspaces/<id>/manifest.json`.

pub mod content_hash;
pub mod tracked;

use std::{collections::BTreeMap, path::Path};

use sedimentree_core::id::SedimentreeId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracked::Tracked;

use crate::serde_base58;

/// Manifest tracking all files in a workspace.
///
/// Contains the root directory sedimentree ID and maps [`SedimentreeId`] to [`Tracked`] metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Root directory sedimentree ID (random, generated at workspace init).
    #[serde(with = "serde_base58::sedimentree_id")]
    root_directory_id: SedimentreeId,

    /// Tracked file entries (serialized as array, keyed by `sedimentree_id` internally).
    #[serde(with = "json_entries")]
    entries: BTreeMap<SedimentreeId, Tracked>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

impl Manifest {
    /// Creates an empty manifest with a new random root directory ID.
    ///
    /// Generates a 16-byte random ID (zero-padded to 32 bytes) for compatibility
    /// with automerge-repo's 16-byte document IDs.
    ///
    /// # Panics
    ///
    /// Panics if the system random number generator fails.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root_directory_id: crate::generate_sedimentree_id(),
            entries: BTreeMap::new(),
        }
    }

    /// Creates an empty manifest with a specific root directory ID.
    ///
    /// Use this when cloning a workspace with a known root ID.
    #[must_use]
    pub const fn with_root_id(root_directory_id: SedimentreeId) -> Self {
        Self {
            root_directory_id,
            entries: BTreeMap::new(),
        }
    }

    /// Get the root directory sedimentree ID.
    #[must_use]
    pub const fn root_directory_id(&self) -> SedimentreeId {
        self.root_directory_id
    }

    /// Loads a manifest from the given path.
    ///
    /// Returns an empty manifest if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or decoded.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        match std::fs::read_to_string(path) {
            Ok(json) => Ok(serde_json::from_str(&json)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Saves the manifest to the given path atomically.
    ///
    /// Uses a temp-file-then-rename pattern to prevent readers from seeing
    /// a partially-written file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
        let json = serde_json::to_string_pretty(self)?;
        crate::atomic_write::atomic_write(path, json.as_bytes())?;
        Ok(())
    }

    /// Tracks a new file.
    ///
    /// If a file with the same path is already tracked, it is replaced.
    pub fn track(&mut self, entry: Tracked) {
        // Remove any existing entry with the same path
        self.entries
            .retain(|_, v| v.relative_path != entry.relative_path);

        self.entries.insert(entry.sedimentree_id, entry);
    }

    /// Untracks a file by its relative path.
    ///
    /// Returns the removed entry if found.
    pub fn untrack(&mut self, relative_path: &Path) -> Option<Tracked> {
        let id = self
            .entries
            .iter()
            .find(|(_, v)| v.relative_path == relative_path)
            .map(|(k, _)| *k)?;

        self.entries.remove(&id)
    }

    /// Untracks a file by its sedimentree ID.
    ///
    /// Returns the removed entry if found.
    pub fn untrack_by_id(&mut self, id: &SedimentreeId) -> Option<Tracked> {
        self.entries.remove(id)
    }

    /// Looks up a tracked file by its relative path.
    #[must_use]
    pub fn get_by_path(&self, relative_path: &Path) -> Option<&Tracked> {
        self.entries
            .values()
            .find(|v| v.relative_path == relative_path)
    }

    /// Looks up a tracked file by its relative path (mutable).
    #[must_use]
    pub fn get_by_path_mut(&mut self, relative_path: &Path) -> Option<&mut Tracked> {
        self.entries
            .values_mut()
            .find(|v| v.relative_path == relative_path)
    }

    /// Looks up a tracked file by its Sedimentree ID.
    #[must_use]
    pub fn get_by_id(&self, id: &SedimentreeId) -> Option<&Tracked> {
        self.entries.get(id)
    }

    /// Returns `true` if no files are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of tracked files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterates over all tracked files.
    pub fn iter(&self) -> impl Iterator<Item = &Tracked> {
        self.entries.values()
    }

    /// Iterates over all tracked files mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tracked> {
        self.entries.values_mut()
    }
}

/// Serde helper for entries map - serializes as array (keys derived from `entry.sedimentree_id`).
mod json_entries {
    use std::collections::BTreeMap;

    use sedimentree_core::id::SedimentreeId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Tracked;

    pub(super) fn serialize<S: Serializer>(
        entries: &BTreeMap<SedimentreeId, Tracked>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let vec: Vec<&Tracked> = entries.values().collect();
        vec.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<SedimentreeId, Tracked>, D::Error> {
        let vec: Vec<Tracked> = Vec::deserialize(deserializer)?;
        Ok(vec.into_iter().map(|e| (e.sedimentree_id, e)).collect())
    }
}

/// Error loading or saving the manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// JSON decode error.
    #[error("JSON decode error: {0}")]
    Decode(#[from] serde_json::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use sedimentree_core::crypto::digest::Digest;
    use testresult::TestResult;

    use crate::file::{file_type::FileType, state::FileState};
    use content_hash::{self, FileSystemContent};
    use tracked::{Sedimentree, Tracked};

    fn random_id() -> Result<SedimentreeId, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)?;
        Ok(SedimentreeId::new(bytes))
    }

    fn dummy_fs_digest() -> Digest<FileSystemContent> {
        content_hash::hash_bytes(b"test content")
    }

    fn dummy_sedimentree_digest() -> Digest<Sedimentree> {
        Digest::force_from_bytes([0u8; 32])
    }

    #[test]
    fn track_and_get_by_path() -> TestResult {
        let mut manifest = Manifest::new();
        let id = random_id()?;
        let entry = Tracked::new(
            id,
            PathBuf::from("foo/bar.txt"),
            FileType::Text,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        );

        manifest.track(entry);

        let found = manifest.get_by_path(Path::new("foo/bar.txt"));
        assert!(found.is_some());
        assert_eq!(found.ok_or("entry not found")?.sedimentree_id, id);
        Ok(())
    }

    #[test]
    fn track_replaces_same_path() -> TestResult {
        let mut manifest = Manifest::new();

        let id1 = random_id()?;
        let entry1 = Tracked::new(
            id1,
            PathBuf::from("foo.txt"),
            FileType::Text,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        );
        manifest.track(entry1);

        let id2 = random_id()?;
        let entry2 = Tracked::new(
            id2,
            PathBuf::from("foo.txt"),
            FileType::Binary,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        );
        manifest.track(entry2);

        assert_eq!(manifest.len(), 1);
        let found = manifest.get_by_path(Path::new("foo.txt"));
        assert_eq!(found.ok_or("entry not found")?.sedimentree_id, id2);
        Ok(())
    }

    #[test]
    fn untrack_removes_entry() -> TestResult {
        let mut manifest = Manifest::new();
        let id = random_id()?;
        let entry = Tracked::new(
            id,
            PathBuf::from("test.rs"),
            FileType::Text,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        );

        manifest.track(entry);
        assert_eq!(manifest.len(), 1);

        let removed = manifest.untrack(Path::new("test.rs"));
        assert!(removed.is_some());
        assert_eq!(manifest.len(), 0);
        Ok(())
    }

    #[test]
    fn untrack_nonexistent_returns_none() {
        let mut manifest = Manifest::new();
        let removed = manifest.untrack(Path::new("nonexistent.txt"));
        assert!(removed.is_none());
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn roundtrip_via_json() {
        use bolero::check;

        check!()
            .with_type::<Vec<([u8; 32], String, bool)>>()
            .for_each(|entries: &Vec<([u8; 32], String, bool)>| {
                let mut manifest = Manifest::new();

                for (id_bytes, path_str, is_text) in entries {
                    let id = SedimentreeId::new(*id_bytes);
                    let file_type = if *is_text {
                        FileType::Text
                    } else {
                        FileType::Binary
                    };
                    manifest.track(Tracked::new(
                        id,
                        PathBuf::from(path_str),
                        file_type,
                        dummy_fs_digest(),
                        dummy_sedimentree_digest(),
                    ));
                }

                let json = serde_json::to_string(&manifest).expect("serialize");
                let decoded: Manifest = serde_json::from_str(&json).expect("deserialize");

                assert_eq!(decoded.len(), manifest.len());
                for entry in manifest.iter() {
                    let found = decoded.get_by_path(&entry.relative_path);
                    assert!(
                        found.is_some(),
                        "missing path after roundtrip: {:?}",
                        entry.relative_path
                    );
                    let found = found.expect("checked above");
                    assert_eq!(found.file_type, entry.file_type);
                    assert_eq!(found.sedimentree_id, entry.sedimentree_id);
                }
            });
    }

    #[test]
    fn save_and_load() -> TestResult {
        let dir = tempfile::tempdir()?;
        let manifest_path = dir.path().join("manifest.json");

        let mut manifest = Manifest::new();
        let id = random_id()?;
        manifest.track(Tracked::new(
            id,
            PathBuf::from("test.txt"),
            FileType::Text,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        ));

        manifest.save(&manifest_path)?;
        let loaded = Manifest::load(&manifest_path)?;

        assert_eq!(loaded.len(), 1);
        assert!(loaded.get_by_path(Path::new("test.txt")).is_some());
        Ok(())
    }

    #[test]
    fn load_nonexistent_returns_empty() -> TestResult {
        let dir = tempfile::tempdir()?;
        let manifest_path = dir.path().join("does_not_exist.json");

        let manifest = Manifest::load(&manifest_path)?;
        assert!(manifest.is_empty());
        Ok(())
    }

    #[test]
    fn iter_returns_all_entries() -> TestResult {
        let mut manifest = Manifest::new();
        manifest.track(Tracked::new(
            random_id()?,
            PathBuf::from("a.txt"),
            FileType::Text,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        ));
        manifest.track(Tracked::new(
            random_id()?,
            PathBuf::from("b.txt"),
            FileType::Text,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        ));
        manifest.track(Tracked::new(
            random_id()?,
            PathBuf::from("c.txt"),
            FileType::Binary,
            dummy_fs_digest(),
            dummy_sedimentree_digest(),
        ));

        let paths: Vec<_> = manifest.iter().map(|e| &e.relative_path).collect();
        assert_eq!(paths.len(), 3);
        Ok(())
    }

    #[test]
    fn file_state_detection() -> TestResult {
        let dir = tempfile::tempdir()?;
        let file_path = dir.path().join("test.txt");

        // Create file with known content
        std::fs::write(&file_path, "original content")?;
        let original_hash = content_hash::hash_file(&file_path)?;

        let entry = Tracked::new(
            random_id()?,
            PathBuf::from("test.txt"),
            FileType::Text,
            original_hash,
            dummy_sedimentree_digest(),
        );

        // Clean state: file unchanged
        assert_eq!(entry.state(dir.path()), FileState::Clean);

        // Modified state: file changed
        std::fs::write(&file_path, "modified content")?;
        assert_eq!(entry.state(dir.path()), FileState::Modified);

        // Missing state: file deleted
        std::fs::remove_file(&file_path)?;
        assert_eq!(entry.state(dir.path()), FileState::Missing);
        Ok(())
    }
}
