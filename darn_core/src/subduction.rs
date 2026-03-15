//! Subduction instance management for darn workspaces.
//!
//! This module provides type aliases and helper functions for creating and
//! managing Subduction instances that handle all sedimentree storage, signing,
//! and peer-to-peer synchronization.
//!
//! Storage is shared globally at `~/.config/darn/storage/` for deduplication
//! across workspaces.
//!
//! The [`DarnConnection`] enum abstracts over WebSocket and Iroh transports,
//! implementing the `Connection<Sendable>` trait so that the Subduction engine
//! is transport-agnostic.

use std::{convert::Infallible, path::Path, sync::Arc, time::Duration};

use future_form::Sendable;
use futures::future::BoxFuture;
use sedimentree_core::commit::CountLeadingZeroBytes;
use sedimentree_fs_storage::FsStorage;
use subduction_core::{
    connection::{
        Connection,
        authenticated::Authenticated,
        message::{BatchSyncRequest, BatchSyncResponse, Message, RequestId},
    },
    policy::open::OpenPolicy,
    subduction::{
        Subduction,
        builder::SubductionBuilder,
        error::{AddConnectionError, HydrationError, IoError},
    },
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};
use thiserror::Error;

use crate::config::{self, NoConfigDir};

/// Type alias for the concrete WebSocket client connection.
pub type WsConnection = TokioWebSocketClient<MemorySigner, TimeoutTokio>;

/// Type alias for the concrete Iroh client connection.
#[cfg(feature = "iroh")]
pub type IrohConnection = subduction_iroh::connection::IrohConnection<TimeoutTokio>;

/// Transport-agnostic connection for darn.
///
/// Wraps either a WebSocket or Iroh connection, dispatching
/// all [`Connection`] trait methods to the inner variant.
#[derive(Debug, Clone)]
pub enum DarnConnection {
    /// WebSocket relay connection.
    WebSocket(Box<WsConnection>),

    /// Iroh direct QUIC connection.
    #[cfg(feature = "iroh")]
    Iroh(IrohConnection),
}

impl PartialEq for DarnConnection {
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

/// Unified call error across transports.
#[derive(Debug, Clone, Copy, Error)]
pub enum DarnCallError {
    /// WebSocket call error.
    #[error("websocket: {0}")]
    WebSocket(#[from] subduction_websocket::error::CallError),

    /// Iroh call error.
    #[cfg(feature = "iroh")]
    #[error("iroh: {0}")]
    Iroh(#[from] subduction_iroh::error::CallError),
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

impl Connection<Sendable> for DarnConnection {
    type SendError = DarnSendError;
    type RecvError = DarnRecvError;
    type CallError = DarnCallError;
    type DisconnectionError = DarnDisconnectionError;

    fn next_request_id(&self) -> BoxFuture<'_, RequestId> {
        match self {
            Self::WebSocket(c) => c.next_request_id(),
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => c.next_request_id(),
        }
    }

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

    fn send(&self, message: &Message) -> BoxFuture<'_, Result<(), Self::SendError>> {
        // Clone to decouple the message lifetime from the returned future lifetime.
        let message = message.clone();
        match self {
            Self::WebSocket(c) => {
                Box::pin(async move { c.send(&message).await.map_err(DarnSendError::from) })
            }
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => {
                Box::pin(async move { c.send(&message).await.map_err(DarnSendError::from) })
            }
        }
    }

    fn recv(&self) -> BoxFuture<'_, Result<Message, Self::RecvError>> {
        match self {
            Self::WebSocket(c) => Box::pin(async { c.recv().await.map_err(DarnRecvError::from) }),
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => Box::pin(async { c.recv().await.map_err(DarnRecvError::from) }),
        }
    }

    fn call(
        &self,
        req: BatchSyncRequest,
        timeout: Option<Duration>,
    ) -> BoxFuture<'_, Result<BatchSyncResponse, Self::CallError>> {
        match self {
            Self::WebSocket(c) => {
                Box::pin(async move { c.call(req, timeout).await.map_err(DarnCallError::from) })
            }
            #[cfg(feature = "iroh")]
            Self::Iroh(c) => {
                Box::pin(async move { c.call(req, timeout).await.map_err(DarnCallError::from) })
            }
        }
    }
}

/// Type alias for an authenticated darn connection.
pub type AuthenticatedDarnConnection = Authenticated<DarnConnection, Sendable>;

/// Type alias for the Subduction instance used by darn workspaces.
///
/// This configures Subduction with:
/// - `Sendable` future form (thread-safe async)
/// - `FsStorage` for persistent storage
/// - `DarnConnection` for transport-agnostic peer connections
/// - `OpenPolicy` for permissive access (can be made stricter later)
/// - `MemorySigner` for ed25519 signing
/// - `CountLeadingZeroBytes` depth metric for fragment building
pub type DarnSubduction = Subduction<
    'static,
    Sendable,
    FsStorage,
    DarnConnection,
    OpenPolicy,
    MemorySigner,
    CountLeadingZeroBytes,
    256,
>;

/// Concrete error type for adding connections.
///
/// Uses `Infallible` for the policy rejection type since `OpenPolicy` always allows connections.
pub type DarnAddConnectionError = AddConnectionError<Infallible>;

/// Concrete error type for I/O operations during sync.
pub type DarnIoError = IoError<Sendable, FsStorage, DarnConnection>;

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
