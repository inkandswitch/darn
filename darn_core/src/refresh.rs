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
pub fn update_automerge_content(
    doc: &mut Automerge,
    new_content: Content,
) -> Result<(), RefreshError> {
    // For text, we need to get the content_id first (read-only)
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
        Content::Bytes(_) => None,
    };

    doc.transact::<_, _, AutomergeError>(|tx| {
        match new_content {
            Content::Text(text) => {
                let (content_id, old_len) = content_info.expect("content_info set for text");
                let old_len_isize = isize::try_from(old_len).unwrap_or(isize::MAX);
                tx.splice_text(&content_id, 0, old_len_isize, &text)?;
            }
            Content::Bytes(bytes) => {
                tx.put(ROOT, "content", automerge::ScalarValue::Bytes(bytes))?;
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
    use testresult::TestResult;

    #[test]
    fn update_text_content() -> TestResult {
        let doc = File::text("test.txt", "original content");
        let mut am_doc = doc.to_automerge()?;

        let new_content = Content::Text("updated content".to_string());
        update_automerge_content(&mut am_doc, new_content)?;

        let loaded = File::from_automerge(&am_doc)?;
        assert_eq!(loaded.content, Content::Text("updated content".to_string()));

        Ok(())
    }

    #[test]
    fn update_binary_content() -> TestResult {
        let doc = File::binary("test.bin", vec![1, 2, 3]);
        let mut am_doc = doc.to_automerge()?;

        let new_content = Content::Bytes(vec![4, 5, 6, 7]);
        update_automerge_content(&mut am_doc, new_content)?;

        let loaded = File::from_automerge(&am_doc)?;
        assert_eq!(loaded.content, Content::Bytes(vec![4, 5, 6, 7]));

        Ok(())
    }
}
