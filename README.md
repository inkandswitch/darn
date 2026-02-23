# darn 🪡🧦

<u>D</u>irectory-based <u>A</u>utomerge <u>R</u>eplication <u>N</u>ode

A CLI for managing CRDT-backed files with automatic conflict resolution and peer-to-peer synchronization.

## Overview

`darn` brings collaborative editing to your filesystem. Files are stored as [Automerge] documents and synchronized peer-to-peer using [Subduction]. Think of it as "Dropbox meets git" - but without merge conflicts or the vendor lock-in.

```
┌─────────────────┐      ┌─────────────────┐      ┌─────────────────┐
│   Your Machine  │      │  Peer's Machine │      │  Another Peer   │
│                 │ sync │                 │ sync │                 │
│  .darn/         │─────▶│  .darn/         │─────▶│  .darn/         │
│  └── storage/   │◀─────│  └── storage/   │◀─────│  └── storage/   │
│  ...            │      │  ...            │      │  ...            │
└─────────────────┘      └─────────────────┘      └─────────────────┘
```

## Features

- **Local-first**: Works offline, syncs when connected
- **Conflict-free**: CRDTs automatically merge concurrent edits
- **P2P sync**: No central server required (though you can run one)
- **Text collaboration**: Character-level merging for text files

## Quick Start

```bash
# Initialize a workspace
darn init

# Track some files
darn track README.md src/*.rs

# Check status
darn tree

# Sync with peers
darn sync
```

See [`darn_cli/README.md`] for full CLI documentation.

## Installation

```bash
# From source
cargo install --path darn_cli

# With Nix
nix develop
cargo build --release
```

## Crates

| Crate         | Description                                   |
|---------------|-----------------------------------------------|
| [`darn_core`] | Core library - workspace, documents, manifest |
| [`darn_cli`]  | CLI binary - user-facing commands             |

## How It Works

### File Storage

Files are converted to Automerge documents following the Patchwork schema:

| File Type | Storage Format | Merge Semantics             |
|-----------|----------------|-----------------------------|
| Text      | `Text` object  | Character-level CRDT        |
| Binary    | `Bytes` scalar | Whole-file last-writer-wins |

### Sync Protocol

darn uses Subduction for peer-to-peer synchronization:

1. **Sedimentree** partitions documents into content-addressed fragments
2. **Strata** (hash-based depth levels) enable efficient set reconciliation
3. **WebSocket** transport with Ed25519 authentication

## Development

```bash
cargo build    # Build
cargo test     # Test
cargo clippy   # Lint
```

See [HACKING.md] for development details.

## License

Apache-2.0 OR MIT

<!-- Links -->

[Automerge]: https://automerge.org/
[HACKING.md]: HACKING.md
[Subduction]: https://github.com/inkandswitch/subduction
[`darn_cli`]: darn_cli/
[`darn_core`]: darn_core/
[`darn_cli/README.md`]: darn_cli/README.md
