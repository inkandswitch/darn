//! Result of refreshing tracked files.

use std::path::PathBuf;

use crate::refresh::RefreshError;

/// Diff produced by refreshing all tracked files.
#[derive(Debug, Default)]
pub struct RefreshDiff {
    /// Files that were updated.
    pub updated: Vec<PathBuf>,

    /// Files that are missing from disk.
    pub missing: Vec<PathBuf>,

    /// Files that encountered errors during refresh.
    pub errors: Vec<(PathBuf, RefreshError)>,
}

impl RefreshDiff {
    /// Returns `true` if no files were updated and no errors occurred.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.errors.is_empty()
    }

    /// Returns the number of successfully updated files.
    #[must_use]
    pub const fn updated_count(&self) -> usize {
        self.updated.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_files_not_empty() {
        let diff = RefreshDiff {
            updated: vec![PathBuf::from("foo.txt")],
            ..Default::default()
        };
        assert!(!diff.is_empty());
        assert_eq!(diff.updated_count(), 1);
    }

    #[test]
    fn errors_make_not_empty() {
        let err = RefreshError::InvalidDocument("test".into());
        let diff = RefreshDiff {
            errors: vec![(PathBuf::from("bad.txt"), err)],
            ..Default::default()
        };
        assert!(!diff.is_empty(), "errors should make is_empty false");
        assert_eq!(diff.updated_count(), 0);
    }

    /// `is_empty` intentionally ignores `missing` — missing files are not
    /// actionable changes, just informational.
    #[test]
    fn missing_alone_is_still_empty() {
        let diff = RefreshDiff {
            missing: vec![PathBuf::from("gone.txt")],
            ..Default::default()
        };
        assert!(
            diff.is_empty(),
            "missing files alone should not make is_empty false"
        );
    }
}
