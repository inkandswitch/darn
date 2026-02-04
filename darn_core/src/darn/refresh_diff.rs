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
