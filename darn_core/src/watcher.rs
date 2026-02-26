//! Filesystem watcher for automatic file tracking and sync.
//!
//! This module provides a filesystem watcher that:
//! - Detects new files and auto-tracks them (unless ignored)
//! - Detects modifications to tracked files and refreshes them
//! - Batches changes and optionally syncs with peers
//!
//! # Example
//!
//! ```ignore
//! use darn_core::watcher::{Watcher, WatcherConfig, WatchEvent};
//!
//! let config = WatcherConfig::default();
//! let (watcher, mut rx) = Watcher::new(&darn, config)?;
//!
//! while let Some(event) = rx.recv().await {
//!     match event {
//!         WatchEvent::FileCreated(path) => println!("New: {}", path.display()),
//!         WatchEvent::FileModified(path) => println!("Modified: {}", path.display()),
//!         WatchEvent::FileDeleted(path) => println!("Deleted: {}", path.display()),
//!         WatchEvent::Error(e) => eprintln!("Error: {e}"),
//!     }
//! }
//! ```

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult, Debouncer};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{ignore::IgnoreRules, manifest::Manifest};

/// Default debounce duration for filesystem events.
const DEFAULT_DEBOUNCE_MS: u64 = 300;

/// Configuration for the filesystem watcher.
#[derive(Debug, Clone, Copy)]
pub struct WatcherConfig {
    /// How long to wait before processing events (debounce).
    pub debounce_duration: Duration,

    /// Whether to auto-track new files that aren't ignored.
    pub auto_track: bool,

    /// Whether to auto-refresh modified tracked files.
    pub auto_refresh: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_duration: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            auto_track: true,
            auto_refresh: true,
        }
    }
}

/// Events emitted by the watcher.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A new file was created (and should be tracked).
    FileCreated(PathBuf),

    /// A tracked file was modified.
    FileModified(PathBuf),

    /// A tracked file was deleted.
    FileDeleted(PathBuf),

    /// A file was renamed (from, to).
    FileRenamed {
        /// Old path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
    },

    /// An error occurred while watching.
    Error(String),

    /// Batch of changes is ready to be processed.
    BatchReady(WatchBatch),
}

/// A batch of filesystem changes ready for processing.
#[derive(Debug, Clone, Default)]
pub struct WatchBatch {
    /// New files to track.
    pub created: Vec<PathBuf>,

    /// Modified files to refresh.
    pub modified: Vec<PathBuf>,

    /// Deleted files.
    pub deleted: Vec<PathBuf>,

    /// Renamed files (from, to).
    pub renamed: Vec<(PathBuf, PathBuf)>,
}

impl WatchBatch {
    /// Returns true if the batch has any changes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.created.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.renamed.is_empty()
    }

    /// Total number of changes in the batch.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.created.len() + self.modified.len() + self.deleted.len() + self.renamed.len()
    }
}

/// Errors from the watcher.
#[derive(Debug, Error)]
pub enum WatcherError {
    /// Failed to create watcher.
    #[error("failed to create watcher: {0}")]
    Create(String),

    /// Failed to watch path.
    #[error("failed to watch path: {0}")]
    Watch(String),

    /// Failed to load ignore rules.
    #[error("failed to load ignore rules: {0}")]
    IgnoreRules(#[from] crate::ignore::IgnorePatternError),
}

/// Filesystem watcher for a darn workspace.
///
/// The watcher monitors the workspace directory for changes and emits
/// events that can be processed to auto-track and sync files.
pub struct Watcher {
    /// The underlying debounced watcher.
    #[allow(dead_code)] // Kept alive to continue watching
    debouncer: Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>,

    /// Workspace root path.
    root: PathBuf,

    /// Watcher configuration.
    #[allow(dead_code)]
    config: WatcherConfig,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("root", &self.root)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Create a new watcher for the given workspace root.
    ///
    /// Returns the watcher and a channel receiver for events.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be created.
    pub fn new(
        root: &Path,
        config: WatcherConfig,
    ) -> Result<(Self, mpsc::UnboundedReceiver<WatchEvent>), WatcherError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let root = root.to_path_buf();
        let root_clone = root.clone();

        // Create debounced watcher
        let debouncer = new_debouncer(
            config.debounce_duration,
            move |result: DebounceEventResult| {
                Self::handle_events(result, &root_clone, &tx);
            },
        )
        .map_err(|e| WatcherError::Create(e.to_string()))?;

        let watcher = Self {
            debouncer,
            root,
            config,
        };

        Ok((watcher, rx))
    }

    /// Start watching the workspace directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be watched.
    pub fn start(&mut self) -> Result<(), WatcherError> {
        self.debouncer
            .watcher()
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(|e| WatcherError::Watch(e.to_string()))
    }

    /// Stop watching.
    pub fn stop(&mut self) {
        drop(self.debouncer.watcher().unwatch(&self.root));
    }

    /// Handle debounced filesystem events.
    fn handle_events(
        result: DebounceEventResult,
        root: &Path,
        tx: &mpsc::UnboundedSender<WatchEvent>,
    ) {
        match result {
            Ok(events) => {
                for event in events {
                    let path = &event.path;

                    // Skip .darn marker file
                    if path
                        .strip_prefix(root)
                        .is_ok_and(|p| p.as_os_str() == ".darn")
                    {
                        continue;
                    }

                    // Skip hidden files
                    if path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                    {
                        continue;
                    }

                    // Convert to relative path
                    let relative = match path.strip_prefix(root) {
                        Ok(p) => p.to_path_buf(),
                        Err(_) => continue,
                    };

                    // Determine event type based on path existence
                    // Note: notify-debouncer-mini provides DebouncedEventKind which
                    // doesn't distinguish between create/modify/delete, so we check
                    // the filesystem state
                    let event = if path.exists() {
                        if path.is_file() {
                            // Could be create or modify - we'll let the processor decide
                            WatchEvent::FileModified(relative)
                        } else {
                            // Directory - skip
                            continue;
                        }
                    } else {
                        WatchEvent::FileDeleted(relative)
                    };

                    drop(tx.send(event));
                }
            }
            Err(error) => {
                drop(tx.send(WatchEvent::Error(error.to_string())));
            }
        }
    }
}

/// Processes watch events and batches them for efficient handling.
///
/// This struct accumulates events and can be flushed to produce a batch
/// of changes ready for processing.
#[derive(Debug)]
pub struct WatchEventProcessor {
    /// Workspace root.
    root: PathBuf,

    /// Ignore rules.
    ignore_rules: IgnoreRules,

    /// Paths that have been modified.
    modified: HashSet<PathBuf>,

    /// Paths that have been deleted.
    deleted: HashSet<PathBuf>,

    /// Paths of tracked files (for distinguishing new vs modified).
    tracked_paths: HashSet<PathBuf>,
}

impl WatchEventProcessor {
    /// Create a new event processor.
    ///
    /// # Errors
    ///
    /// Returns an error if ignore rules cannot be loaded.
    pub fn new(root: &Path, manifest: &Manifest) -> Result<Self, WatcherError> {
        let ignore_rules = IgnoreRules::from_workspace_root(root)?;

        let tracked_paths: HashSet<_> = manifest.iter().map(|e| e.relative_path.clone()).collect();

        Ok(Self {
            root: root.to_path_buf(),
            ignore_rules,
            modified: HashSet::new(),
            deleted: HashSet::new(),
            tracked_paths,
        })
    }

    /// Update the set of tracked paths (call after tracking new files).
    pub fn update_tracked_paths(&mut self, manifest: &Manifest) {
        self.tracked_paths = manifest.iter().map(|e| e.relative_path.clone()).collect();
    }

    /// Process a watch event.
    ///
    /// Returns true if the event was processed (not ignored).
    pub fn process(&mut self, event: WatchEvent) -> bool {
        match event {
            WatchEvent::FileModified(path) | WatchEvent::FileCreated(path) => {
                // Check if ignored
                if self.ignore_rules.is_ignored(&path, false) {
                    return false;
                }

                // Remove from deleted if it was there (file restored)
                self.deleted.remove(&path);

                // Add to modified
                self.modified.insert(path);
                true
            }

            WatchEvent::FileDeleted(path) => {
                // Only track deletion of files we were tracking
                if !self.tracked_paths.contains(&path) {
                    return false;
                }

                // Remove from modified if it was there
                self.modified.remove(&path);

                // Add to deleted
                self.deleted.insert(path);
                true
            }

            WatchEvent::FileRenamed { from, to } => {
                // Handle rename as delete + create
                if self.tracked_paths.contains(&from) {
                    self.deleted.insert(from);
                }

                if !self.ignore_rules.is_ignored(&to, false) {
                    self.modified.insert(to);
                }

                true
            }

            WatchEvent::Error(_) | WatchEvent::BatchReady(_) => false,
        }
    }

    /// Flush accumulated events into a batch.
    ///
    /// Separates modified files into created (new) and modified (existing tracked).
    pub fn flush(&mut self) -> WatchBatch {
        let mut batch = WatchBatch::default();

        // Separate modified into created vs modified
        for path in self.modified.drain() {
            if self.tracked_paths.contains(&path) {
                batch.modified.push(path);
            } else {
                batch.created.push(path);
            }
        }

        // Move deleted
        batch.deleted = self.deleted.drain().collect();

        // Sort for consistent ordering
        batch.created.sort();
        batch.modified.sort();
        batch.deleted.sort();

        batch
    }

    /// Check if there are pending events.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.modified.is_empty() || !self.deleted.is_empty()
    }

    /// Get the workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Result of processing a batch of watch events.
#[derive(Debug, Default)]
pub struct WatchProcessResult {
    /// Files that were newly tracked.
    pub tracked: Vec<PathBuf>,

    /// Files that were refreshed.
    pub refreshed: Vec<PathBuf>,

    /// Files that are missing (deleted from disk).
    pub deleted: Vec<PathBuf>,

    /// Errors that occurred.
    pub errors: Vec<(PathBuf, String)>,
}

impl WatchProcessResult {
    /// Check if there are any changes.
    #[must_use]
    pub const fn has_changes(&self) -> bool {
        !self.tracked.is_empty() || !self.refreshed.is_empty() || !self.deleted.is_empty()
    }

    /// Total number of affected files.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.tracked.len() + self.refreshed.len() + self.deleted.len()
    }
}

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    #[test]
    fn watcher_config_default() {
        let config = WatcherConfig::default();
        assert_eq!(config.debounce_duration, Duration::from_millis(300));
        assert!(config.auto_track);
        assert!(config.auto_refresh);
    }

    #[test]
    fn watch_batch_is_empty() {
        let batch = WatchBatch::default();
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn watch_batch_len() {
        let batch = WatchBatch {
            created: vec![PathBuf::from("a.txt")],
            modified: vec![PathBuf::from("b.txt"), PathBuf::from("c.txt")],
            deleted: vec![],
            renamed: vec![(PathBuf::from("d.txt"), PathBuf::from("e.txt"))],
        };
        assert!(!batch.is_empty());
        assert_eq!(batch.len(), 4);
    }

    #[test]
    fn event_processor_ignores_config_patterns() {
        use crate::dotfile::{AttributeMap, DarnConfig};
        use crate::workspace::WorkspaceId;

        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let manifest = Manifest::new();

        // Create .darn config with ignore pattern
        let config = DarnConfig::with_fields(
            WorkspaceId::from_bytes([1; 16]),
            sedimentree_core::id::SedimentreeId::new([2; 32]),
            vec!["*.log".to_string()],
            AttributeMap::default(),
        );
        config.save(temp_dir.path()).expect("save config");

        let mut processor =
            WatchEventProcessor::new(temp_dir.path(), &manifest).expect("create processor");

        // Should be ignored
        assert!(!processor.process(WatchEvent::FileModified(PathBuf::from("test.log"))));

        // Should not be ignored
        assert!(processor.process(WatchEvent::FileModified(PathBuf::from("test.txt"))));
    }

    #[test]
    fn event_processor_separates_created_and_modified() {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let mut manifest = Manifest::new();

        // Add a tracked file
        let tracked = crate::manifest::tracked::Tracked::new(
            sedimentree_core::id::SedimentreeId::new([1u8; 32]),
            PathBuf::from("existing.txt"),
            crate::file::file_type::FileType::Text,
            crate::manifest::content_hash::hash_bytes(&[0u8; 32]),
            sedimentree_core::crypto::digest::Digest::force_from_bytes([0u8; 32]),
        );
        manifest.track(tracked);

        let mut processor =
            WatchEventProcessor::new(temp_dir.path(), &manifest).expect("create processor");

        // Modify tracked file
        processor.process(WatchEvent::FileModified(PathBuf::from("existing.txt")));

        // Create new file
        processor.process(WatchEvent::FileModified(PathBuf::from("new.txt")));

        let batch = processor.flush();

        assert_eq!(batch.created, vec![PathBuf::from("new.txt")]);
        assert_eq!(batch.modified, vec![PathBuf::from("existing.txt")]);
    }

    #[test]
    fn event_processor_handles_delete_restore_cycle() {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let mut manifest = Manifest::new();

        // Add a tracked file
        let tracked = crate::manifest::tracked::Tracked::new(
            sedimentree_core::id::SedimentreeId::new([1u8; 32]),
            PathBuf::from("file.txt"),
            crate::file::file_type::FileType::Text,
            crate::manifest::content_hash::hash_bytes(&[0u8; 32]),
            sedimentree_core::crypto::digest::Digest::force_from_bytes([0u8; 32]),
        );
        manifest.track(tracked);

        let mut processor =
            WatchEventProcessor::new(temp_dir.path(), &manifest).expect("create processor");

        // Delete file
        processor.process(WatchEvent::FileDeleted(PathBuf::from("file.txt")));
        assert!(processor.has_pending());

        // Restore file (before flush)
        processor.process(WatchEvent::FileModified(PathBuf::from("file.txt")));

        let batch = processor.flush();

        // Should be in modified, not deleted
        assert!(batch.deleted.is_empty());
        assert_eq!(batch.modified, vec![PathBuf::from("file.txt")]);
    }
}
