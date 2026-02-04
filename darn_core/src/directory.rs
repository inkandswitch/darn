//! Directory documents for syncing file paths between replicas.
//!
//! Directories in `darn` are stored as Automerge documents following the Patchwork convention:
//!
//! ```ignore
//! Directory {
//!   "@patchwork": { type: "folder" },
//!   name: string,
//!   entries: [
//!     { name: string, type: "file" | "folder", sedimentree_id: bytes },
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
use automerge::{transaction::Transactable, AutoCommit, ObjType, ReadDoc, ROOT};
use sedimentree_core::id::SedimentreeId;
use thiserror::Error;

/// A directory represented as an Automerge document.
///
/// This is the in-memory representation of a tracked directory. It can be
/// converted to/from an `AutoCommit` document for persistence and sync.
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
    pub fn to_automerge(&self) -> Result<AutoCommit, SerializeError> {
        let mut doc = AutoCommit::new();

        // Create @patchwork object
        let patchwork = doc.put_object(ROOT, "@patchwork", ObjType::Map)?;
        doc.put(&patchwork, "type", "folder")?;

        // Set name
        doc.put(ROOT, "name", self.name.as_str())?;

        let entries = doc.put_object(ROOT, "entries", ObjType::List)?;
        for (idx, entry) in self.entries.iter().enumerate() {
            let entry_obj = doc.insert_object(&entries, idx, ObjType::Map)?;
            doc.put(&entry_obj, "name", entry.name.as_str())?;
            doc.put(&entry_obj, "type", entry.entry_type.as_str())?;
            doc.put(
                &entry_obj,
                "sedimentree_id",
                automerge::ScalarValue::Bytes(entry.sedimentree_id.as_bytes().to_vec()),
            )?;
        }

        Ok(doc)
    }

    /// Loads a directory document from an Automerge document.
    ///
    /// # Errors
    ///
    /// Returns an error if the document doesn't match the expected schema.
    pub fn from_automerge(doc: &AutoCommit) -> Result<Self, DeserializeError> {
        // Read name
        let name = get_string(doc, ROOT, "name")?;

        // Read entries
        let Some((automerge::Value::Object(ObjType::List), entries_id)) =
            doc.get(ROOT, "entries")?
        else {
            return Err(DeserializeError::InvalidSchema(
                "missing entries array".into(),
            ));
        };

        let mut entries = Vec::new();
        for idx in 0..doc.length(&entries_id) {
            let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                doc.get(&entries_id, idx)?
            else {
                return Err(DeserializeError::InvalidSchema(format!(
                    "entry {idx} is not an object"
                )));
            };

            let entry_name = get_string(doc, entry_id.clone(), "name")?;

            let entry_type_str = get_string(doc, entry_id.clone(), "type")?;
            let entry_type = EntryType::from_str(&entry_type_str).ok_or_else(|| {
                DeserializeError::InvalidSchema(format!("invalid entry type: {entry_type_str}"))
            })?;

            let sedimentree_id = match doc.get(&entry_id, "sedimentree_id")? {
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
                        "missing sedimentree_id".into(),
                    ));
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
        doc: &mut AutoCommit,
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
        doc: &mut AutoCommit,
        name: &str,
        sedimentree_id: SedimentreeId,
    ) -> Result<(), SerializeError> {
        Self::add_entry_to_doc(doc, name, EntryType::Folder, sedimentree_id)
    }

    /// Adds an entry to an existing Automerge directory document.
    fn add_entry_to_doc(
        doc: &mut AutoCommit,
        name: &str,
        entry_type: EntryType,
        sedimentree_id: SedimentreeId,
    ) -> Result<(), SerializeError> {
        // Get entries array
        let entries_id = match doc.get(ROOT, "entries")? {
            Some((automerge::Value::Object(ObjType::List), id)) => id,
            _ => {
                // Create entries array if it doesn't exist
                doc.put_object(ROOT, "entries", ObjType::List)?
            }
        };

        // Check if entry with same name already exists, remove it
        let mut to_remove = None;
        for idx in 0..doc.length(&entries_id) {
            if let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                doc.get(&entries_id, idx)?
            {
                if let Some((automerge::Value::Scalar(s), _)) = doc.get(&entry_id, "name")? {
                    if let automerge::ScalarValue::Str(existing_name) = s.as_ref() {
                        if existing_name == name {
                            to_remove = Some(idx);
                            break;
                        }
                    }
                }
            }
        }

        if let Some(idx) = to_remove {
            doc.delete(&entries_id, idx)?;
        }

        // Add new entry at the end
        let length = doc.length(&entries_id);
        let entry_obj = doc.insert_object(&entries_id, length, ObjType::Map)?;
        doc.put(&entry_obj, "name", name)?;
        doc.put(&entry_obj, "type", entry_type.as_str())?;
        doc.put(
            &entry_obj,
            "sedimentree_id",
            automerge::ScalarValue::Bytes(sedimentree_id.as_bytes().to_vec()),
        )?;

        Ok(())
    }

    /// Removes an entry from an existing Automerge directory document.
    ///
    /// Returns `true` if an entry was removed, `false` if no entry with that name existed.
    ///
    /// # Errors
    ///
    /// Returns an error if Automerge operations fail.
    pub fn remove_entry_from_doc(doc: &mut AutoCommit, name: &str) -> Result<bool, SerializeError> {
        let entries_id = match doc.get(ROOT, "entries")? {
            Some((automerge::Value::Object(ObjType::List), id)) => id,
            _ => return Ok(false),
        };

        // Find entry with matching name
        for idx in 0..doc.length(&entries_id) {
            if let Some((automerge::Value::Object(ObjType::Map), entry_id)) =
                doc.get(&entries_id, idx)?
            {
                if let Some((automerge::Value::Scalar(s), _)) = doc.get(&entry_id, "name")? {
                    if let automerge::ScalarValue::Str(existing_name) = s.as_ref() {
                        if existing_name == name {
                            doc.delete(&entries_id, idx)?;
                            return Ok(true);
                        }
                    }
                }
            }
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
    pub fn init_doc(doc: &mut AutoCommit, name: &str) -> Result<(), SerializeError> {
        // Only initialize if not already a directory
        if doc.get(ROOT, "@patchwork")?.is_none() {
            let patchwork = doc.put_object(ROOT, "@patchwork", ObjType::Map)?;
            doc.put(&patchwork, "type", "folder")?;
            doc.put(ROOT, "name", name)?;
            doc.put_object(ROOT, "entries", ObjType::List)?;
        }
        Ok(())
    }
}

/// Helper to get a string value from an Automerge document.
#[allow(clippy::wildcard_enum_match_arm)] // Exhaustively matching ScalarValue variants is fragile
fn get_string(
    doc: &AutoCommit,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn random_id() -> SedimentreeId {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).expect("getrandom failed");
        SedimentreeId::new(bytes)
    }

    #[test]
    fn new_directory_is_empty() {
        let dir = Directory::new("src");
        assert!(dir.is_empty());
        assert_eq!(dir.len(), 0);
        assert_eq!(dir.name, "src");
    }

    #[test]
    fn add_file_entry() {
        let mut dir = Directory::new("src");
        let id = random_id();

        dir.add_file("main.rs", id);

        assert_eq!(dir.len(), 1);
        let entry = dir.get("main.rs").expect("entry exists");
        assert_eq!(entry.name, "main.rs");
        assert_eq!(entry.entry_type, EntryType::File);
        assert_eq!(entry.sedimentree_id, id);
    }

    #[test]
    fn add_folder_entry() {
        let mut dir = Directory::root();
        let id = random_id();

        dir.add_folder("src", id);

        assert_eq!(dir.len(), 1);
        let entry = dir.get("src").expect("entry exists");
        assert_eq!(entry.name, "src");
        assert_eq!(entry.entry_type, EntryType::Folder);
        assert_eq!(entry.sedimentree_id, id);
    }

    #[test]
    fn add_replaces_same_name() {
        let mut dir = Directory::new("test");
        let id1 = random_id();
        let id2 = random_id();

        dir.add_file("foo.txt", id1);
        dir.add_file("foo.txt", id2);

        assert_eq!(dir.len(), 1);
        let entry = dir.get("foo.txt").expect("entry exists");
        assert_eq!(entry.sedimentree_id, id2);
    }

    #[test]
    fn remove_entry() {
        let mut dir = Directory::new("test");
        let id = random_id();

        dir.add_file("foo.txt", id);
        assert_eq!(dir.len(), 1);

        let removed = dir.remove("foo.txt");
        assert!(removed.is_some());
        assert_eq!(dir.len(), 0);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut dir = Directory::new("test");
        let removed = dir.remove("nonexistent");
        assert!(removed.is_none());
    }

    #[test]
    fn automerge_roundtrip_empty() {
        let dir = Directory::new("empty");

        let am = dir.to_automerge().expect("to_automerge");
        let loaded = Directory::from_automerge(&am).expect("from_automerge");

        assert_eq!(loaded.name, "empty");
        assert!(loaded.is_empty());
    }

    #[test]
    fn automerge_roundtrip_with_entries() {
        let mut dir = Directory::root();
        let file_id = random_id();
        let folder_id = random_id();

        dir.add_file("README.md", file_id);
        dir.add_folder("src", folder_id);

        let am = dir.to_automerge().expect("to_automerge");
        let loaded = Directory::from_automerge(&am).expect("from_automerge");

        assert_eq!(loaded.name, "");
        assert_eq!(loaded.len(), 2);

        let readme = loaded.get("README.md").expect("README.md exists");
        assert_eq!(readme.entry_type, EntryType::File);
        assert_eq!(readme.sedimentree_id, file_id);

        let src = loaded.get("src").expect("src exists");
        assert_eq!(src.entry_type, EntryType::Folder);
        assert_eq!(src.sedimentree_id, folder_id);
    }

    #[test]
    fn entry_type_str_roundtrip() {
        assert_eq!(
            EntryType::from_str(EntryType::File.as_str()),
            Some(EntryType::File)
        );
        assert_eq!(
            EntryType::from_str(EntryType::Folder.as_str()),
            Some(EntryType::Folder)
        );
        assert_eq!(EntryType::from_str("unknown"), None);
    }
}
