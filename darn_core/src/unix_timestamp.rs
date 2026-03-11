//! Unix timestamp utilities.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// Unix timestamp in seconds since the epoch.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    minicbor::Encode,
    minicbor::Decode,
)]
#[serde(transparent)]
#[cbor(transparent)]
pub struct UnixTimestamp(#[n(0)] u64);

impl UnixTimestamp {
    /// Creates a timestamp from seconds since the Unix epoch.
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(secs)
    }

    /// Returns the current time as a Unix timestamp.
    #[must_use]
    pub fn now() -> Self {
        Self(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
    }

    /// Returns the timestamp as seconds since the Unix epoch.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0
    }
}

impl fmt::Display for UnixTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;

    #[test]
    fn cbor_roundtrip() {
        check!().with_type::<u64>().for_each(|secs: &u64| {
            let ts = UnixTimestamp::from_secs(*secs);
            let mut buf = Vec::new();
            minicbor::encode(&ts, &mut buf).expect("encode");
            let decoded: UnixTimestamp = minicbor::decode(&buf).expect("decode");
            assert_eq!(decoded, ts);
        });
    }
}
