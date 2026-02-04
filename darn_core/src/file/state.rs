//! File state tracking.

use std::fmt;

/// State of a tracked file relative to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// File unchanged since last commit.
    Clean,

    /// File modified on disk (hash differs).
    Modified,

    /// File deleted from disk.
    Missing,
}

impl fmt::Display for FileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clean => f.write_str("clean"),
            Self::Modified => f.write_str("modified"),
            Self::Missing => f.write_str("missing"),
        }
    }
}
