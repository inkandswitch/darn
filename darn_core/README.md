# darn_core

Core library for darn - CRDT-backed filesystem management.

## Overview

`darn_core` provides the foundational types and logic for managing files as Automerge CRDT documents. It handles:

- **Workspace management** - `.darn/` directory structure
- **File ↔ document mapping** - Converting files to/from Automerge documents
- **Manifest tracking** - Mapping file paths to Sedimentree IDs
- **Change detection** - BLAKE3 content hashing for efficient diffing
- **Ignore patterns** - `.darnignore` support (gitignore syntax)

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                        darn_core                         │
├──────────────────────────────────────────────────────────┤
│  workspace.rs   │ .darn/ directory management            │
│  file.rs        │ File ↔ Automerge conversion            │
│  manifest.rs    │ Tracked, ContentHash, FileState        │
│  refresh.rs     │ Change detection & incremental commits │
│  signer.rs      │ Ed25519 key management                 │
│  config.rs      │ Global config (~/.config/darn/)        │
│  ignore.rs      │ .darnignore pattern matching           │
└──────────────────────────────────────────────────────────┘
```

## Key Types

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

### Workspace

Manages the `.darn/` directory:

```rust
use darn_core::workspace::Workspace;

// Initialize new workspace
let ws = Workspace::init(".")?;

// Open existing
let ws = Workspace::open(".")?;

// Access components
let manifest = ws.load_manifest()?;
let storage = ws.storage()?;
let peer_id = ws.peer_id()?;
```

### `Tracked` & `FileState`

Track files with change detection:

```rust
use darn_core::manifest::{Tracked, FileState, ContentHash};

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
- **Text files** → `Text` object (character-level CRDT merging)
- **Binary files** → `Bytes` scalar (last-writer-wins)

## Serialization

| Data              | Format    | Notes                                      |
|-------------------|-----------|--------------------------------------------|
| File documents    | Automerge | Direct API (`put`, `get`), not serde       |
| Manifest          | CBOR      | Via `minicbor` for `Tracked` entries   |

File documents use Automerge's direct API rather than serde integration. This gives explicit control over CRDT types (e.g., `ObjType::Text` for character-level merging vs `ScalarValue::Bytes` for LWW semantics).

## Dependencies

| Crate              | Purpose                        |
|--------------------|--------------------------------|
| `automerge`        | CRDT document storage          |
| `sedimentree_core` | Content-addressed partitioning |
| `sedimentree_fs`   | Filesystem storage backend     |
| `blake3`           | Content hashing                |
| `minicbor`         | CBOR serialization             |

## License

Apache-2.0 OR MIT
