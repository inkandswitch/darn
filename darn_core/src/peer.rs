//! Peer configuration for sync.
//!
//! Peers are stored globally in `~/.config/darn/peers/` as individual JSON files,
//! one per configured peer. This allows the same peer to be used across
//! multiple workspaces.

use std::{collections::BTreeMap, fmt, path::Path, str::FromStr};

use sedimentree_core::{crypto::digest::Digest, id::SedimentreeId, sedimentree::Sedimentree};
use serde::{Deserialize, Serialize};
use subduction_core::{connection::handshake::Audience, peer::id::PeerId};
use thiserror::Error;

use crate::{serde_base58, unix_timestamp::UnixTimestamp};

/// A validated peer name.
///
/// Peer names are used as filenames, so they must be valid filesystem names.
/// Names are alphanumeric with hyphens and underscores allowed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerName(String);

impl PeerName {
    /// Create a new peer name from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is invalid.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidPeerName> {
        let name = name.into();
        Self::validate(&name)?;
        Ok(Self(name))
    }

    /// Validate a peer name.
    fn validate(name: &str) -> Result<(), InvalidPeerName> {
        if name.is_empty() {
            return Err(InvalidPeerName::Empty);
        }

        if name.len() > 64 {
            return Err(InvalidPeerName::TooLong(name.len()));
        }

        // Must start with alphanumeric
        if !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return Err(InvalidPeerName::InvalidStart);
        }

        // Only allow alphanumeric, hyphen, underscore
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
                return Err(InvalidPeerName::InvalidChar(c));
            }
        }

        Ok(())
    }

    /// Get the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Convert to the underlying String.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for PeerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PeerName {
    type Err = InvalidPeerName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for PeerName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Errors for invalid peer names.
#[derive(Debug, Clone, Copy, Error)]
pub enum InvalidPeerName {
    /// Name is empty.
    #[error("peer name cannot be empty")]
    Empty,

    /// Name is too long.
    #[error("peer name too long: {0} chars (max 64)")]
    TooLong(usize),

    /// Name starts with invalid character.
    #[error("peer name must start with alphanumeric character")]
    InvalidStart,

    /// Name contains invalid character.
    #[error("peer name contains invalid character: {0:?}")]
    InvalidChar(char),
}

/// A configured peer for sync.
///
/// Each peer has a name (used as filename), a WebSocket URL,
/// and an audience configuration for authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    /// Human-readable name for the peer.
    pub name: PeerName,

    /// WebSocket URL for connecting to the peer.
    pub url: String,

    /// Authentication target: Known(PeerId) or Discover(DiscoveryId).
    #[serde(with = "serde_base58::audience")]
    pub audience: Audience,

    /// When this peer was added.
    pub added_at: UnixTimestamp,

    /// When we last successfully synced with this peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<UnixTimestamp>,

    /// Per-sedimentree sync state: maps `sedimentree_id` to the digest last synced to this peer.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        with = "serde_base58::synced_digests"
    )]
    pub synced_digests: BTreeMap<SedimentreeId, Digest<Sedimentree>>,
}

impl Peer {
    /// Create a peer with discovery mode.
    ///
    /// The service name is derived from the URL by stripping the protocol
    /// (e.g., `ws://relay.example.com:9000` → `relay.example.com:9000`).
    /// Both sides hash this identifier to create the discovery ID.
    ///
    /// Use this when you don't know the peer's ID ahead of time.
    /// After connecting, the peer's ID will be learned and can be
    /// updated to use `Audience::Known`.
    #[must_use]
    pub fn discover(name: PeerName, url: String) -> Self {
        let service_name = url
            .strip_prefix("wss://")
            .or_else(|| url.strip_prefix("ws://"))
            .unwrap_or(&url);

        Self {
            audience: Audience::discover(service_name.as_bytes()),
            name,
            url,
            added_at: UnixTimestamp::now(),
            last_synced_at: None,
            synced_digests: BTreeMap::new(),
        }
    }

    /// Create a peer with a known [`PeerId`].
    ///
    /// Use this when you know the peer's identity ahead of time.
    #[must_use]
    pub fn known(name: PeerName, url: String, peer_id: PeerId) -> Self {
        Self {
            audience: Audience::known(peer_id),
            name,
            url,
            added_at: UnixTimestamp::now(),
            last_synced_at: None,
            synced_digests: BTreeMap::new(),
        }
    }

    /// Load a peer from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or decoded.
    pub fn load(path: &Path) -> Result<Self, PeerError> {
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }

    /// Save a peer to a JSON file atomically.
    ///
    /// Uses a temp-file-then-rename pattern to prevent readers from seeing
    /// a partially-written file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), PeerError> {
        let json = serde_json::to_string_pretty(self)?;
        crate::atomic_write::atomic_write(path, json.as_bytes())?;
        Ok(())
    }

    /// Check if this peer uses discovery mode.
    #[must_use]
    pub const fn is_discovery(&self) -> bool {
        matches!(self.audience, Audience::Discover(_))
    }

    /// Check if this peer has a known peer ID.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self.audience, Audience::Known(_))
    }

    /// Get the peer ID if known.
    #[must_use]
    pub const fn peer_id(&self) -> Option<PeerId> {
        match self.audience {
            Audience::Known(id) => Some(id),
            Audience::Discover(_) => None,
        }
    }

    /// Update to use a known peer ID.
    ///
    /// Call this after connecting via discovery mode to save the
    /// learned peer identity for future connections.
    pub const fn set_known(&mut self, peer_id: PeerId) {
        self.audience = Audience::known(peer_id);
    }

    /// Record a successful sync, updating the timestamp and per-file digests.
    ///
    /// Call this after `sync_with_peer` completes successfully.
    pub fn record_sync(
        &mut self,
        synced_files: impl IntoIterator<Item = (SedimentreeId, Digest<Sedimentree>)>,
    ) {
        self.last_synced_at = Some(UnixTimestamp::now());
        for (id, digest) in synced_files {
            self.synced_digests.insert(id, digest);
        }
    }

    /// Check if a sedimentree is synced to this peer with the given digest.
    ///
    /// Returns `true` if the stored digest matches the provided one.
    #[must_use]
    pub fn is_synced(&self, id: &SedimentreeId, current_digest: &Digest<Sedimentree>) -> bool {
        self.synced_digests
            .get(id)
            .is_some_and(|synced| synced == current_digest)
    }

    /// Get the digest that was last synced for a sedimentree, if any.
    #[must_use]
    pub fn synced_digest(&self, id: &SedimentreeId) -> Option<&Digest<Sedimentree>> {
        self.synced_digests.get(id)
    }
}

/// Errors from peer operations.
#[derive(Debug, Error)]
pub enum PeerError {
    /// I/O error reading or writing peer file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON decode error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Peer not found.
    #[error("peer not found: {0}")]
    NotFound(String),

    /// Invalid peer name.
    #[error("invalid peer name: {0}")]
    InvalidName(#[from] InvalidPeerName),

    /// Could not determine config directory.
    #[error("could not determine config directory: {0}")]
    NoConfigDir(#[from] crate::config::NoConfigDir),
}

// ============================================================================
// Global peer management functions
// ============================================================================

/// List all globally configured peers.
///
/// Peers are stored in `~/.config/darn/peers/`.
///
/// # Errors
///
/// Returns an error if the peers directory cannot be read.
pub fn list_peers() -> Result<Vec<Peer>, PeerError> {
    let peers_dir = crate::config::global_peers_dir()?;
    if !peers_dir.exists() {
        return Ok(vec![]);
    }

    let mut peers = Vec::new();
    for entry in std::fs::read_dir(&peers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            peers.push(Peer::load(&path)?);
        }
    }

    // Sort by name for consistent ordering
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(peers)
}

/// Get a globally configured peer by name.
///
/// # Errors
///
/// Returns an error if the peer file cannot be read.
pub fn get_peer(name: &PeerName) -> Result<Option<Peer>, PeerError> {
    let peers_dir = crate::config::global_peers_dir()?;
    let path = peers_dir.join(format!("{}.json", name.as_str()));
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(Peer::load(&path)?))
}

/// Add or update a globally configured peer.
///
/// # Errors
///
/// Returns an error if the peer file cannot be written.
pub fn add_peer(peer: &Peer) -> Result<(), PeerError> {
    let peers_dir = crate::config::global_peers_dir()?;
    if !peers_dir.exists() {
        std::fs::create_dir_all(&peers_dir)?;
    }
    let path = peers_dir.join(format!("{}.json", peer.name.as_str()));
    peer.save(&path)
}

/// Remove a globally configured peer by name.
///
/// Returns `true` if the peer was removed, `false` if it didn't exist.
///
/// # Errors
///
/// Returns an error if the peer file cannot be removed.
pub fn remove_peer(name: &PeerName) -> Result<bool, PeerError> {
    let peers_dir = crate::config::global_peers_dir()?;
    let path = peers_dir.join(format!("{}.json", name.as_str()));
    if path.exists() {
        std::fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;
    use testresult::TestResult;

    #[test]
    fn peer_name_validation_consistent_with_rules() {
        check!().with_type::<String>().for_each(|s: &String| {
            let is_valid = !s.is_empty()
                && s.len() <= 64
                && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');

            match PeerName::new(s) {
                Ok(name) => {
                    assert!(is_valid, "PeerName accepted invalid input: {s:?}");
                    assert_eq!(name.as_str(), s);
                }
                Err(_) => {
                    assert!(!is_valid, "PeerName rejected valid input: {s:?}");
                }
            }
        });
    }

    #[test]
    fn discover_creates_discovery_audience() -> TestResult {
        let name = PeerName::new("test")?;
        let peer = Peer::discover(name, "ws://localhost:9000".into());
        assert!(peer.is_discovery());
        assert!(!peer.is_known());
        assert!(peer.peer_id().is_none());
        Ok(())
    }

    #[test]
    fn known_creates_known_audience() -> TestResult {
        let name = PeerName::new("test")?;
        let peer_id = PeerId::new([1u8; 32]);
        let peer = Peer::known(name, "ws://localhost:9000".into(), peer_id);
        assert!(peer.is_known());
        assert!(!peer.is_discovery());
        assert_eq!(peer.peer_id(), Some(peer_id));
        Ok(())
    }

    #[test]
    fn set_known_updates_audience() -> TestResult {
        let name = PeerName::new("test")?;
        let mut peer = Peer::discover(name, "ws://localhost:9000".into());
        assert!(peer.is_discovery());

        let peer_id = PeerId::new([2u8; 32]);
        peer.set_known(peer_id);
        assert!(peer.is_known());
        assert_eq!(peer.peer_id(), Some(peer_id));
        Ok(())
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn roundtrip_json() {
        let dir = tempfile::tempdir().expect("create tempdir");
        check!()
            .with_type::<(String, String)>()
            .for_each(|(name_str, url_str)| {
                // Skip inputs that don't pass PeerName validation
                let Ok(name) = PeerName::new(name_str) else {
                    return;
                };

                let peer = Peer::discover(name, url_str.clone());
                let path = dir.path().join("test.json");
                peer.save(&path).expect("save");

                let loaded = Peer::load(&path).expect("load");
                assert_eq!(loaded.name.as_str(), peer.name.as_str());
                assert_eq!(loaded.url, peer.url);
                assert_eq!(loaded.added_at.as_secs(), peer.added_at.as_secs());
            });
    }

    #[test]
    fn json_format_discovery() -> TestResult {
        let name = PeerName::new("test")?;
        let peer = Peer::discover(name, "ws://localhost:9000".into());
        let json = serde_json::to_string_pretty(&peer)?;

        // Should contain "discover" mode
        assert!(json.contains("\"mode\": \"discover\""));
        Ok(())
    }

    #[test]
    fn json_format_known() -> TestResult {
        let name = PeerName::new("test")?;
        let peer_id = PeerId::new([1u8; 32]);
        let peer = Peer::known(name, "ws://localhost:9000".into(), peer_id);
        let json = serde_json::to_string_pretty(&peer)?;

        // Should contain "known" mode
        assert!(json.contains("\"mode\": \"known\""));
        Ok(())
    }
}
