# Data Model

## Workspace Layout

```
project/
├── .darn/
│   ├── manifest.json       # Tracked file metadata
│   └── storage/            # Content-addressed storage
├── .darnignore             # Ignore patterns (gitignore syntax)
└── <tracked files>
```

## Global Configuration

```
~/.config/darn/
├── signer/
│   └── signing_key.ed25519   # Identity keypair
└── peers/
    └── <name>.json           # Per-peer configuration
```

## Manifest

The manifest is the source of truth for what darn tracks. It lives at `.darn/manifest.json`.

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

## Directory Tree

The workspace directory structure is tracked separately from file contents. Each directory has its own ID and contains entries pointing to child files/directories.

```
Root Directory (ID: abc123)
├── src/ (ID: def456)
│   ├── main.rs (ID: 111...)
│   └── lib.rs (ID: 222...)
└── README.md (ID: 333...)
```

This enables:
- **Rename detection**: Moving a file updates the directory tree, not the file's ID
- **Remote discovery**: When syncing, new files appear in the directory tree first
- **Structural merges**: Concurrent directory edits (adding different files) merge cleanly

## Peer Configuration

Peers are stored in `~/.config/darn/peers/<name>.json`:

```rust
struct Peer {
    name: PeerName,                           // Human-readable name
    url: String,                              // WebSocket URL
    audience: Audience,                       // How to authenticate
    synced_digests: BTreeMap<...>,            // Last-synced state per file
    last_synced_at: Option<UnixTimestamp>,    // For display
}
```

### Audience

Two modes for peer authentication:

| Mode | Use Case |
|------|----------|
| `Known(PeerId)` | Direct peer-to-peer (you know their public key) |
| `Discover(service)` | Via relay server (key learned on first connect) |

### Sync State

`synced_digests` tracks what each peer has seen. This enables:
- Skipping files that haven't changed since last sync
- Knowing when a peer is behind
- Resuming interrupted syncs

## Ignore Patterns

`.darnignore` uses gitignore syntax:

```
# Ignore build artifacts
target/
*.o

# Ignore editor backups
*~
*.swp
```

Ignored files are:
- Not tracked on `darn sync`
- Not shown in `darn tree`
- Still visible in the filesystem (darn doesn't hide anything)
