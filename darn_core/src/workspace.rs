//! Workspace layout and centralized storage.
//!
//! Workspaces store their data under `~/.config/darn/workspaces/<id>/` with:
//! - A manifest tracking files
//! - Per-workspace storage for sedimentree blobs
//!
//! The user's project directory contains a `.darn` JSON marker file.
//!
//! # Directory Layout
//!
//! ```text
//! ~/.config/darn/
//! ├── signer/
//! │   └── signing_key.ed25519
//! ├── peers/
//! │   └── {name}.json
//! ├── workspaces.json             # registry: id → path
//! └── workspaces/
//!     └── <workspace-id>/
//!         ├── manifest.json
//!         └── storage/
//!             └── {sedimentree_id}/
//!                 └── blobs/
//!
//! ~/projects/myproject/
//! ├── .darn                       # JSON marker file (id, ignore, attributes)
//! └── ... user files ...
//! ```

pub mod id;
pub mod layout;
pub mod registry;

pub use id::WorkspaceId;
pub use layout::WorkspaceLayout;
pub use registry::WorkspaceRegistry;
