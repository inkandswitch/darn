//! Global configuration for `darn`.
//!
//! Manages the global config directory at `~/.config/darn/` which contains
//! the user's signer and other global settings.
//!
//! # Environment Variables
//!
//! - `DARN_CONFIG_DIR`: Override the global config directory (useful for testing
//!   with different identities).

use std::path::PathBuf;

use thiserror::Error;

/// Environment variable to override the global config directory.
const CONFIG_DIR_ENV: &str = "DARN_CONFIG_DIR";

/// Subdirectory for signer within the config directory.
const SIGNER_DIR: &str = "signer";

/// Subdirectory for peers within the config directory.
const PEERS_DIR: &str = "peers";

/// Subdirectory for shared storage within the config directory.
const STORAGE_DIR: &str = "storage";

/// Subdirectory for workspaces within the config directory.
const WORKSPACES_DIR: &str = "workspaces";

/// Returns the global `darn` config directory.
///
/// If `DARN_CONFIG_DIR` is set, uses that path. Otherwise defaults to
/// `~/.config/darn/`.
///
/// # Errors
///
/// Returns [`NoConfigDir`] if the home directory cannot be determined
/// (and no override is set).
pub fn global_config_dir() -> Result<PathBuf, NoConfigDir> {
    if let Ok(override_dir) = std::env::var(CONFIG_DIR_ENV) {
        return Ok(PathBuf::from(override_dir));
    }
    dirs::home_dir()
        .map(|p| p.join(".config").join("darn"))
        .ok_or(NoConfigDir)
}

/// Returns the global signer directory.
///
/// Default: `~/.config/darn/signer/`
///
/// # Errors
///
/// Returns [`NoConfigDir`] if the config directory cannot be determined.
pub fn global_signer_dir() -> Result<PathBuf, NoConfigDir> {
    Ok(global_config_dir()?.join(SIGNER_DIR))
}

/// Returns the global peers directory.
///
/// Default: `~/.config/darn/peers/`
///
/// # Errors
///
/// Returns [`NoConfigDir`] if the config directory cannot be determined.
pub fn global_peers_dir() -> Result<PathBuf, NoConfigDir> {
    Ok(global_config_dir()?.join(PEERS_DIR))
}

/// Returns the global shared storage directory.
///
/// Default: `~/.config/darn/storage/`
///
/// This is where all sedimentree blobs, commits, and fragments are stored,
/// shared across all workspaces for deduplication.
///
/// # Errors
///
/// Returns [`NoConfigDir`] if the config directory cannot be determined.
pub fn global_storage_dir() -> Result<PathBuf, NoConfigDir> {
    Ok(global_config_dir()?.join(STORAGE_DIR))
}

/// Returns the global workspaces directory.
///
/// Default: `~/.config/darn/workspaces/`
///
/// Each workspace has a subdirectory here with its manifest and ping-pong trees.
///
/// # Errors
///
/// Returns [`NoConfigDir`] if the config directory cannot be determined.
pub fn global_workspaces_dir() -> Result<PathBuf, NoConfigDir> {
    Ok(global_config_dir()?.join(WORKSPACES_DIR))
}

/// Returns `true` if the global config directory exists.
#[must_use]
pub fn global_config_exists() -> bool {
    global_config_dir().map(|p| p.exists()).unwrap_or(false)
}

/// Returns `true` if the global signer exists.
#[must_use]
pub fn global_signer_exists() -> bool {
    global_signer_dir()
        .map(|p| p.join("signing_key.ed25519").exists())
        .unwrap_or(false)
}

/// Ensures the global config directory exists.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn ensure_global_config_dir() -> Result<PathBuf, EnsureConfigError> {
    let dir = global_config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Could not determine config directory (HOME not set).
#[derive(Debug, Clone, Copy, Error)]
#[error("could not determine config directory (is $HOME set?)")]
pub struct NoConfigDir;

/// Error ensuring the config directory exists.
#[derive(Debug, Error)]
pub enum EnsureConfigError {
    /// HOME directory not set.
    #[error(transparent)]
    NoConfigDir(#[from] NoConfigDir),

    /// I/O error creating directory.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use testresult::TestResult;

    #[test]
    fn global_config_dir_ends_with_darn() -> TestResult {
        let dir = global_config_dir()?;
        assert!(dir.ends_with("darn"), "config dir should end with 'darn'");
        assert!(
            dir.to_string_lossy().contains(".config"),
            "config dir should be under .config"
        );

        Ok(())
    }

    #[test]
    fn global_signer_dir_is_under_config_dir() -> TestResult {
        let config = global_config_dir()?;
        let signer = global_signer_dir()?;
        assert!(
            signer.starts_with(&config),
            "signer dir should be under config dir"
        );
        assert!(
            signer.ends_with("signer"),
            "signer dir should end with 'signer'"
        );

        Ok(())
    }
}
