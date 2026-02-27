//! # `darn_core`
//!
//! Core library for managing collaborative directories using Automerge and Subduction.
//!
//! This crate provides the fundamental types and operations for:
//! - Workspace management (`.darn` config file + centralized storage)
//! - File-to-Automerge document mapping
//! - Signer and peer ID management
//! - Change tracking and history

#![forbid(unsafe_code)]

use sedimentree_core::id::SedimentreeId;

/// Generate a new random `SedimentreeId` compatible with automerge-repo.
///
/// Generates 16 random bytes (zero-padded to 32 bytes) so that when encoded
/// as an automerge URL using `directory::sedimentree_id_to_url`, it produces
/// a valid 16-byte bs58check URL that automerge-repo can parse.
///
/// # Panics
///
/// Panics if the system random number generator fails.
#[must_use]
pub fn generate_sedimentree_id() -> SedimentreeId {
    let mut bytes = [0u8; 32];
    #[allow(clippy::expect_used)]
    getrandom::getrandom(&mut bytes[..16]).expect("system RNG unavailable");
    // Leave bytes 16..32 as zeros
    SedimentreeId::new(bytes)
}

pub mod atomic_write;
pub mod attributes;
pub mod config;
pub mod darn;
pub mod directory;
pub mod discover;
pub mod dotfile;
pub mod file;
pub mod ignore;
pub mod manifest;
pub mod path;
pub mod peer;
pub mod refresh;
pub mod sedimentree;
pub mod serde_base58;
pub mod signer;
pub mod staged_update;
pub mod subduction;
pub mod sync_progress;
pub mod unix_timestamp;
pub mod watcher;
pub mod workspace;
