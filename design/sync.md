# Sync

## Overview

Syncing in darn is a four-phase process:

1. **Discover** — Find new untracked files in the workspace
2. **Refresh** — Detect and store local modifications
3. **Exchange** — Send/receive data with peers
4. **Apply** — Write remote changes to disk (via staged batch update)

## `darn sync`

### Phase 1: Discover New Files

Walk the workspace looking for untracked files:

```
for each file in workspace:
    if file is ignored: skip
    if file is already tracked: skip

    # New file found
    generate random ID
    store file content
    add to directory tree
    add to manifest
```

Discovery runs in parallel for performance. New files are presented to the user for confirmation (unless `--force` is passed).

### Phase 2: Refresh Local Changes

Check each tracked file for modifications:

```
for each tracked file:
    current_hash = hash(file on disk)

    if current_hash != tracked.file_system_digest:
        # File was modified locally
        load existing history
        compute diff
        store new changes
        update file_system_digest
```

This is fast because we only read files whose hash changed.

### Phase 3: Exchange with Peers

For each configured peer:

```
connect to peer (WebSocket or Iroh)
authenticate (Ed25519 signatures)

for each tracked file:
    exchange history bidirectionally
    # Both sides end up with merged history
```

The sync protocol is efficient—only missing data transfers, not entire files. Progress is shown per-sedimentree with items sent/received counts.

### Phase 4: Apply Remote Changes

After exchange, stage and apply changes atomically:

```
# Stage to temp directory (workspace untouched)
for each tracked file:
    new_hash = hash(stored history)

    if new_hash != tracked.sedimentree_digest:
        # Peer sent new data
        load merged history
        reconstruct file content
        write to staging dir

# Commit: parallel renames into workspace
rename all staged files into workspace
delete removed files
clean up empty directories
update manifest

# Also handle structural changes
for each new file in directory tree:
    add to manifest
    write to disk

for each deleted file (in manifest but not in tree):
    remove from disk
    remove from manifest
```

## `darn clone`

Clone creates a new workspace from a remote root directory ID:

```
darn clone <root_id> my-project
```

1. Create target directory (error if non-empty)
2. Initialize workspace with remote root directory ID
3. Connect to all configured peers
4. Recursively traverse directory tree, syncing each sedimentree
5. Stage all files to temp directory
6. Commit: parallel renames into workspace

## `darn watch`

Continuous sync mode:

```
start filesystem watcher
connect to all peers

loop:
    on file change:
        if new file and auto-track enabled:
            track it
        refresh modified files

    on push (WebSocket):
        apply remote changes within 1s

    on poll interval (default: 60s):
        full sync with all peers

    on Ctrl+C:
        disconnect and exit
```

## Conflict Handling

### Text Files

Concurrent edits to the same text file merge automatically at the character level. If Alice adds a line at the top and Bob adds a line at the bottom, both lines appear.

### Binary Files

Binary files use last-writer-wins. The most recent change takes precedence. There's no merge—one version wins.

### Directory Structure

Directory operations (add file, remove file, rename) merge cleanly. If Alice adds `foo.txt` and Bob adds `bar.txt`, both appear.

### Rename vs. Delete-and-Create

Because files have stable IDs, darn distinguishes:
- _Rename_: Same ID, different path — history preserved
- _Delete + Create_: Different IDs — separate histories

## Staged Batch Updates

All file writes from sync and clone use a two-phase approach to prevent external observers (Godot, IDEs, build systems) from seeing half-written workspace state:

```
Phase 1: Stage (slow, workspace untouched)
    Write files to .darn-staging-<random>/ inside workspace root
    Same filesystem guarantees atomic rename

Phase 2: Commit (fast, parallel renames)
    Create parent directories
    Rename all staged files into workspace (parallel, via tokio::spawn)
    Delete removed files (parallel)
    Clean up empty directories
    Apply manifest patches
```

Protection layers:
- _Watcher_: Explicitly skips paths containing the staging dir prefix
- _Discovery_: `walkdir` filter skips `.`-prefixed entries (covers staging dirs)
- _Ignore rules_: `.darn-staging-*/` in default ignore patterns

## Progress Reporting

During sync, darn shows per-sedimentree progress:

```
◆ darn sync
│ Syncing with origin...
│ ▼ [3/5] src/main.rs (8 items)
│ ▲ [3/5] src/main.rs (2 items)
│ ✔ Synced with origin (▼12 ▲3)
```

## Cancellation

Pressing Ctrl+C during sync:
- Aborts immediately
- Discards any unsaved progress
- Leaves workspace in last consistent state
