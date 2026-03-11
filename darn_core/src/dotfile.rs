//! The `.darn` configuration file.
//!
//! Each workspace has a single `.darn` JSON file at its root that serves as
//! both a workspace marker and configuration store. It replaces the former
//! the former `.darn/` directory, `.darnignore`, and `.darnattributes` files.
//!
//! # Format
//!
//! ```json
//! {
//!   "id": "a1b2c3d4e5f6...",
//!   "root_directory_id": "automerge:2u4x5b6JdSMDkyyMrQRzb8dreHhL",
//!   "ignore": [".git/", "*.log"],
//!   "attributes": {
//!     "binary": ["*.lock", "*.min.js"],
//!     "immutable": ["dist/**"],
//!     "text": ["*.md"]
//!   }
//! }
//! ```

use std::path::{Path, PathBuf};

use sedimentree_core::id::SedimentreeId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{serde_base58, workspace::id::WorkspaceId};

/// Name of the dotfile marker.
pub const DOTFILE_NAME: &str = ".darn";

/// Default ignore patterns written on `darn init`.
const DEFAULT_IGNORE: &[&str] = &[
    "# Version control",
    ".git/",
    "",
    "# Build artifacts",
    "target/",
    "node_modules/",
    "",
    "# Darn internals",
    ".darn-staging-*/",
    "",
    "# Environment",
    ".env",
];

/// Default immutable attribute patterns written on `darn init`.
///
/// These are valid UTF-8 files where character-level CRDT merging would
/// produce semantically invalid results. Stored as LWW strings.
const DEFAULT_IMMUTABLE: &[&str] = &[
    // Source maps
    "*.js.map",
    "*.css.map",
    "*.map",
    // Minified files
    "*.min.js",
    "*.min.css",
    // Lock files
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "Cargo.lock",
    "Gemfile.lock",
    "poetry.lock",
    "composer.lock",
];

/// The `.darn` file contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DarnConfig {
    /// Workspace identifier (hex-encoded, derived from canonical path).
    pub id: WorkspaceId,

    /// Root directory sedimentree ID (serialized as `automerge:` URL).
    #[serde(with = "serde_base58::automerge_url")]
    pub root_directory_id: SedimentreeId,

    /// When true, newly ingested text files use LWW string semantics
    /// (`ScalarValue::Str`) instead of character-level CRDT merging.
    /// Binary files are unaffected. Already-tracked files keep their type.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_immutable: bool,

    /// Gitignore-style patterns to exclude from sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,

    /// File type attribute overrides.
    #[serde(default, skip_serializing_if = "AttributeMap::is_empty")]
    pub attributes: AttributeMap,
}

/// Map of file type overrides keyed by classification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttributeMap {
    /// Patterns for binary (last-writer-wins) files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binary: Vec<String>,

    /// Patterns for immutable text (LWW string, no character merging) files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub immutable: Vec<String>,

    /// Patterns for text (character-level CRDT) files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text: Vec<String>,
}

impl AttributeMap {
    /// Returns `true` if all lists are empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.binary.is_empty() && self.immutable.is_empty() && self.text.is_empty()
    }
}

impl DarnConfig {
    /// Create a new config with default ignore/attribute patterns.
    #[must_use]
    pub fn new(id: WorkspaceId, root_directory_id: SedimentreeId) -> Self {
        Self {
            id,
            root_directory_id,
            force_immutable: false,
            ignore: default_ignore_patterns(),
            attributes: default_attribute_map(),
        }
    }

    /// Create a config with explicit fields (no defaults applied).
    #[must_use]
    pub const fn with_fields(
        id: WorkspaceId,
        root_directory_id: SedimentreeId,
        force_immutable: bool,
        ignore: Vec<String>,
        attributes: AttributeMap,
    ) -> Self {
        Self {
            id,
            root_directory_id,
            force_immutable,
            ignore,
            attributes,
        }
    }

    /// Load a `.darn` config from the given workspace root.
    ///
    /// # Errors
    ///
    /// Returns an error if the file doesn't exist or can't be parsed.
    pub fn load(root: &Path) -> Result<Self, DotfileError> {
        let path = root.join(DOTFILE_NAME);
        let content = std::fs::read_to_string(&path).map_err(DotfileError::Io)?;
        serde_json::from_str(&content).map_err(DotfileError::Parse)
    }

    /// Save this config to the `.darn` file in the given workspace root atomically.
    ///
    /// Uses a temp-file-then-rename pattern to prevent readers from seeing
    /// a partially-written file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can't be written.
    pub fn save(&self, root: &Path) -> Result<(), DotfileError> {
        let path = root.join(DOTFILE_NAME);
        let content = serde_json::to_string_pretty(self).map_err(DotfileError::Parse)?;
        crate::atomic_write::atomic_write(&path, (content + "\n").as_bytes())
            .map_err(DotfileError::Io)
    }

    /// Create a new `.darn` file with defaults and save it.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can't be written.
    pub fn create(
        root: &Path,
        id: WorkspaceId,
        root_directory_id: SedimentreeId,
    ) -> Result<Self, DotfileError> {
        let config = Self::new(id, root_directory_id);
        config.save(root)?;
        Ok(config)
    }

    /// Walk up from `start` looking for a `.darn` file (not directory).
    ///
    /// Returns the directory containing the `.darn` file.
    ///
    /// # Errors
    ///
    /// Returns [`DotfileNotFound`] if no `.darn` file is found.
    pub fn find_root(start: &Path) -> Result<PathBuf, DotfileNotFound> {
        let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

        loop {
            let dotfile = current.join(DOTFILE_NAME);
            if dotfile.is_file() {
                return Ok(current);
            }

            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => return Err(DotfileNotFound),
            }
        }
    }
}

/// Default ignore patterns for a new workspace.
fn default_ignore_patterns() -> Vec<String> {
    DEFAULT_IGNORE
        .iter()
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(|s| (*s).to_string())
        .collect()
}

/// Default attribute map for a new workspace.
fn default_attribute_map() -> AttributeMap {
    AttributeMap {
        binary: Vec::new(),
        immutable: DEFAULT_IMMUTABLE.iter().map(|s| (*s).to_string()).collect(),
        text: Vec::new(),
    }
}

/// No `.darn` file found in any parent directory.
#[derive(Debug, Clone, Copy, Error)]
#[error("not a darn workspace (or any parent): .darn file not found")]
pub struct DotfileNotFound;

/// Error reading or writing the `.darn` file.
#[derive(Debug, Error)]
pub enum DotfileError {
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(std::io::Error),

    /// JSON parse error.
    #[error("parse error: {0}")]
    Parse(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    use testresult::TestResult;

    #[test]
    fn roundtrip() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([1; 16]);
        // Use a realistic SedimentreeId: 16 random bytes + 16 zero bytes
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[2; 16]);
        let sed_id = SedimentreeId::new(bytes);

        let config = DarnConfig::create(dir.path(), id, sed_id)?;
        let loaded = DarnConfig::load(dir.path())?;

        assert_eq!(config.id, loaded.id);
        assert_eq!(config.root_directory_id, loaded.root_directory_id);
        assert_eq!(config.ignore, loaded.ignore);
        assert_eq!(config.attributes.binary, loaded.attributes.binary);

        Ok(())
    }

    #[test]
    fn force_immutable_roundtrip() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([3; 16]);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[4; 16]);
        let sed_id = SedimentreeId::new(bytes);

        let config = DarnConfig::with_fields(
            id,
            sed_id,
            true,
            vec![".git/".to_string()],
            AttributeMap {
                binary: Vec::new(),
                immutable: Vec::new(),
                text: Vec::new(),
            },
        );
        config.save(dir.path())?;

        let loaded = DarnConfig::load(dir.path())?;
        assert!(
            loaded.force_immutable,
            "force_immutable should survive roundtrip"
        );

        // Verify JSON contains the field
        let json = std::fs::read_to_string(dir.path().join(DOTFILE_NAME))?;
        assert!(
            json.contains("\"force_immutable\": true"),
            "JSON should contain force_immutable"
        );

        Ok(())
    }

    #[test]
    fn force_immutable_false_omitted_from_json() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([5; 16]);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[6; 16]);
        let sed_id = SedimentreeId::new(bytes);

        let config = DarnConfig::create(dir.path(), id, sed_id)?;
        assert!(!config.force_immutable);

        let json = std::fs::read_to_string(dir.path().join(DOTFILE_NAME))?;
        assert!(
            !json.contains("force_immutable"),
            "false should be omitted from JSON"
        );

        Ok(())
    }

    #[test]
    fn find_root_finds_dotfile() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([1; 16]);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[2; 16]);
        let sed_id = SedimentreeId::new(bytes);
        DarnConfig::create(dir.path(), id, sed_id)?;

        let subdir = dir.path().join("a").join("b");
        std::fs::create_dir_all(&subdir)?;

        let root = DarnConfig::find_root(&subdir)?;
        assert_eq!(root, dir.path().canonicalize()?);

        Ok(())
    }

    #[test]
    fn find_root_not_found() -> TestResult {
        let dir = tempfile::tempdir()?;
        let result = DarnConfig::find_root(dir.path());
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn defaults_populated() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([1; 16]);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[2; 16]);
        let sed_id = SedimentreeId::new(bytes);

        let config = DarnConfig::create(dir.path(), id, sed_id)?;

        assert!(!config.ignore.is_empty(), "ignore should have defaults");
        assert!(
            config.ignore.contains(&".git/".to_string()),
            "should contain .git/"
        );
        assert!(
            config.attributes.binary.is_empty(),
            "binary patterns should be empty by default"
        );
        assert!(
            !config.attributes.immutable.is_empty(),
            "immutable patterns should have defaults"
        );
        assert!(
            config
                .attributes
                .immutable
                .contains(&"Cargo.lock".to_string()),
            "should contain Cargo.lock in immutable"
        );

        Ok(())
    }

    #[test]
    fn ignores_darn_directory() -> TestResult {
        let dir = tempfile::tempdir()?;

        // Create a .darn directory (old style) — should NOT be found
        std::fs::create_dir_all(dir.path().join(".darn"))?;
        let result = DarnConfig::find_root(dir.path());
        assert!(result.is_err(), ".darn directory should not match");

        Ok(())
    }

    #[test]
    fn root_directory_id_serialized_as_automerge_url() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([7; 16]);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[8; 16]);
        let sed_id = SedimentreeId::new(bytes);

        DarnConfig::create(dir.path(), id, sed_id)?;

        let json = std::fs::read_to_string(dir.path().join(DOTFILE_NAME))?;
        assert!(
            json.contains("\"root_directory_id\": \"automerge:"),
            "root_directory_id should be an automerge URL, got: {json}"
        );

        Ok(())
    }

    #[test]
    fn plain_bs58_root_directory_id_rejected() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = WorkspaceId::from_bytes([9; 16]);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&[10; 16]);
        let sed_id = SedimentreeId::new(bytes);

        // Write a .darn file with the old plain-bs58 format (no automerge: prefix)
        let legacy_json = serde_json::json!({
            "id": id.to_string(),
            "root_directory_id": bs58::encode(sed_id.as_bytes()).into_string(),
            "ignore": [".git/"],
        });
        std::fs::write(
            dir.path().join(DOTFILE_NAME),
            serde_json::to_string_pretty(&legacy_json)?,
        )?;

        let result = DarnConfig::load(dir.path());
        assert!(result.is_err(), "plain bs58 should be rejected");

        Ok(())
    }
}
