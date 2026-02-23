//! # `darn_core`
//!
//! Core library for managing collaborative directories using Automerge and Subduction.
//!
//! This crate provides the fundamental types and operations for:
//! - Workspace management (`.darn/` directory)
//! - File-to-Automerge document mapping
//! - Signer and peer ID management
//! - Change tracking and history

#![forbid(unsafe_code)]

pub mod config;
pub mod darn;
pub mod directory;
pub mod discover;
pub mod file;
pub mod ignore;
pub mod manifest;
pub mod path;
pub mod peer;
pub mod refresh;
pub mod sedimentree;
pub mod serde_base58;
pub mod signer;
pub mod subduction;
pub mod sync_progress;
pub mod unix_timestamp;
pub mod watcher;
pub mod workspace;
