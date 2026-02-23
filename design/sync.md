# Sync

## Overview

Syncing in darn is a four-phase process:

1. **Discover** — Find new untracked files in the workspace
2. **Refresh** — Detect and store local modifications
3. **Exchange** — Send/receive data with peers
4. **Apply** — Write remote changes to disk

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

Discovery runs in parallel for performance. Progress shows completed files and how many are still being processed.

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
connect to peer (WebSocket)
authenticate (Ed25519 signatures)

for each tracked file:
    exchange history bidirectionally
    # Both sides end up with merged history
```

The sync protocol is efficient—only missing data transfers, not entire files.

### Phase 4: Apply Remote Changes

After exchange, check what changed:

```
for each tracked file:
    new_hash = hash(stored history)
    
    if new_hash != tracked.sedimentree_digest:
        # Peer sent new data
        load merged history
        reconstruct file content
        write to disk
        update both digests

# Also handle structural changes
for each new file in directory tree:
    add to manifest
    write to disk

for each deleted file (in manifest but not in tree):
    remove from disk
    remove from manifest
```

## `darn watch`

Continuous sync mode:

```
start filesystem watcher
connect to all peers

loop:
    on file change:
        if new file and --track enabled:
            track it
        refresh modified files
    
    on interval (or immediately if --interval 0):
        sync with all peers
        apply remote changes
    
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
- **Rename**: Same ID, different path → history preserved
- **Delete + Create**: Different IDs → separate histories

## Progress Reporting

During sync, darn shows:

```
Scanning (15/100)... src/main.rs (+3 in progress)
```

- `15/100` — Completed / Total files
- `src/main.rs` — Most recently completed file
- `+3 in progress` — Files currently being processed in parallel

## Cancellation

Pressing Ctrl+C during sync:
- Aborts immediately
- Discards any unsaved progress
- Leaves workspace in last consistent state
