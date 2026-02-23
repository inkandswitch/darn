//! Workspace layout and atomic tree management.
//!
//! Workspaces are stored under `~/.config/darn/workspaces/<id>/` with:
//! - A manifest tracking files
//! - Ping-pong trees for atomic swaps
//! - A `current` symlink pointing to the active tree
//!
//! The user's project directory is a symlink to the `current` tree.
//!
//! # Directory Layout
//!
//! ```text
//! ~/.config/darn/
//! ├── signing_key.ed25519
//! ├── storage/                    # shared content-addressed store
//! │   ├── blobs/
//! │   ├── commits/
//! │   └── fragments/
//! └── workspaces/
//!     └── <workspace-id>/
//!         ├── manifest.json
//!         ├── trees/
//!         │   ├── a/              # ping-pong tree A
//!         │   └── b/              # ping-pong tree B
//!         └── current -> a        # symlink to active tree
//!
//! ~/projects/myproject -> ~/.config/darn/workspaces/<id>/trees/current
//! ```

pub mod id;
pub mod layout;
pub mod registry;
pub mod swap;

pub use id::WorkspaceId;
pub use layout::WorkspaceLayout;
pub use registry::WorkspaceRegistry;
