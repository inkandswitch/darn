# Overview

Darn brings version control semantics to local-first file sync. It tracks files in a workspace, detects changes, and synchronizes with peers—automatically merging concurrent edits.

## Architecture

```
┌───────────────────────────────────────────────────────────────────┐
│                          User Filesystem                          │
│                                                                   │
│   project/                                                        │
│   ├── .darn/              (workspace metadata)                    │
│   │   ├── manifest.json   (file tracking)                         │
│   │   └── storage/        (content-addressed storage)             │
│   ├── .darnignore         (ignore patterns)                       │
│   ├── src/                                                        │
│   └── README.md           (tracked files)                         │
└───────────────────────────────────────────────────────────────────┘
        │                           ▲
        │ read/write                │ sync
        ▼                           │
┌───────────────────────────────────────────────────────────────────┐
│                           darn_core                               │
│                                                                   │
│   ┌─────────────┐    ┌───────────┐    ┌────────────────────────┐  │
│   │   Manifest  │    │   File    │    │      Directory         │  │
│   │ (tracking)  │    │ (content) │    │ (tree structure)       │  │
│   └──────┬──────┘    └─────┬─────┘    └───────────┬────────────┘  │
│          │                 │                      │               │
│          └────────┬────────┴──────────────────────┘               │
│                   │                                               │
│                   ▼                                               │
│         ┌───────────────────────────────────────────┐             │
│         │              Storage Layer                │             │
│         │  (Subduction for sync, FsStorage for I/O) │             │
│         └───────────────────────────────────────────┘             │
└───────────────────────────────────────────────────────────────────┘
```

## Core Concepts

### Workspace

A directory containing a `.darn/` folder. The workspace root is the sync boundary—all tracked files live under it.

### Manifest

The `manifest.json` file tracks which files are managed by darn. Each entry maps a unique ID to a file path, along with metadata for change detection.

### File Identity

Each tracked file gets a random 32-byte ID that remains stable across renames and moves. This ID is used to:
- Look up the file's history
- Sync with peers
- Detect when a file has been renamed vs. deleted-and-recreated

### Change Detection

Darn uses two hashes per file:
- **Filesystem hash**: Detects local edits (did the user modify this file?)
- **Storage hash**: Detects remote changes (did a peer send new data?)

### Directory Tree

The workspace structure is itself tracked as a tree of directories, each with its own ID. This enables:
- Syncing the directory structure itself
- Detecting new files from peers
- Handling renames and moves

## CLI Commands

| Command | Description |
|---------|-------------|
| `init` | Initialize a workspace in the current directory |
| `clone <id>` | Clone a workspace by root directory ID |
| `sync` | Discover new files and sync with peers |
| `watch` | Watch for changes and auto-sync |
| `tree` | Show tracked files with their IDs |
| `stat <file>` | Show file statistics and sync state |
| `peer add/list/remove` | Manage peer connections |
| `ignore/unignore` | Manage `.darnignore` patterns |

## Modules

| Module | Responsibility |
|--------|----------------|
| `darn` | Main entry point: workspace management, sync orchestration |
| `manifest` | File tracking and change detection |
| `file` | Reading/writing file content |
| `directory` | Directory tree operations |
| `peer` | Peer configuration and sync state |
| `discover` | Parallel file discovery |
| `watcher` | Filesystem monitoring for auto-sync |
