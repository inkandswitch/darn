//! File type for merge strategy selection.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Content kind determines the CRDT merge strategy.
///
/// Serialized as lowercase strings: `"text"`, `"binary"`, or `"immutable"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    /// Character-level CRDT merging (Automerge `Text`).
    Text,

    /// Last-writer-wins binary (Automerge `Bytes`).
    Binary,

    /// Last-writer-wins text (Automerge `ScalarValue::Str`).
    ///
    /// Content is valid UTF-8 and human-readable, but the entire string is
    /// replaced atomically on update — no character-level merging. Useful
    /// when you want text files stored as readable strings but don't need
    /// collaborative editing semantics.
    Immutable,
}

impl FileType {
    /// Returns `true` if this is text content (character-level CRDT).
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Text)
    }

    /// Returns `true` if this is binary content.
    #[must_use]
    pub const fn is_binary(self) -> bool {
        matches!(self, Self::Binary)
    }

    /// Returns `true` if this is immutable text (LWW string).
    #[must_use]
    pub const fn is_immutable(self) -> bool {
        matches!(self, Self::Immutable)
    }

    /// Parse from a MIME type string.
    ///
    /// Any `text/*` MIME type maps to `Text`, everything else to `Binary`.
    /// Note: `Immutable` cannot be inferred from MIME type alone — it
    /// requires an explicit override.
    #[must_use]
    pub fn from_mime_type(mime: &str) -> Self {
        if mime.starts_with("text/") {
            Self::Text
        } else {
            Self::Binary
        }
    }
}

impl fmt::Display for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text => f.write_str("text/plain"),
            Self::Binary => f.write_str("application/octet-stream"),
            Self::Immutable => f.write_str("text/plain; immutable"),
        }
    }
}

impl<Ctx> minicbor::Encode<Ctx> for FileType {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut Ctx,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.str(&self.to_string())?;
        Ok(())
    }
}

impl<'b, Ctx> minicbor::Decode<'b, Ctx> for FileType {
    fn decode(
        d: &mut minicbor::Decoder<'b>,
        _ctx: &mut Ctx,
    ) -> Result<Self, minicbor::decode::Error> {
        let mime = d.str()?;
        if mime == "text/plain; immutable" {
            Ok(Self::Immutable)
        } else {
            Ok(Self::from_mime_type(mime))
        }
    }
}
