//! Workspace directory layout.
//!
//! Provides paths to all components of a workspace's centralized storage
//! under `~/.config/darn/workspaces/<id>/`.

use std::path::{Path, PathBuf};

use super::WorkspaceId;
use crate::config::{self, NoConfigDir};

/// Subdirectory names within the global config.
const STORAGE_DIR: &str = "storage";
const WORKSPACES_DIR: &str = "workspaces";

/// Manifest filename within a workspace's centralized storage.
const MANIFEST_FILE: &str = "manifest.json";

/// Provides paths to all components of a workspace's centralized storage.
///
/// All storage lives under `~/.config/darn/workspaces/<id>/`. The user's
/// project directory contains only a `.darn` marker file.
#[derive(Debug, Clone)]
pub struct WorkspaceLayout {
    /// The workspace ID.
    id: WorkspaceId,

    /// The global config directory (`~/.config/darn/`).
    config_dir: PathBuf,
}

impl WorkspaceLayout {
    /// Create a new layout for the given workspace ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the global config directory cannot be determined.
    pub fn new(id: WorkspaceId) -> Result<Self, NoConfigDir> {
        Ok(Self {
            id,
            config_dir: config::global_config_dir()?,
        })
    }

    /// Create a layout with a custom config directory (for testing).
    #[must_use]
    pub const fn with_config_dir(id: WorkspaceId, config_dir: PathBuf) -> Self {
        Self { id, config_dir }
    }

    /// The workspace ID.
    #[must_use]
    pub const fn id(&self) -> WorkspaceId {
        self.id
    }

    // ========================================================================
    // Global paths
    // ========================================================================

    /// Global config directory (`~/.config/darn/`).
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Global shared storage directory (`~/.config/darn/storage/`).
    #[must_use]
    pub fn global_storage_dir(&self) -> PathBuf {
        self.config_dir.join(STORAGE_DIR)
    }

    /// Directory containing all workspaces (`~/.config/darn/workspaces/`).
    #[must_use]
    pub fn workspaces_dir(&self) -> PathBuf {
        self.config_dir.join(WORKSPACES_DIR)
    }

    // ========================================================================
    // Workspace paths
    // ========================================================================

    /// This workspace's directory (`~/.config/darn/workspaces/<id>/`).
    #[must_use]
    pub fn workspace_dir(&self) -> PathBuf {
        self.workspaces_dir().join(self.id.to_hex())
    }

    /// Path to the manifest file (`~/.config/darn/workspaces/<id>/manifest.json`).
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.workspace_dir().join(MANIFEST_FILE)
    }

    /// Per-workspace storage directory (`~/.config/darn/workspaces/<id>/storage/`).
    #[must_use]
    pub fn storage_dir(&self) -> PathBuf {
        self.workspace_dir().join(STORAGE_DIR)
    }

    // ========================================================================
    // Directory creation
    // ========================================================================

    /// Create all directories for this workspace's centralized storage.
    ///
    /// # Errors
    ///
    /// Returns an error if directories cannot be created.
    pub fn create_dirs(&self) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(self.storage_dir())
    }
}

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths_are_consistent() {
        let id = WorkspaceId::from_bytes([0; 16]);
        let layout = WorkspaceLayout::with_config_dir(id, PathBuf::from("/test/config"));

        assert_eq!(layout.config_dir(), Path::new("/test/config"));
        assert_eq!(
            layout.global_storage_dir(),
            PathBuf::from("/test/config/storage")
        );
        assert_eq!(
            layout.workspace_dir(),
            PathBuf::from("/test/config/workspaces/00000000000000000000000000000000")
        );
        assert_eq!(
            layout.manifest_path(),
            PathBuf::from("/test/config/workspaces/00000000000000000000000000000000/manifest.json")
        );
        assert_eq!(
            layout.storage_dir(),
            PathBuf::from("/test/config/workspaces/00000000000000000000000000000000/storage")
        );
    }
}
