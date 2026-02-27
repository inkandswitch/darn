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
├── darn_core/    # Library crate (workspace management, file types, sync logic)
└── darn_cli/     # Binary crate (CLI commands, interactive prompts, theme)
```

See `cargo doc --open` for detailed module documentation.

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
- `~/.config/darn/workspaces.json` — Auto-healing workspace registry
- `~/.config/darn/workspaces/<id>/` — Per-workspace manifest + sedimentree storage
- Override with `DARN_CONFIG_DIR` env var

### Workspace

- `.darn` JSON marker file at workspace root (not a directory)
- Contains workspace ID, ignore patterns, and attribute overrides
- Manifest and storage live under `~/.config/darn/workspaces/<id>/`

### File Documents

Files are stored as Automerge documents with the Patchwork schema:

- Text files → `Text` CRDT (character-level merging)
- Binary files → `Bytes` (last-writer-wins)
