//! Directory documents for syncing file paths between replicas.
//!
//! Directories in `darn` are stored as Automerge documents following the Patchwork convention:
//!
//! ```ignore
//! FolderDoc {
//!   "@patchwork": { type: "folder" },
//!   title: string,
//!   docs: [
//!     { name: string, type: "file" | "folder", url: "automerge:<bs58check>" },
//!     ...
//!   ],
//! }
//! ```
//!
//! Each directory is its own Automerge CRDT, allowing concurrent edits to different
//! directories to merge automatically. The root directory has a well-known sedimentree ID
//! derived from a fixed seed.

pub mod entry;

use self::entry::{DirectoryEntry, EntryType};
use automerge::{transaction::Transactable, Automerge, AutomergeError, ObjType, ReadDoc, ROOT};
use sedimentree_core::id::SedimentreeId;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Encode bytes as bs58check (matching JavaScript's `bs58check` library).
///
/// JavaScript's `bs58check` encodes as: `base58(payload || sha256d(payload)[..4])`.
/// Note: Rust's `bs58::encode().with_check()` prepends a version byte (`0x00`),
/// which is incompatible with the JS library. This function computes the checksum
/// manually to match.
fn bs58check_encode(payload: &[u8]) -> String {
    let checksum = Sha256::digest(Sha256::digest(payload));

    let mut buf = Vec::with_capacity(payload.len() + 4);
    buf.extend_from_slice(payload);
    #[allow(clippy::indexing_slicing)] // SHA-256 always produces 32 bytes; 4 < 32
    buf.extend_from_slice(&checksum[..4]);

    bs58::encode(buf).into_string()
}

/// Decode a bs58check string (matching JavaScript's `bs58check` library).
///
/// Returns the payload bytes after verifying the 4-byte SHA-256d checksum.
///
/// # Errors
///
/// Returns an error if the base58 encoding is invalid, the input is too
/// short, or the checksum doesn't match.
pub fn bs58check_decode(encoded: &str) -> Result<Vec<u8>, String> {
    let bytes = bs58::decode(encoded)
        .into_vec()
        .map_err(|e| format!("invalid base58: {e}"))?;

    if bytes.len() < 5 {
        return Err("too short for bs58check".into());
    }

    let (payload, checksum) = bytes.split_at(bytes.len() - 4);
    let expected = Sha256::digest(Sha256::digest(payload));

    #[allow(clippy::indexing_slicing)] // SHA-256 always produces 32 bytes; 4 < 32
    if checksum != &expected[..4] {
        return Err("checksum mismatch".into());
    }

    Ok(payload.to_vec())
}

/// Convert a `SedimentreeId` to an Automerge URL string.
///
/// Uses bs58check encoding of the first 16 bytes (automerge-repo convention).
/// The remaining 16 bytes of the sedimentree ID are zero-padding.
#[must_use]
pub fn sedimentree_id_to_url(id: SedimentreeId) -> String {
    let bytes = id.as_bytes();
    format!("automerge:{}", bs58check_encode(&bytes[..16]))
}

/// Parse an Automerge URL string to a `SedimentreeId`.
///
/// Expects `automerge:<bs58check>` encoding a 16-byte document ID,
/// which is zero-padded to 32 bytes for `SedimentreeId`.
fn url_to_sedimentree_id(url: &str) -> Result<SedimentreeId, DeserializeError> {
    let encoded = url.strip_prefix("automerge:").ok_or_else(|| {
        DeserializeError::InvalidSchema(format!("url must start with 'automerge:': {url}"))
    })?;

    let bytes = bs58check_decode(encoded)
        .map_err(|e| DeserializeError::InvalidSchema(format!("invalid bs58check in url: {e}")))?;

    if bytes.len() != 16 {
        return Err(DeserializeError::InvalidSchema(format!(
            "url must encode 16 bytes, got {}",
            bytes.len()
        )));
    }

    let mut arr = [0u8; 32];
    arr[..16].copy_from_slice(&bytes);
    Ok(SedimentreeId::new(arr))
}

/// A directory represented as an Automerge document.
///
/// This is the in-memory representation of a tracked directory. It can be
/// converted to/from an Automerge document for persistence and sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    /// Directory name (just the final component, not full path).
    pub name: String,

    /// Entries in this directory (files and subdirectories).
    pub entries: Vec<DirectoryEntry>,
}

impl Directory {
    /// Creates a new empty directory with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entries: Vec::new(),
        }
    }

    /// Creates the root directory.
    #[must_use]
    pub fn root() -> Self {
        Self::new("")
    }

    /// Adds a file entry to this directory.
    pub fn add_file(&mut self, name: impl Into<String>, sedimentree_id: SedimentreeId) {
        let name = name.into();

        // Remove existing entry with same name if present
        self.entries.retain(|e| e.name != name);

        self.entries.push(DirectoryEntry {
            name,
            entry_type: EntryType::File,
            sedimentree_id,
        });
    }

    /// Adds a folder entry to this directory.
    pub fn add_folder(&mut self, name: impl Into<String>, sedimentree_id: SedimentreeId) {
        let name = name.into();

        // Remove existing entry with same name if present
        self.entries.retain(|e| e.name != name);

        self.entries.push(DirectoryEntry {
            name,
            entry_type: EntryType::Folder,
            sedimentree_id,
        });
    }

    /// Removes an entry by name.
    ///
    /// Returns the removed entry if found.
    pub fn remove(&mut self, name: &str) -> Option<DirectoryEntry> {
        let pos = self.entries.iter().position(|e| e.name == name)?;
        Some(self.entries.remove(pos))
    }

    /// Gets an entry by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&DirectoryEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Returns `true` if this directory is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of entries in this directory.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Converts this directory document into an Automerge document.
    ///
    /// # Errors
    ///
    /// Returns an error if the Automerge operations fail.
    pub fn to_automerge(&self) -> Result<Automerge, SerializeError> {
        let mut doc = Automerge::new();

        doc.transact::<_, _, AutomergeError>(|tx| {
            // Patchwork metadata
            let patchwork = tx.put_object(ROOT, "@patchwork", ObjType::Map)?;
            let pw_type = tx.put_object(&patchwork, "type", ObjType::Text)?;
            tx.splice_text(&pw_type, 0, 0, "folder")?;

            let title = tx.put_object(ROOT, "title", ObjType::Text)?;
            tx.splice_text(&title, 0, 0, self.name.as_str())?;

            let docs = tx.put_object(ROOT, "docs", ObjType::List)?;
            for (idx, entry) in self.entries.iter().enumerate() {
                let entry_obj = tx.insert_object(&docs, idx, ObjType::Map)?;
                let name = tx.put_object(&entry_obj, "name", ObjType::Text)?;
                tx.splice_text(&name, 0, 0, entry.name.as_str())?;
                let entry_type = tx.put_object(&entry_obj, "type", ObjType::Text)?;
                tx.splice_text(&entry_type, 0, 0, entry.entry_type.as_str())?;
                let url = tx.put_object(&entry_obj, "url", ObjType::Text)?;
                tx.splice_text(&url, 0, 0, &sedimentree_id_to_url(entry.sedimentree_id))?;
            }

            Ok(())
        })
        .map_err(|f| f.error)?;

        Ok(doc)
    }

    /// Loads a directory document from an Automerge document.
    ///
    /// # Errors
    ///
    /// Returns an error if the document doesn't match the expected schema.
    pub fn from_automerge(doc: &Automerge) -> Result<Self, DeserializeError> {
        let name = get_text(doc, ROOT, "title")?;

        let Some((automerge::Value::Object(ObjType::List), docs_id)) = doc.get(ROOT, "docs")?
        else {
            return Err(DeserializeError::InvalidSchema("missing docs array".into()));
        };

        let mut entries = Vec::new();
        for idx in 0..doc.length(&docs_id) {
            let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                doc.get(&docs_id, idx)?
            else {
                return Err(DeserializeError::InvalidSchema(format!(
                    "entry {idx} is not an object"
                )));
            };

            let entry_name = get_text(doc, entry_id.clone(), "name")?;

            let entry_type_str = get_text(doc, entry_id.clone(), "type")?;
            let entry_type = EntryType::parse(&entry_type_str).ok_or_else(|| {
                DeserializeError::InvalidSchema(format!("invalid entry type: {entry_type_str}"))
            })?;

            let url_str = get_text(doc, entry_id, "url")?;
            let sedimentree_id = url_to_sedimentree_id(&url_str)?;

            entries.push(DirectoryEntry {
                name: entry_name,
                entry_type,
                sedimentree_id,
            });
        }

        Ok(Self { name, entries })
    }

    /// Adds a file entry to an existing Automerge directory document.
    ///
    /// This modifies the document in place, preserving its change history.
    ///
    /// # Errors
    ///
    /// Returns an error if the document doesn't have an entries array or
    /// if Automerge operations fail.
    pub fn add_file_to_doc(
        doc: &mut Automerge,
        name: &str,
        sedimentree_id: SedimentreeId,
    ) -> Result<(), SerializeError> {
        Self::add_entry_to_doc(doc, name, EntryType::File, sedimentree_id)
    }

    /// Adds a folder entry to an existing Automerge directory document.
    ///
    /// This modifies the document in place, preserving its change history.
    ///
    /// # Errors
    ///
    /// Returns an error if the document doesn't have an entries array or
    /// if Automerge operations fail.
    pub fn add_folder_to_doc(
        doc: &mut Automerge,
        name: &str,
        sedimentree_id: SedimentreeId,
    ) -> Result<(), SerializeError> {
        Self::add_entry_to_doc(doc, name, EntryType::Folder, sedimentree_id)
    }

    /// Adds an entry to an existing Automerge directory document.
    fn add_entry_to_doc(
        doc: &mut Automerge,
        name: &str,
        entry_type: EntryType,
        sedimentree_id: SedimentreeId,
    ) -> Result<(), SerializeError> {
        let name = name.to_string();
        let url = sedimentree_id_to_url(sedimentree_id);
        let entry_type_str = entry_type.as_str();

        doc.transact::<_, _, AutomergeError>(|tx| {
            let docs_id = match tx.get(ROOT, "docs")? {
                Some((automerge::Value::Object(ObjType::List), id)) => id,
                _ => tx.put_object(ROOT, "docs", ObjType::List)?,
            };

            // Check if entry with same name already exists, remove it.
            let mut to_remove = None;
            for idx in 0..tx.length(&docs_id) {
                if let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                    tx.get(&docs_id, idx)?
                {
                    let existing_name = match tx.get(&entry_id, "name")? {
                        Some((automerge::Value::Object(ObjType::Text), id)) => Some(tx.text(&id)?),
                        _ => None,
                    };
                    if existing_name.as_deref() == Some(&name) {
                        to_remove = Some(idx);
                        break;
                    }
                }
            }

            if let Some(idx) = to_remove {
                tx.delete(&docs_id, idx)?;
            }

            // Add new entry at the end
            let length = tx.length(&docs_id);
            let entry_obj = tx.insert_object(&docs_id, length, ObjType::Map)?;
            let entry_name = tx.put_object(&entry_obj, "name", ObjType::Text)?;
            tx.splice_text(&entry_name, 0, 0, name.as_str())?;
            let entry_type = tx.put_object(&entry_obj, "type", ObjType::Text)?;
            tx.splice_text(&entry_type, 0, 0, entry_type_str)?;
            let entry_url = tx.put_object(&entry_obj, "url", ObjType::Text)?;
            tx.splice_text(&entry_url, 0, 0, url.as_str())?;

            Ok(())
        })
        .map_err(|f| f.error)?;

        Ok(())
    }

    /// Removes an entry from an existing Automerge directory document.
    ///
    /// Returns `true` if an entry was removed, `false` if no entry with that name existed.
    ///
    /// # Errors
    ///
    /// Returns an error if Automerge operations fail.
    pub fn remove_entry_from_doc(doc: &mut Automerge, name: &str) -> Result<bool, SerializeError> {
        // First, find the index to remove (read-only)
        let to_remove = {
            let Some((automerge::Value::Object(ObjType::List), docs_id)) = doc.get(ROOT, "docs")?
            else {
                return Ok(false);
            };

            let mut found = None;
            for idx in 0..doc.length(&docs_id) {
                if let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                    doc.get(&docs_id, idx)?
                {
                    let existing_name = match doc.get(&entry_id, "name")? {
                        Some((automerge::Value::Object(ObjType::Text), id)) => Some(doc.text(&id)?),
                        _ => None,
                    };
                    if existing_name.as_deref() == Some(name) {
                        found = Some(idx);
                        break;
                    }
                }
            }
            found
        };

        // If found, delete in a transaction
        if let Some(idx) = to_remove {
            doc.transact::<_, _, AutomergeError>(|tx| {
                let Some((automerge::Value::Object(ObjType::List), docs_id)) =
                    tx.get(ROOT, "docs")?
                else {
                    return Ok(());
                };
                tx.delete(&docs_id, idx)?;
                Ok(())
            })
            .map_err(|f| f.error)?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Initializes an Automerge document as a directory if not already initialized.
    ///
    /// This is used when creating a new directory or when the document doesn't
    /// have the required structure yet.
    ///
    /// # Errors
    ///
    /// Returns an error if Automerge operations fail.
    pub fn init_doc(doc: &mut Automerge, name: &str) -> Result<(), SerializeError> {
        // Only initialize if not already a directory
        if doc.get(ROOT, "title")?.is_none() {
            let name = name.to_string();
            doc.transact::<_, _, AutomergeError>(|tx| {
                let patchwork = tx.put_object(ROOT, "@patchwork", ObjType::Map)?;
                let pw_type = tx.put_object(&patchwork, "type", ObjType::Text)?;
                tx.splice_text(&pw_type, 0, 0, "folder")?;

                let title = tx.put_object(ROOT, "title", ObjType::Text)?;
                tx.splice_text(&title, 0, 0, name.as_str())?;
                tx.put_object(ROOT, "docs", ObjType::List)?;
                Ok(())
            })
            .map_err(|f| f.error)?;
        }
        Ok(())
    }
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

    /// Generate a random 16-byte ID (zero-padded to 32) matching our convention
    /// for automerge-repo compatibility.
    fn random_id() -> Result<SedimentreeId, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes[..16])?;
        Ok(SedimentreeId::new(bytes))
    }

    #[test]
    fn new_directory_is_empty() {
        let dir = Directory::new("src");
        assert!(dir.is_empty());
        assert_eq!(dir.len(), 0);
        assert_eq!(dir.name, "src");
    }

    #[test]
    fn add_file_entry() -> TestResult {
        let mut dir = Directory::new("src");
        let id = random_id()?;

        dir.add_file("main.rs", id);

        assert_eq!(dir.len(), 1);
        let entry = dir.get("main.rs").ok_or("entry not found")?;
        assert_eq!(entry.name, "main.rs");
        assert_eq!(entry.entry_type, EntryType::File);
        assert_eq!(entry.sedimentree_id, id);
        Ok(())
    }

    #[test]
    fn add_folder_entry() -> TestResult {
        let mut dir = Directory::root();
        let id = random_id()?;

        dir.add_folder("src", id);

        assert_eq!(dir.len(), 1);
        let entry = dir.get("src").ok_or("entry not found")?;
        assert_eq!(entry.name, "src");
        assert_eq!(entry.entry_type, EntryType::Folder);
        assert_eq!(entry.sedimentree_id, id);
        Ok(())
    }

    #[test]
    fn add_replaces_same_name() -> TestResult {
        let mut dir = Directory::new("test");
        let id1 = random_id()?;
        let id2 = random_id()?;

        dir.add_file("foo.txt", id1);
        dir.add_file("foo.txt", id2);

        assert_eq!(dir.len(), 1);
        let entry = dir.get("foo.txt").ok_or("entry not found")?;
        assert_eq!(entry.sedimentree_id, id2);
        Ok(())
    }

    #[test]
    fn remove_entry() -> TestResult {
        let mut dir = Directory::new("test");
        let id = random_id()?;

        dir.add_file("foo.txt", id);
        assert_eq!(dir.len(), 1);

        let removed = dir.remove("foo.txt");
        assert!(removed.is_some());
        assert_eq!(dir.len(), 0);
        Ok(())
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut dir = Directory::new("test");
        let removed = dir.remove("nonexistent");
        assert!(removed.is_none());
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn directory_automerge_roundtrip() {
        check!()
            .with_type::<(String, Vec<(String, bool, [u8; 16])>)>()
            .for_each(|(name, entries)| {
                let mut dir = Directory::new(name);
                for (entry_name, is_folder, id_bytes) in entries {
                    // Pad 16 bytes to 32 (our automerge-repo convention)
                    let mut full = [0u8; 32];
                    full[..16].copy_from_slice(id_bytes);
                    let id = SedimentreeId::new(full);

                    if *is_folder {
                        dir.add_folder(entry_name, id);
                    } else {
                        dir.add_file(entry_name, id);
                    }
                }

                let am = dir.to_automerge().expect("to_automerge");
                let loaded = Directory::from_automerge(&am).expect("from_automerge");

                assert_eq!(loaded.name, dir.name);
                assert_eq!(loaded.len(), dir.len());

                for entry in &dir.entries {
                    let found = loaded.get(&entry.name).expect("entry should exist");
                    assert_eq!(found.entry_type, entry.entry_type);
                    assert_eq!(found.sedimentree_id, entry.sedimentree_id);
                }
            });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn bs58check_roundtrip() {
        check!()
            .with_type::<[u8; 16]>()
            .for_each(|payload: &[u8; 16]| {
                let encoded = bs58check_encode(payload);
                let decoded = bs58check_decode(&encoded).expect("decode");
                assert_eq!(&decoded, payload);
            });
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn sedimentree_url_roundtrip() {
        check!()
            .with_type::<[u8; 16]>()
            .for_each(|id_bytes: &[u8; 16]| {
                let mut full = [0u8; 32];
                full[..16].copy_from_slice(id_bytes);
                let id = SedimentreeId::new(full);

                let url = sedimentree_id_to_url(id);
                assert!(url.starts_with("automerge:"));

                let recovered = url_to_sedimentree_id(&url).expect("parse url");
                assert_eq!(recovered, id);
            });
    }

    #[test]
    fn entry_type_str_roundtrip() {
        assert_eq!(
            EntryType::parse(EntryType::File.as_str()),
            Some(EntryType::File)
        );
        assert_eq!(
            EntryType::parse(EntryType::Folder.as_str()),
            Some(EntryType::Folder)
        );
        assert_eq!(EntryType::parse("unknown"), None);
    }

    #[test]
    fn url_format_used_in_serialization() -> TestResult {
        let mut dir = Directory::new("test");
        let id = random_id()?;
        dir.add_file("test.txt", id);

        let am = dir.to_automerge()?;

        // Check that url field exists and is a string starting with "automerge:"
        let Some((automerge::Value::Object(ObjType::List), docs_id)) = am.get(ROOT, "docs")? else {
            panic!("docs should be a list")
        };

        let Some((automerge::Value::Object(ObjType::Map), entry_id)) = am.get(&docs_id, 0)? else {
            panic!("entry should be a map")
        };

        let url = get_text(&am, entry_id, "url")?;
        assert!(
            url.starts_with("automerge:"),
            "url should start with 'automerge:'"
        );
        Ok(())
    }
}
