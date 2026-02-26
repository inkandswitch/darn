//! File attributes for `darn` workspaces.
//!
//! Determines whether files should be treated as text (character-level CRDT)
//! or binary (last-writer-wins) based on patterns in the `.darn` config file.
//!
//! # `.darn` Config Example
//!
//! ```json
//! {
//!   "attributes": {
//!     "binary": ["*.lock", "*.min.js", "*.map"],
//!     "text": ["*.md"]
//!   }
//! }
//! ```
//!
//! Supported classifications:
//! - `text` — Character-level CRDT merging (Automerge `Text`)
//! - `binary` — Last-writer-wins (Automerge `Bytes`)

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;

use crate::{dotfile::DarnConfig, file::file_type::FileType};

/// Default patterns that should be treated as binary even if they contain valid UTF-8.
///
/// These are files where character-level merging would produce semantically
/// invalid results, even though the content is technically valid UTF-8.
/// These are compiled into the rules even when `attributes` is empty in the config.
const DEFAULT_BINARY_PATTERNS: &[&str] = &[
    // Source maps contain VLQ-encoded binary data as base64
    "*.js.map",
    "*.css.map",
    "*.map",
    // Minified files are often single lines; char-level merge is meaningless
    "*.min.js",
    "*.min.css",
    // Lock files should be regenerated, not merged
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "Cargo.lock",
    "Gemfile.lock",
    "poetry.lock",
    "composer.lock",
];

/// Attribute matcher for a workspace.
#[derive(Debug, Clone)]
pub struct AttributeRules {
    /// Glob set for binary patterns.
    binary_globs: GlobSet,
    /// Glob set for text patterns.
    text_globs: GlobSet,
}

impl AttributeRules {
    /// Build attribute rules from a loaded `DarnConfig`.
    ///
    /// # Errors
    ///
    /// Returns an error if the attribute patterns cannot be compiled.
    pub fn from_config(_root: &Path, config: &DarnConfig) -> Result<Self, AttributeError> {
        let mut binary_builder = GlobSetBuilder::new();
        let mut text_builder = GlobSetBuilder::new();

        // Add default binary patterns
        for pattern in DEFAULT_BINARY_PATTERNS {
            binary_builder.add(Glob::new(pattern)?);
        }

        // Add user-configured binary patterns from .darn
        for pattern in &config.attributes.binary {
            binary_builder.add(Glob::new(pattern)?);
        }

        // Add user-configured text patterns from .darn
        for pattern in &config.attributes.text {
            text_builder.add(Glob::new(pattern)?);
        }

        Ok(Self {
            binary_globs: binary_builder.build()?,
            text_globs: text_builder.build()?,
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
    /// Text patterns take precedence over binary (user overrides win).
    #[must_use]
    pub fn get_attribute(&self, path: &Path) -> Option<FileType> {
        let path_str = path.to_string_lossy();

        // Check text patterns first (user overrides)
        if self.text_globs.is_match(path_str.as_ref()) {
            return Some(FileType::Text);
        }

        // Check binary patterns (defaults + user patterns)
        if self.binary_globs.is_match(path_str.as_ref()) {
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
        let mut binary_builder = GlobSetBuilder::new();

        for pattern in DEFAULT_BINARY_PATTERNS {
            if let Ok(glob) = Glob::new(pattern) {
                binary_builder.add(glob);
            }
        }

        Self {
            binary_globs: binary_builder.build().unwrap_or_else(|_| GlobSet::empty()),
            text_globs: GlobSet::empty(),
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
    fn default_binary_patterns() {
        let rules = AttributeRules::default();

        assert!(rules.is_binary(Path::new("foo.js.map")));
        assert!(rules.is_binary(Path::new("src/bundle.min.js")));
        assert!(rules.is_binary(Path::new("styles.min.css")));
        assert!(rules.is_binary(Path::new("package-lock.json")));
        assert!(rules.is_binary(Path::new("Cargo.lock")));

        assert!(!rules.is_binary(Path::new("foo.js")));
        assert!(!rules.is_binary(Path::new("src/main.rs")));
        assert!(!rules.is_binary(Path::new("README.md")));
    }

    #[test]
    fn get_attribute_returns_none_for_auto() {
        let rules = AttributeRules::default();

        assert_eq!(rules.get_attribute(Path::new("foo.rs")), None);
        assert_eq!(rules.get_attribute(Path::new("README.md")), None);
        assert_eq!(
            rules.get_attribute(Path::new("foo.js.map")),
            Some(FileType::Binary)
        );
    }

    #[test]
    fn absolute_paths_match_patterns() {
        let rules = AttributeRules::default();

        assert!(
            rules.is_binary(Path::new("/Users/test/project/bundle.js.map")),
            "Absolute path to .js.map should be binary"
        );
        assert!(
            rules.is_binary(Path::new("/private/tmp/darn-tenfold/assets/worker.js.map")),
            "Deep absolute path to .js.map should be binary"
        );
        assert!(
            !rules.is_binary(Path::new("/Users/test/project/bundle.js")),
            "Absolute path to .js should NOT be binary"
        );
    }
}
