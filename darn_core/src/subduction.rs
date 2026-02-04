//! Subduction instance management for darn workspaces.
//!
//! This module provides type aliases and helper functions for creating and
//! managing Subduction instances that handle all sedimentree storage, signing,
//! and peer-to-peer synchronization.

use std::{convert::Infallible, sync::Arc};

use future_form::Sendable;
use sedimentree_core::commit::CountLeadingZeroBytes;
use sedimentree_fs_storage::FsStorage;

use subduction_core::{
    connection::nonce_cache::NonceCache,
    crypto::signer::MemorySigner,
    policy::open::OpenPolicy,
    sharded_map::ShardedMap,
    subduction::{Subduction, error::{AttachError, HydrationError, IoError}},
};
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};
use thiserror::Error;

/// Type alias for the WebSocket client connection used by darn.
pub type DarnConnection = TokioWebSocketClient<MemorySigner, TimeoutTokio>;

/// Type alias for the Subduction instance used by darn workspaces.
///
/// This configures Subduction with:
/// - `Sendable` future form (thread-safe async)
/// - `FsStorage` for persistent storage
/// - `TokioWebSocketClient` for peer connections
/// - `OpenPolicy` for permissive access (can be made stricter later)
/// - `MemorySigner` for ed25519 signing
/// - `CountLeadingZeroBytes` depth metric for fragment building
pub type DarnSubduction =
    Subduction<'static, Sendable, FsStorage, DarnConnection, OpenPolicy, MemorySigner, CountLeadingZeroBytes, 256>;

/// Concrete error type for attach operations.
///
/// Uses `Infallible` for the policy rejection type since `OpenPolicy` always allows connections.
pub type DarnAttachError = AttachError<Sendable, FsStorage, DarnConnection, Infallible>;

/// Concrete error type for registration operations.
///
/// Uses `Infallible` for the policy rejection type since `OpenPolicy` always allows connections.
pub type DarnRegistrationError =
    subduction_core::subduction::error::RegistrationError<Infallible>;

/// Concrete error type for I/O operations during sync.
pub type DarnIoError = IoError<Sendable, FsStorage, DarnConnection>;

/// Create a new Subduction instance and spawn its background tasks.
///
/// The listener and manager futures are spawned onto the tokio runtime.
#[must_use]
pub fn spawn(signer: MemorySigner, storage: FsStorage) -> Arc<DarnSubduction> {
    let (subduction, listener_fut, manager_fut) = DarnSubduction::new(
        None, // discovery_id - not using mDNS discovery yet
        signer,
        storage,
        OpenPolicy,
        NonceCache::default(),
        CountLeadingZeroBytes,
        ShardedMap::new(),
        TokioSpawn,
    );

    tokio::spawn(async move {
        if let Err(e) = listener_fut.await {
            tracing::error!("Subduction listener error: {e:?}");
        }
    });

    tokio::spawn(async move {
        if let Err(e) = manager_fut.await {
            tracing::error!("Subduction manager error: {e:?}");
        }
    });

    subduction
}

/// Hydrate a Subduction instance from existing storage and spawn its background tasks.
///
/// This loads all existing sedimentrees from storage.
///
/// # Errors
///
/// Returns an error if hydration fails.
pub async fn hydrate(signer: MemorySigner, storage: FsStorage) -> Result<Arc<DarnSubduction>, SubductionInitError> {
    let (subduction, listener_fut, manager_fut) = Box::pin(DarnSubduction::hydrate(
        None,
        signer,
        storage,
        OpenPolicy,
        NonceCache::default(),
        CountLeadingZeroBytes,
        ShardedMap::new(),
        TokioSpawn,
    ))
    .await
    .map_err(SubductionInitError::Hydration)?;

    tokio::spawn(async move {
        if let Err(e) = listener_fut.await {
            tracing::error!("Subduction listener error: {e:?}");
        }
    });

    tokio::spawn(async move {
        if let Err(e) = manager_fut.await {
            tracing::error!("Subduction manager error: {e:?}");
        }
    });

    Ok(subduction)
}

/// Errors initializing Subduction.
#[derive(Debug, Error)]
pub enum SubductionInitError {
    /// Hydration from storage failed.
    #[error("failed to hydrate from storage: {0}")]
    Hydration(HydrationError<Sendable, FsStorage>),
}
