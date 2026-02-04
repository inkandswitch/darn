# Hacking on `darn`

## Prerequisites

- [Nix](https://nixos.org/) with flakes enabled
- That's it — Nix handles the rest

## Getting Started

```bash
# Enter the dev shell (includes Rust toolchain, cargo, etc.)
nix develop

# Build
cargo build

# Run
cargo run -- --help
```

## Project Structure

```
darn/
├── darn_core/           # Library crate
│   └── src/
│       ├── config.rs        # Global config (~/.config/darn/)
│       ├── file.rs          # File type (Patchwork schema)
│       ├── file/
│       │   ├── content.rs       # Content enum (Text/Bytes)
│       │   ├── file_type.rs     # FileType enum (merge strategy)
│       │   ├── metadata.rs      # Metadata struct
│       │   ├── metadata/
│       │   │   └── permissions.rs
│       │   ├── name.rs          # Name newtype
│       │   └── state.rs         # FileState enum
│       ├── ignore.rs        # .darnignore patterns
│       ├── manifest.rs      # Manifest, ContentHash
│       ├── manifest/
│       │   ├── content_hash.rs
│       │   └── tracked.rs       # Tracked entry
│       ├── path.rs          # Path utilities
│       ├── refresh.rs       # RefreshError
│       ├── signer.rs        # Ed25519 key management
│       ├── sync_progress.rs # Sync progress tracking
│       ├── unix_timestamp.rs
│       ├── watcher.rs       # Filesystem watcher
│       ├── workspace.rs     # .darn/ directory management
│       └── workspace/
│           └── refresh_diff.rs
│
└── darn_cli/            # Binary crate
    └── src/
        ├── main.rs          # CLI with clap + tokio
        ├── commands.rs      # Command implementations
        ├── setup.rs         # First-run signer setup
        └── theme.rs         # Catppuccin Mocha theme
```

## Logging

Logs are _silent by default_ for a clean UI.

```bash
# Enable debug logs
RUST_LOG=debug cargo run -- init .

# Enable logs for specific crates
RUST_LOG=darn_core=debug,darn_cli=info cargo run -- init .

# Or use the verbose flag (forces debug level)
cargo run -- -v init .
```

## Testing

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run a specific test
cargo test test_name
```

## Common Tasks

| Task              | Command                 |
|-------------------|-------------------------|
| Build             | `cargo build`           |
| Build release     | `cargo build --release` |
| Run clippy        | `cargo clippy`          |
| Format code       | `cargo fmt`             |
| Check formatting  | `cargo fmt --check`     |
| Build docs        | `cargo doc --open`      |
| Watch for changes | `cargo watch -x check`  |

## First-Run Setup

`darn` requires an Ed25519 signer at `~/.config/darn/signer/signing_key.ed25519`.

To re-trigger the first-run setup flow:

```bash
rm -rf ~/.config/darn
cargo run -- init .
```

## Architecture Notes

### Global Config

- `~/.config/darn/signer/` — Ed25519 keypair via `subduction_core::MemorySigner`
- `~/.config/darn/peers/` — Peer configurations (shared across workspaces)
- Peer ID displayed as base58

### Workspace

- `.darn/` directory at workspace root
- `manifest.json` — Tracked entries (SedimentreeId ↔ path)
- `storage/` — Automerge documents (via sedimentree)

### File Documents

Files are stored as Automerge documents with the Patchwork schema:

- Text files → `Text` CRDT (character-level merging)
- Binary files → `Bytes` (last-writer-wins)
