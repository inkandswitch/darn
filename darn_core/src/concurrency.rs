//! Concurrency limits for parallel operations.
//!
//! Provides tuned defaults for I/O-bound and network-bound work, rather than
//! using a single CPU-core-based limit for everything.

/// Concurrency for I/O-bound work (file reads, sedimentree store/load).
///
/// Tasks alternate between CPU (offloaded to `spawn_blocking`) and disk I/O
/// wait. Using more tasks than cores keeps the storage device's queue fed
/// while CPU work proceeds on the blocking pool.
///
/// Returns `min(cores * 4, 64)`, falling back to 16 if core count is
/// unavailable.
#[must_use]
pub fn io_bound() -> usize {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(4);
    (cores * 4).min(64)
}

/// Concurrency for network-bound work (sync with peers).
///
/// Network roundtrips are high-latency relative to CPU cost. A flat cap
/// independent of core count keeps many syncs in flight. If the remote peer
/// can't keep up, transport-level backpressure (WebSocket/QUIC flow control)
/// will naturally throttle below this limit.
#[must_use]
pub const fn network_bound() -> usize {
    128
}
