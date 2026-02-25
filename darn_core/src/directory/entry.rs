//! Individual items in a directory

use sedimentree_core::id::SedimentreeId;

/// An entry in a directory (file or subdirectory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Name of the entry (filename or directory name).
    pub name: String,

    /// Type of entry.
    pub entry_type: EntryType,

    /// Sedimentree ID pointing to the file or directory document.
    pub sedimentree_id: SedimentreeId,
}

/// Type of directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// A file (points to a File document).
    File,

    /// A subdirectory (points to another Directory document).
    Folder,
}

impl EntryType {
    /// Returns the string representation used in Automerge.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }

    /// Parses from the string representation.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "folder" => Some(Self::Folder),
            _ => None,
        }
    }
}

impl std::fmt::Display for EntryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
