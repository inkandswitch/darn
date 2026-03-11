//! Progress tracking for sync operations.
//!
//! Provides structures to track and report progress during peer synchronization.

use std::path::PathBuf;

use sedimentree_core::id::SedimentreeId;
use subduction_core::peer::id::PeerId;

/// Progress update during sync.
#[derive(Debug, Clone)]
pub enum SyncProgressEvent {
    /// Starting to sync with a peer.
    ConnectingToPeer {
        /// Name of the peer being connected to.
        peer_name: String,

        /// Display-formatted address of the peer.
        address: String,
    },

    /// Connected to peer.
    Connected {
        /// The peer ID of the connected peer.
        peer_id: PeerId,
    },

    /// Starting to sync sedimentrees.
    StartingSync {
        /// Total number of sedimentrees to sync.
        total_sedimentrees: usize,
    },

    /// A sedimentree sync started.
    SedimentreeStarted {
        /// ID of the sedimentree being synced.
        sedimentree_id: SedimentreeId,

        /// File path if this is a file (None for root directory).
        file_path: Option<PathBuf>,

        /// Zero-based index of the current sedimentree.
        index: usize,

        /// Total number of sedimentrees to sync.
        total: usize,
    },

    /// A sedimentree sync completed.
    SedimentreeCompleted {
        /// ID of the sedimentree that was synced.
        sedimentree_id: SedimentreeId,

        /// Number of items (commits + fragments) received.
        items_received: usize,

        /// Number of items (commits + fragments) sent.
        items_sent: usize,

        /// Zero-based index of the completed sedimentree.
        index: usize,

        /// Total number of sedimentrees to sync.
        total: usize,
    },

    /// Sync completed.
    Completed(SyncSummary),
}

/// Summary of a completed sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncSummary {
    /// Peer we synced with.
    pub peer_id: Option<PeerId>,

    /// Total sedimentrees synced.
    pub sedimentrees_synced: usize,

    /// Commits received from peer.
    pub commits_received: usize,

    /// Fragments received from peer.
    pub fragments_received: usize,

    /// Commits sent to peer.
    pub commits_sent: usize,

    /// Fragments sent to peer.
    pub fragments_sent: usize,

    /// Any sedimentrees that failed to sync.
    pub errors: Vec<(SedimentreeId, String)>,
}

impl SyncSummary {
    /// Create a new empty summary.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a sedimentree sync result to the summary.
    pub const fn add_sync_stats(&mut self, stats: &subduction_core::connection::stats::SyncStats) {
        self.sedimentrees_synced += 1;
        self.commits_received += stats.commits_received;
        self.fragments_received += stats.fragments_received;
        self.commits_sent += stats.commits_sent;
        self.fragments_sent += stats.fragments_sent;
    }

    /// Add an error for a sedimentree.
    pub fn add_error(&mut self, id: SedimentreeId, error: String) {
        self.errors.push((id, error));
    }

    /// Total items received (commits + fragments).
    #[must_use]
    pub const fn total_received(&self) -> usize {
        self.commits_received + self.fragments_received
    }

    /// Total items sent (commits + fragments).
    #[must_use]
    pub const fn total_sent(&self) -> usize {
        self.commits_sent + self.fragments_sent
    }

    /// Returns true if any syncs succeeded.
    #[must_use]
    pub const fn any_success(&self) -> bool {
        self.sedimentrees_synced > 0
    }

    /// Returns true if there were any errors.
    #[must_use]
    pub const fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Result of applying remote changes to local files.
#[derive(Debug, Clone, Default)]
pub struct ApplyResult {
    /// Files that were updated from remote changes.
    pub updated: Vec<PathBuf>,

    /// Files where local and remote both changed (merged via CRDT).
    pub merged: Vec<PathBuf>,

    /// New files created from remote.
    pub created: Vec<PathBuf>,

    /// Files deleted (removed from remote directory tree).
    pub deleted: Vec<PathBuf>,

    /// Errors applying changes.
    pub errors: Vec<(PathBuf, String)>,
}

impl ApplyResult {
    /// Create a new empty result.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if any files were changed.
    #[must_use]
    pub const fn any_changes(&self) -> bool {
        !self.updated.is_empty()
            || !self.merged.is_empty()
            || !self.created.is_empty()
            || !self.deleted.is_empty()
    }

    /// Returns true if there were any errors.
    #[must_use]
    pub const fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Total number of files affected.
    #[must_use]
    pub const fn total_affected(&self) -> usize {
        self.updated.len() + self.merged.len() + self.created.len() + self.deleted.len()
    }

    /// Returns true if any files were deleted.
    #[must_use]
    pub const fn has_deletions(&self) -> bool {
        !self.deleted.is_empty()
    }
}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use bolero::check;
    use subduction_core::connection::stats::SyncStats;

    /// `add_sync_stats` is additive: after N calls the summary fields
    /// equal the element-wise sum, and `sedimentrees_synced == N`.
    #[test]
    fn sync_summary_accumulates_stats() {
        check!()
            .with_type::<Vec<(u16, u16, u16, u16)>>()
            .for_each(|entries| {
                let mut s = SyncSummary::new();
                let mut exp_cr = 0;
                let mut exp_fr = 0;
                let mut exp_cs = 0;
                let mut exp_fs = 0;

                for &(cr, fr, cs, fs) in entries {
                    let (cr, fr, cs, fs) =
                        (cr as usize, fr as usize, cs as usize, fs as usize);
                    s.add_sync_stats(&SyncStats {
                        commits_received: cr,
                        fragments_received: fr,
                        commits_sent: cs,
                        fragments_sent: fs,
                    });
                    exp_cr += cr;
                    exp_fr += fr;
                    exp_cs += cs;
                    exp_fs += fs;
                }

                assert_eq!(s.sedimentrees_synced, entries.len());
                assert_eq!(s.commits_received, exp_cr);
                assert_eq!(s.fragments_received, exp_fr);
                assert_eq!(s.commits_sent, exp_cs);
                assert_eq!(s.fragments_sent, exp_fs);
                assert_eq!(s.total_received(), exp_cr + exp_fr);
                assert_eq!(s.total_sent(), exp_cs + exp_fs);
                assert_eq!(s.any_success(), !entries.is_empty());
            });
    }

    #[test]
    fn apply_result_total_affected_is_sum_of_categories() {
        let r = ApplyResult {
            updated: vec![PathBuf::from("a.txt")],
            merged: vec![PathBuf::from("b.txt")],
            created: vec![PathBuf::from("c.txt"), PathBuf::from("d.txt")],
            deleted: vec![PathBuf::from("e.txt")],
            errors: Vec::new(),
        };
        assert_eq!(
            r.total_affected(),
            r.updated.len() + r.merged.len() + r.created.len() + r.deleted.len()
        );
        assert!(r.any_changes());
        assert!(r.has_deletions());
        assert!(!r.has_errors());
    }
}
