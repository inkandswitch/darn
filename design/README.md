# Darn Design Documentation

_Directory-based Automerge Replication Node_

Darn is a CLI tool for syncing files across machines with automatic conflict resolution. It tracks files in a workspace, detects changes, and synchronizes with peers—merging concurrent edits without manual intervention.

## Documents

| Document | Description |
|----------|-------------|
| [Overview](overview.md) | Architecture, core concepts, CLI commands |
| [Data Model](data-model.md) | Workspace layout, manifest, change detection |
| [Sync](sync.md) | Sync phases, conflict handling, watch mode |

## Design Principles

### Local-First

- Works fully offline
- All data stored locally
- Syncs when peers are available
- No required central server

### Stable File Identity

- Each file gets a random 32-byte ID on first track
- ID stays the same across renames and moves
- Enables history preservation and rename detection

### Efficient Change Detection

- Two hashes per file (filesystem + storage)
- Only read files that changed
- Only transfer data peers don't have

### Automatic Merging

- Text files: character-level merge
- Binary files: last-writer-wins
- Directory structure: set operations

## Dependencies

Darn builds on:
- [Automerge](https://automerge.org/) — CRDT library for mergeable data
- [Subduction](https://github.com/subconsciousnetwork/subduction) — Sync protocol and storage
