# darn CLI

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

## Sync Model

Unlike git, darn uses a Dropbox-like model: _everything syncs by default_. New files are auto-discovered on `darn sync` and tracked automatically. Use `darn ignore` to exclude files you don't want synced.

## Commands

### Initialize a Workspace

```bash
darn init
darn init my-project
```

Creates a `.darn` marker file in the target directory. On first run, also sets up your global signer key at `~/.config/darn/signer/`.

### Clone a Workspace

```bash
darn clone <root_id> my-project
```

Clones a workspace from configured peers by root directory ID (get this from `darn info` on the source machine). Creates the target directory if it doesn't exist, or uses an existing empty directory.

### View Tracked Files

```bash
darn tree
```

Shows all tracked files with state indicators:

```
Workspace: /home/user/project

    src/main.rs  7Hj2fXy...
  M src/lib.rs   9Yz4wAb...
  ! deleted.txt  3Qm5xYz...

3 tracked: 1 clean, 1 modified, 1 missing
```

| Indicator | Meaning |
|-----------|---------|
| ` ` (space) | Clean - file matches stored version |
| `M` | Modified - file changed on disk |
| `!` | Missing - file deleted from disk |

### Sync with Peers

```bash
# Sync with all peers
darn sync

# Sync with a specific peer
darn sync relay

# Preview what would be synced
darn sync --dry-run

# Skip confirmation for new files
darn sync --force
```

Auto-discovers new files and commits local changes before syncing.

### Watch for Changes

```bash
# Watch with default 60s poll interval
darn watch

# Custom interval
darn watch -i 30s

# Push-only (no polling)
darn watch -i 0

# Disable auto-tracking of new files
darn watch --no-track
```

Watches the filesystem for changes and auto-syncs. Incoming changes from peers are applied via WebSocket push within 1 second.

### Manage Peers

```bash
# Add a WebSocket peer (discovery mode)
darn peer add --name relay --websocket wss://relay.example.com

# Add with known peer ID
darn peer add --name friend --websocket ws://192.168.1.50:9000 --peer-id <base58_id>

# Add an Iroh peer
darn peer add --name direct --iroh <node_id>

# List peers
darn peer list

# Remove a peer
darn peer remove relay
```

### Ignore Patterns

```bash
# Add ignore patterns
darn ignore "*.log" "build/"

# Remove ignore patterns
darn unignore "*.log"
```

Patterns use gitignore syntax and are stored in the `.darn` config file. Default patterns (`.git/`, `target/`, `.env`, etc.) are created on init.

### Workspace Info

```bash
darn info
```

Shows global config and workspace details including the root directory ID (needed for `darn clone` on other machines).

### File Stats

```bash
darn stat src/main.rs
darn stat <sedimentree_id>
```

Shows statistics for a tracked file (commits, fragments, sync state).

## Environment Variables

| Variable          | Purpose                                              |
|-------------------|------------------------------------------------------|
| `RUST_LOG`        | Logging level (e.g., `RUST_LOG=debug`)               |
| `DARN_CONFIG_DIR` | Override global config directory (`~/.config/darn/`)  |

## Storage Layout

```
~/.config/darn/                     # Global config
├── signer/
│   └── signing_key.ed25519         # Ed25519 identity
├── peers/
│   └── {name}.json                 # Peer configurations
├── workspaces.json                 # Registry: id → path
└── workspaces/<id>/
    ├── manifest.json               # Tracked files
    └── storage/                    # Sedimentree data

project/
├── .darn                           # JSON marker file
└── ...                             # Your files
```

## License

Apache-2.0 OR MIT
