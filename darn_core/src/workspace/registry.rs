//! Workspace registry.
//!
//! Tracks the mapping between workspace IDs and their original paths,
//! allowing lookup in both directions.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::WorkspaceId;
use crate::config::{self, NoConfigDir};

/// Registry filename within the config directory.
const REGISTRY_FILE: &str = "workspaces.json";

/// Registry tracking all known workspaces.
///
/// The registry maps workspace IDs to their original paths and allows
/// reverse lookup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRegistry {
    /// Map from workspace ID (hex) to workspace metadata.
    workspaces: HashMap<String, WorkspaceEntry>,
}

/// Metadata about a registered workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    /// The original path where the workspace was initialized.
    ///
    /// The workspace root directory (where the `.darn` file lives).
    pub original_path: PathBuf,

    /// Human-readable name (defaults to directory name).
    pub name: String,

    /// Unix timestamp of when the workspace was created.
    pub created_at: u64,
}

impl WorkspaceRegistry {
    /// Load the registry from disk, or create an empty one if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry file exists but cannot be read or parsed.
    pub fn load() -> Result<Self, RegistryError> {
        let path = Self::registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path).map_err(RegistryError::Read)?;
        serde_json::from_str(&contents).map_err(RegistryError::Parse)
    }

    /// Load from a specific path (for testing).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load_from(path: &Path) -> Result<Self, RegistryError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path).map_err(RegistryError::Read)?;
        serde_json::from_str(&contents).map_err(RegistryError::Parse)
    }

    /// Save the registry to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry cannot be written.
    pub fn save(&self) -> Result<(), RegistryError> {
        let path = Self::registry_path()?;
        self.save_to(&path)
    }

    /// Save to a specific path (for testing).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_to(&self, path: &Path) -> Result<(), RegistryError> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(RegistryError::Write)?;
        }

        // Write atomically via temp file (unique suffix avoids parallel test races).
        // On Windows, `fs::rename` fails when the destination exists, so we
        // remove the target first. This makes the operation non-atomic on
        // Windows but avoids half-written files.
        let tid = std::thread::current().id();
        let temp_name = format!(
            "{}.{tid:?}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("registry")
        );
        let temp_path = path.with_file_name(temp_name);
        let contents = serde_json::to_string_pretty(self).map_err(RegistryError::Parse)?;
        fs::write(&temp_path, &contents).map_err(RegistryError::Write)?;

        // Remove existing file on Windows before rename.
        #[cfg(target_os = "windows")]
        if path.exists() {
            fs::remove_file(path).map_err(RegistryError::Write)?;
        }

        fs::rename(&temp_path, path).map_err(RegistryError::Write)?;

        Ok(())
    }

    /// Register a new workspace.
    pub fn register(&mut self, id: WorkspaceId, entry: WorkspaceEntry) {
        self.workspaces.insert(id.to_hex(), entry);
    }

    /// Remove a workspace from the registry.
    pub fn unregister(&mut self, id: WorkspaceId) -> Option<WorkspaceEntry> {
        self.workspaces.remove(&id.to_hex())
    }

    /// Look up a workspace by ID.
    #[must_use]
    pub fn get(&self, id: WorkspaceId) -> Option<&WorkspaceEntry> {
        self.workspaces.get(&id.to_hex())
    }

    /// Find a workspace by its original path.
    #[must_use]
    pub fn find_by_path(&self, path: &Path) -> Option<(WorkspaceId, &WorkspaceEntry)> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.workspaces.iter().find_map(|(hex_id, entry)| {
            let entry_canonical = entry
                .original_path
                .canonicalize()
                .unwrap_or_else(|_| entry.original_path.clone());
            if entry_canonical == canonical {
                let id: WorkspaceId = hex_id.parse().ok()?;
                Some((id, entry))
            } else {
                None
            }
        })
    }

    /// Check if a workspace with the given ID exists.
    #[must_use]
    pub fn contains(&self, id: WorkspaceId) -> bool {
        self.workspaces.contains_key(&id.to_hex())
    }

    /// Iterate over all registered workspaces.
    pub fn iter(&self) -> impl Iterator<Item = (WorkspaceId, &WorkspaceEntry)> {
        self.workspaces.iter().filter_map(|(hex_id, entry)| {
            let id: WorkspaceId = hex_id.parse().ok()?;
            Some((id, entry))
        })
    }

    /// Get the path to the registry file.
    fn registry_path() -> Result<PathBuf, RegistryError> {
        Ok(config::global_config_dir()?.join(REGISTRY_FILE))
    }
}

/// Errors working with the workspace registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// Could not determine config directory.
    #[error(transparent)]
    NoConfigDir(#[from] NoConfigDir),

    /// Failed to read registry file.
    #[error("failed to read registry: {0}")]
    Read(std::io::Error),

    /// Failed to write registry file.
    #[error("failed to write registry: {0}")]
    Write(std::io::Error),

    /// Failed to parse registry JSON.
    #[error("failed to parse registry: {0}")]
    Parse(serde_json::Error),
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use testresult::TestResult;

    fn test_entry(path: &Path) -> WorkspaceEntry {
        WorkspaceEntry {
            original_path: path.to_path_buf(),
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("test")
                .to_string(),
            created_at: 1_234_567_890,
        }
    }

    #[test]
    fn register_and_lookup() -> TestResult {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("test");
        std::fs::create_dir_all(&project)?;

        let mut registry = WorkspaceRegistry::default();
        let id = WorkspaceId::from_path(&project);
        let entry = test_entry(&project);

        registry.register(id, entry.clone());

        let found = registry.get(id).ok_or("should find registered workspace")?;
        assert_eq!(found.original_path, entry.original_path);
        Ok(())
    }

    #[test]
    fn find_by_path() -> TestResult {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("myproject");
        std::fs::create_dir_all(&project)?;

        let mut registry = WorkspaceRegistry::default();
        let id = WorkspaceId::from_path(&project);
        registry.register(id, test_entry(&project));

        let (found_id, _) = registry
            .find_by_path(&project)
            .ok_or("should find by path")?;
        assert_eq!(found_id, id);
        Ok(())
    }

    #[test]
    fn unregister() -> TestResult {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("test");
        std::fs::create_dir_all(&project)?;

        let mut registry = WorkspaceRegistry::default();
        let id = WorkspaceId::from_path(&project);
        registry.register(id, test_entry(&project));

        assert!(registry.contains(id));
        registry.unregister(id);
        assert!(!registry.contains(id));
        Ok(())
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn save_and_load_roundtrip() {
        use bolero::check;

        let dir = tempfile::tempdir().expect("create tempdir");
        let registry_path = dir.path().join("workspaces.json");

        check!().with_type::<Vec<(String, String)>>().for_each(
            |entries: &Vec<(String, String)>| {
                let mut registry = WorkspaceRegistry::default();
                let mut ids = Vec::new();

                for (path_str, name) in entries {
                    let id = WorkspaceId::from_path(Path::new(path_str));
                    let entry = WorkspaceEntry {
                        original_path: PathBuf::from(path_str),
                        name: name.clone(),
                        created_at: 1_234_567_890,
                    };
                    registry.register(id, entry);
                    ids.push(id);
                }

                registry.save_to(&registry_path).expect("save");
                let loaded = WorkspaceRegistry::load_from(&registry_path).expect("load");

                for id in &ids {
                    assert!(loaded.contains(*id), "missing workspace after roundtrip");
                }
            },
        );
    }
}
