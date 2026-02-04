//! File names.

use std::path::Path;

/// A file name (without path components).
///
/// This is the basename of a file, e.g., "README.md" or "Makefile".
/// It never contains path separators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Name(String);

impl Name {
    /// Creates a new file name.
    ///
    /// # Panics
    ///
    /// Panics if the name contains path separators.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            !name.contains('/') && !name.contains('\\'),
            "file name cannot contain path separators: {name}"
        );
        Self(name)
    }

    /// Extracts the file name from a path.
    ///
    /// Returns `None` if the path has no file name component.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| Self(s.to_string()))
    }

    /// Returns the file name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the file extension, if any.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        Path::new(&self.0).extension().and_then(|e| e.to_str())
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    #[test]
    fn rejects_path_separators() {
        check!()
            .with_type::<(String, String)>()
            .for_each(|(prefix, suffix)| {
                // Any name containing forward slash should panic
                let with_slash = format!("{prefix}/{suffix}");
                assert!(
                    std::panic::catch_unwind(|| Name::new(&with_slash)).is_err(),
                    "should reject forward slash in: {with_slash}"
                );

                // Any name containing backslash should panic
                let with_backslash = format!("{prefix}\\{suffix}");
                assert!(
                    std::panic::catch_unwind(|| Name::new(&with_backslash)).is_err(),
                    "should reject backslash in: {with_backslash}"
                );
            });
    }

    #[test]
    fn from_path_extracts_basename() {
        let name = Name::from_path(Path::new("/some/path/to/file.rs")).unwrap();
        assert_eq!(name.as_str(), "file.rs");
    }

    #[test]
    fn from_path_none_for_empty() {
        assert!(Name::from_path(Path::new("")).is_none());
    }
}
