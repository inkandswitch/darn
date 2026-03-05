//! Atomic file writes via temp-file-then-rename.
//!
//! On POSIX, `rename(2)` atomically replaces the destination if it already
//! exists. On Windows, we remove the target first (not fully atomic, but
//! prevents half-written files from appearing as valid data).

use std::{io, path::Path};

use getrandom::getrandom as fill_random;

/// Write `data` to `path` atomically.
///
/// Creates a temporary file in the same directory, writes the data, then
/// renames it over the target. This ensures readers never see a
/// partially-written file.
///
/// # Errors
///
/// Returns an I/O error if any step fails. If the rename fails, the temp
/// file is cleaned up on a best-effort basis.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Unique temp name: random suffix to avoid collisions across threads and processes
    let mut nonce = [0u8; 8];
    fill_random(&mut nonce).map_err(io::Error::other)?;
    let nonce = u64::from_ne_bytes(nonce);
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("data");
    let temp_name = format!("{stem}.{nonce:016x}.tmp");
    let temp_path = path.with_file_name(temp_name);

    // Write to temp file
    std::fs::write(&temp_path, data)?;

    // On Windows, rename fails if the destination exists
    #[cfg(target_os = "windows")]
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            // Clean up temp on failure
            drop(std::fs::remove_file(&temp_path));
            return Err(e);
        }
    }

    // Atomic rename (on POSIX; best-effort on Windows)
    if let Err(e) = std::fs::rename(&temp_path, path) {
        // Clean up temp on failure
        drop(std::fs::remove_file(&temp_path));
        return Err(e);
    }

    Ok(())
}

#[allow(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use testresult::TestResult;

    #[test]
    fn write_creates_file() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("test.json");

        atomic_write(&path, b"hello")?;
        assert_eq!(std::fs::read_to_string(&path)?, "hello");
        Ok(())
    }

    #[test]
    fn write_replaces_existing() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("test.json");

        atomic_write(&path, b"first")?;
        atomic_write(&path, b"second")?;
        assert_eq!(std::fs::read_to_string(&path)?, "second");
        Ok(())
    }

    #[test]
    fn write_creates_parent_dirs() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested").join("dir").join("test.json");

        atomic_write(&path, b"deep")?;
        assert_eq!(std::fs::read_to_string(&path)?, "deep");
        Ok(())
    }

    #[test]
    fn no_temp_file_left_behind() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("test.json");

        atomic_write(&path, b"clean")?;

        let entries: Vec<_> = std::fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "only the target file should remain");
        Ok(())
    }
}
