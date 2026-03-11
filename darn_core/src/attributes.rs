//! File attributes for `darn` workspaces.
//!
//! Determines how files are stored in Automerge based on patterns in the
//! `.darn` config file.
//!
//! # `.darn` Config Example
//!
//! ```json
//! {
//!   "attributes": {
//!     "immutable": ["vendor/**"],
//!     "text": ["*.md"]
//!   }
//! }
//! ```
//!
//! Supported classifications (checked in this priority order):
//! - `immutable` — LWW string, no character merging (Automerge `ScalarValue::Str`)
//! - `text` — Character-level CRDT merging (Automerge `Text`)
//! - `binary` — Last-writer-wins binary (Automerge `Bytes`)
//!
//! Files that are valid UTF-8 but semantically wrong to character-merge
//! (source maps, minified files, lock files) are classified as `immutable`
//! by default. Binary is reserved for actual non-text content (images, Wasm,
//! fonts, etc.) which auto-detection handles.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;

use crate::{dotfile::DarnConfig, file::file_type::FileType};

/// Default patterns that should be treated as immutable (LWW string).
///
/// These are valid UTF-8 files where character-level CRDT merging would
/// produce semantically invalid results. They are stored as LWW strings
/// rather than Text CRDTs.
const DEFAULT_IMMUTABLE_PATTERNS: &[&str] = &[
    // Build output directories: contents are machine-generated artifacts
    "build/**",
    "dist/**",
    // Lock files: machine-generated, should be replaced wholesale
    "*.lock",
    "Cargo.lock",
    "Gemfile.lock",
    "composer.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "poetry.lock",
    "yarn.lock",
    // Minified files: typically single lines, character-merge is meaningless
    "*.min.css",
    "*.min.js",
    // Source maps: VLQ-encoded mappings, meaningless to character-merge
    "*.css.map",
    "*.js.map",
    "*.map",
];

/// Attribute matcher for a workspace.
#[derive(Debug, Clone)]
pub struct AttributeRules {
    /// Glob set for binary patterns (user-configured only).
    binary: GlobSet,
    /// Glob set for immutable text patterns (defaults + user-configured).
    immutable: GlobSet,
    /// Glob set for text patterns (user-configured only).
    text: GlobSet,
}

impl AttributeRules {
    /// Build attribute rules from a loaded `DarnConfig`.
    ///
    /// # Errors
    ///
    /// Returns an error if the attribute patterns cannot be compiled.
    pub fn from_config(_root: &Path, config: &DarnConfig) -> Result<Self, AttributeError> {
        let mut binary_builder = GlobSetBuilder::new();
        let mut immutable_builder = GlobSetBuilder::new();
        let mut text_builder = GlobSetBuilder::new();

        // Default immutable patterns: source maps, minified files, lock files.
        // These are valid UTF-8 but semantically wrong to character-merge.
        for pattern in DEFAULT_IMMUTABLE_PATTERNS {
            immutable_builder.add(Glob::new(pattern)?);
        }

        // User-configured binary patterns from .darn
        for pattern in &config.attributes.binary {
            binary_builder.add(Glob::new(pattern)?);
        }

        // User-configured immutable patterns from .darn
        for pattern in &config.attributes.immutable {
            immutable_builder.add(Glob::new(pattern)?);
        }

        // User-configured text patterns from .darn
        for pattern in &config.attributes.text {
            text_builder.add(Glob::new(pattern)?);
        }

        Ok(Self {
            binary: binary_builder.build()?,
            immutable: immutable_builder.build()?,
            text: text_builder.build()?,
        })
    }

    /// Build attribute rules from a workspace root.
    ///
    /// Loads the `.darn` config file and builds rules from it.
    /// Falls back to defaults if no config is found.
    ///
    /// # Errors
    ///
    /// Returns an error if the attribute patterns cannot be compiled.
    pub fn from_workspace_root(root: &Path) -> Result<Self, AttributeError> {
        match DarnConfig::load(root) {
            Ok(config) => Self::from_config(root, &config),
            Err(_) => Ok(Self::default()),
        }
    }

    /// Get the attribute for a file path.
    ///
    /// Returns `Some(FileType)` if an explicit rule matches, `None` for auto-detect.
    /// Priority: immutable > text > binary.
    ///
    /// For directory-prefix patterns like `dist/**`, pass a workspace-relative
    /// path (e.g. `dist/tool.js`). Absolute paths will only match filename-only
    /// globs like `*.lock`.
    #[must_use]
    pub fn get_attribute(&self, path: &Path) -> Option<FileType> {
        let path_str = path.to_string_lossy();

        // Immutable patterns first (defaults + user)
        if self.immutable.is_match(path_str.as_ref()) {
            return Some(FileType::Immutable);
        }

        // User-configured text patterns
        if self.text.is_match(path_str.as_ref()) {
            return Some(FileType::Text);
        }

        // User-configured binary patterns
        if self.binary.is_match(path_str.as_ref()) {
            return Some(FileType::Binary);
        }

        None
    }

    /// Check if a file should be treated as binary.
    #[must_use]
    pub fn is_binary(&self, path: &Path) -> bool {
        self.get_attribute(path) == Some(FileType::Binary)
    }

    /// Check if a file should be treated as text.
    #[must_use]
    pub fn is_text(&self, path: &Path) -> bool {
        self.get_attribute(path) == Some(FileType::Text)
    }
}

impl Default for AttributeRules {
    fn default() -> Self {
        let mut immutable_builder = GlobSetBuilder::new();

        for pattern in DEFAULT_IMMUTABLE_PATTERNS {
            if let Ok(glob) = Glob::new(pattern) {
                immutable_builder.add(glob);
            }
        }

        Self {
            binary: GlobSet::empty(),
            immutable: immutable_builder
                .build()
                .unwrap_or_else(|_| GlobSet::empty()),
            text: GlobSet::empty(),
        }
    }
}

/// Error building or parsing attribute rules.
#[derive(Debug, Error)]
pub enum AttributeError {
    /// Error reading the attributes file.
    #[error("failed to read attributes: {0}")]
    Io(#[from] std::io::Error),

    /// Error parsing a glob pattern.
    #[error("invalid glob pattern: {0}")]
    Glob(#[from] globset::Error),
}

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_immutable_patterns() {
        let rules = AttributeRules::default();

        assert_eq!(
            rules.get_attribute(Path::new("foo.js.map")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("src/bundle.min.js")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("styles.min.css")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("package-lock.json")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("Cargo.lock")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/index.html")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/assets/chunk-abc.js")),
            Some(FileType::Immutable)
        );
        assert_eq!(
            rules.get_attribute(Path::new("build/output.js")),
            Some(FileType::Immutable)
        );

        assert_eq!(rules.get_attribute(Path::new("foo.js")), None);
        assert_eq!(rules.get_attribute(Path::new("src/main.rs")), None);
        assert_eq!(rules.get_attribute(Path::new("README.md")), None);
    }

    #[test]
    fn get_attribute_returns_none_for_auto() {
        let rules = AttributeRules::default();

        assert_eq!(rules.get_attribute(Path::new("foo.rs")), None);
        assert_eq!(rules.get_attribute(Path::new("README.md")), None);
        assert_eq!(
            rules.get_attribute(Path::new("foo.js.map")),
            Some(FileType::Immutable)
        );
    }

    #[test]
    fn absolute_paths_match_patterns() {
        let rules = AttributeRules::default();

        assert_eq!(
            rules.get_attribute(Path::new("/Users/test/project/bundle.js.map")),
            Some(FileType::Immutable),
            "Absolute path to .js.map should be immutable"
        );
        assert_eq!(
            rules.get_attribute(Path::new("/private/tmp/darn-tenfold/assets/worker.js.map")),
            Some(FileType::Immutable),
            "Deep absolute path to .js.map should be immutable"
        );
        assert_eq!(
            rules.get_attribute(Path::new("/Users/test/project/bundle.js")),
            None,
            "Absolute path to .js should auto-detect"
        );
    }

    #[test]
    fn directory_prefixed_immutable_patterns() {
        use crate::dotfile::{AttributeMap, DarnConfig};
        use crate::workspace::id::WorkspaceId;
        use sedimentree_core::id::SedimentreeId;

        let dir = tempfile::tempdir().expect("tempdir");
        let config = DarnConfig::with_fields(
            WorkspaceId::from_bytes([1; 16]),
            SedimentreeId::new([2; 32]),
            false,
            Vec::new(),
            AttributeMap {
                binary: Vec::new(),
                immutable: vec!["dist/**".to_string()],
                text: Vec::new(),
            },
        );
        config.save(dir.path()).expect("save config");

        let rules = AttributeRules::from_workspace_root(dir.path()).expect("load rules");

        assert_eq!(
            rules.get_attribute(Path::new("dist/index.js")),
            Some(FileType::Immutable),
            "relative dist/index.js should be immutable"
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/assets/chunk.js")),
            Some(FileType::Immutable),
            "relative dist/assets/chunk.js should be immutable"
        );

        assert_eq!(
            rules.get_attribute(Path::new("src/main.ts")),
            None,
            "src/main.ts should auto-detect"
        );
    }

    /// `get_attribute` requires workspace-relative paths for directory-prefix
    /// globs like `dist/**`. Absolute paths won't match such patterns (by
    /// design — callers are responsible for stripping the workspace root).
    /// Filename-only globs like `*.lock` work regardless.
    #[test]
    fn absolute_path_does_not_match_directory_prefix_glob() {
        use crate::dotfile::{AttributeMap, DarnConfig};
        use crate::workspace::id::WorkspaceId;
        use sedimentree_core::id::SedimentreeId;

        let dir = tempfile::tempdir().expect("tempdir");
        let config = DarnConfig::with_fields(
            WorkspaceId::from_bytes([1; 16]),
            SedimentreeId::new([2; 32]),
            false,
            Vec::new(),
            AttributeMap {
                binary: Vec::new(),
                immutable: vec!["dist/**".to_string()],
                text: Vec::new(),
            },
        );
        config.save(dir.path()).expect("save config");

        let rules = AttributeRules::from_workspace_root(dir.path()).expect("load rules");

        // Absolute paths do NOT match directory-prefix globs — this is expected.
        // Callers must pass workspace-relative paths for correct matching.
        assert_eq!(
            rules.get_attribute(Path::new("/Users/test/project/dist/tool.js")),
            None,
            "absolute path should not match dist/** (caller must pass relative path)"
        );

        // Relative paths DO match
        assert_eq!(
            rules.get_attribute(Path::new("dist/tool.js")),
            Some(FileType::Immutable),
            "relative dist/tool.js should match dist/** pattern"
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/assets/chunk-ABC.js")),
            Some(FileType::Immutable),
            "relative nested dist/ path should match dist/** pattern"
        );
    }

    /// Filename-only globs like `*.lock` work with both absolute and relative paths.
    #[test]
    fn filename_only_glob_matches_absolute_and_relative() {
        let rules = AttributeRules::default();

        assert_eq!(
            rules.get_attribute(Path::new("Cargo.lock")),
            Some(FileType::Immutable),
            "relative Cargo.lock should match"
        );
        assert_eq!(
            rules.get_attribute(Path::new("/Users/test/project/Cargo.lock")),
            Some(FileType::Immutable),
            "absolute Cargo.lock should match (filename-only glob)"
        );
        assert_eq!(
            rules.get_attribute(Path::new("/Users/test/project/src/main.rs")),
            None,
            "absolute src/main.rs should not match any default pattern"
        );
    }

    #[test]
    fn dist_immutable_covers_all_file_types() {
        use crate::dotfile::{AttributeMap, DarnConfig};
        use crate::workspace::id::WorkspaceId;
        use sedimentree_core::id::SedimentreeId;

        let dir = tempfile::tempdir().expect("tempdir");
        let config = DarnConfig::with_fields(
            WorkspaceId::from_bytes([1; 16]),
            SedimentreeId::new([2; 32]),
            false,
            Vec::new(),
            AttributeMap {
                binary: Vec::new(),
                immutable: vec!["dist/**".to_string()],
                text: Vec::new(),
            },
        );
        config.save(dir.path()).expect("save config");

        let rules = AttributeRules::from_workspace_root(dir.path()).expect("load rules");

        assert_eq!(
            rules.get_attribute(Path::new("dist/index.js")),
            Some(FileType::Immutable),
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/index.js.map")),
            Some(FileType::Immutable),
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/bundle.min.js")),
            Some(FileType::Immutable),
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/tool.css")),
            Some(FileType::Immutable),
        );
        assert_eq!(
            rules.get_attribute(Path::new("dist/assets/chunk.css.map")),
            Some(FileType::Immutable),
        );
    }
}
