//! CLI command implementations.

// CLI-specific lint allows for this module
#![allow(clippy::format_push_string)]
#![allow(clippy::large_futures)]

use std::{
    fmt::Write as _,
    path::Path, collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
    sync::{Arc, atomic::{AtomicUsize, Ordering}}
};

use console::Style;
use darn_core::{
    darn::Darn,
    directory::{Directory, entry::EntryType},
    file::{File, file_type::FileType, state::FileState},
    manifest::{Manifest, content_hash, tracked::Tracked},
    peer::{Peer, PeerName},
    sedimentree,
    sync_progress::SyncProgressEvent,
    watcher::{WatcherConfig, Watcher, WatchEventProcessor, WatchEvent}
};
use sedimentree_core::id::SedimentreeId;
use subduction_core::{storage::traits::Storage, peer::id::PeerId};
use tracing::info;

/// Style for command references in messages (mauve color).
fn cmd_style() -> Style {
    Style::new().color256(183) // Approximate mauve
}

/// Format a command for display with color.
fn cmd(s: &str) -> String {
    cmd_style().apply_to(s).to_string()
}

/// Format a list of paths as an ASCII tree.
///
/// ```text
/// foo/
/// ├── bar.txt
/// └── baz/
///     └── qux.txt
/// other.txt
/// ```
fn format_paths_as_tree(paths: &[std::path::PathBuf]) -> String {
    // Build a tree structure from paths
    #[derive(Default)]
    struct TreeNode {
        children: BTreeMap<String, TreeNode>,
        is_file: bool,
    }

    let mut root = TreeNode::default();

    for path in paths {
        let mut current = &mut root;
        let components: Vec<_> = path.components().collect();
        let len = components.len();

        for (i, component) in components.into_iter().enumerate() {
            let name = component.as_os_str().to_string_lossy().to_string();
            let is_last = i == len - 1;

            current = current.children.entry(name).or_default();
            if is_last {
                current.is_file = true;
            }
        }
    }

    // Render the tree
    fn render(node: &TreeNode, prefix: &str, output: &mut String) {
        let entries: Vec<_> = node.children.iter().collect();
        let len = entries.len();

        for (i, (name, child)) in entries.into_iter().enumerate() {
            let is_last = i == len - 1;
            let connector = if is_last { "└── " } else { "├── " };
            let child_prefix = if is_last { "    " } else { "│   " };

            if child.is_file && child.children.is_empty() {
                writeln!(output, "{prefix}{connector}{name}").expect("write");
            } else {
                // Directory
                writeln!(output, "{prefix}{connector}{name}/").expect("write");
                render(child, &format!("{prefix}{child_prefix}"), output);
            }
        }
    }

    let mut output = String::new();
    render(&root, "", &mut output);

    // Remove trailing newline
    if output.ends_with('\n') {
        output.pop();
    }

    output
}

/// Initialize a new `darn` workspace.
pub(crate) async fn init(path: &Path) -> anyhow::Result<()> {
    cliclack::intro("darn init")?;

    // Initialize workspace structure
    let initialized = Darn::init(path)?;
    let root = initialized.root().to_path_buf();

    cliclack::log::success(format!("Initialized workspace at {}", root.display()))?;

    // Open workspace to track .darnignore
    let darn = Darn::open(&root).await?;
    let mut manifest = darn.load_manifest()?;

    // Track .darnignore if it exists
    let darnignore_path = root.join(".darnignore");
    if darnignore_path.exists() {
        // Create File from .darnignore
        let doc = darn_core::file::File::from_path(&darnignore_path)?;
        let file_type = darn_core::file::file_type::FileType::Text;

        // Convert to Automerge
        let mut am_doc = doc.into_automerge()?;

        // Generate random SedimentreeId
        let mut id_bytes = [0u8; 32];
        getrandom::getrandom(&mut id_bytes)?;
        let sedimentree_id = sedimentree_core::id::SedimentreeId::new(id_bytes);

        // Store as sedimentree commits
        darn_core::sedimentree::store_document(
            darn.subduction(),
            sedimentree_id,
            &mut am_doc,
        )
        .await?;

        // Add to root directory
        darn_core::sedimentree::add_file_to_directory(
            darn.subduction(),
            manifest.root_directory_id(),
            ".darnignore",
            sedimentree_id,
        )
        .await?;

        // Compute digests
        let file_system_digest =
            darn_core::manifest::content_hash::hash_file(&darnignore_path)?;
        let sedimentree_digest =
            darn_core::sedimentree::compute_digest(darn.subduction(), sedimentree_id)
                .await?;

        // Add to manifest
        let entry = darn_core::manifest::tracked::Tracked::new(
            sedimentree_id,
            std::path::PathBuf::from(".darnignore"),
            file_type,
            file_system_digest,
            sedimentree_digest,
        );
        manifest.track(entry);
        darn.save_manifest(&manifest)?;
    }

    cliclack::outro("Ready to sync")?;

    Ok(())
}

/// Clone a workspace by root directory ID from global peers.
///
/// 1. Parse `root_id` from base58
/// 2. Initialize workspace with that `root_id`
/// 3. Connect to all global peers
/// 4. Sync root directory sedimentree, then recursively sync and write files
#[allow(clippy::too_many_lines)]
pub(crate) async fn clone_cmd(root_id_str: &str, path: &Path) -> anyhow::Result<()> {
    cliclack::intro("darn clone")?;

    // Step 1: Parse root directory ID
    let root_id_bytes = bs58::decode(root_id_str)
        .into_vec()
        .map_err(|e| anyhow::anyhow!("invalid root directory ID (expected base58): {e}"))?;

    if root_id_bytes.len() != 32 {
        anyhow::bail!(
            "root directory ID must be 32 bytes (got {})",
            root_id_bytes.len()
        );
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&root_id_bytes);
    let root_dir_id = SedimentreeId::new(arr);

    let dim = Style::new().dim();
    cliclack::log::info(format!(
        "Root directory ID: {}",
        dim.apply_to(root_id_str)
    ))?;

    // Step 2: Check we have peers configured
    let peers = darn_core::peer::list_peers()?;
    if peers.is_empty() {
        anyhow::bail!("No peers configured. Use `darn peer add` first.");
    }
    let peer_names: Vec<_> = peers.iter().map(|p| p.name.as_str()).collect();
    cliclack::log::info(format!(
        "Using {} configured peer(s): {}",
        peers.len(),
        peer_names.join(", ")
    ))?;

    // Step 3: Initialize workspace with the provided root directory ID
    let initialized = Darn::init_with_root_id(path, root_dir_id)?;
    let root = initialized.root().to_path_buf();
    cliclack::log::success(format!("Initialized workspace at {}", root.display()))?;

    // Step 4: Open workspace with Subduction
    let darn = Darn::open(&root).await?;

    // Step 5: Connect to all peers (but don't sync yet - we'll sync specific sedimentrees)
    let spinner = cliclack::spinner();
    spinner.start("Connecting to peers...");

    let mut connected_peers = 0;
    for peer in &peers {
        match darn.connect_peer(peer).await {
            Ok((connection, peer_id)) => {
                // Register connection (without auto-syncing - we sync specific sedimentrees below)
                if let Err(e) = darn.subduction().register(connection).await {
                    info!(%e, peer = %peer.name, "Failed to register connection");
                    continue;
                }
                info!(peer = %peer.name, %peer_id, "Connected");
                connected_peers += 1;
            }
            Err(e) => {
                info!(%e, peer = %peer.name, "Connection failed");
            }
        }
    }

    if connected_peers == 0 {
        spinner.stop("Failed to connect to any peers");
        anyhow::bail!("Could not connect to any peers");
    }

    spinner.stop(format!("Connected to {connected_peers} peer(s)"));

    // Step 6: Sync and traverse directory tree, writing files
    let mut manifest = darn.load_manifest()?;
    let timeout = Some(Duration::from_secs(30));

    let progress = cliclack::progress_bar(100); // Will update as we discover files
    progress.start("Cloning files...");

    let mut total_received = 0usize;
    let mut total_sent = 0usize;

    let file_count = clone_directory_recursive_with_sync(
        darn.subduction(),
        root_dir_id,
        &root,
        std::path::PathBuf::new(),
        &mut manifest,
        timeout,
        &mut total_received,
        &mut total_sent,
        &progress,
    )
    .await?;

    if file_count == 0 {
        progress.stop("No files found");
        cliclack::outro("Clone complete (empty workspace)")?;
        return Ok(());
    }

    progress.stop(format!(
        "{file_count} file(s) cloned (▼{total_received} ▲{total_sent})"
    ));

    darn.save_manifest(&manifest)?;
    cliclack::outro("Clone complete")?;

    Ok(())
}

/// Recursively clone a directory: sync each sedimentree, then write files.
#[allow(clippy::too_many_arguments)]
async fn clone_directory_recursive_with_sync(
    subduction: &std::sync::Arc<darn_core::subduction::DarnSubduction>,
    dir_id: SedimentreeId,
    workspace_root: &Path,
    current_path: std::path::PathBuf,
    manifest: &mut darn_core::manifest::Manifest,
    timeout: Option<std::time::Duration>,
    total_received: &mut usize,
    total_sent: &mut usize,
    progress: &cliclack::ProgressBar,
) -> anyhow::Result<usize> {
    // First, sync this directory's sedimentree from peers
    let sync_result = subduction.sync_all(dir_id, true, timeout).await?;
    for (_peer_id, (success, stats, _errors)) in &sync_result {
        if *success {
            *total_received += stats.total_received();
            *total_sent += stats.total_sent();
        }
    }

    // Load directory document
    let Some(am_doc) = sedimentree::load_document(subduction, dir_id).await? else {
        info!(?dir_id, "Directory not found after sync");
        return Ok(0);
    };

    let dir = match Directory::from_automerge(&am_doc) {
        Ok(d) => d,
        Err(e) => {
            info!(?dir_id, ?e, "Skipping non-directory sedimentree");
            return Ok(0);
        }
    };

    let mut file_count = 0;

    for entry in &dir.entries {
        let entry_path = current_path.join(&entry.name);

        match entry.entry_type {
            EntryType::File => {
                // Sync this file's sedimentree
                let sync_result = subduction
                    .sync_all(entry.sedimentree_id, true, timeout)
                    .await?;
                for (_peer_id, (success, stats, _errors)) in &sync_result {
                    if *success {
                        *total_received += stats.total_received();
                        *total_sent += stats.total_sent();
                    }
                }

                // Load file document
                let Some(am_doc) =
                    sedimentree::load_document(subduction, entry.sedimentree_id).await?
                else {
                    info!(name = %entry.name, "File sedimentree empty after sync");
                    continue;
                };

                let file = match File::from_automerge(&am_doc) {
                    Ok(f) => f,
                    Err(e) => {
                        info!(name = %entry.name, ?e, "Failed to parse file");
                        continue;
                    }
                };

                // Full path on disk
                let full_path = workspace_root.join(&entry_path);

                // Create parent directories if needed
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                // Write file to disk
                file.write_to_path(&full_path)?;

                // Determine file type
                let file_type = if file.content.is_text() {
                    FileType::Text
                } else {
                    FileType::Binary
                };

                // Compute digests
                let file_system_digest = content_hash::hash_file(&full_path)?;
                let sedimentree_digest =
                    sedimentree::compute_digest(subduction, entry.sedimentree_id).await?;

                // Add to manifest
                let tracked = Tracked::new(
                    entry.sedimentree_id,
                    entry_path.clone(),
                    file_type,
                    file_system_digest,
                    sedimentree_digest,
                );
                manifest.track(tracked);

                file_count += 1;
                progress.set_message(format!("{}", entry_path.display()));
            }

            EntryType::Folder => {
                // Recurse into subdirectory (Box::pin to avoid infinitely-sized future)
                file_count += Box::pin(clone_directory_recursive_with_sync(
                    subduction,
                    entry.sedimentree_id,
                    workspace_root,
                    entry_path,
                    manifest,
                    timeout,
                    total_received,
                    total_sent,
                    progress,
                ))
                .await?;
            }
        }
    }

    Ok(file_count)
}

/// Add patterns to .darnignore.
pub(crate) fn ignore(patterns: &[String]) -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let root = darn.root();

    let mut added_count = 0;

    for pattern in patterns {
        match darn_core::ignore::add_pattern(root, pattern) {
            Ok(true) => {
                cliclack::log::success(format!("Added: {pattern}"))?;
                added_count += 1;
            }
            Ok(false) => {
                cliclack::log::remark(format!("Already ignored: {pattern}"))?;
            }
            Err(e) => {
                cliclack::log::error(format!("Failed to add {pattern}: {e}"))?;
            }
        }
    }

    if added_count > 0 {
        cliclack::log::info(format!(
            "{added_count} pattern(s) added to .darnignore"
        ))?;
    }

    Ok(())
}

/// Remove patterns from .darnignore.
pub(crate) fn unignore(patterns: &[String]) -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let root = darn.root();

    let mut removed_count = 0;

    for pattern in patterns {
        match darn_core::ignore::remove_pattern(root, pattern) {
            Ok(true) => {
                cliclack::log::success(format!("Removed: {pattern}"))?;
                removed_count += 1;
            }
            Ok(false) => {
                cliclack::log::warning(format!("Not in .darnignore: {pattern}"))?;
            }
            Err(e) => {
                cliclack::log::error(format!("Failed to remove {pattern}: {e}"))?;
            }
        }
    }

    if removed_count > 0 {
        cliclack::log::info(format!(
            "{removed_count} pattern(s) removed from .darnignore"
        ))?;
    }

    Ok(())
}

/// Show tracked files as a tree with state indicators.
pub(crate) fn tree() -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let manifest = darn.load_manifest()?;
    let root = darn.root();

    info!(root = %root.display(), "Showing tree");

    cliclack::intro(format!("Workspace: {}", root.display()))?;

    if manifest.is_empty() {
        cliclack::log::remark("No tracked files")?;
        cliclack::outro("Run darn sync to discover and track files")?;
        return Ok(());
    }

    // Collect entries with state
    let mut entries: Vec<_> = manifest.iter().map(|e| (e, e.state(root))).collect();
    entries.sort_by(|a, b| a.0.relative_path.cmp(&b.0.relative_path));

    let mut modified = 0;
    let mut missing = 0;

    // Build file list
    let mut file_list = String::new();
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let dim = Style::new().dim();

    for (entry, state) in &entries {
        let styled_indicator = match state {
            FileState::Clean => " ".to_string(),
            FileState::Modified => {
                modified += 1;
                yellow.apply_to("M").to_string()
            }
            FileState::Missing => {
                missing += 1;
                red.apply_to("!").to_string()
            }
        };
        let sed_id = bs58::encode(entry.sedimentree_id.as_bytes()).into_string();
        writeln!(
            file_list,
            "{} {}  {}",
            styled_indicator,
            entry.relative_path.display(),
            dim.apply_to(&sed_id)
        )
        .expect("write to string");
    }
    // Remove trailing newline
    file_list.pop();

    cliclack::note("Tracked files", &file_list)?;

    let total = entries.len();
    let clean = total - modified - missing;

    let mut summary = format!("{total} tracked: {clean} clean");
    if modified > 0 {
        write!(summary, ", {} {}", yellow.apply_to(modified), yellow.apply_to("modified"))
            .expect("write to string");
    }
    if missing > 0 {
        write!(summary, ", {} {}", red.apply_to(missing), red.apply_to("missing"))
            .expect("write to string");
    }

    cliclack::outro(summary)?;

    Ok(())
}

/// Show stats for a tracked file.
pub(crate) async fn stat(target: &str) -> anyhow::Result<()> {
    let darn = Darn::open(Path::new(".")).await?;
    let manifest = darn.load_manifest()?;
    let root = darn.root();

    // Try to find by path first, then by Sedimentree ID
    let tracked = if let Some(entry) = manifest.get_by_path(Path::new(target)) {
        entry
    } else if let Some(entry) = try_parse_sedimentree_id(target)
        .and_then(|id| manifest.get_by_id(&id))
    {
        entry
    } else {
        cliclack::log::error(format!("Not found: {target}"))?;
        cliclack::log::remark("Specify a tracked file path or Sedimentree ID (base58)")?;
        return Ok(());
    };

    let storage = darn.storage()?;
    let sed_id = tracked.sedimentree_id;

    // Get commit and fragment counts from storage
    let commits =
        Storage::<future_form::Sendable>::load_loose_commits(&storage, sed_id).await?;
    let fragments =
        Storage::<future_form::Sendable>::load_fragments(&storage, sed_id).await?;

    // Get file state
    let state = tracked.state(root);
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let green = Style::new().green();

    let state_styled = match state {
        FileState::Clean => green.apply_to("clean").to_string(),
        FileState::Modified => yellow.apply_to("modified").to_string(),
        FileState::Missing => red.apply_to("missing").to_string(),
    };

    let file_type_str = match tracked.file_type {
        FileType::Text => "text",
        FileType::Binary => "binary",
    };

    cliclack::intro(format!("{}", tracked.relative_path.display()))?;

    // Build stats content
    let dim = Style::new().dim();
    let sed_id_str = bs58::encode(sed_id.as_bytes()).into_string();
    let fs_digest = bs58::encode(tracked.file_system_digest.as_bytes()).into_string();
    let sed_digest = bs58::encode(tracked.sedimentree_digest.as_bytes()).into_string();

    let content = format!(
        "Sedimentree:  {}\n\
         State:        {}\n\
         Type:         {}\n\
         \n\
         Storage:\n\
         Commits:      {}\n\
         Fragments:    {}\n\
         \n\
         Digests:\n\
         File:         {}\n\
         Sedimentree:  {}",
        dim.apply_to(&sed_id_str),
        state_styled,
        file_type_str,
        commits.len(),
        fragments.len(),
        dim.apply_to(&fs_digest),
        dim.apply_to(&sed_digest)
    );

    cliclack::note("Stats", &content)?;
    cliclack::outro("")?;

    Ok(())
}

fn try_parse_sedimentree_id(s: &str) -> Option<SedimentreeId> {
    let bytes = bs58::decode(s).into_vec().ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(SedimentreeId::new(arr))
}

/// Sync with peers.
///
/// First refreshes all modified local files (commits local changes),
/// then syncs with the specified peer or all peers.
///
/// If `dry_run` is true, shows what would happen without actually syncing.
/// If `force` is true, skips confirmation for new file discovery.
pub(crate) async fn sync_cmd(
    peer_name: Option<&str>,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<()> {
    info!(?peer_name, dry_run, force, "Syncing");

    if dry_run {
        return sync_dry_run(peer_name);
    }

    cliclack::intro("darn sync")?;

    let darn = Darn::open(Path::new(".")).await?;
    let mut manifest = darn.load_manifest()?;

    // Step 1: Discover new files (auto-track non-ignored files)
    let spinner = cliclack::spinner();
    spinner.start("Scanning for new files...");
    match darn.discover_new_files(&mut manifest).await {
        Ok(new_files) => {
            spinner.clear();
            if !new_files.is_empty() {
                // Show what was found as a tree
                let tree = format_paths_as_tree(&new_files);
                cliclack::note(format!("Found {} new file(s)", new_files.len()), &tree)?;

                // Confirm unless --force
                let should_track = if force {
                    true
                } else {
                    cliclack::confirm("Track these files?")
                        .initial_value(true)
                        .interact()?
                };

                if should_track {
                    darn.save_manifest(&manifest)?;
                    cliclack::log::success(format!("Tracking {} new file(s)", new_files.len()))?;
                    for path in &new_files {
                        info!(path = %path.display(), "Discovered file");
                    }
                } else {
                    // Reload manifest to discard changes
                    let manifest_reloaded = darn.load_manifest()?;
                    cliclack::log::remark("Skipped new files. Use 'darn ignore <pattern>' to ignore them.")?;
                    // Use the reloaded manifest for the rest of the sync
                    return continue_sync(darn, manifest_reloaded, peer_name).await;
                }
            }
        }
        Err(e) => {
            spinner.clear();
            cliclack::log::warning(format!("File discovery error: {e}"))?;
        }
    }

    continue_sync(darn, manifest, peer_name).await
}

/// Continue sync after file discovery.
#[allow(clippy::too_many_lines)]
async fn continue_sync(
    darn: Darn,
    mut manifest: Manifest,
    peer_name: Option<&str>,
) -> anyhow::Result<()> {

    // Refresh all modified files (commit local changes)
    let spinner = cliclack::spinner();
    spinner.start("Checking for local changes...");
    let result = darn.refresh_all(&mut manifest).await;
    spinner.clear();

    if !result.updated.is_empty() {
        darn.save_manifest(&manifest)?;
        cliclack::log::success(format!("Committed {} local change(s)", result.updated.len()))?;
        for path in &result.updated {
            info!(path = %path.display(), "Refreshed file");
        }
    }

    if !result.missing.is_empty() {
        cliclack::log::warning(format!("{} file(s) missing from disk", result.missing.len()))?;
    }

    if !result.errors.is_empty() {
        for (path, err) in &result.errors {
            cliclack::log::error(format!("Error refreshing {}: {err}", path.display()))?;
        }
    }

    // Step 2: Get peers to connect
    let unopened = Darn::open_without_subduction(Path::new("."))?;
    let mut peers = match peer_name {
        Some(name) => {
            let peer_name = PeerName::new(name)?;
            let p = unopened
                .get_peer(&peer_name)?
                .ok_or_else(|| anyhow::anyhow!("peer not found: {name}"))?;
            vec![p]
        }
        None => unopened.list_peers()?,
    };

    if peers.is_empty() {
        cliclack::log::warning("No peers configured")?;
        cliclack::outro(format!("Use {} to add peers", cmd("darn peer add <name> <url>")))?;
        return Ok(());
    }

    // Collect current sedimentree digests for sync tracking
    let current_digests: Vec<_> = manifest
        .iter()
        .map(|e| (e.sedimentree_id, e.sedimentree_digest))
        .collect();

    // Step 3: Connect and sync with each peer (with progress bars)
    let mut sync_success = false;
    let green = Style::new().green();
    let red = Style::new().red();

    for peer in &mut peers {
        let was_discovery = peer.is_discovery();

        match sync_peer_with_progress(&darn, peer, &manifest).await {
            Ok(summary) => {
                if summary.any_success() {
                    sync_success = true;
                    cliclack::log::success(format!(
                        "{} synced {} files (▼{} ▲{})",
                        green.apply_to(&peer.name),
                        summary.sedimentrees_synced,
                        summary.total_received(),
                        summary.total_sent()
                    ))?;

                    // If we connected via discovery mode, update to known mode with learned peer ID
                    if was_discovery && let Some(learned_peer_id) = summary.peer_id {
                        peer.set_known(learned_peer_id);
                        let id_str = bs58::encode(learned_peer_id.as_bytes()).into_string();
                        cliclack::log::info(format!(
                            "Learned peer ID for {}: {}",
                            peer.name,
                            Style::new().dim().apply_to(&id_str)
                        ))?;
                    }

                    // Record sync state for this peer
                    peer.record_sync(current_digests.iter().copied());
                    unopened.add_peer(peer)?;
                } else {
                    cliclack::log::warning(format!(
                        "{} no data exchanged",
                        peer.name
                    ))?;
                }

                if summary.has_errors() {
                    cliclack::log::warning(format!(
                        "{} error(s) during sync",
                        summary.errors.len()
                    ))?;
                }
            }
            Err(e) => {
                cliclack::log::error(format!("{} {e}", red.apply_to(&peer.name)))?;
            }
        }
    }

    if sync_success {
        // Apply remote changes to local files
        let spinner = cliclack::spinner();
        spinner.start("Applying remote changes...");

        let apply_result = darn.apply_remote_changes(&mut manifest).await;
        spinner.clear();

        // Report results
        if !apply_result.updated.is_empty() {
            cliclack::log::success(format!(
                "{} file(s) updated from remote",
                apply_result.updated.len()
            ))?;
        }

        if !apply_result.merged.is_empty() {
            cliclack::log::info(format!(
                "{} file(s) merged (concurrent changes)",
                apply_result.merged.len()
            ))?;
        }

        if !apply_result.created.is_empty() {
            cliclack::log::success(format!(
                "{} new file(s) from remote",
                apply_result.created.len()
            ))?;
            for path in &apply_result.created {
                cliclack::log::remark(format!("  + {}", path.display()))?;
            }
        }

        if !apply_result.deleted.is_empty() {
            cliclack::log::info(format!(
                "{} file(s) deleted (removed from remote)",
                apply_result.deleted.len()
            ))?;
            for path in &apply_result.deleted {
                cliclack::log::remark(format!("  - {}", path.display()))?;
            }
        }

        if apply_result.has_errors() {
            cliclack::log::warning(format!(
                "{} error(s) applying remote changes",
                apply_result.errors.len()
            ))?;
            for (path, err) in &apply_result.errors {
                cliclack::log::remark(format!("  ! {}: {err}", path.display()))?;
            }
        }

        darn.save_manifest(&manifest)?;
        cliclack::outro("Sync complete")?;
    } else {
        cliclack::outro("Sync failed")?;
    }

    Ok(())
}

/// Sync with a peer, displaying progress via cliclack progress bar.
async fn sync_peer_with_progress(
    darn: &Darn,
    peer: &Peer,
    manifest: &Manifest,
) -> anyhow::Result<darn_core::sync_progress::SyncSummary> {
    let progress_bar = cliclack::progress_bar(1);
    let current = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(1));

    progress_bar.start(format!("Connecting to {}...", peer.name));

    let pb = &progress_bar;
    let current_ref = &current;
    let total_ref = &total;

    let summary = darn
        .sync_with_peer_progress(peer, manifest, |event| {
            match event {
                SyncProgressEvent::ConnectingToPeer { peer_name, .. } => {
                    pb.set_message(format!("Connecting to {peer_name}..."));
                }
                SyncProgressEvent::Connected { .. } => {
                    pb.set_message("Connected, starting sync...");
                }
                SyncProgressEvent::StartingSync { total_sedimentrees } => {
                    total_ref.store(total_sedimentrees, Ordering::SeqCst);
                    pb.set_length(total_sedimentrees.try_into().unwrap_or(u64::MAX));
                    pb.set_message(format!("Syncing {total_sedimentrees} items..."));
                }
                SyncProgressEvent::SedimentreeStarted { file_path, index, total, .. } => {
                    let display_index = index + 1;
                    let msg = match &file_path {
                        Some(path) => format!("[{display_index}/{total}] {}", path.display()),
                        None => format!("[{display_index}/{total}] root directory"),
                    };
                    pb.set_message(msg);
                }
                SyncProgressEvent::SedimentreeCompleted { index, .. } => {
                    current_ref.store(index + 1, Ordering::SeqCst);
                    pb.inc(1);
                }
                SyncProgressEvent::Completed(_) => {
                    // Progress bar will be stopped after this
                }
            }
        })
        .await?;

    progress_bar.stop(format!(
        "Synced with {} (▼{} ▲{})",
        peer.name,
        summary.total_received(),
        summary.total_sent()
    ));

    Ok(summary)
}

/// Dry-run mode: show what would be synced without actually doing it.
fn sync_dry_run(peer_name: Option<&str>) -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let manifest = darn.load_manifest()?;
    let root = darn.root();

    cliclack::intro("Sync dry run")?;

    // Check local changes
    let mut modified = Vec::new();
    let mut missing = Vec::new();

    for entry in manifest.iter() {
        match entry.state(root) {
            FileState::Clean => {}
            FileState::Modified => modified.push(entry.relative_path.clone()),
            FileState::Missing => missing.push(entry.relative_path.clone()),
        }
    }

    let total = manifest.iter().count();

    // Report uncommitted changes
    if !modified.is_empty() || !missing.is_empty() {
        let mut changes = String::new();
        for path in &modified {
            writeln!(changes, "M  {}", path.display()).expect("write to string");
        }
        for path in &missing {
            writeln!(changes, "!  {} (missing)", path.display()).expect("write to string");
        }
        // Remove trailing newline
        changes.pop();

        cliclack::note("Uncommitted changes", &changes)?;
        cliclack::log::info(format!(
            "Would commit {} modified file(s) before syncing",
            modified.len()
        ))?;
    }

    // Check peers
    let peers = match peer_name {
        Some(name) => {
            let peer_name = PeerName::new(name)?;
            if let Some(p) = darn.get_peer(&peer_name)? {
                vec![p]
            } else {
                cliclack::log::error(format!("Peer not found: {name}"))?;
                cliclack::outro("Dry run aborted")?;
                return Ok(());
            }
        }
        None => darn.list_peers()?,
    };

    if peers.is_empty() {
        cliclack::log::warning("No peers configured")?;
        cliclack::outro(format!("Use {} to add peers", cmd("darn peer add <name> <url>")))?;
        return Ok(());
    }

    // Show sync status per peer
    for peer in &peers {
        let peer_id_display = if let Some(id) = peer.peer_id() {
            bs58::encode(id.as_bytes()).into_string()
        } else {
            "(TOFU)".to_string()
        };
        let last_sync = peer
            .last_synced_at
            .map_or_else(|| "never".to_string(), format_timestamp);

        // Count unsynced files for this peer
        let mut unsynced = Vec::new();
        for entry in manifest.iter() {
            if !peer.is_synced(&entry.sedimentree_id, &entry.sedimentree_digest) {
                unsynced.push(&entry.relative_path);
            }
        }

        // Build peer status content
        let mut content = format!("URL:       {}\n", peer.url);
        writeln!(content, "Peer ID:   {peer_id_display}").expect("write to string");
        writeln!(content, "Last sync: {last_sync}").expect("write to string");

        if unsynced.is_empty() {
            write!(content, "Status:    all {total} file(s) synced ✓").expect("write to string");
        } else if unsynced.len() == total {
            let count = unsynced.len();
            write!(content, "Status:    {count} file(s) never synced").expect("write to string");
        } else {
            let count = unsynced.len();
            writeln!(content, "Status:    {count} of {total} file(s) unsynced").expect("write to string");
            for path in unsynced.iter().take(5) {
                writeln!(content, "           - {}", path.display()).expect("write to string");
            }
            if unsynced.len() > 5 {
                let remaining = unsynced.len() - 5;
                write!(content, "           ... and {remaining} more").expect("write to string");
            } else {
                // Remove trailing newline from last path
                content.pop();
            }
        }

        cliclack::note(peer.name.as_str(), &content)?;
    }

    cliclack::outro(format!("Run {} to sync", cmd("darn sync")))?;
    Ok(())
}

/// Format a duration for display.
fn format_duration(d: &std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        let ms = d.as_millis();
        format!("{ms}ms")
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Format a timestamp for display.
fn format_timestamp(ts: darn_core::unix_timestamp::UnixTimestamp) -> String {
    let time = UNIX_EPOCH + Duration::from_secs(ts.as_secs());
    let now = SystemTime::now();

    match now.duration_since(time) {
        Ok(elapsed) => {
            let secs = elapsed.as_secs();
            if secs < 60 {
                format!("{secs}s ago")
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86400)
            }
        }
        Err(_) => "in the future".to_string(),
    }
}

/// Watch for file changes and auto-sync.
///
/// The watcher monitors the workspace for:
/// - New files: Auto-tracked unless `no_track` is true
/// - Modified tracked files: Auto-refreshed to CRDT storage
/// - Optionally syncs with peers at the specified interval
#[allow(clippy::too_many_lines)]
pub(crate) async fn watch(sync_interval: &std::time::Duration, no_track: bool) -> anyhow::Result<()> {
    let darn = Darn::open(Path::new(".")).await?;
    let root = darn.root().to_path_buf();
    let mut manifest = darn.load_manifest()?;

    info!(root = %root.display(), ?sync_interval, no_track, "Starting watch");

    cliclack::intro("darn watch")?;
    cliclack::log::info(format!("Watching {}", root.display()))?;

    if !no_track {
        cliclack::log::warning("New files will be auto-tracked and synced (use --no-track to disable)")?;
    }

    if sync_interval.is_zero() {
        cliclack::log::remark("Sync on change (immediate)")?;
    } else {
        cliclack::log::remark(format!("Sync interval: {}", format_duration(sync_interval)))?;
    }

    if no_track {
        cliclack::log::remark("Auto-track disabled")?;
    }

    // Create watcher and event processor
    let config = WatcherConfig {
        auto_track: !no_track,
        ..WatcherConfig::default()
    };

    let (mut watcher, mut rx) = Watcher::new(&root, config)?;
    let mut processor = WatchEventProcessor::new(&root, &manifest)?;

    watcher.start()?;
    cliclack::log::success("Watcher started")?;
    cliclack::log::remark("Press Ctrl+C to stop")?;
    println!(); // Blank line before events

    // Set up sync interval timer
    // sync_interval of 0 means "sync immediately after local changes, no polling"
    // sync_interval > 0 means "sync every N seconds (polling for remote changes too)"
    let immediate_sync = sync_interval.is_zero();
    let sync_interval_duration = if immediate_sync {
        Duration::from_secs(60 * 60) // 1 hour fallback for periodic remote checks
    } else {
        *sync_interval
    };
    let mut last_sync = std::time::Instant::now();
    let mut has_local_changes = false;

    // Check for incoming push data every 1 second (regardless of polling interval)
    // This is a fast local operation - just checks if digests differ
    // Push updates arrive via WebSocket and are stored immediately, but we need to
    // periodically apply them to disk
    let push_check_interval = Duration::from_secs(1);
    let mut last_push_check = std::time::Instant::now();

    // Get peers for syncing
    let unopened = Darn::open_without_subduction(Path::new("."))?;
    let peers = unopened.list_peers()?;
    let has_peers = !peers.is_empty();

    if !has_peers {
        cliclack::log::warning("No peers configured - sync disabled")?;
    }

    // Initial sync to establish connections and subscriptions (subscribe: true)
    // This ensures WebSocket listeners are running to receive push updates
    if has_peers {
        let spinner = cliclack::spinner();
        spinner.start("Establishing peer connections...");

        let mut connected = 0;
        for peer in &peers {
            match darn.sync_with_peer(peer).await {
                Ok(result) if result.success => {
                    connected += 1;
                    // Also sync any missing sedimentrees
                    if let Some(peer_id) = peer.peer_id() {
                        drop(darn.sync_missing_sedimentrees(&manifest, &peer_id).await);
                    }
                }
                Ok(_) => {
                    info!(peer = %peer.name, "Initial sync incomplete");
                }
                Err(e) => {
                    info!(%e, peer = %peer.name, "Initial sync failed");
                }
            }
        }

        // Apply any remote changes from initial sync
        let apply_result = darn.apply_remote_changes(&mut manifest).await;
        darn.save_manifest(&manifest)?;
        processor.update_tracked_paths(&manifest);

        let total_applied = apply_result.updated.len()
            + apply_result.merged.len()
            + apply_result.created.len()
            + apply_result.deleted.len();

        if total_applied > 0 {
            spinner.stop(format!(
                "Connected to {connected}/{} peers, applied {total_applied} changes",
                peers.len()
            ));
        } else {
            spinner.stop(format!("Connected to {connected}/{} peers", peers.len()));
        }

        last_sync = std::time::Instant::now();
        last_push_check = std::time::Instant::now();
        println!();
    }

    // Styles
    let green = Style::new().green();
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let dim = Style::new().dim();

    // Event loop
    loop {
        // Use a short timeout to check for sync interval
        let timeout = Duration::from_millis(500);

        tokio::select! {
            // Receive watch events
            event = rx.recv() => {
                match event {
                    Some(WatchEvent::FileModified(path)) => {
                        if processor.process(WatchEvent::FileModified(path.clone())) {
                            let is_new = manifest.get_by_path(&path).is_none();
                            if is_new {
                                println!("  {} {}", green.apply_to("+"), path.display());
                            } else {
                                println!("  {} {}", yellow.apply_to("M"), path.display());
                            }
                        }
                    }
                    Some(WatchEvent::FileDeleted(path)) => {
                        if processor.process(WatchEvent::FileDeleted(path.clone())) {
                            println!("  {} {}", red.apply_to("-"), path.display());
                        }
                    }
                    Some(WatchEvent::FileCreated(path)) => {
                        if processor.process(WatchEvent::FileCreated(path.clone())) {
                            println!("  {} {}", green.apply_to("+"), path.display());
                        }
                    }
                    Some(WatchEvent::FileRenamed { from, to }) => {
                        if processor.process(WatchEvent::FileRenamed { from: from.clone(), to: to.clone() }) {
                            println!("  {} {} -> {}", dim.apply_to("R"), from.display(), to.display());
                        }
                    }
                    Some(WatchEvent::Error(e)) => {
                        cliclack::log::error(format!("Watch error: {e}"))?;
                    }
                    Some(WatchEvent::BatchReady(_)) => {
                        // Handled below in batch processing
                    }
                    None => {
                        // Channel closed, watcher stopped
                        break;
                    }
                }
            }

            // Check for Ctrl+C
            _ = tokio::signal::ctrl_c() => {
                println!();
                cliclack::log::info("Stopping...")?;
                break;
            }

            // Timeout for periodic batch processing
            () = tokio::time::sleep(timeout) => {
                // Process batch if we have pending events and enough time has passed
                if processor.has_pending() {
                    let batch = processor.flush();

                    // Track new files
                    if !batch.created.is_empty() && !no_track {
                        for path in &batch.created {
                            match track_single_file(&darn, &mut manifest, path).await {
                                Ok(()) => {
                                    info!(path = %path.display(), "Auto-tracked file");
                                }
                                Err(e) => {
                                    cliclack::log::warning(format!(
                                        "Failed to track {}: {e}",
                                        path.display()
                                    ))?;
                                }
                            }
                        }
                        processor.update_tracked_paths(&manifest);
                    }

                    // Refresh modified files
                    for path in &batch.modified {
                        if let Some(entry) = manifest.get_by_path_mut(path) {
                            match darn.refresh_file(entry).await {
                                Ok(true) => {
                                    info!(path = %path.display(), "Refreshed file");
                                }
                                Ok(false) => {
                                    // No changes needed
                                }
                                Err(e) => {
                                    cliclack::log::warning(format!(
                                        "Failed to refresh {}: {e}",
                                        path.display()
                                    ))?;
                                }
                            }
                        }
                    }

                    // Handle deleted files (just mark them for now)
                    for path in &batch.deleted {
                        info!(path = %path.display(), "File deleted (still tracked)");
                    }

                    // Save manifest if we made changes
                    if !batch.created.is_empty() || !batch.modified.is_empty() {
                        darn.save_manifest(&manifest)?;
                        has_local_changes = true;
                    }
                }

                // Check if we should sync:
                // - immediate_sync mode: sync when we have local changes
                // - interval mode: sync when interval elapsed (polls for remote changes too)
                let should_sync = has_peers && (
                    (immediate_sync && has_local_changes) ||
                    (!immediate_sync && last_sync.elapsed() >= sync_interval_duration)
                );

                if should_sync {
                        println!();
                        let spinner = cliclack::spinner();
                        spinner.start("Syncing with peers...");

                        let mut sync_ok = false;
                        let mut any_received = false;
                        let mut any_sent = false;
                        for peer in &peers {
                            match darn.sync_with_peer(peer).await {
                                Ok(result) => {
                                    if result.success {
                                        sync_ok = true;
                                        if result.stats.total_received() > 0 {
                                            any_received = true;
                                        }
                                        if result.stats.total_sent() > 0 {
                                            any_sent = true;
                                        }
                                    }
                                    // Sync any new sedimentrees discovered in the directory tree
                                    if let Some(peer_id) = peer.peer_id() {
                                        if let Ok(new_count) = darn.sync_missing_sedimentrees(&manifest, &peer_id).await {
                                            if new_count > 0 {
                                                any_received = true;
                                                info!(new_count, peer = %peer.name, "Synced missing sedimentrees");
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    info!(%e, peer = %peer.name, "Sync failed");
                                }
                            }
                        }

                        if sync_ok {
                            // Apply remote changes to local files
                            let apply_result = darn.apply_remote_changes(&mut manifest).await;
                            darn.save_manifest(&manifest)?;
                            processor.update_tracked_paths(&manifest);

                            let mut summary = String::new();
                            if !apply_result.updated.is_empty() {
                                summary.push_str(&format!("{} updated, ", apply_result.updated.len()));
                                for path in &apply_result.updated {
                                    println!("  {} {}", yellow.apply_to("U"), path.display());
                                }
                            }
                            if !apply_result.merged.is_empty() {
                                summary.push_str(&format!("{} merged, ", apply_result.merged.len()));
                                for path in &apply_result.merged {
                                    println!("  {} {}", yellow.apply_to("M"), path.display());
                                }
                            }
                            if !apply_result.created.is_empty() {
                                summary.push_str(&format!("{} new, ", apply_result.created.len()));
                                for path in &apply_result.created {
                                    println!("  {} {}", green.apply_to("+"), path.display());
                                }
                            }
                            if !apply_result.deleted.is_empty() {
                                summary.push_str(&format!("{} deleted, ", apply_result.deleted.len()));
                                for path in &apply_result.deleted {
                                    println!("  {} {}", red.apply_to("-"), path.display());
                                }
                            }
                            if summary.is_empty() {
                                match (any_received, any_sent) {
                                    (true, true) => summary = "synced".to_string(),
                                    (true, false) => summary = "received updates".to_string(),
                                    (false, true) => summary = "sent updates".to_string(),
                                    (false, false) => summary = "no changes".to_string(),
                                }
                            } else {
                                summary = summary.trim_end_matches(", ").to_string();
                            }

                            spinner.stop(format!("Synced ({summary})"));
                        } else {
                            spinner.stop("Sync complete");
                        }

                        last_sync = std::time::Instant::now();
                        last_push_check = std::time::Instant::now();
                        has_local_changes = false;
                        println!();
                }

                // Periodically check if any push data arrived (regardless of polling interval)
                // This is separate from the sync above - it just checks local state
                // that may have been updated by background WebSocket listeners
                if has_peers && last_push_check.elapsed() >= push_check_interval {
                    // apply_remote_changes() is a fast local operation:
                    // - Compares sedimentree digests (local vs stored)
                    // - If they differ, push data arrived - write files to disk
                    // - No network round-trip needed
                    let apply_result = darn.apply_remote_changes(&mut manifest).await;

                    let total_changes = apply_result.updated.len()
                        + apply_result.merged.len()
                        + apply_result.created.len()
                        + apply_result.deleted.len();

                    if total_changes > 0 {
                        darn.save_manifest(&manifest)?;
                        processor.update_tracked_paths(&manifest);

                        // Display what changed
                        for path in &apply_result.updated {
                            println!("  {} {}", yellow.apply_to("U"), path.display());
                        }
                        for path in &apply_result.merged {
                            println!("  {} {}", yellow.apply_to("M"), path.display());
                        }
                        for path in &apply_result.created {
                            println!("  {} {}", green.apply_to("+"), path.display());
                        }
                        for path in &apply_result.deleted {
                            println!("  {} {}", red.apply_to("-"), path.display());
                        }

                        info!(
                            updated = apply_result.updated.len(),
                            merged = apply_result.merged.len(),
                            created = apply_result.created.len(),
                            deleted = apply_result.deleted.len(),
                            "Applied push updates"
                        );
                    }

                    last_push_check = std::time::Instant::now();
                }
            }
        }
    }

    watcher.stop();
    cliclack::outro("Watch stopped")?;

    Ok(())
}

/// Track a single file (helper for watch command).
async fn track_single_file(
    darn: &Darn,
    manifest: &mut Manifest,
    relative_path: &Path,
) -> anyhow::Result<()> {
    let full_path = darn.root().join(relative_path);

    // Create File from path
    let doc = File::from_path(&full_path)?;
    let file_type = if doc.content.is_text() {
        FileType::Text
    } else {
        FileType::Binary
    };

    // Convert to Automerge
    let mut am_doc = doc.into_automerge()?;

    // Generate random SedimentreeId
    let mut id_bytes = [0u8; 32];
    getrandom::getrandom(&mut id_bytes)?;
    let sedimentree_id = sedimentree_core::id::SedimentreeId::new(id_bytes);

    // Store as sedimentree commits
    sedimentree::store_document(darn.subduction(), sedimentree_id, &mut am_doc).await?;

    // Add file to directory tree
    let root_dir_id = manifest.root_directory_id();
    let parent_dir_id = sedimentree::ensure_parent_directories(
        darn.subduction(),
        root_dir_id,
        relative_path,
    )
    .await?;

    let file_name = relative_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path has no filename"))?
        .to_string_lossy();

    sedimentree::add_file_to_directory(
        darn.subduction(),
        parent_dir_id,
        &file_name,
        sedimentree_id,
    )
    .await?;

    // Compute digests
    let file_system_digest = content_hash::hash_file(&full_path)?;
    let sedimentree_digest =
        sedimentree::compute_digest(darn.subduction(), sedimentree_id).await?;

    // Add to manifest
    let entry = Tracked::new(
        sedimentree_id,
        relative_path.to_path_buf(),
        file_type,
        file_system_digest,
        sedimentree_digest,
    );
    manifest.track(entry);

    Ok(())
}

/// Add a peer.
pub(crate) fn peer_add(name: &str, url: &str, peer_id: Option<&str>) -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;

    // Validate peer name
    let peer_name = PeerName::new(name)?;

    // Check if peer already exists
    if darn.get_peer(&peer_name)?.is_some() {
        cliclack::log::error(format!("Peer already exists: {name}"))?;
        return Ok(());
    }

    let peer = if let Some(id_str) = peer_id {
        // Parse peer ID from base58
        let id_bytes = bs58::decode(id_str)
            .into_vec()
            .map_err(|e| anyhow::anyhow!("invalid peer ID (expected base58): {e}"))?;

        if id_bytes.len() != 32 {
            anyhow::bail!("peer ID must be 32 bytes (got {})", id_bytes.len());
        }

        let mut arr = [0u8; 32];
        arr.copy_from_slice(&id_bytes);
        let peer_id = PeerId::new(arr);

        Peer::known(peer_name, url.to_string(), peer_id)
    } else {
        // Discovery mode: service name derived from URL (strip ws:// or wss://)
        Peer::discover(peer_name, url.to_string())
    };

    let peer_id_display = if let Some(id) = peer.peer_id() {
        bs58::encode(id.as_bytes()).into_string()
    } else {
        "[TOFU]".to_string()
    };

    darn.add_peer(&peer)?;

    info!(%name, %url, "Added peer");
    cliclack::log::success(format!("Added peer: {name} ({url})"))?;
    cliclack::log::remark(format!("Peer ID: {peer_id_display}"))?;

    Ok(())
}

/// List known peers.
pub(crate) fn peer_list() -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let peers = darn.list_peers()?;

    info!("Listing peers");

    cliclack::intro("Peers")?;

    if peers.is_empty() {
        cliclack::log::remark("No peers configured")?;
        cliclack::outro(format!("Use {} to add peers", cmd("darn peer add <name> <url>")))?;
        return Ok(());
    }

    let dim = Style::new().dim();

    // Build peer list content
    for peer in &peers {
        let peer_id_display = if let Some(id) = peer.peer_id() {
            let id_str = bs58::encode(id.as_bytes()).into_string();
            dim.apply_to(&id_str).to_string()
        } else {
            "(TOFU)".to_string()
        };
        let last_sync = peer
            .last_synced_at
            .map_or_else(|| "never".to_string(), format_timestamp);

        let mut content = format!("URL:       {}\n", peer.url);
        content.push_str(&format!("Peer ID:   {peer_id_display}\n"));
        content.push_str(&format!("Last sync: {last_sync}"));

        cliclack::note(peer.name.as_str(), &content)?;
    }

    cliclack::outro(format!("{} peer(s)", peers.len()))?;

    Ok(())
}

/// Remove a peer.
pub(crate) fn peer_remove(name: &str) -> anyhow::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let peer_name = PeerName::new(name)?;

    if darn.remove_peer(&peer_name)? {
        info!(%name, "Removed peer");
        cliclack::log::success(format!("Removed peer: {name}"))?;
    } else {
        cliclack::log::warning(format!("Peer not found: {name}"))?;
    }

    Ok(())
}

/// Show info about global config and current workspace.
pub(crate) fn info() -> anyhow::Result<()> {
    let dim = Style::new().dim();
    let bold = Style::new().bold();

    cliclack::intro("darn info")?;

    // ═══════════════════════════════════════════════════════════════════════
    // Global Configuration
    // ═══════════════════════════════════════════════════════════════════════

    let config_dir = darn_core::config::global_config_dir()?;
    let signer_dir = darn_core::config::global_signer_dir()?;

    // Get peer ID
    let peer_id_str = match darn_core::signer::peer_id(&signer_dir) {
        Ok(peer_id) => bs58::encode(peer_id.as_bytes()).into_string(),
        Err(e) => format!("(error: {e})"),
    };

    // Build global config table
    println!();
    println!("  {}", bold.apply_to("Global Configuration"));
    println!("  ┌─────────────┬────────────────────────────────────────────────┐");
    println!("  │ {:^11} │ {:^46} │", "Field", "Value");
    println!("  ├─────────────┼────────────────────────────────────────────────┤");
    println!(
        "  │ {:<11} │ {:<46} │",
        "Config",
        truncate_path(&config_dir.display().to_string(), 46)
    );
    println!(
        "  │ {:<11} │ {:<46} │",
        "Peer ID",
        dim.apply_to(&peer_id_str)
    );
    println!("  └─────────────┴────────────────────────────────────────────────┘");

    // ═══════════════════════════════════════════════════════════════════════
    // Peers
    // ═══════════════════════════════════════════════════════════════════════

    println!();
    println!("  {}", bold.apply_to("Configured Peers"));

    match darn_core::peer::list_peers() {
        Ok(peers) if peers.is_empty() => {
            println!("  {}", dim.apply_to("(no peers configured)"));
        }
        Ok(peers) => {
            println!("  ┌────────────────┬────────────────────────────────────────┬──────────┐");
            println!("  │ {:^14} │ {:^38} │ {:^8} │", "Name", "URL", "Mode");
            println!("  ├────────────────┼────────────────────────────────────────┼──────────┤");
            for peer in &peers {
                let mode = if peer.is_known() { "known" } else { "discover" };
                println!(
                    "  │ {:<14} │ {:<38} │ {:^8} │",
                    truncate_str(&peer.name.to_string(), 14),
                    truncate_str(&peer.url, 38),
                    mode
                );
            }
            println!("  └────────────────┴────────────────────────────────────────┴──────────┘");
        }
        Err(e) => {
            println!("  {}", dim.apply_to(format!("(error listing peers: {e})")));
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Workspace
    // ═══════════════════════════════════════════════════════════════════════

    println!();
    println!("  {}", bold.apply_to("Workspace"));

    match Darn::open_without_subduction(Path::new(".")) {
        Ok(darn) => {
            let manifest = darn.load_manifest();
            let root_id_str = manifest
                .as_ref()
                .map(|m| bs58::encode(m.root_directory_id().as_bytes()).into_string())
                .unwrap_or_else(|_| "(error)".to_string());
            let file_count = manifest.as_ref().map(|m| m.len()).unwrap_or(0);

            println!("  ┌─────────────┬────────────────────────────────────────────────┐");
            println!("  │ {:^11} │ {:^46} │", "Field", "Value");
            println!("  ├─────────────┼────────────────────────────────────────────────┤");
            println!(
                "  │ {:<11} │ {:<46} │",
                "Root",
                truncate_path(&darn.root().display().to_string(), 46)
            );
            println!(
                "  │ {:<11} │ {:<46} │",
                "Root Dir ID",
                dim.apply_to(&root_id_str)
            );
            println!(
                "  │ {:<11} │ {:<46} │",
                "Files",
                format!("{file_count} tracked")
            );
            println!("  └─────────────┴────────────────────────────────────────────────┘");

            // Show tracked files if any
            if let Ok(manifest) = manifest {
                if !manifest.is_empty() {
                    println!();
                    println!("  {}", bold.apply_to("Tracked Files"));
                    println!("  ┌──────────────────────────────────────────┬────────┬─────────────────────┐");
                    println!("  │ {:^40} │ {:^6} │ {:^19} │", "Path", "Type", "State");
                    println!("  ├──────────────────────────────────────────┼────────┼─────────────────────┤");

                    for entry in manifest.iter() {
                        let state = entry.state(darn.root());
                        let state_str = match state {
                            FileState::Clean => "clean",
                            FileState::Modified => "modified",
                            FileState::Missing => "missing",
                        };
                        let type_str = if entry.file_type.is_text() {
                            "text"
                        } else {
                            "binary"
                        };
                        println!(
                            "  │ {:<40} │ {:^6} │ {:^19} │",
                            truncate_str(&entry.relative_path.display().to_string(), 40),
                            type_str,
                            state_str
                        );
                    }
                    println!("  └──────────────────────────────────────────┴────────┴─────────────────────┘");
                }
            }
        }
        Err(_) => {
            println!("  {}", dim.apply_to("(not in a darn workspace)"));
        }
    }

    println!();
    cliclack::outro("")?;

    Ok(())
}

/// Truncate a string to fit within a given width, adding "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s[..max_len].to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Truncate a path string, preferring to show the end (filename) if truncated.
fn truncate_path(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 6 {
        s[..max_len].to_string()
    } else {
        format!("...{}", &s[s.len() - (max_len - 3)..])
    }
}
