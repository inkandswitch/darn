//! Refresh error types and Automerge content update helpers.

use automerge::{transaction::Transactable, Automerge, AutomergeError, ObjType, ReadDoc, ROOT};
use thiserror::Error;

use crate::file::content::Content;
use crate::sedimentree::SedimentreeError;

/// Updates Automerge document content with new file content, consuming it.
///
/// For text files, this performs a full text replacement using `splice_text`.
/// For binary files, this uses `put` to replace the bytes (LWW semantics).
///
/// Takes ownership of `new_content` to avoid cloning binary data.
///
/// # Errors
///
/// Returns an error if:
/// - The document schema is invalid (content field missing or wrong type)
/// - An Automerge operation fails
///
/// # Panics
///
/// Panics if the internal content-info state is inconsistent (should not
/// happen in practice since the `Text` branch always populates it).
pub fn update_automerge_content(
    doc: &mut Automerge,
    new_content: Content,
) -> Result<(), RefreshError> {
    // For text (CRDT), we need to get the content_id first (read-only)
    let content_info = match &new_content {
        Content::Text(_) => {
            let Some((automerge::Value::Object(ObjType::Text), content_id)) =
                doc.get(ROOT, "content")?
            else {
                return Err(RefreshError::InvalidDocument(
                    "content must be Text object".into(),
                ));
            };
            let old_len = doc.text(&content_id)?.chars().count();
            Some((content_id, old_len))
        }
        Content::Bytes(_) | Content::ImmutableString(_) => None,
    };

    doc.transact::<_, _, AutomergeError>(|tx| {
        match new_content {
            Content::Text(text) => {
                // content_info is always Some when new_content is Text (set in the match above)
                #[allow(clippy::expect_used)]
                let (content_id, old_len) = content_info.expect("content_info set for text");
                let old_len_isize = isize::try_from(old_len).unwrap_or(isize::MAX);
                tx.splice_text(&content_id, 0, old_len_isize, &text)?;
            }
            Content::Bytes(bytes) => {
                tx.put(ROOT, "content", automerge::ScalarValue::Bytes(bytes))?;
            }
            Content::ImmutableString(text) => {
                tx.put(ROOT, "content", automerge::ScalarValue::Str(text.into()))?;
            }
        }
        Ok(())
    })
    .map_err(|f| f.error)?;

    Ok(())
}

/// Error during file refresh.
#[derive(Debug, Error)]
pub enum RefreshError {
    /// I/O error reading file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Error reading file document.
    #[error("failed to read file: {0}")]
    ReadFile(#[from] crate::file::ReadFileError),

    /// Automerge error.
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),

    /// Storage error.
    #[error("storage error: {0}")]
    Storage(Box<SedimentreeError>),

    /// Invalid document schema.
    #[error("invalid document: {0}")]
    InvalidDocument(String),
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::File;
    use bolero::check;

    #[allow(clippy::expect_used)]
    #[test]
    fn update_text_content_roundtrip() {
        check!()
            .with_type::<(String, String)>()
            .for_each(|(original, updated)| {
                let doc = File::text("test.txt", original);
                let mut am_doc = doc.to_automerge().expect("to_automerge");

                let new_content = Content::Text(updated.clone());
                update_automerge_content(&mut am_doc, new_content).expect("update");

                let loaded = File::from_automerge(&am_doc).expect("from_automerge");
                assert_eq!(loaded.content, Content::Text(updated.clone()));
            });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn update_immutable_string_content_roundtrip() {
        check!()
            .with_type::<(String, String)>()
            .for_each(|(original, updated)| {
                let doc = File::immutable("test.txt", original);
                let mut am_doc = doc.to_automerge().expect("to_automerge");

                let new_content = Content::ImmutableString(updated.clone());
                update_automerge_content(&mut am_doc, new_content).expect("update");

                let loaded = File::from_automerge(&am_doc).expect("from_automerge");
                assert_eq!(loaded.content, Content::ImmutableString(updated.clone()));
            });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn update_binary_content_roundtrip() {
        check!()
            .with_type::<(Vec<u8>, Vec<u8>)>()
            .for_each(|(original, updated)| {
                let doc = File::binary("test.bin", original.clone());
                let mut am_doc = doc.to_automerge().expect("to_automerge");

                let new_content = Content::Bytes(updated.clone());
                update_automerge_content(&mut am_doc, new_content).expect("update");

                let loaded = File::from_automerge(&am_doc).expect("from_automerge");
                assert_eq!(loaded.content, Content::Bytes(updated.clone()));
            });
    }

    /// Regression: refreshing an `ImmutableString` doc with `Text` content
    /// (i.e., what `from_path_with_attributes` returns when `force_immutable`
    /// is not passed) must fail — proving that the coercion in `darn.rs` is
    /// necessary. Without `Content::coerce_to`, this exact scenario would
    /// hit `InvalidDocument("content must be Text object")`.
    #[allow(clippy::expect_used)]
    #[test]
    fn refresh_immutable_doc_with_text_content_fails_without_coercion() {
        let doc = File::immutable("readme.txt", "original");
        let mut am_doc = doc.to_automerge().expect("to_automerge");

        // Simulate what the refresh path would produce without coercion:
        // disk file re-detected as Text instead of ImmutableString.
        let mismatched = Content::Text("updated".into());
        let result = update_automerge_content(&mut am_doc, mismatched);

        assert!(
            result.is_err(),
            "Text content on an ImmutableString doc must fail"
        );
    }

    /// Verify the fix: coercing `Text` → `ImmutableString` before refresh works.
    #[allow(clippy::expect_used)]
    #[test]
    fn refresh_immutable_doc_with_coerced_content_succeeds() {
        use crate::file::{content, file_type::FileType};

        let doc = File::immutable("readme.txt", "original");
        let mut am_doc = doc.to_automerge().expect("to_automerge");

        // Simulate the fixed refresh path: re-detected as Text, then coerced.
        let redetected = content::Content::Text("updated".into());
        let coerced = redetected.coerce_to(FileType::Immutable);

        update_automerge_content(&mut am_doc, coerced).expect("coerced update should succeed");

        let loaded = File::from_automerge(&am_doc).expect("from_automerge");
        assert_eq!(loaded.content, Content::ImmutableString("updated".into()));
    }
}
