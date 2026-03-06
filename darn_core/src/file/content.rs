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

impl Content {
    /// Reinterprets this content to match the given [`FileType`].
    ///
    /// This is used during refresh so that already-tracked files keep
    /// their manifest file type rather than being re-detected from scratch.
    /// The underlying data is unchanged — only the variant wrapper is adjusted.
    ///
    /// # Conversions
    ///
    /// | Content variant    | Target type | Result                     |
    /// |--------------------|-------------|----------------------------|
    /// | `Text(s)`          | `Immutable` | `ImmutableString(s)`       |
    /// | `ImmutableString(s)` | `Text`    | `Text(s)`                  |
    /// | `Text(s)`          | `Binary`    | `Bytes(s.into_bytes())`    |
    /// | `Bytes(b)`         | `Text`      | `Text(String::from_utf8_lossy)` |
    /// | same kind          | same kind   | unchanged                  |
    #[must_use]
    pub fn coerce_to(self, target: FileType) -> Self {
        match (self, target) {
            // Already matching
            (c @ Self::Text(_), FileType::Text)
            | (c @ Self::Bytes(_), FileType::Binary)
            | (c @ Self::ImmutableString(_), FileType::Immutable) => c,

            // Text ↔ ImmutableString (lossless, just changes merge strategy)
            (Self::Text(s), FileType::Immutable) => Self::ImmutableString(s),
            (Self::ImmutableString(s), FileType::Text) => Self::Text(s),

            // Text/ImmutableString → Binary
            (Self::Text(s) | Self::ImmutableString(s), FileType::Binary) => {
                Self::Bytes(s.into_bytes())
            }

            // Binary → Text/ImmutableString (lossy fallback — unlikely in practice)
            (Self::Bytes(b), FileType::Text) => Self::Text(
                String::from_utf8(b)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
            ),
            (Self::Bytes(b), FileType::Immutable) => Self::ImmutableString(
                String::from_utf8(b)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_text_to_immutable() {
        let content = Content::Text("hello".into());
        let coerced = content.coerce_to(FileType::Immutable);
        assert_eq!(coerced, Content::ImmutableString("hello".into()));
    }

    #[test]
    fn coerce_immutable_to_text() {
        let content = Content::ImmutableString("hello".into());
        let coerced = content.coerce_to(FileType::Text);
        assert_eq!(coerced, Content::Text("hello".into()));
    }

    #[test]
    fn coerce_same_type_is_identity() {
        let text = Content::Text("hello".into());
        assert_eq!(text.clone().coerce_to(FileType::Text), text);

        let bytes = Content::Bytes(vec![1, 2, 3]);
        assert_eq!(bytes.clone().coerce_to(FileType::Binary), bytes);

        let immutable = Content::ImmutableString("hello".into());
        assert_eq!(immutable.clone().coerce_to(FileType::Immutable), immutable);
    }

    #[test]
    fn coerce_text_to_binary() {
        let content = Content::Text("hello".into());
        let coerced = content.coerce_to(FileType::Binary);
        assert_eq!(coerced, Content::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn coerce_binary_to_immutable_valid_utf8() {
        let content = Content::Bytes(b"hello".to_vec());
        let coerced = content.coerce_to(FileType::Immutable);
        assert_eq!(coerced, Content::ImmutableString("hello".into()));
    }

    #[test]
    fn coerce_binary_to_text_invalid_utf8() {
        let content = Content::Bytes(vec![0xFF, 0xFE]);
        let coerced = content.coerce_to(FileType::Text);
        // Should use lossy conversion, not panic
        assert!(coerced.is_text());
    }
}
