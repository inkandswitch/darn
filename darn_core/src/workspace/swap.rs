//! Atomic tree swap operations.
//!
//! Implements the ping-pong tree swap pattern for atomic working tree updates.

use std::{fs, io, path::Path};

use thiserror::Error;
use walkdir::WalkDir;

use super::layout::{LayoutError, WorkspaceLayout};

/// Atomically swap the current tree to the inactive tree.
///
/// This performs an atomic symlink swap using the rename trick:
/// 1. Create a new symlink `current.tmp` pointing to the target tree
/// 2. Rename `current.tmp` to `current` (atomic on POSIX)
///
/// # Errors
///
/// Returns an error if the symlink operations fail.
pub fn atomic_swap(layout: &WorkspaceLayout) -> Result<(), SwapError> {
    let target_tree = layout.inactive_tree()?;
    let link_path = layout.current_link();
    let temp_link = link_path.with_file_name("current.tmp");

    // Remove temp link if it exists from a previous failed swap
    if temp_link.symlink_metadata().is_ok() {
        fs::remove_file(&temp_link).map_err(SwapError::RemoveTempLink)?;
    }

    // Create new symlink pointing to target tree
    std::os::unix::fs::symlink(target_tree.dir_name(), &temp_link)
        .map_err(SwapError::CreateTempLink)?;

    // Atomic rename
    fs::rename(&temp_link, &link_path).map_err(SwapError::AtomicRename)?;

    Ok(())
}

/// Clear the inactive tree in preparation for building a new state.
///
/// # Errors
///
/// Returns an error if the directory cannot be cleared.
pub fn clear_inactive_tree(layout: &WorkspaceLayout) -> Result<(), SwapError> {
    let inactive_path = layout.inactive_tree_path()?;

    // Remove all contents but keep the directory
    if inactive_path.exists() {
        for entry in fs::read_dir(&inactive_path).map_err(SwapError::ClearTree)? {
            let entry = entry.map_err(SwapError::ClearTree)?;
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path).map_err(SwapError::ClearTree)?;
            } else {
                fs::remove_file(&path).map_err(SwapError::ClearTree)?;
            }
        }
    }

    Ok(())
}

/// Copy ignored files from the active tree to the inactive tree.
///
/// Ignored files are those present in the active tree but not tracked
/// in the manifest. This preserves user files like `.env`, IDE configs, etc.
///
/// # Arguments
///
/// * `layout` - The workspace layout
/// * `is_tracked` - Predicate that returns true if a relative path is tracked
///
/// # Errors
///
/// Returns an error if copying fails.
pub fn copy_ignored_files<F>(layout: &WorkspaceLayout, is_tracked: F) -> Result<usize, SwapError>
where
    F: Fn(&Path) -> bool,
{
    let active_tree = layout.active_tree_path()?;
    let inactive_tree = layout.inactive_tree_path()?;

    let mut copied_count = 0;

    for entry in WalkDir::new(&active_tree)
        .into_iter()
        .filter_map(Result::ok)
    {
        let full_path = entry.path();

        // Skip the root directory itself
        if full_path == active_tree {
            continue;
        }

        let relative = full_path
            .strip_prefix(&active_tree)
            .map_err(|_| SwapError::StripPrefix)?;

        // Skip tracked files - they'll be materialized from storage
        if is_tracked(relative) {
            continue;
        }

        // Skip internal darn files (shouldn't exist in tree, but just in case)
        if relative.starts_with(".darn") {
            continue;
        }

        let dest = inactive_tree.join(relative);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest).map_err(SwapError::CopyIgnored)?;
        } else if entry.file_type().is_symlink() {
            // Copy symlink itself, not its target
            let target = fs::read_link(full_path).map_err(SwapError::CopyIgnored)?;
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(SwapError::CopyIgnored)?;
            }
            // Remove existing if present
            if dest.symlink_metadata().is_ok() {
                fs::remove_file(&dest).map_err(SwapError::CopyIgnored)?;
            }
            std::os::unix::fs::symlink(target, &dest).map_err(SwapError::CopyIgnored)?;
            copied_count += 1;
        } else if entry.file_type().is_file() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(SwapError::CopyIgnored)?;
            }
            fs::copy(full_path, &dest).map_err(SwapError::CopyIgnored)?;
            copied_count += 1;

            // Preserve permissions
            let metadata = fs::metadata(full_path).map_err(SwapError::CopyIgnored)?;
            fs::set_permissions(&dest, metadata.permissions()).map_err(SwapError::CopyIgnored)?;
        }
        // Skip special files (sockets, FIFOs, devices)
    }

    Ok(copied_count)
}

/// Create or update the user's project symlink.
///
/// The symlink at `project_path` points to the workspace's `current` tree.
///
/// # Errors
///
/// Returns an error if the symlink cannot be created.
pub fn create_project_symlink(
    layout: &WorkspaceLayout,
    project_path: &Path,
) -> Result<(), SwapError> {
    let target = layout.current_link();

    // If something exists at the project path, we need to handle it
    if project_path.symlink_metadata().is_ok() {
        // Check if it's already a symlink to our target
        if project_path.is_symlink() {
            let existing_target =
                fs::read_link(project_path).map_err(SwapError::CreateProjectLink)?;
            if existing_target == target {
                return Ok(()); // Already correct
            }
        }

        // Remove the existing path (symlink or otherwise)
        // NOTE: This is destructive! The caller should have migrated contents first.
        if project_path.is_dir() && !project_path.is_symlink() {
            fs::remove_dir_all(project_path).map_err(SwapError::CreateProjectLink)?;
        } else {
            fs::remove_file(project_path).map_err(SwapError::CreateProjectLink)?;
        }
    }

    // Create parent directory if needed
    if let Some(parent) = project_path.parent() {
        fs::create_dir_all(parent).map_err(SwapError::CreateProjectLink)?;
    }

    std::os::unix::fs::symlink(&target, project_path).map_err(SwapError::CreateProjectLink)?;

    Ok(())
}

/// Errors during tree swap operations.
#[derive(Debug, Error)]
pub enum SwapError {
    /// Failed to determine active/inactive tree.
    #[error(transparent)]
    Layout(#[from] LayoutError),

    /// Failed to remove temporary symlink.
    #[error("failed to remove temporary symlink: {0}")]
    RemoveTempLink(io::Error),

    /// Failed to create temporary symlink.
    #[error("failed to create temporary symlink: {0}")]
    CreateTempLink(io::Error),

    /// Failed to atomically rename symlink.
    #[error("failed to atomic rename: {0}")]
    AtomicRename(io::Error),

    /// Failed to clear inactive tree.
    #[error("failed to clear tree: {0}")]
    ClearTree(io::Error),

    /// Failed to strip path prefix.
    #[error("failed to compute relative path")]
    StripPrefix,

    /// Failed to copy ignored files.
    #[error("failed to copy ignored files: {0}")]
    CopyIgnored(io::Error),

    /// Failed to create project symlink.
    #[error("failed to create project symlink: {0}")]
    CreateProjectLink(io::Error),
}

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{layout::ActiveTree, WorkspaceId};
    use std::collections::HashSet;

    fn setup_test_workspace() -> (tempfile::TempDir, WorkspaceLayout) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let id = WorkspaceId::from_bytes([0; 16]);
        let layout = WorkspaceLayout::with_config_dir(id, dir.path().to_path_buf());

        layout.create_dirs().expect("create dirs");
        layout.init_current_link().expect("init current link");

        (dir, layout)
    }

    #[test]
    fn atomic_swap_changes_active_tree() {
        let (_dir, layout) = setup_test_workspace();

        assert_eq!(layout.active_tree().unwrap(), ActiveTree::A);

        atomic_swap(&layout).expect("swap");

        assert_eq!(layout.active_tree().unwrap(), ActiveTree::B);

        atomic_swap(&layout).expect("swap again");

        assert_eq!(layout.active_tree().unwrap(), ActiveTree::A);
    }

    #[test]
    fn clear_inactive_tree_removes_contents() {
        let (_dir, layout) = setup_test_workspace();

        // Create some files in tree B (inactive)
        let tree_b = layout.tree_b();
        fs::create_dir_all(tree_b.join("subdir")).expect("create subdir");
        fs::write(tree_b.join("file.txt"), "hello").expect("write file");
        fs::write(tree_b.join("subdir/nested.txt"), "world").expect("write nested");

        clear_inactive_tree(&layout).expect("clear");

        // Tree B should be empty but exist
        assert!(tree_b.exists());
        assert!(fs::read_dir(&tree_b).unwrap().next().is_none());
    }

    #[test]
    fn copy_ignored_files_preserves_untracked() {
        let (_dir, layout) = setup_test_workspace();

        // Create files in tree A (active)
        let tree_a = layout.tree_a();
        fs::write(tree_a.join("tracked.txt"), "tracked content").expect("write tracked");
        fs::write(tree_a.join(".env"), "SECRET=value").expect("write env");
        fs::create_dir_all(tree_a.join(".vscode")).expect("create vscode");
        fs::write(tree_a.join(".vscode/settings.json"), "{}").expect("write vscode");

        // Track only tracked.txt
        let tracked: HashSet<_> = [Path::new("tracked.txt").to_path_buf()]
            .into_iter()
            .collect();

        let copied = copy_ignored_files(&layout, |p| tracked.contains(p)).expect("copy");

        // Should have copied .env and .vscode/settings.json
        assert_eq!(copied, 2);

        let tree_b = layout.tree_b();
        assert!(tree_b.join(".env").exists());
        assert!(tree_b.join(".vscode/settings.json").exists());
        assert!(!tree_b.join("tracked.txt").exists());
    }
}
