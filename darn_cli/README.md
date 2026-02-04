# darn

**D**irectory-based **A**utomerge **R**eplication **N**ode

CLI for darn - sync directories over CRDTs.

## Installation

```bash
cargo install --path .
```

Or from the workspace root:

```bash
cargo install --path darn_cli
```

## Commands

### Initialize a Workspace

```bash
darn init
```

Creates a `.darn/` directory in the current folder. On first run, also sets up your global signer key at `~/.config/darn/signer/`.

### Track Files

```bash
darn track myfile.txt
darn track src/*.rs
```

Converts files to Automerge documents and stores them in `.darn/storage/`.

### View Tracked Files

```bash
darn tree
```

Shows all tracked files with state indicators:

```
Workspace: /home/user/project

    src/main.rs
  M src/lib.rs
  ! deleted_file.txt

3 tracked: 1 clean, 1 modified, 1 missing
```

| Indicator | Meaning |
|-----------|---------|
| ` ` (space) | Clean - file matches stored version |
| `M` | Modified - file changed on disk |
| `!` | Missing - file deleted from disk |

### Stop Tracking

```bash
darn untrack myfile.txt
```

Removes from manifest but keeps the local file.

### Sync with Peers

```bash
# Sync with all peers
darn sync

# Sync with specific peer
darn sync --peer ws://192.168.1.50:8080
```

Automatically commits any local changes before syncing.

### Manage Peers

```bash
darn peer add ws://192.168.1.50:8080
darn peer list
darn peer remove ws://192.168.1.50:8080
```

### Watch for Changes

```bash
darn watch
```

_(Not yet implemented)_ Auto-sync when files change.

## Ignore Patterns

Create a `.darnignore` file (gitignore syntax):

```gitignore
.git/

# Build artifacts
target/
*.o

# Editor files
*.swp
*~

# Secrets
.env
*.key
```

The `.darn/` directory is always ignored.

## Environment Variables

| Variable   | Purpose                                     |
|------------|---------------------------------------------|
| `RUST_LOG` | Logging level (e.g., `RUST_LOG=debug`) |

## Storage Layout

```
.darn/
├── manifest.cbor       # Tracked file mappings
├── storage/
│   ├── blobs/          # Content-addressed blobs
│   └── trees/{id}/     # Per-document sedimentree
└── peers/              # Peer information (future)

~/.config/darn/
└── signer/
    └── private.key     # Ed25519 signing key
```

## License

Apache-2.0 OR MIT
