//! Subduction instance management for darn workspaces.
//!
//! This module provides type aliases and helper functions for creating and
//! managing Subduction instances that handle all sedimentree storage, signing,
//! and peer-to-peer synchronization.
//!
//! Storage is shared globally at `~/.config/darn/storage/` for deduplication
//! across workspaces.
//!
//! The [`DarnTransport`] enum abstracts over WebSocket and Iroh transports,
//! implementing the `Transport<Sendable>` trait so that the Subduction engine
//! is transport-agnostic. It is wrapped in [`MessageTransport`] to provide
//! the typed `Connection<Sendable, SyncMessage>` required by Subduction.

use std::{convert::Infallible, path::Path, sync::Arc};

use future_form::Sendable;
use futures::future::BoxFuture;
use sedimentree_core::commit::CountLeadingZeroBytes;
use sedimentree_fs_storage::FsStorage;
use subduction_core::{
    authenticated::Authenticated,
    connection::message::SyncMessage,
    handler::sync::SyncHandler,
    policy::open::OpenPolicy,
    subduction::{
        Subduction,
        builder::SubductionBuilder,
        error::{AddConnectionError, HydrationError, IoError},
    },
    transport::{Transport, message::MessageTransport},
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};
use thiserror::Error;

use crate::config::{self, NoConfigDir};

/// Type alias for the concrete WebSocket client transport.
pub type WsTransport = TokioWebSocketClient<MemorySigner>;

/// Type alias for the concrete Iroh client transport.
#[cfg(feature = "iroh")]
pub type IrohTransportInner = subduction_iroh::transport::IrohTransport;

/// Transport-agnostic connection for darn.
///
/// Wraps either a WebSocket or Iroh transport, dispatching
/// all [`Transport`] trait methods to the inner variant.
#[derive(Debug, Clone)]
pub enum DarnTransport {
    /// WebSocket relay transport.
    WebSocket(Box<WsTransport>),

    /// Iroh direct QUIC transport.
    #[cfg(feature = "iroh")]
    Iroh(IrohTransportInner),
}

impl PartialEq for DarnTransport {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::WebSocket(a), Self::WebSocket(b)) => a == b,
            #[cfg(feature = "iroh")]
            (Self::Iroh(a), Self::Iroh(b)) => a == b,
            #[cfg(feature = "iroh")]
            _ => false,
        }
    }
}

/// Unified send error across transports.
#[derive(Debug, Clone, Copy, Error)]
pub enum DarnSendError {
    /// WebSocket send error.
    #[error("websocket: {0}")]
    WebSocket(#[from] subduction_websocket::error::SendError),

    /// Iroh send error.
    #[cfg(feature = "iroh")]
    #[error("iroh: {0}")]
    Iroh(#[from] subduction_iroh::error::SendError),
}

/// Unified receive error across transports.
#[derive(Debug, Clone, Copy, Error)]
pub enum DarnRecvError {
    /// WebSocket receive error.
    #[error("websocket: {0}")]
    WebSocket(#[from] subduction_websocket::error::RecvError),

    /// Iroh receive error.
    #[cfg(feature = "iroh")]
    #[error("iroh: {0}")]
    Iroh(#[from] subduction_iroh::error::RecvError),
}

/// Unified disconnection error across transports.
#[derive(Debug, Clone, Copy, Error)]
pub enum DarnDisconnectionError {
    /// WebSocket disconnection error.
    #[error("websocket: {0}")]
    WebSocket(#[from] subduction_websocket::error::DisconnectionError),

    /// Iroh disconnection error.
    #[cfg(feature = "iroh")]
    #[error("iroh: {0}")]
    Iroh(#[from] subduction_iroh::error::DisconnectionError),
}

impl Transport<Sendable> for DarnTransport {
    type SendError = DarnSendError;
    type RecvError = DarnRecvError;
    type DisconnectionError = DarnDisconnectionError;

    fn disconnect(&self) -> BoxFuture<'_, Result<(), Self::DisconnectionError>> {
        match self {
            Self::WebSocket(c) => {
                Box::pin(async { c.disconnect().await.map_err(DarnDisconnectionError::from) })
            }
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => {
                Box::pin(async { c.disconnect().await.map_err(DarnDisconnectionError::from) })
            }
        }
    }

    fn send_bytes(&self, bytes: &[u8]) -> BoxFuture<'_, Result<(), Self::SendError>> {
        let bytes = bytes.to_vec();
        match self {
            Self::WebSocket(c) => {
                Box::pin(async move { c.send_bytes(&bytes).await.map_err(DarnSendError::from) })
            }
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => {
                Box::pin(async move { c.send_bytes(&bytes).await.map_err(DarnSendError::from) })
            }
        }
    }

    fn recv_bytes(&self) -> BoxFuture<'_, Result<Vec<u8>, Self::RecvError>> {
        match self {
            Self::WebSocket(c) => {
                Box::pin(async { c.recv_bytes().await.map_err(DarnRecvError::from) })
            }
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => Box::pin(async { c.recv_bytes().await.map_err(DarnRecvError::from) }),
        }
    }
}

/// The connection type used by Subduction: a message-framed wrapper around
/// the transport-agnostic [`DarnTransport`].
pub type DarnConnection = MessageTransport<DarnTransport>;

/// Type alias for an authenticated darn connection.
pub type AuthenticatedDarnConnection = Authenticated<DarnConnection, Sendable>;

/// The concrete `SyncHandler` type for darn.
type DarnSyncHandler =
    SyncHandler<Sendable, FsStorage, DarnConnection, OpenPolicy, CountLeadingZeroBytes, 256>;

/// Type alias for the Subduction instance used by darn workspaces.
///
/// This configures Subduction with:
/// - `Sendable` future form (thread-safe async)
/// - `FsStorage` for persistent storage
/// - `DarnConnection` (`MessageTransport<DarnTransport>`) for transport-agnostic peer connections
/// - `SyncHandler` as the default handler
/// - `OpenPolicy` for permissive access (can be made stricter later)
/// - `MemorySigner` for ed25519 signing
/// - `TimeoutTokio` for roundtrip call timeouts
/// - `CountLeadingZeroBytes` depth metric for fragment building
pub type DarnSubduction = Subduction<
    'static,
    Sendable,
    FsStorage,
    DarnConnection,
    DarnSyncHandler,
    OpenPolicy,
    MemorySigner,
    TimeoutTokio,
    CountLeadingZeroBytes,
    256,
>;

/// Concrete error type for adding connections.
///
/// Uses `Infallible` for the policy rejection type since `OpenPolicy` always allows connections.
pub type DarnAddConnectionError = AddConnectionError<Infallible>;

/// Concrete error type for I/O operations during sync.
pub type DarnIoError = IoError<Sendable, FsStorage, DarnConnection, SyncMessage>;

/// Create global storage at the standard location.
///
/// # Errors
///
/// Returns an error if the storage directory cannot be determined or created.
pub fn create_global_storage() -> Result<FsStorage, StorageError> {
    let storage_dir = config::global_storage_dir()?;
    std::fs::create_dir_all(&storage_dir)?;
    FsStorage::new(storage_dir).map_err(StorageError::from)
}

/// Create storage at a custom path (for testing).
///
/// # Errors
///
/// Returns an error if storage cannot be created.
pub fn create_storage_at(path: &Path) -> Result<FsStorage, StorageError> {
    std::fs::create_dir_all(path)?;
    FsStorage::new(path.to_path_buf()).map_err(StorageError::from)
}

/// Create a new Subduction instance and spawn its background tasks.
///
/// The listener and manager futures are spawned onto the tokio runtime.
#[must_use]
pub fn spawn(signer: MemorySigner, storage: FsStorage) -> Arc<DarnSubduction> {
    let (subduction, _handler, listener_fut, manager_fut) = SubductionBuilder::new()
        .signer(signer)
        .storage(storage, Arc::new(OpenPolicy))
        .spawner(TokioSpawn)
        .timer(TimeoutTokio)
        .build::<Sendable, DarnConnection>();

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
/// This loads all existing sedimentrees from storage, pre-populates a
/// `ShardedMap`, and builds Subduction with the hydrated state.
///
/// # Errors
///
/// Returns an error if hydration fails.
pub async fn hydrate(
    signer: MemorySigner,
    storage: FsStorage,
) -> Result<Arc<DarnSubduction>, SubductionInitError> {
    use sedimentree_core::sedimentree::Sedimentree;
    use subduction_core::{sharded_map::ShardedMap, storage::traits::Storage};

    // Hydrate: load all sedimentree state from storage
    let sedimentrees = Arc::new(ShardedMap::new());
    let ids = Storage::<Sendable>::load_all_sedimentree_ids(&storage)
        .await
        .map_err(|e| SubductionInitError::Hydration(HydrationError::LoadAllIdsError(e)))?;

    for id in ids {
        let verified_commits = Storage::<Sendable>::load_loose_commits(&storage, id)
            .await
            .map_err(|e| {
                SubductionInitError::Hydration(HydrationError::LoadLooseCommitsError(e))
            })?;
        let verified_fragments = Storage::<Sendable>::load_fragments(&storage, id)
            .await
            .map_err(|e| SubductionInitError::Hydration(HydrationError::LoadFragmentsError(e)))?;

        let commits = verified_commits
            .into_iter()
            .map(|vm| vm.into_full_parts().1)
            .collect();
        let fragments = verified_fragments
            .into_iter()
            .map(|vm| vm.into_full_parts().1)
            .collect();

        let tree = Sedimentree::new(fragments, commits);
        sedimentrees.insert(id, tree).await;
    }

    let (subduction, _handler, listener_fut, manager_fut) = SubductionBuilder::new()
        .signer(signer)
        .storage(storage, Arc::new(OpenPolicy))
        .spawner(TokioSpawn)
        .timer(TimeoutTokio)
        .sedimentrees(sedimentrees)
        .build::<Sendable, DarnConnection>();

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

    /// Storage initialization failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Errors creating storage.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Could not determine config directory.
    #[error(transparent)]
    NoConfigDir(#[from] NoConfigDir),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// `FsStorage` creation failed.
    #[error("storage error: {0}")]
    FsStorage(#[from] sedimentree_fs_storage::FsStorageError),
}
