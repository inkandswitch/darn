//! Files as Automerge documents.
//!
//! Files in `darn` are stored as Automerge documents following the Patchwork convention:
//!
//! ```ignore
//! FileDocument {
//!   "@patchwork": { type: "file" },
//!   name: string,
//!   content: Text | Bytes,
//!   extension: string,
//!   mimeType: string,
//!   metadata: { permissions: number },
//! }
//! ```
//!
//! - Text files use `Text` (character-level CRDT with automatic merging)
//! - Binary files use `Bytes` (last-writer-wins semantics)
//! - `metadata.permissions` stores the Unix file mode (e.g. `0o644`)

pub mod content;
pub mod file_type;
pub mod metadata;
pub mod name;
pub mod state;

use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use automerge::{transaction::Transactable, Automerge, AutomergeError, ObjType, ReadDoc, ROOT};
use thiserror::Error;

use crate::attributes::AttributeRules;

/// Chunk size for streaming UTF-8 validation.
const UTF8_CHECK_CHUNK_SIZE: usize = 64 * 1024; // 64 KB

/// Files larger than this are treated as binary by default.
///
/// Character-level CRDT merging is expensive for large files, and files this
/// size are almost always generated (bundler output, build artifacts, etc.)
/// rather than hand-edited. Users can override with `*.ext` in the `.darn` config.
const LARGE_FILE_THRESHOLD: u64 = 1024 * 1024; // 1 MB

/// A file represented as an Automerge document.
///
/// This is the in-memory representation of a tracked file. It can be
/// converted to/from an Automerge document for persistence and sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// File name without path (e.g., "README.md", "Makefile").
    pub name: name::Name,

    /// File content (text or binary).
    pub content: content::Content,

    /// File metadata.
    pub metadata: metadata::Metadata,
}

impl File {
    /// Creates a new text file document.
    #[must_use]
    pub fn text(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name::Name::new(name),
            content: content::Content::Text(content.into()),
            metadata: metadata::Metadata::default(),
        }
    }

    /// Creates a new binary file document.
    #[must_use]
    pub fn binary(name: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name::Name::new(name),
            content: content::Content::Bytes(content.into()),
            metadata: metadata::Metadata::default(),
        }
    }

    /// Creates a new immutable text file document (LWW string, no character merging).
    #[must_use]
    pub fn immutable(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name::Name::new(name),
            content: content::Content::ImmutableString(content.into()),
            metadata: metadata::Metadata::default(),
        }
    }

    /// Creates a file document from a filesystem path.
    ///
    /// Automatically detects whether the file is text or binary using streaming
    /// UTF-8 validation. This avoids reading the entire file twice for large files.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn from_path(path: &Path) -> Result<Self, ReadFileError> {
        Self::from_path_full(path, None, false)
    }

    /// Creates a file document from a filesystem path with attribute rules.
    ///
    /// If `attributes` is provided and matches the file path, the specified
    /// file type (text/binary) is used. Otherwise, falls back to automatic
    /// detection via streaming UTF-8 validation.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn from_path_with_attributes(
        path: &Path,
        attributes: Option<&AttributeRules>,
    ) -> Result<Self, ReadFileError> {
        Self::from_path_full(path, attributes, false)
    }

    /// Creates a file document from a filesystem path with full options.
    ///
    /// When `force_immutable` is true, text files are stored as
    /// [`Content::ImmutableString`] (LWW string) instead of [`Content::Text`]
    /// (character-level CRDT). Binary files are unaffected. This only
    /// applies to _newly ingested_ files — already-tracked files keep their
    /// existing type on refresh.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn from_path_full(
        path: &Path,
        attributes: Option<&AttributeRules>,
        force_immutable: bool,
    ) -> Result<Self, ReadFileError> {
        let name = name::Name::from_path(path)
            .ok_or_else(|| ReadFileError::InvalidPath(path.to_path_buf()))?;

        let file_metadata = std::fs::metadata(path)?;

        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            file_metadata.permissions().mode() & 0o777
        };

        #[cfg(not(unix))]
        let permissions = 0o644;

        // Check if attributes specify a file type
        let file_content = match attributes.and_then(|a| a.get_attribute(path)) {
            Some(file_type::FileType::Binary) => {
                // Explicitly binary — read as bytes without UTF-8 check
                content::Content::Bytes(std::fs::read(path)?)
            }
            Some(file_type::FileType::Immutable) => {
                // Explicitly immutable — LWW string
                content::Content::ImmutableString(std::fs::read_to_string(path)?)
            }
            Some(file_type::FileType::Text) => {
                // Explicitly text — force_immutable overrides to LWW string
                let s = std::fs::read_to_string(path)?;
                if force_immutable {
                    content::Content::ImmutableString(s)
                } else {
                    content::Content::Text(s)
                }
            }
            None => {
                // Large files default to binary — character-level CRDT is too expensive
                if file_metadata.len() > LARGE_FILE_THRESHOLD {
                    content::Content::Bytes(std::fs::read(path)?)
                } else {
                    // Auto-detect using streaming UTF-8 validation
                    let detected = streaming_utf8_read(path)?;
                    if force_immutable {
                        match detected {
                            content::Content::Text(s) => content::Content::ImmutableString(s),
                            other => other, // Binary stays binary
                        }
                    } else {
                        detected
                    }
                }
            }
        };

        Ok(Self {
            name,
            content: file_content,
            metadata: metadata::Metadata::from_mode(permissions),
        })
    }

    /// Sets the file permissions from a Unix mode.
    #[must_use]
    pub const fn with_permissions(mut self, mode: u32) -> Self {
        self.metadata = metadata::Metadata::from_mode(mode);
        self
    }

    /// Converts this file document into an Automerge document.
    ///
    /// This borrows `self`, cloning binary content if needed. For binary files,
    /// prefer [`into_automerge`](Self::into_automerge) to avoid the clone.
    ///
    /// # Errors
    ///
    /// Returns an error if the Automerge operations fail.
    pub fn to_automerge(&self) -> Result<Automerge, SerializeError> {
        let mut doc = Automerge::new();

        let extension = extract_extension(self.name.as_str());
        let is_readable_text = self.content.is_text() || self.content.is_immutable_string();
        let mime_type = mime_type_for_extension(&extension, is_readable_text);
        let mode = self.metadata.mode();

        doc.transact::<_, _, AutomergeError>(|tx| {
            // Patchwork metadata
            let patchwork = tx.put_object(ROOT, "@patchwork", ObjType::Map)?;
            let pw_type = tx.put_object(&patchwork, "type", ObjType::Text)?;
            tx.splice_text(&pw_type, 0, 0, "file")?;

            let name_obj = tx.put_object(ROOT, "name", ObjType::Text)?;
            tx.splice_text(&name_obj, 0, 0, self.name.as_str())?;

            match &self.content {
                content::Content::Text(text) => {
                    let text_obj = tx.put_object(ROOT, "content", ObjType::Text)?;
                    tx.splice_text(&text_obj, 0, 0, text)?;
                }
                content::Content::Bytes(bytes) => {
                    tx.put(
                        ROOT,
                        "content",
                        automerge::ScalarValue::Bytes(bytes.clone()),
                    )?;
                }
                content::Content::ImmutableString(text) => {
                    tx.put(
                        ROOT,
                        "content",
                        automerge::ScalarValue::Str(text.clone().into()),
                    )?;
                }
            }

            let ext_obj = tx.put_object(ROOT, "extension", ObjType::Text)?;
            tx.splice_text(&ext_obj, 0, 0, extension.as_str())?;
            let mime_obj = tx.put_object(ROOT, "mimeType", ObjType::Text)?;
            tx.splice_text(&mime_obj, 0, 0, mime_type.as_str())?;

            let metadata_obj = tx.put_object(ROOT, "metadata", ObjType::Map)?;
            tx.put(&metadata_obj, "permissions", i64::from(mode))?;

            Ok(())
        })
        .map_err(|f| f.error)?;

        Ok(doc)
    }

    /// Converts this file document into an Automerge document, consuming self.
    ///
    /// For binary files, this avoids cloning the content bytes. Text files
    /// are copied into Automerge's Text CRDT regardless.
    ///
    /// # Errors
    ///
    /// Returns an error if the Automerge operations fail.
    pub fn into_automerge(self) -> Result<Automerge, SerializeError> {
        let mut doc = Automerge::new();

        let extension = extract_extension(self.name.as_str());
        let is_readable_text = self.content.is_text() || self.content.is_immutable_string();
        let mime_type = mime_type_for_extension(&extension, is_readable_text);
        let mode = self.metadata.mode();
        let name = self.name;
        let content = self.content;

        doc.transact::<_, _, AutomergeError>(|tx| {
            // Patchwork metadata
            let patchwork = tx.put_object(ROOT, "@patchwork", ObjType::Map)?;
            let pw_type = tx.put_object(&patchwork, "type", ObjType::Text)?;
            tx.splice_text(&pw_type, 0, 0, "file")?;

            let name_obj = tx.put_object(ROOT, "name", ObjType::Text)?;
            tx.splice_text(&name_obj, 0, 0, name.as_str())?;

            match &content {
                content::Content::Text(text) => {
                    let text_obj = tx.put_object(ROOT, "content", ObjType::Text)?;
                    tx.splice_text(&text_obj, 0, 0, text)?;
                }
                content::Content::Bytes(bytes) => {
                    tx.put(
                        ROOT,
                        "content",
                        automerge::ScalarValue::Bytes(bytes.clone()),
                    )?;
                }
                content::Content::ImmutableString(text) => {
                    tx.put(
                        ROOT,
                        "content",
                        automerge::ScalarValue::Str(text.clone().into()),
                    )?;
                }
            }

            let ext_obj = tx.put_object(ROOT, "extension", ObjType::Text)?;
            tx.splice_text(&ext_obj, 0, 0, extension.as_str())?;
            let mime_obj = tx.put_object(ROOT, "mimeType", ObjType::Text)?;
            tx.splice_text(&mime_obj, 0, 0, mime_type.as_str())?;

            let metadata_obj = tx.put_object(ROOT, "metadata", ObjType::Map)?;
            tx.put(&metadata_obj, "permissions", i64::from(mode))?;

            Ok(())
        })
        .map_err(|f| f.error)?;

        Ok(doc)
    }

    /// Loads a file document from an Automerge document.
    ///
    /// # Errors
    ///
    /// Returns an error if the document doesn't match the expected schema.
    pub fn from_automerge(doc: &Automerge) -> Result<Self, DeserializeError> {
        let name = name::Name::new(get_text(doc, ROOT, "name")?);

        let file_content = match doc.get(ROOT, "content")? {
            Some((automerge::Value::Object(ObjType::Text), id)) => {
                let text = doc.text(&id)?;
                content::Content::Text(text)
            }
            Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                automerge::ScalarValue::Bytes(bytes) => content::Content::Bytes(bytes.clone()),
                automerge::ScalarValue::Str(smol_str) => {
                    content::Content::ImmutableString(smol_str.to_string())
                }
                _ => {
                    return Err(DeserializeError::InvalidSchema(
                        "content must be Text, Str, or Bytes".into(),
                    ));
                }
            },
            _ => {
                return Err(DeserializeError::InvalidSchema(
                    "missing content field".into(),
                ));
            }
        };

        #[allow(clippy::wildcard_enum_match_arm)]
        // Read permissions from metadata.permissions (Patchwork convention)
        let permissions = match doc.get(ROOT, "metadata")? {
            Some((automerge::Value::Object(ObjType::Map), metadata_id)) => {
                match doc.get(&metadata_id, "permissions")? {
                    Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                        automerge::ScalarValue::Int(i) => u32::try_from(*i).unwrap_or(0o644),
                        automerge::ScalarValue::Uint(u) => u32::try_from(*u).unwrap_or(0o644),
                        _ => 0o644,
                    },
                    _ => 0o644,
                }
            }
            _ => 0o644,
        };

        Ok(Self {
            name,
            content: file_content,
            metadata: metadata::Metadata::from_mode(permissions),
        })
    }

    /// Writes this file to a staging directory (simple direct write).
    ///
    /// Unlike [`write_to_path`](Self::write_to_path), this skips the
    /// atomic temp-file-then-rename pattern since staging directories are
    /// not observed by external tools. Permissions are still set on Unix.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn write_to_staging(&self, path: &Path) -> Result<(), WriteFileError> {
        match &self.content {
            content::Content::Text(text) | content::Content::ImmutableString(text) => {
                std::fs::write(path, text)?;
            }
            content::Content::Bytes(bytes) => std::fs::write(path, bytes)?,
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(self.metadata.mode());
            std::fs::set_permissions(path, perms)?;
        }

        Ok(())
    }

    /// Writes this file document to the filesystem atomically.
    ///
    /// Uses a temp-file-then-rename pattern so readers never see a
    /// partially-written file. On Unix, permissions are set on the temp
    /// file _before_ the rename so the target appears with correct mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn write_to_path(&self, path: &Path) -> Result<(), WriteFileError> {
        // Build a unique temp path in the same directory (same filesystem for rename)
        let tid = std::thread::current().id();
        let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let temp_name = format!(".{stem}.{tid:?}.tmp");
        let temp_path = path.with_file_name(temp_name);

        // Write content to temp file
        match &self.content {
            content::Content::Text(text) | content::Content::ImmutableString(text) => {
                std::fs::write(&temp_path, text)?;
            }
            content::Content::Bytes(bytes) => std::fs::write(&temp_path, bytes)?,
        }

        // Set permissions on temp file before rename so the target appears
        // with correct mode immediately
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(self.metadata.mode());
            if let Err(e) = std::fs::set_permissions(&temp_path, perms) {
                drop(std::fs::remove_file(&temp_path));
                return Err(e.into());
            }
        }

        // On Windows, rename fails if the destination exists
        #[cfg(target_os = "windows")]
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                drop(std::fs::remove_file(&temp_path));
                return Err(e.into());
            }
        }

        // Atomic rename
        if let Err(e) = std::fs::rename(&temp_path, path) {
            drop(std::fs::remove_file(&temp_path));
            return Err(e.into());
        }

        Ok(())
    }
}

/// Extract file extension from a filename.
fn extract_extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Get MIME type for a file extension.
fn mime_type_for_extension(extension: &str, is_text: bool) -> String {
    match extension.to_lowercase().as_str() {
        // Text formats
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" | "map" => "application/json",
        "xml" => "application/xml",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "rs" => "text/x-rust",
        "ts" => "text/typescript",
        "tsx" => "text/typescript-jsx",
        "jsx" => "text/javascript-jsx",
        "yaml" | "yml" => "text/yaml",
        "toml" => "text/toml",
        "sh" => "text/x-shellscript",

        // Image formats
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",

        // Other binary formats
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",

        // Default based on content type
        _ => {
            if is_text {
                "text/plain"
            } else {
                "application/octet-stream"
            }
        }
    }
    .to_string()
}

/// Get a Text CRDT string value from an Automerge document.
fn get_text(doc: &Automerge, obj: automerge::ObjId, key: &str) -> Result<String, DeserializeError> {
    match doc.get(obj, key)? {
        Some((automerge::Value::Object(ObjType::Text), id)) => Ok(doc.text(&id)?),
        _ => Err(DeserializeError::InvalidSchema(format!(
            "missing {key} Text field"
        ))),
    }
}

/// Reads a file with streaming UTF-8 validation.
///
/// Returns `Content::Text` if valid UTF-8, `Content::Bytes` otherwise.
/// Only reads the file once - validates UTF-8 as chunks are read.
#[allow(clippy::indexing_slicing)] // all slice bounds are validated by loop/UTF-8 logic
fn streaming_utf8_read(path: &Path) -> Result<content::Content, std::io::Error> {
    let file = std::fs::File::open(path)?;
    #[allow(clippy::cast_possible_truncation)] // files > usize::MAX can't be allocated anyway
    let file_len = file.metadata()?.len() as usize;
    let mut reader = BufReader::with_capacity(UTF8_CHECK_CHUNK_SIZE, file);

    // Pre-allocate buffer for the expected file size
    let mut bytes = Vec::with_capacity(file_len);
    let mut chunk = vec![0u8; UTF8_CHECK_CHUNK_SIZE].into_boxed_slice();

    // Track incomplete UTF-8 sequence at chunk boundary (max 3 bytes for UTF-8)
    let mut pending: [u8; 3] = [0; 3];
    let mut pending_len: usize = 0;

    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }

        let start = bytes.len();

        // Prepend pending bytes if any
        if pending_len > 0 {
            bytes.extend_from_slice(&pending[..pending_len]);
            pending_len = 0;
        }

        bytes.extend_from_slice(&chunk[..n]);

        // Validate the portion we just added
        match std::str::from_utf8(&bytes[start..]) {
            Ok(_) => {
                // Valid UTF-8, continue
            }
            Err(e) => {
                let valid_up_to = e.valid_up_to();

                if e.error_len().is_none() {
                    // Incomplete sequence at end - move trailing bytes to pending
                    let abs_valid = start + valid_up_to;
                    pending_len = bytes.len() - abs_valid;
                    pending[..pending_len].copy_from_slice(&bytes[abs_valid..]);
                    bytes.truncate(abs_valid);
                } else {
                    // Invalid UTF-8 - re-read as binary
                    return Ok(content::Content::Bytes(std::fs::read(path)?));
                }
            }
        }
    }

    // Check for incomplete sequence at end of file
    if pending_len > 0 {
        return Ok(content::Content::Bytes(std::fs::read(path)?));
    }

    // We validated all chunks - conversion cannot fail
    #[allow(clippy::expect_used)] // UTF-8 validity was verified chunk-by-chunk above
    Ok(content::Content::Text(
        String::from_utf8(bytes).expect("validated UTF-8"),
    ))
}

/// Error reading a file from disk.
#[derive(Debug, Error)]
pub enum ReadFileError {
    /// Invalid file path (no filename).
    #[error("invalid path: {0}")]
    InvalidPath(PathBuf),

    /// I/O error reading file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Error writing a file to disk.
#[derive(Debug, Error)]
#[error("I/O error: {0}")]
pub struct WriteFileError(#[from] std::io::Error);

/// Error serializing to Automerge document.
#[derive(Debug, Error)]
#[error("automerge error: {0}")]
pub struct SerializeError(#[from] automerge::AutomergeError);

/// Error deserializing from Automerge document.
#[derive(Debug, Error)]
pub enum DeserializeError {
    /// Automerge API error.
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),

    /// Document doesn't match expected schema.
    #[error("invalid document: {0}")]
    InvalidSchema(String),
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;
    use testresult::TestResult;

    #[allow(clippy::expect_used)]
    #[test]
    fn text_automerge_roundtrip() {
        check!().with_type::<String>().for_each(|text: &String| {
            let doc = File::text("test.txt", text);
            let am = doc.to_automerge().expect("to_automerge");
            let loaded = File::from_automerge(&am).expect("from_automerge");

            assert_eq!(doc.name, loaded.name);
            assert_eq!(doc.content, loaded.content);
        });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn binary_automerge_roundtrip() {
        check!().with_type::<Vec<u8>>().for_each(|bytes: &Vec<u8>| {
            let doc = File::binary("test.bin", bytes.clone());
            let am = doc.to_automerge().expect("to_automerge");
            let loaded = File::from_automerge(&am).expect("from_automerge");

            assert_eq!(doc.name, loaded.name);
            assert_eq!(doc.content, loaded.content);
        });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn permissions_automerge_roundtrip() {
        check!().with_type::<u16>().for_each(|&bits| {
            let mode = u32::from(bits) & 0o777;
            let doc = File::text("test.sh", "#!/bin/sh").with_permissions(mode);
            let am = doc.to_automerge().expect("to_automerge");
            let loaded = File::from_automerge(&am).expect("from_automerge");

            assert_eq!(loaded.metadata.mode(), mode);
        });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn filesystem_roundtrip() {
        let dir = tempfile::tempdir().expect("create tempdir");
        check!().with_type::<Vec<u8>>().for_each(|bytes: &Vec<u8>| {
            let original_path = dir.path().join("original.bin");
            let output_path = dir.path().join("copy.bin");
            std::fs::write(&original_path, bytes).expect("write");

            let doc = File::from_path(&original_path).expect("from_path");
            doc.write_to_path(&output_path).expect("write_to_path");

            let written = std::fs::read(&output_path).expect("read back");
            assert_eq!(
                &written, bytes,
                "content should survive filesystem roundtrip"
            );
        });
    }

    #[test]
    fn with_permissions_builder() {
        let doc = File::text("test.txt", "content").with_permissions(0o755);
        assert_eq!(doc.metadata.mode(), 0o755);
    }

    #[test]
    fn large_file_defaults_to_binary() -> TestResult {
        let dir = tempfile::tempdir()?;
        let file_path = dir.path().join("big.txt");

        // Create a file just over the threshold (valid UTF-8 content)
        #[allow(clippy::cast_possible_truncation)]
        let content = "x".repeat(LARGE_FILE_THRESHOLD as usize + 1);
        std::fs::write(&file_path, &content)?;

        let doc = File::from_path(&file_path)?;

        assert!(
            matches!(doc.content, content::Content::Bytes(_)),
            "large file should default to binary"
        );
        Ok(())
    }

    #[test]
    fn large_file_respects_text_attribute() -> TestResult {
        use crate::attributes::AttributeRules;
        use crate::dotfile::{AttributeMap, DarnConfig};
        use crate::workspace::WorkspaceId;
        use sedimentree_core::id::SedimentreeId;

        let dir = tempfile::tempdir()?;
        let file_path = dir.path().join("big.txt");

        // Create .darn config with text attribute for *.txt
        let config = DarnConfig::with_fields(
            WorkspaceId::from_bytes([1; 16]),
            SedimentreeId::new([2; 32]),
            false,
            Vec::new(),
            AttributeMap {
                binary: Vec::new(),
                immutable: Vec::new(),
                text: vec!["*.txt".to_string()],
            },
        );
        config.save(dir.path())?;

        // Create a file just over the threshold
        #[allow(clippy::cast_possible_truncation)]
        let content = "x".repeat(LARGE_FILE_THRESHOLD as usize + 1);
        std::fs::write(&file_path, &content)?;

        let attrs = AttributeRules::from_workspace_root(dir.path())?;
        let doc = File::from_path_with_attributes(&file_path, Some(&attrs))?;

        assert!(
            matches!(doc.content, content::Content::Text(_)),
            "explicit text attribute should override size heuristic"
        );
        Ok(())
    }

    #[test]
    fn extension_and_mimetype_set() -> TestResult {
        let doc = File::text("script.js", "console.log('hello');");
        let am = doc.to_automerge()?;

        // Check extension
        let ext = get_text(&am, ROOT, "extension")?;
        assert_eq!(ext, "js");

        // Check mimeType
        let mime = get_text(&am, ROOT, "mimeType")?;
        assert_eq!(mime, "text/javascript");
        Ok(())
    }
}
