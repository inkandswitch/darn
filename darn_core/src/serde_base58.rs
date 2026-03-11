//! Serde helpers for base58-encoding 32-byte values.
//!
//! Used for human-readable JSON serialization of `SedimentreeId`, `Digest`, `PeerId`, etc.

use serde::{Deserialize, Deserializer, Serializer, de};

/// Serialize a 32-byte array as base58.
///
/// # Errors
///
/// Returns a serializer error if the output format rejects the string.
pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&bs58::encode(bytes).into_string())
}

/// Deserialize a base58 string to a 32-byte array.
///
/// # Errors
///
/// Returns an error if the input is not valid base58 or is not exactly 32 bytes.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
    let s = String::deserialize(deserializer)?;
    let bytes = bs58::decode(&s)
        .into_vec()
        .map_err(|e| de::Error::custom(format!("invalid base58: {e}")))?;

    bytes
        .try_into()
        .map_err(|v: Vec<u8>| de::Error::custom(format!("expected 32 bytes, got {}", v.len())))
}

/// Serde module for `SedimentreeId` (wraps 32-byte array).
///
/// Serializes as plain base58 of all 32 bytes (internal storage format).
pub mod sedimentree_id {
    use sedimentree_core::id::SedimentreeId;
    use serde::{Deserializer, Serializer};

    /// Serialize `SedimentreeId` as base58.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the string.
    pub fn serialize<S: Serializer>(id: &SedimentreeId, serializer: S) -> Result<S::Ok, S::Error> {
        super::serialize(id.as_bytes(), serializer)
    }

    /// Deserialize base58 to `SedimentreeId`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not valid base58 or is not exactly 32 bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SedimentreeId, D::Error> {
        let bytes = super::deserialize(deserializer)?;
        Ok(SedimentreeId::new(bytes))
    }
}

/// Serde module for `SedimentreeId` as an Automerge URL.
///
/// Serializes as `automerge:<bs58check(first 16 bytes)>`.
/// Deserializes from either:
/// - New format: `automerge:<bs58check>` (16-byte payload, zero-padded to 32)
/// - Legacy format: plain base58 of all 32 bytes (for backward compatibility)
pub mod automerge_url {
    use sedimentree_core::id::SedimentreeId;
    use serde::{Deserialize, Deserializer, Serializer, de};

    use crate::directory::{bs58check_decode, sedimentree_id_to_url};

    /// Serialize `SedimentreeId` as an Automerge URL.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the string.
    pub fn serialize<S: Serializer>(id: &SedimentreeId, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&sedimentree_id_to_url(*id))
    }

    /// Deserialize an Automerge URL to `SedimentreeId`.
    ///
    /// Expects `automerge:<bs58check>` encoding a 16-byte document ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not a valid automerge URL.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SedimentreeId, D::Error> {
        let s = String::deserialize(deserializer)?;

        let encoded = s
            .strip_prefix("automerge:")
            .ok_or_else(|| de::Error::custom(format!("expected 'automerge:' prefix, got: {s}")))?;

        let bytes = bs58check_decode(encoded)
            .map_err(|e| de::Error::custom(format!("invalid automerge URL: {e}")))?;

        if bytes.len() != 16 {
            return Err(de::Error::custom(format!(
                "automerge URL must encode 16 bytes, got {}",
                bytes.len()
            )));
        }

        let mut arr = [0u8; 32];
        arr[..16].copy_from_slice(&bytes);
        Ok(SedimentreeId::new(arr))
    }
}

/// Serde module for `Digest<T>` (wraps 32-byte array).
pub mod digest {
    use sedimentree_core::crypto::digest::Digest;
    use serde::{Deserializer, Serializer};

    /// Serialize `Digest<T>` as base58.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the string.
    pub fn serialize<T, S: Serializer>(
        digest: &Digest<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        super::serialize(digest.as_bytes(), serializer)
    }

    /// Deserialize base58 to `Digest<T>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not valid base58 or is not exactly 32 bytes.
    pub fn deserialize<'de, T, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Digest<T>, D::Error> {
        let bytes = super::deserialize(deserializer)?;
        Ok(Digest::force_from_bytes(bytes))
    }
}

/// Serde module for `PeerId` (wraps 32-byte array).
pub mod peer_id {
    use serde::{Deserializer, Serializer};
    use subduction_core::peer::id::PeerId;

    /// Serialize `PeerId` as base58.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the string.
    pub fn serialize<S: Serializer>(id: &PeerId, serializer: S) -> Result<S::Ok, S::Error> {
        super::serialize(id.as_bytes(), serializer)
    }

    /// Deserialize base58 to `PeerId`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not valid base58 or is not exactly 32 bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PeerId, D::Error> {
        let bytes = super::deserialize(deserializer)?;
        Ok(PeerId::new(bytes))
    }
}

/// Serde module for `DiscoveryId` (wraps 32-byte array).
pub mod discovery_id {
    use serde::{Deserializer, Serializer};
    use subduction_core::connection::handshake::DiscoveryId;

    /// Serialize `DiscoveryId` as base58.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the string.
    pub fn serialize<S: Serializer>(id: &DiscoveryId, serializer: S) -> Result<S::Ok, S::Error> {
        super::serialize(id.as_bytes(), serializer)
    }

    /// Deserialize base58 to `DiscoveryId`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not valid base58 or is not exactly 32 bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DiscoveryId, D::Error> {
        let bytes = super::deserialize(deserializer)?;
        // Use from_raw, not new - the bytes are already the hash
        Ok(DiscoveryId::from_raw(bytes))
    }
}

/// Serde module for `BTreeMap<SedimentreeId, Digest<Sedimentree>>`.
///
/// Serializes as an array of `{"id": "...", "digest": "..."}` objects.
pub mod synced_digests {
    use std::collections::BTreeMap;

    use sedimentree_core::{crypto::digest::Digest, id::SedimentreeId, sedimentree::Sedimentree};
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        id: String,
        digest: String,
    }

    /// Serialize the map as a list of entries.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the data.
    pub fn serialize<S: Serializer>(
        map: &BTreeMap<SedimentreeId, Digest<Sedimentree>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let entries: Vec<Entry> = map
            .iter()
            .map(|(id, digest)| Entry {
                id: bs58::encode(id.as_bytes()).into_string(),
                digest: bs58::encode(digest.as_bytes()).into_string(),
            })
            .collect();
        entries.serialize(serializer)
    }

    /// Deserialize a list of entries to the map.
    ///
    /// # Errors
    ///
    /// Returns an error if any entry has invalid base58 or incorrect byte length.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<SedimentreeId, Digest<Sedimentree>>, D::Error> {
        let entries: Vec<Entry> = Vec::deserialize(deserializer)?;
        let mut map = BTreeMap::new();

        for entry in entries {
            let id_bytes = bs58::decode(&entry.id)
                .into_vec()
                .map_err(|e| de::Error::custom(format!("invalid base58 id: {e}")))?;
            let id_arr: [u8; 32] = id_bytes.try_into().map_err(|v: Vec<u8>| {
                de::Error::custom(format!("id: expected 32 bytes, got {}", v.len()))
            })?;

            let digest_bytes = bs58::decode(&entry.digest)
                .into_vec()
                .map_err(|e| de::Error::custom(format!("invalid base58 digest: {e}")))?;
            let digest_arr: [u8; 32] = digest_bytes.try_into().map_err(|v: Vec<u8>| {
                de::Error::custom(format!("digest: expected 32 bytes, got {}", v.len()))
            })?;

            map.insert(
                SedimentreeId::new(id_arr),
                Digest::force_from_bytes(digest_arr),
            );
        }

        Ok(map)
    }
}

/// Serde module for `Audience` enum (Known or Discover).
pub mod audience {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
    use subduction_core::connection::handshake::{Audience, DiscoveryId};
    use subduction_core::peer::id::PeerId;

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "mode", content = "id")]
    enum AudienceHelper {
        #[serde(rename = "known")]
        Known(String),
        #[serde(rename = "discover")]
        Discover(String),
    }

    /// Serialize `Audience` as tagged enum with base58 ID.
    ///
    /// # Errors
    ///
    /// Returns a serializer error if the output format rejects the data.
    pub fn serialize<S: Serializer>(audience: &Audience, serializer: S) -> Result<S::Ok, S::Error> {
        let helper = match audience {
            Audience::Known(id) => AudienceHelper::Known(bs58::encode(id.as_bytes()).into_string()),
            Audience::Discover(id) => {
                AudienceHelper::Discover(bs58::encode(id.as_bytes()).into_string())
            }
        };
        helper.serialize(serializer)
    }

    /// Deserialize tagged enum to `Audience`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not valid base58 or is not exactly 32 bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Audience, D::Error> {
        let helper = AudienceHelper::deserialize(deserializer)?;
        match helper {
            AudienceHelper::Known(s) => {
                let bytes = bs58::decode(&s)
                    .into_vec()
                    .map_err(|e| de::Error::custom(format!("invalid base58: {e}")))?;
                let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
                    de::Error::custom(format!("expected 32 bytes, got {}", v.len()))
                })?;
                Ok(Audience::Known(PeerId::new(arr)))
            }
            AudienceHelper::Discover(s) => {
                let bytes = bs58::decode(&s)
                    .into_vec()
                    .map_err(|e| de::Error::custom(format!("invalid base58: {e}")))?;
                let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
                    de::Error::custom(format!("expected 32 bytes, got {}", v.len()))
                })?;
                // Use from_raw, not new - the bytes are already the hash
                Ok(Audience::Discover(DiscoveryId::from_raw(arr)))
            }
        }
    }
}
