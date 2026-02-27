# Overview

Darn brings version control semantics to local-first file sync. It tracks files in a workspace, detects changes, and synchronizes with peers—automatically merging concurrent edits.

## Architecture

```
┌───────────────────────────────────────────────────────────────────┐
│                          User Filesystem                          │
│                                                                   │
│   project/                                                        │
│   ├── .darn               (JSON marker + config)                  │
│   ├── src/                                                        │
│   └── README.md           (synced files)                          │
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
                            │
                            ▼
┌───────────────────────────────────────────────────────────────────┐
│                    Centralized Storage                             │
│   ~/.config/darn/workspaces/<id>/                                 │
│   ├── manifest.json                                               │
│   └── storage/                                                    │
└───────────────────────────────────────────────────────────────────┘
```

## Core Concepts

### Workspace

A directory containing a `.darn` JSON marker file. The workspace root is the sync boundary—all non-ignored files under it are synced automatically.

### Sync Model

Darn uses a Dropbox-like model: everything syncs by default. New files are auto-discovered on `darn sync`. Use `darn ignore` to exclude files (patterns stored in the `.darn` config file).

### Manifest

The `manifest.json` file (stored under `~/.config/darn/workspaces/<id>/`) tracks which files are managed by darn. Each entry maps a unique ID to a file path, along with metadata for change detection.

### File Identity

Each tracked file gets a random 32-byte ID that remains stable across renames and moves. This ID is used to:
- Look up the file's history
- Sync with peers
- Detect when a file has been renamed vs. deleted-and-recreated

### Change Detection

Darn uses two hashes per file:
- _Filesystem hash_: Detects local edits (did the user modify this file?)
- _Storage hash_: Detects remote changes (did a peer send new data?)

### Directory Tree

The workspace structure is itself tracked as a tree of Automerge directory documents, each with its own ID. This enables:
- Syncing the directory structure itself
- Detecting new files from peers
- Handling renames and moves

## CLI Commands

| Command | Description |
|---------|-------------|
| `init` | Initialize a workspace in the current directory |
| `clone <id> <path>` | Clone a workspace into a new directory |
| `sync` | Discover new files and sync with peers |
| `watch` | Watch for changes and auto-sync |
| `tree` | Show tracked files with their IDs |
| `stat <file>` | Show file statistics and sync state |
| `peer add/list/remove` | Manage peer connections |
| `ignore/unignore` | Manage ignore patterns |
| `info` | Show global config and workspace info |

## Modules

| Module | Responsibility |
|--------|----------------|
| `darn` | Main entry point: workspace management, sync orchestration |
| `manifest` | File tracking and change detection |
| `file` | Reading/writing file content |
| `directory` | Directory tree operations |
| `dotfile` | `.darn` config file management |
| `peer` | Peer configuration and sync state |
| `ignore` | Gitignore-style pattern matching |
| `staged_update` | Two-phase batch writes for atomic updates |
| `watcher` | Filesystem monitoring for auto-sync |
