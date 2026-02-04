//! Path utilities.

use std::path::{Component, Path, PathBuf};

/// Normalize a path without requiring the file to exist.
///
/// Handles `./`, `../`, and redundant separators. Unlike [`std::fs::canonicalize`],
/// this works on paths that don't exist on disk.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use darn_core::path::normalize;
///
/// assert_eq!(normalize(Path::new("foo/./bar")), Path::new("foo/bar"));
/// assert_eq!(normalize(Path::new("foo/bar/../baz")), Path::new("foo/baz"));
/// assert_eq!(normalize(Path::new("./foo")), Path::new("foo"));
/// ```
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            c @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                result.push(c);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_current_dir() {
        assert_eq!(normalize(Path::new("foo/./bar")), Path::new("foo/bar"));
        assert_eq!(normalize(Path::new("./foo")), Path::new("foo"));
        assert_eq!(normalize(Path::new("foo/.")), Path::new("foo"));
    }

    #[test]
    fn resolves_parent_dir() {
        assert_eq!(normalize(Path::new("foo/bar/../baz")), Path::new("foo/baz"));
        assert_eq!(normalize(Path::new("foo/../bar")), Path::new("bar"));
    }

    #[test]
    fn preserves_absolute_paths() {
        assert_eq!(normalize(Path::new("/foo/bar")), Path::new("/foo/bar"));
        assert_eq!(normalize(Path::new("/foo/./bar")), Path::new("/foo/bar"));
    }

    #[test]
    fn handles_empty_result() {
        assert_eq!(normalize(Path::new("foo/..")), Path::new(""));
        assert_eq!(normalize(Path::new(".")), Path::new(""));
    }
}
