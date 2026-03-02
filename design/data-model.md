# Data Model

## Workspace Layout

A workspace is any directory containing a `.darn` JSON marker file. All storage and metadata lives in the global config directory, not the workspace itself.

```
project/
├── .darn                           # JSON marker (workspace ID, ignore, attributes)
└── <synced files>

~/.config/darn/                     # Override with DARN_CONFIG_DIR
├── signer/
│   └── signing_key.ed25519         # Ed25519 identity
├── peers/
│   └── <name>.json                 # Per-peer configuration
├── workspaces.json                 # Registry: workspace ID → path
└── workspaces/<id>/
    ├── manifest.json               # Tracked file metadata
    └── storage/                    # Managed by sedimentree_fs_storage
```

### The `.darn` File

A single JSON file replaces the former `.darn/` directory, `.darnignore`, and `.darnattributes`:

```json
{
  "id": "a1b2c3d4e5f6...",
  "root_directory_id": "5K8v3QmXyz...",
  "ignore": [".git/", "target/", ".env"],
  "attributes": {
    "binary": ["*.lock", "*.min.js"],
    "text": ["*.md"]
  }
}
```

The `.darn` file is itself synced, so ignore patterns and attributes propagate across peers.

## Manifest

The manifest is the source of truth for what darn tracks. It lives at `~/.config/darn/workspaces/<id>/manifest.json`.

```rust
struct Manifest {
    root_directory_id: SedimentreeId,
    entries: BTreeMap<SedimentreeId, Tracked>,
}
```

Each tracked file has:

```rust
struct Tracked {
    sedimentree_id: SedimentreeId,           // Stable identity
    relative_path: PathBuf,                   // Current location
    file_type: FileType,                      // Text or Binary
    tracked_at: UnixTimestamp,                // When first tracked
    file_system_digest: Digest<...>,          // Hash of file on disk
    sedimentree_digest: Digest<...>,          // Hash of stored history
}
```

### Change Detection

Two hashes enable efficient change detection without reading file contents:

| Hash | What it hashes | Detects |
|------|----------------|---------|
| `file_system_digest` | Raw bytes on disk | Local edits by user |
| `sedimentree_digest` | Stored history | New data from peers |

**Local change detection:**
```
if hash(file_on_disk) != tracked.file_system_digest:
    # User modified the file → needs refresh
```

**Remote change detection:**
```
if hash(stored_history) != tracked.sedimentree_digest:
    # Peer sent new data → needs apply
```

## File Types

Darn distinguishes text and binary files for merge behavior:

| Type | Detection | Merge Strategy |
|------|-----------|----------------|
| Text | Valid UTF-8 | Character-level merge (concurrent edits preserved) |
| Binary | Invalid UTF-8 | Last-writer-wins |

File type overrides can be configured in the `.darn` file via the `attributes` field (e.g., force `*.lock` to binary).

## Directory Tree

The workspace directory structure is tracked separately from file contents. Each directory is an Automerge document with its own sedimentree ID, containing entries that point to child files and subdirectories.

```
Root Directory (ID: abc123)
├── src/ (ID: def456)
│   ├── main.rs (ID: 111...)
│   └── lib.rs (ID: 222...)
└── README.md (ID: 333...)
```

```rust
struct Directory {
    name: String,                    // Final path component ("" for root)
    entries: Vec<DirectoryEntry>,
}

struct DirectoryEntry {
    name: String,                    // Filename or subdirectory name
    entry_type: EntryType,           // File or Folder
    sedimentree_id: SedimentreeId,   // Points to File or Directory doc
}
```

This enables:
- _Rename detection_: Moving a file updates the directory tree, not the file's ID
- _Remote discovery_: When syncing, new files appear in the directory tree first
- _Structural merges_: Concurrent directory edits (adding different files) merge cleanly

## Peer Configuration

Peers are stored globally in `~/.config/darn/peers/<name>.json`:

```rust
struct Peer {
    name: PeerName,                           // Human-readable name
    address: PeerAddress,                     // WebSocket or Iroh
    audience: Audience,                       // How to authenticate
    last_synced_at: Option<UnixTimestamp>,    // When we last synced
    synced_digests: BTreeMap<...>,            // Last-synced state per file
}

enum PeerAddress {
    WebSocket { url: String },
    Iroh { node_id: String, relay_url: Option<String> },
}
```

### Audience

Two modes for peer authentication:

| Mode | Use Case |
|------|----------|
| `Known(PeerId)` | Direct peer-to-peer (you know their verifying key) |
| `Discover(service)` | Via relay server (key learned on first connect) |

### Sync State

`synced_digests` tracks what each peer has seen. This enables:
- Skipping files that haven't changed since last sync
- Knowing when a peer is behind
- `--dry-run` showing what would be synced without connecting

## Ignore Patterns

Ignore patterns live in the `.darn` file's `ignore` array and use gitignore syntax. Default patterns are created on `darn init`:

```
.git/
target/
node_modules/
.darn-staging-*/
.env
```

Manage patterns with:

```bash
darn ignore "*.log"       # Add pattern
darn unignore "*.log"     # Remove pattern
```

Ignored files are:
- Not tracked on `darn sync`
- Not shown in `darn tree`
- Still visible in the filesystem (darn doesn't hide anything)

## Staged Batch Updates

Remote changes from sync and clone use a two-phase write strategy to prevent external observers (IDEs, build systems) from seeing half-written state:

```
Phase 1: Stage (slow, workspace untouched)
    Write files to .darn-staging-<random>/ inside workspace root
    Same filesystem guarantees atomic rename

Phase 2: Commit (fast, parallel renames)
    Rename all staged files into workspace (parallel)
    Delete removed files (parallel)
    Clean up empty directories
    Update manifest
```
