//! File attributes for `darn` workspaces.
//!
//! Supports gitattributes-style patterns via `.darnattributes` files.
//! Determines whether files should be treated as text (character-level CRDT)
//! or binary (last-writer-wins).
//!
//! # Format
//!
//! Each line in `.darnattributes` specifies a pattern and attribute:
//!
//! ```text
//! # Comments start with #
//! *.js.map binary
//! *.min.js binary
//! *.txt text
//! ```
//!
//! Supported attributes:
//! - `text` — Character-level CRDT merging (Automerge `Text`)
//! - `binary` — Last-writer-wins (Automerge `Bytes`)
//! - `auto` — Detect based on content (default)

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;

use crate::file::file_type::FileType;

/// Name of the attributes file.
const DARNATTRIBUTES_FILE: &str = ".darnattributes";

/// Default patterns that should be treated as binary even if they contain valid UTF-8.
///
/// These are files where character-level merging would produce semantically
/// invalid results, even though the content is technically valid UTF-8.
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

/// Default content for a new `.darnattributes` file.
const DEFAULT_DARNATTRIBUTES: &str = "\
# darn attributes file
# Patterns here control how files are stored and merged
#
# Format: <pattern> <attribute>
#
# Attributes:
#   text   - Character-level CRDT merging (for source code, prose)
#   binary - Last-writer-wins (for generated files, images)
#   auto   - Detect based on content (default)
#
# Examples:
#   *.md text
#   *.png binary
#   *.min.js binary

# Source maps (contain encoded binary data)
*.js.map binary
*.css.map binary

# Minified files (character-level merge is meaningless)
*.min.js binary
*.min.css binary

# Lock files (should be regenerated, not merged)
package-lock.json binary
pnpm-lock.yaml binary
yarn.lock binary
Cargo.lock binary
";

/// Attribute value for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Attribute {
    /// Character-level CRDT merging.
    Text,
    /// Last-writer-wins binary.
    Binary,
    /// Detect based on content (default).
    #[default]
    Auto,
}

impl Attribute {
    /// Parse from a string.
    fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "text" => Some(Self::Text),
            "binary" => Some(Self::Binary),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// Attribute matcher for a workspace.
#[derive(Debug, Clone)]
pub struct AttributeRules {
    /// Glob set for binary patterns.
    binary_globs: GlobSet,
    /// Glob set for text patterns.
    text_globs: GlobSet,
}

impl AttributeRules {
    /// Build attribute rules from a workspace root.
    ///
    /// Reads `.darnattributes` if present and adds default patterns.
    ///
    /// # Errors
    ///
    /// Returns an error if the attribute patterns cannot be compiled.
    pub fn from_workspace_root(root: &Path) -> Result<Self, AttributeError> {
        let mut binary_builder = GlobSetBuilder::new();
        let mut text_builder = GlobSetBuilder::new();

        // Add default binary patterns
        for pattern in DEFAULT_BINARY_PATTERNS {
            binary_builder.add(Glob::new(pattern)?);
        }

        // Load .darnattributes if it exists
        let attrs_path = root.join(DARNATTRIBUTES_FILE);
        if attrs_path.exists() {
            let content = std::fs::read_to_string(&attrs_path)?;
            for (line_num, line) in content.lines().enumerate() {
                let line = line.trim();

                // Skip empty lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                // Parse "pattern attribute"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() != 2 {
                    return Err(AttributeError::ParseError {
                        line: line_num + 1,
                        message: "expected '<pattern> <attribute>'".to_string(),
                    });
                }

                let pattern = parts[0];
                let attr_str = parts[1];

                let attribute =
                    Attribute::from_str(attr_str).ok_or_else(|| AttributeError::ParseError {
                        line: line_num + 1,
                        message: format!(
                            "unknown attribute '{attr_str}' (expected text, binary, or auto)"
                        ),
                    })?;

                let glob = Glob::new(pattern)?;

                match attribute {
                    Attribute::Binary => {
                        binary_builder.add(glob);
                    }
                    Attribute::Text => {
                        text_builder.add(glob);
                    }
                    Attribute::Auto => {
                        // Auto means "remove from both lists" - but since we're building
                        // fresh, just don't add to either. User can use this to override
                        // a default pattern.
                        // For now, we don't support removing defaults via 'auto'.
                        // A more sophisticated approach would track negations.
                    }
                }
            }
        }

        Ok(Self {
            binary_globs: binary_builder.build()?,
            text_globs: text_builder.build()?,
        })
    }

    /// Get the attribute for a file path.
    ///
    /// Returns `Some(FileType)` if an explicit rule matches, `None` for auto-detect.
    /// Later patterns in `.darnattributes` take precedence over earlier ones and defaults.
    #[must_use]
    pub fn get_attribute(&self, path: &Path) -> Option<FileType> {
        // Convert to string for matching (use forward slashes for consistency)
        let path_str = path.to_string_lossy();

        // Check text patterns first (user overrides)
        if self.text_globs.is_match(path_str.as_ref()) {
            return Some(FileType::Text);
        }

        // Check binary patterns (defaults + user patterns)
        if self.binary_globs.is_match(path_str.as_ref()) {
            return Some(FileType::Binary);
        }

        // No match - auto-detect
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

/// Create a default `.darnattributes` file in the workspace.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub fn create_default_darnattributes(root: &Path) -> Result<(), std::io::Error> {
    let path = root.join(DARNATTRIBUTES_FILE);
    if !path.exists() {
        std::fs::write(&path, DEFAULT_DARNATTRIBUTES)?;
    }
    Ok(())
}

/// Error building or parsing attribute rules.
#[derive(Debug, Error)]
pub enum AttributeError {
    /// Error reading the attributes file.
    #[error("failed to read .darnattributes: {0}")]
    Io(#[from] std::io::Error),

    /// Error parsing a glob pattern.
    #[error("invalid glob pattern: {0}")]
    Glob(#[from] globset::Error),

    /// Error parsing the attributes file.
    #[error("parse error on line {line}: {message}")]
    ParseError {
        /// Line number where the error occurred.
        line: usize,
        /// Description of the parse error.
        message: String,
    },
}

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binary_patterns() {
        let rules = AttributeRules::default();

        // Should be binary
        assert!(rules.is_binary(Path::new("foo.js.map")));
        assert!(rules.is_binary(Path::new("src/bundle.min.js")));
        assert!(rules.is_binary(Path::new("styles.min.css")));
        assert!(rules.is_binary(Path::new("package-lock.json")));
        assert!(rules.is_binary(Path::new("Cargo.lock")));

        // Should be auto (not explicitly binary)
        assert!(!rules.is_binary(Path::new("foo.js")));
        assert!(!rules.is_binary(Path::new("src/main.rs")));
        assert!(!rules.is_binary(Path::new("README.md")));
    }

    #[test]
    fn attribute_from_str() {
        assert_eq!(Attribute::from_str("text"), Some(Attribute::Text));
        assert_eq!(Attribute::from_str("TEXT"), Some(Attribute::Text));
        assert_eq!(Attribute::from_str("binary"), Some(Attribute::Binary));
        assert_eq!(Attribute::from_str("Binary"), Some(Attribute::Binary));
        assert_eq!(Attribute::from_str("auto"), Some(Attribute::Auto));
        assert_eq!(Attribute::from_str("unknown"), None);
    }

    #[test]
    fn get_attribute_returns_none_for_auto() {
        let rules = AttributeRules::default();

        // Regular files should return None (auto-detect)
        assert_eq!(rules.get_attribute(Path::new("foo.rs")), None);
        assert_eq!(rules.get_attribute(Path::new("README.md")), None);

        // Binary files should return Some(Binary)
        assert_eq!(
            rules.get_attribute(Path::new("foo.js.map")),
            Some(FileType::Binary)
        );
    }

    #[test]
    fn absolute_paths_match_patterns() {
        let rules = AttributeRules::default();

        // Absolute paths should still match *.js.map pattern
        assert!(
            rules.is_binary(Path::new("/Users/test/project/bundle.js.map")),
            "Absolute path to .js.map should be binary"
        );
        assert!(
            rules.is_binary(Path::new("/private/tmp/darn-tenfold/assets/worker.js.map")),
            "Deep absolute path to .js.map should be binary"
        );

        // But regular .js files should not
        assert!(
            !rules.is_binary(Path::new("/Users/test/project/bundle.js")),
            "Absolute path to .js should NOT be binary"
        );
    }
}
