//! Workspace directory layout.
//!
//! Provides paths to all components of a workspace's directory structure.

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::WorkspaceId;
use crate::config::{self, NoConfigDir};

/// Subdirectory names within the global config.
const STORAGE_DIR: &str = "storage";
const WORKSPACES_DIR: &str = "workspaces";

/// Subdirectory names within a workspace.
const TREES_DIR: &str = "trees";
const TREE_A: &str = "a";
const TREE_B: &str = "b";
const CURRENT_LINK: &str = "current";
const MANIFEST_FILE: &str = "manifest.json";

/// Provides paths to all components of a workspace.
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

    /// Directory containing the ping-pong trees (`~/.config/darn/workspaces/<id>/trees/`).
    #[must_use]
    pub fn trees_dir(&self) -> PathBuf {
        self.workspace_dir().join(TREES_DIR)
    }

    /// Path to tree A (`~/.config/darn/workspaces/<id>/trees/a/`).
    #[must_use]
    pub fn tree_a(&self) -> PathBuf {
        self.trees_dir().join(TREE_A)
    }

    /// Path to tree B (`~/.config/darn/workspaces/<id>/trees/b/`).
    #[must_use]
    pub fn tree_b(&self) -> PathBuf {
        self.trees_dir().join(TREE_B)
    }

    /// Path to the `current` symlink (`~/.config/darn/workspaces/<id>/trees/current`).
    #[must_use]
    pub fn current_link(&self) -> PathBuf {
        self.trees_dir().join(CURRENT_LINK)
    }

    // ========================================================================
    // Tree operations
    // ========================================================================

    /// Get the currently active tree (resolves the `current` symlink).
    ///
    /// # Errors
    ///
    /// Returns an error if the symlink doesn't exist or can't be read.
    pub fn active_tree(&self) -> Result<ActiveTree, LayoutError> {
        let link = self.current_link();
        if !link.exists() {
            return Err(LayoutError::NoCurrentTree);
        }

        let target = std::fs::read_link(&link).map_err(LayoutError::ReadLink)?;
        let target_name = target
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(LayoutError::InvalidLinkTarget)?;

        match target_name {
            TREE_A => Ok(ActiveTree::A),
            TREE_B => Ok(ActiveTree::B),
            _ => Err(LayoutError::InvalidLinkTarget),
        }
    }

    /// Get the path to the active tree directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the current symlink can't be resolved.
    pub fn active_tree_path(&self) -> Result<PathBuf, LayoutError> {
        match self.active_tree()? {
            ActiveTree::A => Ok(self.tree_a()),
            ActiveTree::B => Ok(self.tree_b()),
        }
    }

    /// Get the inactive tree (the one not currently pointed to by `current`).
    ///
    /// # Errors
    ///
    /// Returns an error if the current symlink can't be resolved.
    pub fn inactive_tree(&self) -> Result<ActiveTree, LayoutError> {
        Ok(self.active_tree()?.opposite())
    }

    /// Get the path to the inactive tree directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the current symlink can't be resolved.
    pub fn inactive_tree_path(&self) -> Result<PathBuf, LayoutError> {
        match self.inactive_tree()? {
            ActiveTree::A => Ok(self.tree_a()),
            ActiveTree::B => Ok(self.tree_b()),
        }
    }

    // ========================================================================
    // Directory creation
    // ========================================================================

    /// Create all directories for this workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if directories cannot be created.
    pub fn create_dirs(&self) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(self.global_storage_dir())?;
        std::fs::create_dir_all(self.workspace_dir())?;
        std::fs::create_dir_all(self.tree_a())?;
        std::fs::create_dir_all(self.tree_b())?;
        Ok(())
    }

    /// Initialize the `current` symlink to point to tree A.
    ///
    /// # Errors
    ///
    /// Returns an error if the symlink cannot be created.
    pub fn init_current_link(&self) -> Result<(), std::io::Error> {
        let link = self.current_link();
        if link.exists() || link.symlink_metadata().is_ok() {
            std::fs::remove_file(&link)?;
        }
        // Use relative symlink for portability
        std::os::unix::fs::symlink(TREE_A, &link)
    }
}

/// Which tree is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTree {
    /// Tree A is active.
    A,
    /// Tree B is active.
    B,
}

impl ActiveTree {
    /// Get the opposite tree.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    /// Get the directory name for this tree.
    #[must_use]
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::A => TREE_A,
            Self::B => TREE_B,
        }
    }
}

/// Errors working with workspace layout.
#[derive(Debug, Error)]
pub enum LayoutError {
    /// The `current` symlink doesn't exist.
    #[error("no current tree symlink")]
    NoCurrentTree,

    /// Failed to read the symlink.
    #[error("failed to read current symlink: {0}")]
    ReadLink(std::io::Error),

    /// The symlink points to an unexpected target.
    #[error("current symlink points to invalid target")]
    InvalidLinkTarget,
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
            layout.tree_a(),
            PathBuf::from("/test/config/workspaces/00000000000000000000000000000000/trees/a")
        );
    }

    #[test]
    fn active_tree_opposite() {
        assert_eq!(ActiveTree::A.opposite(), ActiveTree::B);
        assert_eq!(ActiveTree::B.opposite(), ActiveTree::A);
    }
}
