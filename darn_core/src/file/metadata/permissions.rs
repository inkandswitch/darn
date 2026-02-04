//! Unix file permissions.

use std::{fmt, ops::BitOr};

/// Unix file permissions (owner, group, other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    owner: PermissionSet,
    group: PermissionSet,
    other: PermissionSet,
}

impl Permissions {
    /// Default permissions for new files (0o644 = rw-r--r--).
    pub const DEFAULT: Self = Self {
        owner: PermissionSet::READ_WRITE,
        group: PermissionSet::READ,
        other: PermissionSet::READ,
    };

    /// Creates permissions from the three permission sets.
    #[must_use]
    pub const fn new(owner: PermissionSet, group: PermissionSet, other: PermissionSet) -> Self {
        Self {
            owner,
            group,
            other,
        }
    }

    /// Creates permissions from a Unix mode (e.g., 0o755).
    #[must_use]
    pub const fn from_mode(mode: u32) -> Self {
        Self {
            owner: PermissionSet::from_bits(((mode >> 6) & 0o7) as u8),
            group: PermissionSet::from_bits(((mode >> 3) & 0o7) as u8),
            other: PermissionSet::from_bits((mode & 0o7) as u8),
        }
    }

    /// Returns the owner's permissions.
    #[must_use]
    pub const fn owner(self) -> PermissionSet {
        self.owner
    }

    /// Returns the group's permissions.
    #[must_use]
    pub const fn group(self) -> PermissionSet {
        self.group
    }

    /// Returns the other's permissions.
    #[must_use]
    pub const fn other(self) -> PermissionSet {
        self.other
    }

    /// Returns the Unix mode representation (e.g., 0o755).
    #[must_use]
    pub const fn mode(self) -> u32 {
        ((self.owner.bits() as u32) << 6)
            | ((self.group.bits() as u32) << 3)
            | (self.other.bits() as u32)
    }

    /// Returns `true` if the owner has execute permission.
    #[must_use]
    pub const fn is_executable(self) -> bool {
        self.owner.contains(Permission::Execute)
    }

    /// Returns the full rwx string (e.g., "rwxr-xr-x").
    #[must_use]
    pub fn rwx(&self) -> String {
        format!(
            "{}{}{}",
            self.owner.rwx(),
            self.group.rwx(),
            self.other.rwx()
        )
    }
}

impl Default for Permissions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<u32> for Permissions {
    fn from(mode: u32) -> Self {
        Self::from_mode(mode)
    }
}

impl From<Permissions> for u32 {
    fn from(perms: Permissions) -> Self {
        perms.mode()
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rwx())
    }
}

/// Individual permission type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Read permission.
    Read,

    /// Write permission.
    Write,

    /// Execute permission.
    Execute,
}

impl Permission {
    const fn bit(self) -> u8 {
        match self {
            Self::Read => 0b100,
            Self::Write => 0b010,
            Self::Execute => 0b001,
        }
    }
}

/// A set of permissions (any combination of read, write, execute).
///
/// Supports bitwise OR to combine permissions:
/// ```
/// use darn_core::file::metadata::permissions::{Permission, PermissionSet};
///
/// let rw = PermissionSet::from(Permission::Read) | Permission::Write;
/// assert!(rw.contains(Permission::Read));
/// assert!(rw.contains(Permission::Write));
/// assert!(!rw.contains(Permission::Execute));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PermissionSet(u8);

impl PermissionSet {
    /// No permissions.
    pub const NONE: Self = Self(0);

    /// Read permission only.
    pub const READ: Self = Self(0b100);

    /// Write permission only.
    pub const WRITE: Self = Self(0b010);

    /// Execute permission only.
    pub const EXECUTE: Self = Self(0b001);

    /// All permissions (read, write, execute).
    pub const ALL: Self = Self(0b111);

    /// Read and write permissions.
    pub const READ_WRITE: Self = Self(0b110);

    /// Read and execute permissions.
    pub const READ_EXECUTE: Self = Self(0b101);

    /// Creates a permission set from raw bits (only lowest 3 bits used).
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b111)
    }

    /// Returns the raw bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns `true` if this set contains the given permission.
    #[must_use]
    pub const fn contains(self, perm: Permission) -> bool {
        self.0 & perm.bit() != 0
    }

    /// Returns `true` if this set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the rwx string representation (e.g., "rwx", "r-x", "---").
    #[must_use]
    pub fn rwx(self) -> String {
        let mut s = String::with_capacity(3);
        s.push(if self.contains(Permission::Read) {
            'r'
        } else {
            '-'
        });
        s.push(if self.contains(Permission::Write) {
            'w'
        } else {
            '-'
        });
        s.push(if self.contains(Permission::Execute) {
            'x'
        } else {
            '-'
        });
        s
    }
}

impl From<Permission> for PermissionSet {
    fn from(perm: Permission) -> Self {
        Self(perm.bit())
    }
}

impl BitOr for PermissionSet {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOr<Permission> for PermissionSet {
    type Output = Self;

    fn bitor(self, rhs: Permission) -> Self {
        Self(self.0 | rhs.bit())
    }
}

impl fmt::Display for PermissionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rwx())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    #[test]
    fn permission_set_bitwise_or() {
        let rw = PermissionSet::from(Permission::Read) | Permission::Write;
        assert!(rw.contains(Permission::Read));
        assert!(rw.contains(Permission::Write));
        assert!(!rw.contains(Permission::Execute));

        let rwx = rw | Permission::Execute;
        assert_eq!(rwx, PermissionSet::ALL);
    }

    #[test]
    fn permissions_mode_roundtrip() {
        check!().with_type::<u16>().for_each(|&bits| {
            let mode = u32::from(bits) & 0o777;
            let perms = Permissions::from_mode(mode);
            assert_eq!(perms.mode(), mode);
        });
    }

    #[test]
    fn permissions_rwx_format() {
        assert_eq!(Permissions::from_mode(0o755).rwx(), "rwxr-xr-x");
        assert_eq!(Permissions::from_mode(0o644).rwx(), "rw-r--r--");
        assert_eq!(Permissions::from_mode(0o700).rwx(), "rwx------");
        assert_eq!(Permissions::from_mode(0o777).rwx(), "rwxrwxrwx");
        assert_eq!(Permissions::from_mode(0o000).rwx(), "---------");
    }

    #[test]
    fn permissions_is_executable() {
        assert!(Permissions::from_mode(0o755).is_executable());
        assert!(Permissions::from_mode(0o100).is_executable());
        assert!(!Permissions::from_mode(0o644).is_executable());
    }
}
