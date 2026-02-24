//! Ignore patterns for `darn` workspaces.
//!
//! Supports gitignore-style patterns via `.darnignore` files.
//! The `.darn/` directory is always ignored.

use std::{io::Write, path::Path};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use thiserror::Error;

/// Default patterns that are always ignored.
const ALWAYS_IGNORED: &[&str] = &[".darn", ".darn/", ".darn/**"];

/// Name of the ignore file.
const DARNIGNORE_FILE: &str = ".darnignore";

/// Default content for a new `.darnignore` file.
const DEFAULT_DARNIGNORE: &str = "\
# darn ignore file
# Patterns here will be excluded from sync (gitignore syntax)

# Version control
.git/

# Example patterns:
# *.log
# target/
# node_modules/
# .env
";

/// Ignore pattern matcher for a workspace.
#[derive(Debug)]
pub struct IgnoreRules {
    matcher: Gitignore,
}

impl IgnoreRules {
    /// Build ignore rules from a workspace root.
    ///
    /// Reads `.darnignore` if present and adds default patterns.
    ///
    /// # Errors
    ///
    /// Returns an error if the ignore patterns cannot be compiled.
    pub fn from_workspace_root(root: &Path) -> Result<Self, IgnorePatternError> {
        let mut builder = GitignoreBuilder::new(root);

        // Always ignore .darn directory
        for pattern in ALWAYS_IGNORED {
            builder.add_line(None, pattern)?;
        }

        // Load .darnignore if it exists
        let darnignore_path = root.join(".darnignore");
        if darnignore_path.exists()
            && let Some(err) = builder.add(&darnignore_path)
        {
            return Err(err.into());
        }

        let matcher = builder.build()?;

        Ok(Self { matcher })
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

/// Create a default `.darnignore` file if it doesn't exist.
///
/// Returns `true` if the file was created, `false` if it already exists.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub fn create_default(root: &Path) -> Result<bool, std::io::Error> {
    let path = root.join(DARNIGNORE_FILE);

    if path.exists() {
        return Ok(false);
    }

    std::fs::write(&path, DEFAULT_DARNIGNORE)?;
    Ok(true)
}

/// Add a pattern to the `.darnignore` file.
///
/// Creates the file if it doesn't exist. Appends to the end if it does.
/// Does nothing if the pattern already exists.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub fn add_pattern(root: &Path, pattern: &str) -> Result<bool, std::io::Error> {
    let path = root.join(DARNIGNORE_FILE);
    let pattern = pattern.trim();

    // Check if pattern already exists
    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        if content.lines().any(|line| line.trim() == pattern) {
            return Ok(false); // Already exists
        }
    }

    // Append pattern
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    // Add newline before if file exists and doesn't end with newline
    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        if !content.is_empty() && !content.ends_with('\n') {
            writeln!(file)?;
        }
    }

    writeln!(file, "{pattern}")?;
    Ok(true)
}

/// Remove a pattern from the `.darnignore` file.
///
/// Returns `true` if the pattern was found and removed, `false` otherwise.
///
/// # Errors
///
/// Returns an error if the file cannot be read or written.
pub fn remove_pattern(root: &Path, pattern: &str) -> Result<bool, std::io::Error> {
    let path = root.join(DARNIGNORE_FILE);
    let pattern = pattern.trim();

    if !path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&path)?;
    let lines: Vec<&str> = content.lines().collect();

    let filtered: Vec<&str> = lines
        .iter()
        .filter(|line| line.trim() != pattern)
        .copied()
        .collect();

    if filtered.len() == lines.len() {
        return Ok(false); // Pattern not found
    }

    // Write back without the pattern
    let new_content = if filtered.is_empty() {
        String::new()
    } else {
        filtered.join("\n") + "\n"
    };

    std::fs::write(&path, new_content)?;
    Ok(true)
}

/// List all patterns in the `.darnignore` file.
///
/// Returns an empty vec if the file doesn't exist.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn list_patterns(root: &Path) -> Result<Vec<String>, std::io::Error> {
    let path = root.join(DARNIGNORE_FILE);

    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(String::from)
        .collect())
}

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    #[test]
    fn darn_dir_always_ignored() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        check!()
            .with_type::<Vec<String>>()
            .for_each(|segments: &Vec<String>| {
                // Filter to valid path segments
                let segments: Vec<_> = segments
                    .iter()
                    .filter(|s| !s.is_empty() && !s.contains('/') && !s.contains('\\'))
                    .collect();

                // .darn itself
                assert!(rules.is_ignored(Path::new(".darn"), true));

                // .darn/<anything>
                if !segments.is_empty() {
                    let suffix: std::path::PathBuf = segments.iter().collect();
                    let path = Path::new(".darn").join(&suffix);
                    assert!(
                        rules.is_ignored(&path, false),
                        "expected .darn/{} to be ignored",
                        suffix.display()
                    );
                }
            });
    }

    #[test]
    fn non_darn_paths_not_ignored_by_default() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        check!().with_type::<String>().for_each(|segment: &String| {
            // Skip invalid or special segments
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
    fn darnignore_patterns_respected() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join(".darnignore"), "*.log\ntarget/\n")
            .expect("write .darnignore");

        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        assert!(rules.is_ignored(Path::new("debug.log"), false));
        assert!(rules.is_ignored(Path::new("logs/app.log"), false));
        assert!(rules.is_ignored(Path::new("target"), true));
        assert!(rules.is_ignored(Path::new("target/debug/binary"), false));
        assert!(!rules.is_ignored(Path::new("src/main.rs"), false));
    }

    #[test]
    fn negation_patterns_work() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join(".darnignore"), "*.log\n!important.log\n")
            .expect("write .darnignore");

        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        assert!(rules.is_ignored(Path::new("debug.log"), false));
        assert!(!rules.is_ignored(Path::new("important.log"), false));
    }

    #[test]
    fn missing_darnignore_is_fine() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let rules = IgnoreRules::from_workspace_root(dir.path()).expect("build rules");

        assert!(rules.is_ignored(Path::new(".darn"), true));
        assert!(!rules.is_ignored(Path::new("foo.txt"), false));
    }
}
