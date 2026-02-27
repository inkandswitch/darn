//! Ignore patterns for `darn` workspaces.
//!
//! Supports gitignore-style patterns loaded from the `.darn` config file.
//! The `.darn` file itself is always ignored.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use thiserror::Error;

use crate::dotfile::DarnConfig;

/// Default patterns that are always ignored (hardcoded, not user-editable).
const ALWAYS_IGNORED: &[&str] = &[".darn"];

/// Ignore pattern matcher for a workspace.
#[derive(Debug)]
pub struct IgnoreRules {
    matcher: Gitignore,
}

impl IgnoreRules {
    /// Build ignore rules from a loaded `DarnConfig`.
    ///
    /// # Errors
    ///
    /// Returns an error if the ignore patterns cannot be compiled.
    pub fn from_config(root: &Path, config: &DarnConfig) -> Result<Self, IgnorePatternError> {
        let mut builder = GitignoreBuilder::new(root);

        // Always ignore .darn marker file
        for pattern in ALWAYS_IGNORED {
            builder.add_line(None, pattern)?;
        }

        // Add user-configured ignore patterns from .darn file
        for pattern in &config.ignore {
            builder.add_line(None, pattern)?;
        }

        let matcher = builder.build()?;
        Ok(Self { matcher })
    }

    /// Build ignore rules from a workspace root.
    ///
    /// Loads the `.darn` config file and builds rules from it.
    /// Falls back to hardcoded always-ignored patterns if no config is found.
    ///
    /// # Errors
    ///
    /// Returns an error if the ignore patterns cannot be compiled.
    pub fn from_workspace_root(root: &Path) -> Result<Self, IgnorePatternError> {
        if let Ok(config) = DarnConfig::load(root) {
            Self::from_config(root, &config)
        } else {
            // No .darn file — just use hardcoded patterns
            let mut builder = GitignoreBuilder::new(root);
            for pattern in ALWAYS_IGNORED {
                builder.add_line(None, pattern)?;
            }
            let matcher = builder.build()?;
            Ok(Self { matcher })
        }
    }

    /// Check if a path should be ignored.
    ///
    /// The path should be relative to the workspace root.
    /// For directories, pass `is_dir = true` for correct matching.
    #[must_use]
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.matcher
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
    }
}

/// Error building ignore patterns.
#[derive(Debug, Error)]
#[error("failed to parse ignore patterns: {0}")]
pub struct IgnorePatternError(#[from] ignore::Error);

/// Add a pattern to the `.darn` config's ignore list.
///
/// Does nothing if the pattern already exists.
///
/// # Errors
///
/// Returns an error if the config cannot be read or written.
pub fn add_pattern(root: &Path, pattern: &str) -> Result<bool, IgnoreMutateError> {
    let pattern = pattern.trim().to_string();
    let mut config = DarnConfig::load(root)?;

    if config.ignore.iter().any(|p| p == &pattern) {
        return Ok(false);
    }

    config.ignore.push(pattern);
    config.save(root)?;
    Ok(true)
}

/// Remove a pattern from the `.darn` config's ignore list.
///
/// Returns `true` if the pattern was found and removed, `false` otherwise.
///
/// # Errors
///
/// Returns an error if the config cannot be read or written.
pub fn remove_pattern(root: &Path, pattern: &str) -> Result<bool, IgnoreMutateError> {
    let pattern = pattern.trim();
    let mut config = DarnConfig::load(root)?;

    let initial_len = config.ignore.len();
    config.ignore.retain(|p| p != pattern);

    if config.ignore.len() == initial_len {
        return Ok(false);
    }

    config.save(root)?;
    Ok(true)
}

/// List all user-configured ignore patterns from the `.darn` config.
///
/// Returns an empty vec if the config doesn't exist.
///
/// # Errors
///
/// Returns an error if the config cannot be read.
pub fn list_patterns(root: &Path) -> Result<Vec<String>, IgnoreMutateError> {
    match DarnConfig::load(root) {
        Ok(config) => Ok(config.ignore),
        Err(_) => Ok(Vec::new()),
    }
}

/// Error mutating ignore patterns.
#[derive(Debug, Error)]
pub enum IgnoreMutateError {
    /// Error reading/writing the `.darn` config.
    #[error(transparent)]
    Dotfile(#[from] crate::dotfile::DotfileError),
}

#[allow(clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceId;
    use bolero::check;
    use sedimentree_core::id::SedimentreeId;
    use testresult::TestResult;

    /// Create a `.darn` config file for testing.
    fn create_test_config(root: &Path, ignore: Vec<String>) {
        let id = WorkspaceId::from_bytes([1; 16]);
        let sed_id = SedimentreeId::new([2; 32]);
        let config =
            DarnConfig::with_fields(id, sed_id, ignore, crate::dotfile::AttributeMap::default());
        config.save(root).expect("save test config");
    }

    #[test]
    fn darn_file_always_ignored() {
        let dir = tempfile::tempdir().expect("create tempdir");
        create_test_config(dir.path(), Vec::new());
        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        // .darn file should be ignored
        assert!(rules.is_ignored(Path::new(".darn"), false));
    }

    #[test]
    fn non_darn_paths_not_ignored_by_default() {
        let dir = tempfile::tempdir().expect("create tempdir");
        create_test_config(dir.path(), Vec::new());
        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        check!().with_type::<String>().for_each(|segment: &String| {
            if segment.is_empty()
                || segment.contains('/')
                || segment.contains('\\')
                || segment == "."
                || segment == ".."
                || segment == ".darn"
            {
                return;
            }

            let path = Path::new(segment);
            assert!(
                !rules.is_ignored(path, false),
                "expected '{segment}' to NOT be ignored by default"
            );
        });
    }

    #[test]
    fn config_ignore_patterns_respected() -> TestResult {
        let dir = tempfile::tempdir()?;
        create_test_config(dir.path(), vec!["*.log".to_string(), "target/".to_string()]);

        let rules = IgnoreRules::from_workspace_root(dir.path())?;

        assert!(rules.is_ignored(Path::new("debug.log"), false));
        assert!(rules.is_ignored(Path::new("logs/app.log"), false));
        assert!(rules.is_ignored(Path::new("target"), true));
        assert!(rules.is_ignored(Path::new("target/debug/binary"), false));
        assert!(!rules.is_ignored(Path::new("src/main.rs"), false));

        Ok(())
    }

    #[test]
    fn negation_patterns_work() -> TestResult {
        let dir = tempfile::tempdir()?;
        create_test_config(
            dir.path(),
            vec!["*.log".to_string(), "!important.log".to_string()],
        );

        let rules = IgnoreRules::from_workspace_root(dir.path())?;

        assert!(rules.is_ignored(Path::new("debug.log"), false));
        assert!(!rules.is_ignored(Path::new("important.log"), false));

        Ok(())
    }

    #[test]
    fn missing_config_uses_defaults() -> TestResult {
        let dir = tempfile::tempdir()?;
        // No .darn file
        let rules = IgnoreRules::from_workspace_root(dir.path())?;

        assert!(rules.is_ignored(Path::new(".darn"), false));
        assert!(!rules.is_ignored(Path::new("foo.txt"), false));

        Ok(())
    }

    #[test]
    fn add_and_remove_pattern() -> TestResult {
        let dir = tempfile::tempdir()?;
        create_test_config(dir.path(), Vec::new());

        // Add a pattern
        assert!(add_pattern(dir.path(), "*.log")?);
        assert!(!add_pattern(dir.path(), "*.log")?); // duplicate

        let patterns = list_patterns(dir.path())?;
        assert!(patterns.contains(&"*.log".to_string()));

        // Remove it
        assert!(remove_pattern(dir.path(), "*.log")?);
        assert!(!remove_pattern(dir.path(), "*.log")?); // already gone

        let patterns = list_patterns(dir.path())?;
        assert!(!patterns.contains(&"*.log".to_string()));

        Ok(())
    }
}
