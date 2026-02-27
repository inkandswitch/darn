# darn_core

Core library for darn - CRDT-backed filesystem management.

## Overview

`darn_core` provides the foundational types and logic for managing files as Automerge CRDT documents. It handles:

- _Workspace management_ - `.darn` marker file and centralized storage
- _File-to-document mapping_ - Converting files to/from Automerge documents
- _Manifest tracking_ - Mapping file paths to Sedimentree IDs
- _Change detection_ - BLAKE3 content hashing for efficient diffing
- _Ignore patterns_ - Gitignore-style patterns stored in `.darn` config
- _Directory tree sync_ - Patchwork-style directory structure as Automerge docs
- _Staged batch writes_ - Two-phase commit for atomic workspace updates

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                        darn_core                         │
├──────────────────────────────────────────────────────────┤
│  darn.rs          │ Workspace management + sync methods  │
│  file.rs          │ File ↔ Automerge conversion          │
│  directory.rs     │ Directory tree (Patchwork-style)     │
│  manifest.rs      │ Tracked, ContentHash, FileState      │
│  dotfile.rs       │ .darn config file (JSON)             │
│  ignore.rs        │ Ignore pattern matching              │
│  staged_update.rs │ Two-phase batch file writes          │
│  refresh.rs       │ Change detection & incremental sync  │
│  peer.rs          │ Peer config (JSON)                   │
│  signer.rs        │ Ed25519 key management               │
│  config.rs        │ Global config (~/.config/darn/)      │
│  watcher.rs       │ Filesystem watcher for auto-sync     │
└──────────────────────────────────────────────────────────┘
```

## Key Types

### Darn

Workspace management and sync orchestration:

```rust
use darn_core::darn::Darn;

// Initialize new workspace
let ws = Darn::init(Path::new("my-project"))?;

// Open existing
let darn = Darn::open(Path::new("my-project")).await?;

// Access components
let manifest = darn.load_manifest()?;
let subduction = darn.subduction();
```

### File

Patchwork-compatible file representation:

```rust
use darn_core::file::{File, content::Content};

// Create from file
let doc = File::from_path("example.txt")?;

// Or construct directly
let doc = File::text("notes.txt", "Hello, world!");

// Convert to Automerge
let mut am_doc = doc.to_automerge()?;
let bytes = am_doc.save();
```

### `Tracked` & `FileState`

Track files with change detection:

```rust
use darn_core::manifest::tracked::{Tracked, FileState};

// Check file state
match entry.state(workspace_root) {
    FileState::Clean => println!("unchanged"),
    FileState::Modified => println!("changed on disk"),
    FileState::Missing => println!("deleted"),
}
```

## File Document Schema

Patchwork-inspired schema:

| Field         | Type              | Description                       |
|---------------|-------------------|-----------------------------------|
| `name`        | `Name`            | Validated filename (no path seps) |
| `content`     | `Text` or `Bytes` | File content                      |
| `metadata`    | `Metadata`        | Permissions (u32, rwx display)    |

Content encoding:
- _Text files_ → `Text` object (character-level CRDT merging)
- _Binary files_ → `Bytes` scalar (last-writer-wins)

## Serialization

| Data              | Format    | Notes                                      |
|-------------------|-----------|--------------------------------------------|
| File documents    | Automerge | Direct API (`put`, `get`), not serde       |
| Manifest          | JSON      | Via `serde_json`, base58 for 32-byte types |
| Peer config       | JSON      | One file per peer in `~/.config/darn/peers/`|
| Sedimentree data  | CBOR      | Via `minicbor`, managed by Subduction      |

File documents use Automerge's direct API rather than serde integration. This gives explicit control over CRDT types (e.g., `ObjType::Text` for character-level merging vs `ScalarValue::Bytes` for LWW semantics).

## Dependencies

| Crate              | Purpose                        |
|--------------------|--------------------------------|
| `automerge`        | CRDT document storage          |
| `sedimentree_core` | Content-addressed partitioning |
| `sedimentree_fs`   | Filesystem storage backend     |
| `subduction_core`  | Sync protocol                  |
| `blake3`           | Content hashing                |
| `serde` / `serde_json` | JSON serialization        |

## License

Apache-2.0 OR MIT
