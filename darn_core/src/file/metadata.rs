//! File metadata.

pub mod permissions;

use permissions::Permissions;

/// File metadata stored in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    /// Unix file permissions.
    pub permissions: Permissions,
}

impl Metadata {
    /// Creates metadata with the given permissions.
    #[must_use]
    pub const fn new(permissions: Permissions) -> Self {
        Self { permissions }
    }

    /// Creates metadata from a Unix mode (e.g., 0o644).
    #[must_use]
    pub const fn from_mode(mode: u32) -> Self {
        Self {
            permissions: Permissions::from_mode(mode),
        }
    }

    /// Returns the Unix mode representation (e.g., 0o755).
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.permissions.mode()
    }

    /// Returns `true` if this is executable by the owner.
    #[must_use]
    pub const fn is_executable(&self) -> bool {
        self.permissions.is_executable()
    }
}

impl std::fmt::Display for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.permissions)
    }
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            permissions: Permissions::DEFAULT,
        }
    }
}

impl From<u32> for Metadata {
    fn from(mode: u32) -> Self {
        Self::from_mode(mode)
    }
}

impl From<Metadata> for u32 {
    fn from(meta: Metadata) -> Self {
        meta.mode()
    }
}
