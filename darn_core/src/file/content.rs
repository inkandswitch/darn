//! File content.

use super::file_type::FileType;

/// Content stored in a file document.
///
/// Text files get character-level CRDT merging, binary files use
/// last-writer-wins byte replacement, and immutable text files store
/// UTF-8 strings with last-writer-wins replacement (no character merging).
///
/// # Future Work
///
/// Large files are currently loaded entirely into memory. A future version
/// may add chunked/streaming storage or external blob references for files
/// beyond a certain size threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// UTF-8 text content (character-level CRDT).
    Text(String),

    /// Binary content (last-writer-wins).
    Bytes(Vec<u8>),

    /// UTF-8 text content with last-writer-wins semantics.
    ///
    /// Stored as an Automerge `ScalarValue::Str` — the entire string is
    /// replaced atomically on update. Human-readable in Patchwork/JS
    /// (appears as a plain string, not a `Uint8Array`), but without
    /// character-level merge support.
    ImmutableString(String),
    // TODO Consider adding large file support with external blob references
}

impl Content {
    /// Returns `true` if this is text content (character-level CRDT).
    #[must_use]
    pub const fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    /// Returns `true` if this is binary content.
    #[must_use]
    pub const fn is_bytes(&self) -> bool {
        matches!(self, Self::Bytes(_))
    }

    /// Returns `true` if this is immutable text (LWW string).
    #[must_use]
    pub const fn is_immutable_string(&self) -> bool {
        matches!(self, Self::ImmutableString(_))
    }

    /// Returns the text content if this is a text or immutable text document.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) | Self::ImmutableString(s) => Some(s),
            Self::Bytes(_) => None,
        }
    }

    /// Returns the binary content if this is a binary document.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Text(_) | Self::ImmutableString(_) => None,
            Self::Bytes(b) => Some(b),
        }
    }
}

impl From<Content> for FileType {
    fn from(c: Content) -> Self {
        match c {
            Content::Text(_) => FileType::Text,
            Content::Bytes(_) => FileType::Binary,
            Content::ImmutableString(_) => FileType::Immutable,
        }
    }
}

impl From<&Content> for FileType {
    fn from(c: &Content) -> Self {
        match c {
            Content::Text(_) => FileType::Text,
            Content::Bytes(_) => FileType::Binary,
            Content::ImmutableString(_) => FileType::Immutable,
        }
    }
}
