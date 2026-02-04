//! Signer management for `darn`.
//!
//! Handles Ed25519 keypair generation, storage, and loading.
//! The signer produces a [`PeerId`] for identification.

use std::path::Path;

use subduction_core::crypto::signer::MemorySigner;
use subduction_core::peer::id::PeerId;
use thiserror::Error;

/// File name for the signing key.
const SIGNING_KEY_FILENAME: &str = "signing_key.ed25519";

/// Generate a new signer keypair and persist it to disk.
///
/// The signing key is stored as raw 32 bytes at `signer_dir/signing_key.ed25519`.
/// The public key is derived from the private key and not stored separately.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the key cannot be written.
pub fn generate_and_save(signer_dir: &Path) -> Result<MemorySigner, GenerateSignerError> {
    std::fs::create_dir_all(signer_dir)?;

    let private_key_path = signer_dir.join(SIGNING_KEY_FILENAME);

    let mut key_bytes = [0u8; 32];
    getrandom::getrandom(&mut key_bytes)?;

    let signer = MemorySigner::from_bytes(&key_bytes);

    std::fs::write(&private_key_path, key_bytes)?;

    // Clear the key bytes from memory
    key_bytes.fill(0);

    tracing::info!(
        peer_id = %hex::encode(signer.peer_id().as_bytes()),
        "Generated new signer"
    );

    Ok(signer)
}

/// Load an existing signer from disk.
///
/// # Errors
///
/// Returns an error if the key file doesn't exist or is invalid.
pub fn load(signer_dir: &Path) -> Result<MemorySigner, LoadSignerError> {
    let private_key_path = signer_dir.join(SIGNING_KEY_FILENAME);

    let key_bytes = std::fs::read(&private_key_path)?;

    if key_bytes.len() != 32 {
        return Err(LoadSignerError::InvalidKeyLength {
            expected: 32,
            actual: key_bytes.len(),
        });
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&key_bytes);

    let signer = MemorySigner::from_bytes(&arr);

    // Clear the temporary array
    arr.fill(0);

    tracing::debug!(
        peer_id = %hex::encode(signer.peer_id().as_bytes()),
        "Loaded signer"
    );

    Ok(signer)
}

/// Load an existing signer or generate a new one if none exists.
///
/// # Errors
///
/// Returns an error if loading fails (other than not found) or generation fails.
pub fn load_or_generate(signer_dir: &Path) -> Result<MemorySigner, SignerError> {
    let private_key_path = signer_dir.join(SIGNING_KEY_FILENAME);

    if private_key_path.exists() {
        Ok(load(signer_dir)?)
    } else {
        Ok(generate_and_save(signer_dir)?)
    }
}

/// Get the peer ID from a signer directory without loading the full signer.
///
/// # Errors
///
/// Returns an error if the signer cannot be loaded.
pub fn peer_id(signer_dir: &Path) -> Result<PeerId, LoadSignerError> {
    let signer = load(signer_dir)?;
    Ok(signer.peer_id())
}

/// Error generating and saving a new signer.
#[derive(Debug, Error)]
pub enum GenerateSignerError {
    /// Key generation failed.
    #[error("key generation failed: {0}")]
    KeyGeneration(#[from] getrandom::Error),

    /// I/O error writing key.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Error loading an existing signer.
#[derive(Debug, Error)]
pub enum LoadSignerError {
    /// Key file has wrong length.
    #[error("invalid key length: expected {expected}, got {actual}")]
    InvalidKeyLength {
        /// Expected length in bytes.
        expected: usize,

        /// Actual length in bytes.
        actual: usize,
    },

    /// I/O error reading key.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Error from `load_or_generate` which can fail either way.
#[derive(Debug, Error)]
pub enum SignerError {
    /// Generation failed.
    #[error(transparent)]
    Generate(#[from] GenerateSignerError),

    /// Loading failed.
    #[error(transparent)]
    Load(#[from] LoadSignerError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_save_creates_key_file() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path().join("signer");

        let _signer = generate_and_save(&signer_dir).expect("generate_and_save");

        let key_path = signer_dir.join(SIGNING_KEY_FILENAME);
        assert!(key_path.exists(), "private key file should exist");

        let key_bytes = std::fs::read(&key_path).expect("read key file");
        assert_eq!(key_bytes.len(), 32, "key should be 32 bytes");
    }

    #[test]
    fn generate_creates_unique_keys() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let dir1 = dir.path().join("s1");
        let dir2 = dir.path().join("s2");

        let signer1 = generate_and_save(&dir1).expect("generate_and_save 1");
        let signer2 = generate_and_save(&dir2).expect("generate_and_save 2");

        assert_ne!(
            signer1.peer_id(),
            signer2.peer_id(),
            "each generation should produce a unique peer ID"
        );
    }

    #[test]
    fn load_roundtrip() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path().join("signer");

        let original = generate_and_save(&signer_dir).expect("generate_and_save");
        let loaded = load(&signer_dir).expect("load");

        assert_eq!(
            original.peer_id(),
            loaded.peer_id(),
            "loaded signer should match original"
        );
    }

    #[test]
    fn load_nonexistent_fails() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path().join("nonexistent");

        let result = load(&signer_dir);
        assert!(result.is_err(), "loading nonexistent signer should fail");
    }

    #[test]
    fn load_invalid_key_length_fails() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path();
        let key_path = signer_dir.join(SIGNING_KEY_FILENAME);

        // Write a key with wrong length
        std::fs::write(&key_path, [0u8; 16]).expect("write invalid key");

        let result = load(signer_dir);
        assert!(result.is_err(), "loading invalid key should fail");

        match result {
            Err(LoadSignerError::InvalidKeyLength { expected, actual }) => {
                assert_eq!(expected, 32);
                assert_eq!(actual, 16);
            }
            other => panic!("expected InvalidKeyLength, got {other:?}"),
        }
    }

    #[test]
    fn load_or_generate_creates_when_missing() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path().join("signer");

        let signer = load_or_generate(&signer_dir).expect("load_or_generate");

        let key_path = signer_dir.join(SIGNING_KEY_FILENAME);
        assert!(key_path.exists(), "key file should be created");
        assert!(!signer.peer_id().as_bytes().is_empty());
    }

    #[test]
    fn load_or_generate_loads_when_exists() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path().join("signer");

        let original = generate_and_save(&signer_dir).expect("generate_and_save");
        let loaded = load_or_generate(&signer_dir).expect("load_or_generate");

        assert_eq!(
            original.peer_id(),
            loaded.peer_id(),
            "should load existing signer"
        );
    }

    #[test]
    fn peer_id_extraction() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let signer_dir = dir.path().join("signer");

        let signer = generate_and_save(&signer_dir).expect("generate_and_save");
        let extracted = peer_id(&signer_dir).expect("peer_id");

        assert_eq!(signer.peer_id(), extracted);
    }
}
