//! Directory documents for syncing file paths between replicas.
//!
//! Directories in `darn` are stored as Automerge documents following the Patchwork convention:
//!
//! ```ignore
//! FolderDoc {
//!   title: string,
//!   docs: [
//!     { name: string, type: "file" | "folder", url: "automerge:<base58>" },
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
use automerge::{Automerge, AutomergeError, ObjType, ROOT, ReadDoc, transaction::Transactable};
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
/// Uses bs58check encoding of the first 16 bytes for compatibility with automerge-repo,
/// which expects 16-byte document IDs. The remaining 16 bytes of the sedimentree ID
/// are assumed to be zero-padding.
#[must_use]
pub fn sedimentree_id_to_url(id: SedimentreeId) -> String {
    let bytes = id.as_bytes();
    format!("automerge:{}", bs58check_encode(&bytes[..16]))
}

/// Parse an Automerge URL string to a `SedimentreeId`.
///
/// Accepts both 16-byte (automerge-repo style) and 32-byte (legacy darn) IDs.
/// For 16-byte IDs, zero-pads to 32 bytes for `SedimentreeId`.
fn url_to_sedimentree_id(url: &str) -> Result<SedimentreeId, DeserializeError> {
    let encoded = url.strip_prefix("automerge:").ok_or_else(|| {
        DeserializeError::InvalidSchema(format!("url must start with 'automerge:': {url}"))
    })?;

    // Try JS-compatible bs58check first, then Rust's bs58check (version byte),
    // then plain bs58 for backward compatibility
    let bytes = bs58check_decode(encoded)
        .or_else(|_| {
            bs58::decode(encoded)
                .with_check(None)
                .into_vec()
                .map_err(|e| e.to_string())
        })
        .or_else(|_| bs58::decode(encoded).into_vec().map_err(|e| e.to_string()))
        .map_err(|e| DeserializeError::InvalidSchema(format!("invalid base58 in url: {e}")))?;

    let arr: [u8; 32] = match bytes.len() {
        16 => {
            // 16-byte automerge-repo style ID: zero-pad to 32 bytes
            let mut arr = [0u8; 32];
            arr[..16].copy_from_slice(&bytes);
            arr
        }
        32 => {
            // 32-byte legacy darn ID
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| DeserializeError::InvalidSchema("invalid 32-byte id".into()))?
        }
        n => {
            return Err(DeserializeError::InvalidSchema(format!(
                "url must encode 16 or 32 bytes, got {n}"
            )));
        }
    };

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
            tx.put(ROOT, "title", self.name.as_str())?;

            let docs = tx.put_object(ROOT, "docs", ObjType::List)?;
            for (idx, entry) in self.entries.iter().enumerate() {
                let entry_obj = tx.insert_object(&docs, idx, ObjType::Map)?;
                tx.put(&entry_obj, "name", entry.name.as_str())?;
                tx.put(&entry_obj, "type", entry.entry_type.as_str())?;
                tx.put(
                    &entry_obj,
                    "url",
                    sedimentree_id_to_url(entry.sedimentree_id),
                )?;
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
        // Read title (patchwork-next uses "title", fall back to "name" for compatibility)
        let name = get_string(doc, ROOT, "title").or_else(|_| get_string(doc, ROOT, "name"))?;

        // Read docs array (patchwork-next uses "docs", fall back to "entries" for compatibility)
        let docs_id = match doc.get(ROOT, "docs")? {
            Some((automerge::Value::Object(ObjType::List), id)) => id,
            _ => {
                // Fall back to "entries" for backward compatibility
                match doc.get(ROOT, "entries")? {
                    Some((automerge::Value::Object(ObjType::List), id)) => id,
                    _ => {
                        return Err(DeserializeError::InvalidSchema("missing docs array".into()));
                    }
                }
            }
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

            let entry_name = get_string(doc, entry_id.clone(), "name")?;

            let entry_type_str = get_string(doc, entry_id.clone(), "type")?;
            let entry_type = EntryType::parse(&entry_type_str).ok_or_else(|| {
                DeserializeError::InvalidSchema(format!("invalid entry type: {entry_type_str}"))
            })?;

            // Try new "url" field first, fall back to legacy "sedimentree_id" bytes
            let sedimentree_id = match doc.get(&entry_id, "url")? {
                Some((automerge::Value::Scalar(s), _)) => {
                    if let automerge::ScalarValue::Str(url) = s.as_ref() {
                        url_to_sedimentree_id(url)?
                    } else {
                        return Err(DeserializeError::InvalidSchema(
                            "url must be a string".into(),
                        ));
                    }
                }
                _ => {
                    // Fall back to legacy sedimentree_id bytes
                    match doc.get(&entry_id, "sedimentree_id")? {
                        Some((automerge::Value::Scalar(s), _)) => {
                            if let automerge::ScalarValue::Bytes(bytes) = s.as_ref() {
                                let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                                    DeserializeError::InvalidSchema(
                                        "sedimentree_id must be 32 bytes".into(),
                                    )
                                })?;
                                SedimentreeId::new(arr)
                            } else {
                                return Err(DeserializeError::InvalidSchema(
                                    "sedimentree_id must be bytes".into(),
                                ));
                            }
                        }
                        _ => {
                            return Err(DeserializeError::InvalidSchema(
                                "missing url field".into(),
                            ));
                        }
                    }
                }
            };

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
            // Get docs array (use "docs" for patchwork-next, fall back to "entries")
            let docs_id = match tx.get(ROOT, "docs")? {
                Some((automerge::Value::Object(ObjType::List), id)) => id,
                _ => match tx.get(ROOT, "entries")? {
                    Some((automerge::Value::Object(ObjType::List), id)) => id,
                    _ => tx.put_object(ROOT, "docs", ObjType::List)?,
                },
            };

            // Check if entry with same name already exists, remove it
            let mut to_remove = None;
            for idx in 0..tx.length(&docs_id) {
                if let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                    tx.get(&docs_id, idx)?
                    && let Some((automerge::Value::Scalar(s), _)) = tx.get(&entry_id, "name")?
                    && let automerge::ScalarValue::Str(existing_name) = s.as_ref()
                    && existing_name == &name
                {
                    to_remove = Some(idx);
                    break;
                }
            }

            if let Some(idx) = to_remove {
                tx.delete(&docs_id, idx)?;
            }

            // Add new entry at the end
            let length = tx.length(&docs_id);
            let entry_obj = tx.insert_object(&docs_id, length, ObjType::Map)?;
            tx.put(&entry_obj, "name", name.as_str())?;
            tx.put(&entry_obj, "type", entry_type_str)?;
            tx.put(&entry_obj, "url", url.as_str())?;

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
            let docs_id = match doc.get(ROOT, "docs")? {
                Some((automerge::Value::Object(ObjType::List), id)) => id,
                _ => match doc.get(ROOT, "entries")? {
                    Some((automerge::Value::Object(ObjType::List), id)) => id,
                    _ => return Ok(false),
                },
            };

            let mut found = None;
            for idx in 0..doc.length(&docs_id) {
                if let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                    doc.get(&docs_id, idx)?
                    && let Some((automerge::Value::Scalar(s), _)) = doc.get(&entry_id, "name")?
                    && let automerge::ScalarValue::Str(existing_name) = s.as_ref()
                    && existing_name == name
                {
                    found = Some(idx);
                    break;
                }
            }
            found
        };

        // If found, delete in a transaction
        if let Some(idx) = to_remove {
            doc.transact::<_, _, AutomergeError>(|tx| {
                let docs_id = match tx.get(ROOT, "docs")? {
                    Some((automerge::Value::Object(ObjType::List), id)) => id,
                    _ => match tx.get(ROOT, "entries")? {
                        Some((automerge::Value::Object(ObjType::List), id)) => id,
                        _ => return Ok(()),
                    },
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
        // Only initialize if not already a directory (check for "title" or legacy "@patchwork")
        if doc.get(ROOT, "title")?.is_none() && doc.get(ROOT, "@patchwork")?.is_none() {
            let name = name.to_string();
            doc.transact::<_, _, AutomergeError>(|tx| {
                tx.put(ROOT, "title", name.as_str())?;
                tx.put_object(ROOT, "docs", ObjType::List)?;
                Ok(())
            })
            .map_err(|f| f.error)?;
        }
        Ok(())
    }
}

/// Helper to get a string value from an Automerge document.
#[allow(clippy::wildcard_enum_match_arm)] // only Str is valid; all other variants are the same error
fn get_string(
    doc: &Automerge,
    obj: automerge::ObjId,
    key: &str,
) -> Result<String, DeserializeError> {
    match doc.get(obj, key)? {
        Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
            automerge::ScalarValue::Str(s) => Ok(s.to_string()),
            _ => Err(DeserializeError::InvalidSchema(format!(
                "{key} must be a string"
            ))),
        },
        _ => Err(DeserializeError::InvalidSchema(format!(
            "missing {key} field"
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

        let url = get_string(&am, entry_id, "url")?;
        assert!(
            url.starts_with("automerge:"),
            "url should start with 'automerge:'"
        );
        Ok(())
    }
}
