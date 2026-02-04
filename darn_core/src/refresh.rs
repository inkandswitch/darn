//! Refresh error types and Automerge content update helpers.

use automerge::{transaction::Transactable, AutoCommit, ObjType, ReadDoc, ROOT};
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
    doc: &mut AutoCommit,
    new_content: Content,
) -> Result<(), RefreshError> {
    match new_content {
        Content::Text(text) => {
            // Get the content text object
            let Some((automerge::Value::Object(ObjType::Text), content_id)) =
                doc.get(ROOT, "content")?
            else {
                return Err(RefreshError::InvalidDocument(
                    "content must be Text object".into(),
                ));
            };

            // Replace all text content
            let old_len = doc.text(&content_id)?.chars().count();
            let old_len_isize = isize::try_from(old_len).unwrap_or(isize::MAX);
            doc.splice_text(&content_id, 0, old_len_isize, &text)?;
        }
        Content::Bytes(bytes) => {
            // Replace bytes directly (LWW, no clone needed)
            doc.put(ROOT, "content", automerge::ScalarValue::Bytes(bytes))?;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::File;

    #[test]
    fn update_text_content() {
        let doc = File::text("test.txt", "original content");
        let mut am_doc = doc.to_automerge().expect("to_automerge");

        let new_content = Content::Text("updated content".to_string());
        update_automerge_content(&mut am_doc, new_content).expect("update content");

        let loaded = File::from_automerge(&am_doc).expect("from_automerge");
        assert_eq!(loaded.content, Content::Text("updated content".to_string()));
    }

    #[test]
    fn update_binary_content() {
        let doc = File::binary("test.bin", vec![1, 2, 3]);
        let mut am_doc = doc.to_automerge().expect("to_automerge");

        let new_content = Content::Bytes(vec![4, 5, 6, 7]);
        update_automerge_content(&mut am_doc, new_content).expect("update content");

        let loaded = File::from_automerge(&am_doc).expect("from_automerge");
        assert_eq!(loaded.content, Content::Bytes(vec![4, 5, 6, 7]));
    }
}
