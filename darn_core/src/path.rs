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

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    #[test]
    fn normalize_is_idempotent() {
        check!().with_type::<String>().for_each(|s: &String| {
            let path = Path::new(s);
            let once = normalize(path);
            let twice = normalize(&once);
            assert_eq!(once, twice, "normalize should be idempotent");
        });
    }

    #[test]
    fn normalize_output_has_no_dot_or_dotdot() {
        check!().with_type::<String>().for_each(|s: &String| {
            let result = normalize(Path::new(s));
            for component in result.components() {
                assert!(
                    !matches!(component, Component::CurDir | Component::ParentDir),
                    "normalized path should not contain `.` or `..`: {result:?}"
                );
            }
        });
    }
}
