//! CLI command implementations.

// CLI-specific lint allows for this module
#![allow(clippy::format_push_string)]
#![allow(clippy::large_futures)]

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use console::Style;
use darn_core::{
    darn::Darn,
    directory::{Directory, entry::EntryType, sedimentree_id_to_url},
    discover::{DiscoverProgress, DiscoverResult},
    file::{File, file_type::FileType, state::FileState},
    manifest::{Manifest, content_hash, tracked::Tracked},
    peer::{Peer, PeerName},
    sedimentree,
    sync_progress::SyncProgressEvent,
    watcher::{WatchEvent, WatchEventProcessor, Watcher, WatcherConfig},
};
use sedimentree_core::id::SedimentreeId;
use subduction_core::{peer::id::PeerId, storage::traits::Storage};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::output::Output;

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
pub(crate) async fn init(path: &Path, out: &Output) -> eyre::Result<()> {
    out.intro("darn init")?;

    // Initialize workspace structure
    let initialized = Darn::init(path)?;
    let root = initialized.root().to_path_buf();

    out.success(&format!("Initialized workspace at {}", root.display()))?;

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

        // Generate random SedimentreeId (16-byte for automerge-repo compatibility)
        let sedimentree_id = darn_core::generate_sedimentree_id();

        // Store as sedimentree commits
        darn_core::sedimentree::store_document(darn.subduction(), sedimentree_id, &mut am_doc)
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
        let file_system_digest = darn_core::manifest::content_hash::hash_file(&darnignore_path)?;
        let sedimentree_digest =
            darn_core::sedimentree::compute_digest(darn.subduction(), sedimentree_id).await?;

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

    if out.is_porcelain() {
        let root_dir_url = sedimentree_id_to_url(manifest.root_directory_id());
        out.kv("root", &root.display().to_string())?;
        out.kv("root_dir_id", &root_dir_url)?;
    }

    out.outro("Ready to sync")?;

    Ok(())
}

/// Clone a workspace by root directory ID from global peers.
///
/// 1. Parse `root_id` from base58
/// 2. Initialize workspace with that `root_id`
/// 3. Connect to all global peers
/// 4. Sync root directory sedimentree, then recursively sync and write files
#[allow(clippy::too_many_lines)]
pub(crate) async fn clone_cmd(root_id_str: &str, path: &Path, out: &Output) -> eyre::Result<()> {
    out.intro("darn clone")?;

    // Step 1: Parse root directory ID (accepts automerge URL or plain base58)
    let root_id_bytes = parse_automerge_url(root_id_str)?;
    let root_dir_id = SedimentreeId::new(root_id_bytes);

    let display_url = sedimentree_id_to_url(root_dir_id);
    if out.is_porcelain() {
        out.kv("root_dir_id", &display_url)?;
    } else {
        let dim = Style::new().dim();
        cliclack::log::info(format!("Root directory: {}", dim.apply_to(&display_url)))?;
    }

    // Step 2: Check we have peers configured
    let peers = darn_core::peer::list_peers()?;
    if peers.is_empty() {
        eyre::bail!("No peers configured. Use `darn peer add` first.");
    }
    let peer_names: Vec<_> = peers.iter().map(|p| p.name.as_str()).collect();
    out.info(&format!(
        "Using {} configured peer(s): {}",
        peers.len(),
        peer_names.join(", ")
    ))?;

    // Step 3: Initialize workspace with the provided root directory ID
    let initialized = Darn::init_with_root_id(path, root_dir_id)?;
    let root = initialized.root().to_path_buf();
    out.success(&format!("Initialized workspace at {}", root.display()))?;

    // Step 4: Open workspace with Subduction
    let darn = Darn::open(&root).await?;

    // Step 5: Connect to all peers (but don't sync yet - we'll sync specific sedimentrees)
    let spinner = out.spinner("Connecting to peers...");

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
        eyre::bail!("Could not connect to any peers");
    }

    spinner.stop(format!("Connected to {connected_peers} peer(s)"));

    // Step 6: Sync and traverse directory tree, writing files
    let mut manifest = darn.load_manifest()?;
    let timeout = Some(Duration::from_secs(30));

    let progress = out.progress(100, "Cloning files...");

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
        out,
    )
    .await?;

    if file_count == 0 {
        progress.stop("No files found");
        out.outro("Clone complete (empty workspace)")?;
        return Ok(());
    }

    progress.stop(format!(
        "{file_count} file(s) cloned (▼{total_received} ▲{total_sent})"
    ));

    darn.save_manifest(&manifest)?;

    if out.is_porcelain() {
        out.kv("root", &root.display().to_string())?;
        out.kv("files_cloned", &file_count.to_string())?;
        out.kv("received", &total_received.to_string())?;
        out.kv("sent", &total_sent.to_string())?;
    }

    out.outro("Clone complete")?;

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
    progress: &crate::output::Progress,
    out: &Output,
) -> eyre::Result<usize> {
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
                if out.is_porcelain() {
                    out.kv("cloned", &entry_path.display().to_string())?;
                } else {
                    progress.set_message(format!("{}", entry_path.display()));
                }
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
                    out,
                ))
                .await?;
            }
        }
    }

    Ok(file_count)
}

/// Add patterns to .darnignore.
pub(crate) fn ignore(patterns: &[String], out: &Output) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let root = darn.root();

    let mut added_count = 0;

    for pattern in patterns {
        match darn_core::ignore::add_pattern(root, pattern) {
            Ok(true) => {
                if out.is_porcelain() {
                    out.kv("added", pattern)?;
                } else {
                    out.success(&format!("Added: {pattern}"))?;
                }
                added_count += 1;
            }
            Ok(false) => {
                if out.is_porcelain() {
                    out.kv("exists", pattern)?;
                } else {
                    out.remark(&format!("Already ignored: {pattern}"))?;
                }
            }
            Err(e) => {
                out.error(&format!("Failed to add {pattern}: {e}"))?;
            }
        }
    }

    if !out.is_porcelain() && added_count > 0 {
        out.info(&format!("{added_count} pattern(s) added to .darnignore"))?;
    }

    Ok(())
}

/// Remove patterns from .darnignore.
pub(crate) fn unignore(patterns: &[String], out: &Output) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let root = darn.root();

    let mut removed_count = 0;

    for pattern in patterns {
        match darn_core::ignore::remove_pattern(root, pattern) {
            Ok(true) => {
                if out.is_porcelain() {
                    out.kv("removed", pattern)?;
                } else {
                    out.success(&format!("Removed: {pattern}"))?;
                }
                removed_count += 1;
            }
            Ok(false) => {
                if out.is_porcelain() {
                    out.kv("not_found", pattern)?;
                } else {
                    out.warning(&format!("Not in .darnignore: {pattern}"))?;
                }
            }
            Err(e) => {
                out.error(&format!("Failed to remove {pattern}: {e}"))?;
            }
        }
    }

    if !out.is_porcelain() && removed_count > 0 {
        out.info(&format!(
            "{removed_count} pattern(s) removed from .darnignore"
        ))?;
    }

    Ok(())
}

/// Show tracked files as a tree with state indicators.
pub(crate) fn tree(out: &Output) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let manifest = darn.load_manifest()?;
    let root = darn.root();

    info!(root = %root.display(), "Showing tree");

    if out.is_porcelain() {
        // Porcelain: tab-separated lines: state\tpath\tsedimentree_url
        if manifest.is_empty() {
            return Ok(());
        }

        let mut entries: Vec<_> = manifest.iter().collect();
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        for entry in &entries {
            let state = entry.state(root);
            let state_str = match state {
                FileState::Clean => "clean",
                FileState::Modified => "modified",
                FileState::Missing => "missing",
            };
            let url = sedimentree_id_to_url(entry.sedimentree_id);
            println!("{state_str}\t{}\t{url}", entry.relative_path.display());
        }
        return Ok(());
    }

    // Human mode
    out.intro(&format!("Workspace: {}", root.display()))?;

    if manifest.is_empty() {
        out.remark("No tracked files")?;
        out.outro("Run darn sync to discover and track files")?;
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
        let url = sedimentree_id_to_url(entry.sedimentree_id);
        writeln!(
            file_list,
            "{} {}  {}",
            styled_indicator,
            entry.relative_path.display(),
            dim.apply_to(&url)
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
        write!(
            summary,
            ", {} {}",
            yellow.apply_to(modified),
            yellow.apply_to("modified")
        )
        .expect("write to string");
    }
    if missing > 0 {
        write!(
            summary,
            ", {} {}",
            red.apply_to(missing),
            red.apply_to("missing")
        )
        .expect("write to string");
    }

    out.outro(&summary)?;

    Ok(())
}

/// Show stats for a tracked file.
pub(crate) async fn stat(target: &str, out: &Output) -> eyre::Result<()> {
    let darn = Darn::open(Path::new(".")).await?;
    let manifest = darn.load_manifest()?;
    let root = darn.root();

    // Try to find by path first, then by Sedimentree ID
    let tracked = if let Some(entry) = manifest.get_by_path(Path::new(target)) {
        entry
    } else if let Some(entry) =
        try_parse_sedimentree_id(target).and_then(|id| manifest.get_by_id(&id))
    {
        entry
    } else {
        out.error(&format!("Not found: {target}"))?;
        out.remark("Specify a tracked file path or Sedimentree ID (base58)")?;
        return Ok(());
    };

    let storage = darn.storage()?;
    let sed_id = tracked.sedimentree_id;

    // Get commit and fragment counts from storage
    let commits = Storage::<future_form::Sendable>::load_loose_commits(&storage, sed_id).await?;
    let fragments = Storage::<future_form::Sendable>::load_fragments(&storage, sed_id).await?;

    // Get file state
    let state = tracked.state(root);
    let state_str = match state {
        FileState::Clean => "clean",
        FileState::Modified => "modified",
        FileState::Missing => "missing",
    };

    let file_type_str = match tracked.file_type {
        FileType::Text => "text",
        FileType::Binary => "binary",
    };

    let sed_id_str = sedimentree_id_to_url(sed_id);
    let fs_digest = bs58::encode(tracked.file_system_digest.as_bytes()).into_string();
    let sed_digest = bs58::encode(tracked.sedimentree_digest.as_bytes()).into_string();

    if out.is_porcelain() {
        println!("path\t{}", tracked.relative_path.display());
        println!("sedimentree\t{sed_id_str}");
        println!("state\t{state_str}");
        println!("type\t{file_type_str}");
        println!("commits\t{}", commits.len());
        println!("fragments\t{}", fragments.len());
        println!("digest_fs\t{fs_digest}");
        println!("digest_sed\t{sed_digest}");
        return Ok(());
    }

    // Human mode
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let green = Style::new().green();
    let dim = Style::new().dim();

    let state_styled = match state {
        FileState::Clean => green.apply_to("clean").to_string(),
        FileState::Modified => yellow.apply_to("modified").to_string(),
        FileState::Missing => red.apply_to("missing").to_string(),
    };

    cliclack::intro(format!("{}", tracked.relative_path.display()))?;

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

/// Parse an automerge URL or plain base58 into a 32-byte ID.
///
/// Accepts:
/// - `automerge:<base58check>` (with checksum validation)
/// - Plain base58 (no checksum, for backward compatibility)
///
/// # Errors
///
/// Returns an error if the input is invalid or not 32 bytes.
fn parse_automerge_url(s: &str) -> eyre::Result<[u8; 32]> {
    let bytes = if let Some(encoded) = s.strip_prefix("automerge:") {
        // Try JS-compatible bs58check first, then Rust's with_check, then plain bs58
        darn_core::directory::bs58check_decode(encoded)
            .or_else(|_| {
                bs58::decode(encoded)
                    .with_check(None)
                    .into_vec()
                    .map_err(|e| e.to_string())
            })
            .or_else(|_| bs58::decode(encoded).into_vec().map_err(|e| e.to_string()))
            .map_err(|e| eyre::eyre!("invalid automerge URL: {e}"))?
    } else {
        // Plain base58 (no checksum)
        bs58::decode(s)
            .into_vec()
            .map_err(|e| eyre::eyre!("invalid base58: {e}"))?
    };

    // Accept 16-byte IDs (zero-pad to 32) or 32-byte IDs
    match bytes.len() {
        16 => {
            let mut arr = [0u8; 32];
            arr[..16].copy_from_slice(&bytes);
            Ok(arr)
        }
        32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(arr)
        }
        n => eyre::bail!("ID must be 16 or 32 bytes (got {n})"),
    }
}

/// Try to parse a sedimentree ID from an automerge URL or plain base58.
fn try_parse_sedimentree_id(s: &str) -> Option<SedimentreeId> {
    parse_automerge_url(s).ok().map(SedimentreeId::new)
}

/// Sync with peers.
///
/// First refreshes all modified local files (commits local changes),
/// then syncs with the specified peer or all peers.
///
/// If `dry_run` is true, shows what would happen without actually syncing.
/// If `force` is true, skips confirmation for new file discovery.
/// In porcelain mode, `force` is implied (no interactive prompts).
pub(crate) async fn sync_cmd(
    peer_name: Option<&str>,
    dry_run: bool,
    force: bool,
    out: &Output,
) -> eyre::Result<()> {
    info!(?peer_name, dry_run, force, "Syncing");

    // Porcelain mode implies --force (no interactive prompts)
    let force = force || out.is_porcelain();

    if dry_run {
        return sync_dry_run(peer_name, out);
    }

    out.intro("darn sync")?;

    let darn = Darn::open(Path::new(".")).await?;
    let mut manifest = darn.load_manifest()?;

    // Phase 1: Scan for new files (fast, no side effects)
    let spinner = out.spinner("Scanning for new files...");

    let candidates = match darn.scan_new_files(&manifest) {
        Ok(c) => c,
        Err(e) => {
            spinner.stop("Scan failed");
            out.warning(&format!("File scan error: {e}"))?;
            return continue_sync(darn, manifest, peer_name, out).await;
        }
    };

    spinner.stop(format!("Found {} new file(s)", candidates.len()));

    // Show candidates and ask for confirmation before ingesting
    if !candidates.is_empty() {
        // Convert absolute paths to relative for display
        let relative_paths: Vec<PathBuf> = candidates
            .iter()
            .filter_map(|p| p.strip_prefix(darn.root()).ok().map(Path::to_path_buf))
            .collect();

        if out.is_porcelain() {
            for p in &relative_paths {
                out.kv("new_file", &p.display().to_string())?;
            }
        } else {
            let tree = format_paths_as_tree(&relative_paths);
            cliclack::note(format!("Found {} new file(s)", candidates.len()), &tree)?;
        }

        // Confirm unless --force (porcelain always forces)
        let should_track = force || out.confirm("Track these files?", true)?;

        if should_track {
            // Phase 2: Ingest files (only after confirmation)
            let total_files = candidates.len();
            let progress_bar = out.progress(total_files as u64, "Processing files...");

            // Set up cancellation token for Ctrl+C
            let cancel_token = CancellationToken::new();
            let cancel_token_clone = cancel_token.clone();

            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_token_clone.cancel();
                }
            });

            // Track last position to know when to increment
            let last_completed = std::sync::atomic::AtomicUsize::new(0);

            // Progress callback updates progress bar
            let progress_callback = |progress: DiscoverProgress<'_>| {
                // Increment progress bar for each newly completed file
                let prev =
                    last_completed.swap(progress.completed, std::sync::atomic::Ordering::Relaxed);
                let newly_completed = progress.completed.saturating_sub(prev);
                for _ in 0..newly_completed {
                    progress_bar.inc(newly_completed as u64);
                }

                // Update message with current file
                let msg = match progress.last_completed {
                    Some(file) => format!("{}", file.display()),
                    None => "Processing...".to_string(),
                };
                progress_bar.set_message(msg);
            };

            let result = darn
                .ingest_files(candidates, &mut manifest, progress_callback, &cancel_token)
                .await;

            match result {
                Ok(DiscoverResult {
                    new_files,
                    errors,
                    cancelled,
                }) => {
                    progress_bar.stop(format!("Processed {total_files} file(s)"));

                    if cancelled {
                        out.warning("Processing cancelled")?;
                        return Ok(());
                    }

                    // Report any errors
                    for (path, err) in &errors {
                        out.warning(&format!("{}: {}", path.display(), err))?;
                    }

                    if !new_files.is_empty() {
                        darn.save_manifest(&manifest)?;
                        if out.is_porcelain() {
                            for path in &new_files {
                                out.kv("tracked", &path.display().to_string())?;
                            }
                        } else {
                            out.success(&format!("Tracking {} new file(s)", new_files.len()))?;
                        }
                        for path in &new_files {
                            info!(path = %path.display(), "Tracked file");
                        }
                    }
                }
                Err(e) => {
                    progress_bar.stop("Processing failed");
                    out.warning(&format!("Processing error: {e}"))?;
                }
            }
        } else {
            out.remark("Skipped. Use 'darn ignore <pattern>' to ignore them.")?;
        }
    }

    continue_sync(darn, manifest, peer_name, out).await
}

/// Continue sync after file discovery.
#[allow(clippy::too_many_lines)]
async fn continue_sync(
    darn: Darn,
    mut manifest: Manifest,
    peer_name: Option<&str>,
    out: &Output,
) -> eyre::Result<()> {
    // Refresh all modified files (commit local changes)
    let spinner = out.spinner("Checking for local changes...");
    let result = darn.refresh_all(&mut manifest).await;
    spinner.clear();

    if !result.updated.is_empty() {
        darn.save_manifest(&manifest)?;
        if out.is_porcelain() {
            for path in &result.updated {
                out.kv("committed", &path.display().to_string())?;
            }
        } else {
            out.success(&format!(
                "Committed {} local change(s)",
                result.updated.len()
            ))?;
        }
        for path in &result.updated {
            info!(path = %path.display(), "Refreshed file");
        }
    }

    if !result.missing.is_empty() {
        if out.is_porcelain() {
            for path in &result.missing {
                out.kv("missing", &path.display().to_string())?;
            }
        } else {
            out.warning(&format!(
                "{} file(s) missing from disk",
                result.missing.len()
            ))?;
        }
    }

    if !result.errors.is_empty() {
        for (path, err) in &result.errors {
            out.error(&format!("Error refreshing {}: {err}", path.display()))?;
        }
    }

    // Step 2: Get peers to connect
    let unopened = Darn::open_without_subduction(Path::new("."))?;
    let mut peers = match peer_name {
        Some(name) => {
            let peer_name = PeerName::new(name)?;
            let p = unopened
                .get_peer(&peer_name)?
                .ok_or_else(|| eyre::eyre!("peer not found: {name}"))?;
            vec![p]
        }
        None => unopened.list_peers()?,
    };

    if peers.is_empty() {
        out.warning("No peers configured")?;
        if !out.is_porcelain() {
            out.outro(&format!(
                "Use {} to add peers",
                cmd("darn peer add <name> <url>")
            ))?;
        }
        return Ok(());
    }

    // Collect current sedimentree digests for sync tracking
    let current_digests: Vec<_> = manifest
        .iter()
        .map(|e| (e.sedimentree_id, e.sedimentree_digest))
        .collect();

    // Step 3: Connect and sync with each peer (with progress bars)
    let mut sync_success = false;

    for peer in &mut peers {
        let was_discovery = peer.is_discovery();

        match sync_peer_with_progress(&darn, peer, &manifest, out).await {
            Ok(summary) => {
                if summary.any_success() {
                    sync_success = true;
                    if out.is_porcelain() {
                        println!(
                            "synced\t{}\t{}\t{}\t{}",
                            peer.name,
                            summary.sedimentrees_synced,
                            summary.total_received(),
                            summary.total_sent()
                        );
                    } else {
                        let green = Style::new().green();
                        out.success(&format!(
                            "{} synced {} files (▼{} ▲{})",
                            green.apply_to(&peer.name),
                            summary.sedimentrees_synced,
                            summary.total_received(),
                            summary.total_sent()
                        ))?;
                    }

                    // If we connected via discovery mode, update to known mode with learned peer ID
                    if was_discovery && let Some(learned_peer_id) = summary.peer_id {
                        peer.set_known(learned_peer_id);
                        let id_str = bs58::encode(learned_peer_id.as_bytes()).into_string();
                        if out.is_porcelain() {
                            println!("learned_peer_id\t{}\t{id_str}", peer.name);
                        } else {
                            out.info(&format!(
                                "Learned peer ID for {}: {}",
                                peer.name,
                                Style::new().dim().apply_to(&id_str)
                            ))?;
                        }
                    }

                    // Record sync state for this peer
                    peer.record_sync(current_digests.iter().copied());
                    unopened.add_peer(peer)?;
                } else {
                    out.warning(&format!("{} no data exchanged", peer.name))?;
                }

                if summary.has_errors() {
                    out.warning(&format!("{} error(s) during sync", summary.errors.len()))?;
                }
            }
            Err(e) => {
                if out.is_porcelain() {
                    println!("error\t{}\t{e}", peer.name);
                } else {
                    let red = Style::new().red();
                    out.error(&format!("{} {e}", red.apply_to(&peer.name)))?;
                }
            }
        }
    }

    if sync_success {
        // Apply remote changes to local files
        let spinner = out.spinner("Applying remote changes...");

        let apply_result = darn.apply_remote_changes(&mut manifest).await;
        spinner.clear();

        // Report results
        if out.is_porcelain() {
            for path in &apply_result.updated {
                println!("updated\t{}", path.display());
            }
            for path in &apply_result.merged {
                println!("merged\t{}", path.display());
            }
            for path in &apply_result.created {
                println!("created\t{}", path.display());
            }
            for path in &apply_result.deleted {
                println!("deleted\t{}", path.display());
            }
            for (path, err) in &apply_result.errors {
                println!("error\t{}\t{err}", path.display());
            }
        } else {
            if !apply_result.updated.is_empty() {
                out.success(&format!(
                    "{} file(s) updated from remote",
                    apply_result.updated.len()
                ))?;
            }

            if !apply_result.merged.is_empty() {
                out.info(&format!(
                    "{} file(s) merged (concurrent changes)",
                    apply_result.merged.len()
                ))?;
            }

            if !apply_result.created.is_empty() {
                out.success(&format!(
                    "{} new file(s) from remote",
                    apply_result.created.len()
                ))?;
                for path in &apply_result.created {
                    out.remark(&format!("  + {}", path.display()))?;
                }
            }

            if !apply_result.deleted.is_empty() {
                out.info(&format!(
                    "{} file(s) deleted (removed from remote)",
                    apply_result.deleted.len()
                ))?;
                for path in &apply_result.deleted {
                    out.remark(&format!("  - {}", path.display()))?;
                }
            }

            if apply_result.has_errors() {
                out.warning(&format!(
                    "{} error(s) applying remote changes",
                    apply_result.errors.len()
                ))?;
                for (path, err) in &apply_result.errors {
                    out.remark(&format!("  ! {}: {err}", path.display()))?;
                }
            }
        }

        darn.save_manifest(&manifest)?;
        out.outro("Sync complete")?;
    } else {
        out.outro("Sync failed")?;
    }

    Ok(())
}

/// Sync with a peer, displaying progress via cliclack progress bar.
async fn sync_peer_with_progress(
    darn: &Darn,
    peer: &Peer,
    manifest: &Manifest,
    out: &Output,
) -> eyre::Result<darn_core::sync_progress::SyncSummary> {
    let progress_bar = out.progress(1, &format!("Connecting to {}...", peer.name));
    let current = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(1));

    let current_ref = &current;
    let total_ref = &total;
    let is_porcelain = out.is_porcelain();

    let summary = darn
        .sync_with_peer_progress(peer, manifest, |event| {
            match event {
                SyncProgressEvent::ConnectingToPeer { peer_name, .. } => {
                    progress_bar.set_message(format!("Connecting to {peer_name}..."));
                }
                SyncProgressEvent::Connected { .. } => {
                    progress_bar.set_message("Connected, starting sync...");
                }
                SyncProgressEvent::StartingSync { total_sedimentrees } => {
                    total_ref.store(total_sedimentrees, Ordering::SeqCst);
                    progress_bar.set_length(total_sedimentrees.try_into().unwrap_or(u64::MAX));
                    progress_bar.set_message(format!("Syncing {total_sedimentrees} items..."));
                }
                SyncProgressEvent::SedimentreeStarted {
                    file_path,
                    index,
                    total,
                    ..
                } => {
                    let display_index = index + 1;
                    if is_porcelain {
                        let path_str = file_path
                            .as_ref()
                            .map_or("root_directory".to_string(), |p| p.display().to_string());
                        println!("syncing\t{display_index}\t{total}\t{path_str}");
                    } else {
                        let msg = match &file_path {
                            Some(path) => format!("[{display_index}/{total}] {}", path.display()),
                            None => format!("[{display_index}/{total}] root directory"),
                        };
                        progress_bar.set_message(msg);
                    }
                }
                SyncProgressEvent::SedimentreeCompleted { index, .. } => {
                    current_ref.store(index + 1, Ordering::SeqCst);
                    progress_bar.inc(1);
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
fn sync_dry_run(peer_name: Option<&str>, out: &Output) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let manifest = darn.load_manifest()?;
    let root = darn.root();

    out.intro("Sync dry run")?;

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

    if out.is_porcelain() {
        for path in &modified {
            println!("modified\t{}", path.display());
        }
        for path in &missing {
            println!("missing\t{}", path.display());
        }
    } else if !modified.is_empty() || !missing.is_empty() {
        let mut changes = String::new();
        for path in &modified {
            writeln!(changes, "M  {}", path.display()).expect("write to string");
        }
        for path in &missing {
            writeln!(changes, "!  {} (missing)", path.display()).expect("write to string");
        }
        changes.pop();

        cliclack::note("Uncommitted changes", &changes)?;
        out.info(&format!(
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
                out.error(&format!("Peer not found: {name}"))?;
                out.outro("Dry run aborted")?;
                return Ok(());
            }
        }
        None => darn.list_peers()?,
    };

    if peers.is_empty() {
        out.warning("No peers configured")?;
        if !out.is_porcelain() {
            out.outro(&format!(
                "Use {} to add peers",
                cmd("darn peer add <name> <url>")
            ))?;
        }
        return Ok(());
    }

    // Show sync status per peer
    for peer in &peers {
        let peer_id_display = if let Some(id) = peer.peer_id() {
            bs58::encode(id.as_bytes()).into_string()
        } else {
            "(discovery)".to_string()
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

        if out.is_porcelain() {
            println!(
                "peer\t{}\t{}\t{peer_id_display}\t{last_sync}\t{}",
                peer.name,
                peer.url,
                unsynced.len()
            );
            for path in &unsynced {
                println!("unsynced\t{}\t{}", peer.name, path.display());
            }
        } else {
            // Build peer status content
            let mut content = format!("URL:       {}\n", peer.url);
            writeln!(content, "Peer ID:   {peer_id_display}").expect("write to string");
            writeln!(content, "Last sync: {last_sync}").expect("write to string");

            if unsynced.is_empty() {
                write!(content, "Status:    all {total} file(s) synced").expect("write to string");
            } else if unsynced.len() == total {
                let count = unsynced.len();
                write!(content, "Status:    {count} file(s) never synced")
                    .expect("write to string");
            } else {
                let count = unsynced.len();
                writeln!(content, "Status:    {count} of {total} file(s) unsynced")
                    .expect("write to string");
                for path in unsynced.iter().take(5) {
                    writeln!(content, "           - {}", path.display()).expect("write to string");
                }
                if unsynced.len() > 5 {
                    let remaining = unsynced.len() - 5;
                    write!(content, "           ... and {remaining} more")
                        .expect("write to string");
                } else {
                    content.pop();
                }
            }

            cliclack::note(peer.name.as_str(), &content)?;
        }
    }

    if !out.is_porcelain() {
        out.outro(&format!("Run {} to sync", cmd("darn sync")))?;
    }
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
pub(crate) async fn watch(
    sync_interval: &std::time::Duration,
    no_track: bool,
    out: &Output,
) -> eyre::Result<()> {
    let darn = Darn::open(Path::new(".")).await?;
    let root = darn.root().to_path_buf();
    let mut manifest = darn.load_manifest()?;

    info!(root = %root.display(), ?sync_interval, no_track, "Starting watch");

    out.intro("darn watch")?;
    out.info(&format!("Watching {}", root.display()))?;

    if !no_track {
        out.warning("New files will be auto-tracked and synced (use --no-track to disable)")?;
    }

    if sync_interval.is_zero() {
        out.remark("Sync on change (immediate)")?;
    } else {
        out.remark(&format!(
            "Sync interval: {}",
            format_duration(sync_interval)
        ))?;
    }

    if no_track {
        out.remark("Auto-track disabled")?;
    }

    // Create watcher and event processor
    let config = WatcherConfig {
        auto_track: !no_track,
        ..WatcherConfig::default()
    };

    let (mut watcher, mut rx) = Watcher::new(&root, config)?;
    let mut processor = WatchEventProcessor::new(&root, &manifest)?;

    watcher.start()?;
    out.success("Watcher started")?;
    out.remark("Press Ctrl+C to stop")?;
    if !out.is_porcelain() {
        println!(); // Blank line before events
    }

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
        out.warning("No peers configured - sync disabled")?;
    }

    // Initial sync to establish connections and subscriptions (subscribe: true)
    // This ensures WebSocket listeners are running to receive push updates
    if has_peers {
        let spinner = out.spinner("Establishing peer connections...");

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
        if !out.is_porcelain() {
            println!();
        }
    }

    // Styles (only used in human mode)
    let green = Style::new().green();
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let dim = Style::new().dim();
    let is_porcelain = out.is_porcelain();

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
                            if is_porcelain {
                                let kind = if manifest.get_by_path(&path).is_none() { "created" } else { "modified" };
                                println!("{kind}\t{}", path.display());
                            } else {
                                let is_new = manifest.get_by_path(&path).is_none();
                                if is_new {
                                    println!("  {} {}", green.apply_to("+"), path.display());
                                } else {
                                    println!("  {} {}", yellow.apply_to("M"), path.display());
                                }
                            }
                        }
                    }
                    Some(WatchEvent::FileDeleted(path)) => {
                        if processor.process(WatchEvent::FileDeleted(path.clone())) {
                            if is_porcelain {
                                println!("deleted\t{}", path.display());
                            } else {
                                println!("  {} {}", red.apply_to("-"), path.display());
                            }
                        }
                    }
                    Some(WatchEvent::FileCreated(path)) => {
                        if processor.process(WatchEvent::FileCreated(path.clone())) {
                            if is_porcelain {
                                println!("created\t{}", path.display());
                            } else {
                                println!("  {} {}", green.apply_to("+"), path.display());
                            }
                        }
                    }
                    Some(WatchEvent::FileRenamed { from, to }) => {
                        if processor.process(WatchEvent::FileRenamed { from: from.clone(), to: to.clone() }) {
                            if is_porcelain {
                                println!("renamed\t{}\t{}", from.display(), to.display());
                            } else {
                                println!("  {} {} -> {}", dim.apply_to("R"), from.display(), to.display());
                            }
                        }
                    }
                    Some(WatchEvent::Error(e)) => {
                        out.error(&format!("Watch error: {e}"))?;
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
                if !is_porcelain {
                    println!();
                }
                out.info("Stopping...")?;
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
                                    out.warning(&format!(
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
                                    out.warning(&format!(
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
                        if !is_porcelain {
                            println!();
                        }
                        let spinner = out.spinner("Syncing with peers...");

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
                                    if is_porcelain {
                                        println!("updated\t{}", path.display());
                                    } else {
                                        println!("  {} {}", yellow.apply_to("U"), path.display());
                                    }
                                }
                            }
                            if !apply_result.merged.is_empty() {
                                summary.push_str(&format!("{} merged, ", apply_result.merged.len()));
                                for path in &apply_result.merged {
                                    if is_porcelain {
                                        println!("merged\t{}", path.display());
                                    } else {
                                        println!("  {} {}", yellow.apply_to("M"), path.display());
                                    }
                                }
                            }
                            if !apply_result.created.is_empty() {
                                summary.push_str(&format!("{} new, ", apply_result.created.len()));
                                for path in &apply_result.created {
                                    if is_porcelain {
                                        println!("created\t{}", path.display());
                                    } else {
                                        println!("  {} {}", green.apply_to("+"), path.display());
                                    }
                                }
                            }
                            if !apply_result.deleted.is_empty() {
                                summary.push_str(&format!("{} deleted, ", apply_result.deleted.len()));
                                for path in &apply_result.deleted {
                                    if is_porcelain {
                                        println!("deleted\t{}", path.display());
                                    } else {
                                        println!("  {} {}", red.apply_to("-"), path.display());
                                    }
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
                        if !is_porcelain {
                            println!();
                        }
                }

                // Periodically check if any push data arrived (regardless of polling interval)
                if has_peers && last_push_check.elapsed() >= push_check_interval {
                    let apply_result = darn.apply_remote_changes(&mut manifest).await;

                    let total_changes = apply_result.updated.len()
                        + apply_result.merged.len()
                        + apply_result.created.len()
                        + apply_result.deleted.len();

                    if total_changes > 0 {
                        darn.save_manifest(&manifest)?;
                        processor.update_tracked_paths(&manifest);

                        for path in &apply_result.updated {
                            if is_porcelain {
                                println!("updated\t{}", path.display());
                            } else {
                                println!("  {} {}", yellow.apply_to("U"), path.display());
                            }
                        }
                        for path in &apply_result.merged {
                            if is_porcelain {
                                println!("merged\t{}", path.display());
                            } else {
                                println!("  {} {}", yellow.apply_to("M"), path.display());
                            }
                        }
                        for path in &apply_result.created {
                            if is_porcelain {
                                println!("created\t{}", path.display());
                            } else {
                                println!("  {} {}", green.apply_to("+"), path.display());
                            }
                        }
                        for path in &apply_result.deleted {
                            if is_porcelain {
                                println!("deleted\t{}", path.display());
                            } else {
                                println!("  {} {}", red.apply_to("-"), path.display());
                            }
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
    out.outro("Watch stopped")?;

    Ok(())
}

/// Track a single file (helper for watch command).
async fn track_single_file(
    darn: &Darn,
    manifest: &mut Manifest,
    relative_path: &Path,
) -> eyre::Result<()> {
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

    // Generate random SedimentreeId (16-byte for automerge-repo compatibility)
    let sedimentree_id = darn_core::generate_sedimentree_id();

    // Store as sedimentree commits
    sedimentree::store_document(darn.subduction(), sedimentree_id, &mut am_doc).await?;

    // Add file to directory tree
    let root_dir_id = manifest.root_directory_id();
    let parent_dir_id =
        sedimentree::ensure_parent_directories(darn.subduction(), root_dir_id, relative_path)
            .await?;

    let file_name = relative_path
        .file_name()
        .ok_or_else(|| eyre::eyre!("path has no filename"))?
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
    let sedimentree_digest = sedimentree::compute_digest(darn.subduction(), sedimentree_id).await?;

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
pub(crate) fn peer_add(
    name: &str,
    url: &str,
    peer_id: Option<&str>,
    out: &Output,
) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;

    // Validate peer name
    let peer_name = PeerName::new(name)?;

    // Check if peer already exists
    if darn.get_peer(&peer_name)?.is_some() {
        out.error(&format!("Peer already exists: {name}"))?;
        return Ok(());
    }

    let peer = if let Some(id_str) = peer_id {
        // Parse peer ID from base58
        let id_bytes = bs58::decode(id_str)
            .into_vec()
            .map_err(|e| eyre::eyre!("invalid peer ID (expected base58): {e}"))?;

        if id_bytes.len() != 32 {
            eyre::bail!("peer ID must be 32 bytes (got {})", id_bytes.len());
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
        "(discovery)".to_string()
    };

    darn.add_peer(&peer)?;

    info!(%name, %url, "Added peer");

    if out.is_porcelain() {
        println!("name\t{name}");
        println!("url\t{url}");
        println!("peer_id\t{peer_id_display}");
    } else {
        out.success(&format!("Added peer: {name} ({url})"))?;
        out.remark(&format!("Peer ID: {peer_id_display}"))?;
    }

    Ok(())
}

/// List known peers.
pub(crate) fn peer_list(out: &Output) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let peers = darn.list_peers()?;

    info!("Listing peers");

    if out.is_porcelain() {
        // Porcelain: tab-separated lines
        for peer in &peers {
            let peer_id_display = if let Some(id) = peer.peer_id() {
                bs58::encode(id.as_bytes()).into_string()
            } else {
                "discovery".to_string()
            };
            let mode = if peer.is_known() { "known" } else { "discover" };
            let last_sync = peer
                .last_synced_at
                .map_or_else(|| "never".to_string(), |ts| ts.as_secs().to_string());
            println!(
                "{}\t{}\t{mode}\t{peer_id_display}\t{last_sync}",
                peer.name, peer.url
            );
        }
        return Ok(());
    }

    // Human mode
    out.intro("Peers")?;

    if peers.is_empty() {
        out.remark("No peers configured")?;
        out.outro(&format!(
            "Use {} to add peers",
            cmd("darn peer add <name> <url>")
        ))?;
        return Ok(());
    }

    let dim = Style::new().dim();

    for peer in &peers {
        let peer_id_display = if let Some(id) = peer.peer_id() {
            let id_str = bs58::encode(id.as_bytes()).into_string();
            dim.apply_to(&id_str).to_string()
        } else {
            "(discovery)".to_string()
        };
        let last_sync = peer
            .last_synced_at
            .map_or_else(|| "never".to_string(), format_timestamp);

        let mut content = format!("URL:       {}\n", peer.url);
        content.push_str(&format!("Peer ID:   {peer_id_display}\n"));
        content.push_str(&format!("Last sync: {last_sync}"));

        cliclack::note(peer.name.as_str(), &content)?;
    }

    out.outro(&format!("{} peer(s)", peers.len()))?;

    Ok(())
}

/// Remove a peer.
pub(crate) fn peer_remove(name: &str, out: &Output) -> eyre::Result<()> {
    let darn = Darn::open_without_subduction(Path::new("."))?;
    let peer_name = PeerName::new(name)?;

    if darn.remove_peer(&peer_name)? {
        info!(%name, "Removed peer");
        if out.is_porcelain() {
            println!("removed\t{name}");
        } else {
            out.success(&format!("Removed peer: {name}"))?;
        }
    } else {
        if out.is_porcelain() {
            println!("not_found\t{name}");
        } else {
            out.warning(&format!("Peer not found: {name}"))?;
        }
    }

    Ok(())
}

/// Show info about global config and current workspace.
pub(crate) fn info(out: &Output) -> eyre::Result<()> {
    // Global Configuration
    let config_dir = darn_core::config::global_config_dir()?;
    let signer_dir = darn_core::config::global_signer_dir()?;

    let peer_id_str = match darn_core::signer::peer_id(&signer_dir) {
        Ok(peer_id) => bs58::encode(peer_id.as_bytes()).into_string(),
        Err(e) => format!("(error: {e})"),
    };

    if out.is_porcelain() {
        println!("config_dir\t{}", config_dir.display());
        println!("peer_id\t{peer_id_str}");

        // Peers
        if let Ok(peers) = darn_core::peer::list_peers() {
            for peer in &peers {
                let mode = if peer.is_known() { "known" } else { "discover" };
                let peer_id_display = if let Some(id) = peer.peer_id() {
                    bs58::encode(id.as_bytes()).into_string()
                } else {
                    "discovery".to_string()
                };
                println!(
                    "peer\t{}\t{}\t{mode}\t{peer_id_display}",
                    peer.name, peer.url
                );
            }
        }

        // Workspace
        if let Ok(darn) = Darn::open_without_subduction(Path::new(".")) {
            let manifest = darn.load_manifest();
            let root_id_str = manifest
                .as_ref()
                .map(|m| sedimentree_id_to_url(m.root_directory_id()))
                .unwrap_or_else(|_| "(error)".to_string());
            let file_count = manifest.as_ref().map(|m| m.len()).unwrap_or(0);

            println!("workspace_root\t{}", darn.root().display());
            println!("root_dir_id\t{root_id_str}");
            println!("tracked_files\t{file_count}");

            if let Ok(manifest) = manifest {
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
                    let url = sedimentree_id_to_url(entry.sedimentree_id);
                    println!(
                        "file\t{}\t{type_str}\t{state_str}\t{url}",
                        entry.relative_path.display()
                    );
                }
            }
        } else {
            println!("workspace\tnone");
        }

        return Ok(());
    }

    // Human mode
    let dim = Style::new().dim();

    out.intro("darn info")?;

    let global_table = format!(
        "\
┌─────────────┬──────────────────────────────────────────────────────────────┐
│ {:^11} │ {:^60} │
├─────────────┼──────────────────────────────────────────────────────────────┤
│ {:<11} │ {:<60} │
│ {:<11} │ {:<60} │
└─────────────┴──────────────────────────────────────────────────────────────┘",
        "Field",
        "Value",
        "Config",
        truncate_path(&config_dir.display().to_string(), 60),
        "Peer ID",
        &peer_id_str
    );
    cliclack::note("Global Configuration", global_table)?;

    // Configured Peers
    let peers_content = match darn_core::peer::list_peers() {
        Ok(peers) if peers.is_empty() => dim.apply_to("(no peers configured)").to_string(),
        Ok(peers) => {
            let mut table = String::new();
            table.push_str(
                "┌────────────────┬────────────────────────────────────────┬──────────┐\n",
            );
            table.push_str(&format!(
                "│ {:^14} │ {:^38} │ {:^8} │\n",
                "Name", "URL", "Mode"
            ));
            table.push_str(
                "├────────────────┼────────────────────────────────────────┼──────────┤\n",
            );
            for peer in &peers {
                let mode = if peer.is_known() { "known" } else { "discover" };
                table.push_str(&format!(
                    "│ {:<14} │ {:<38} │ {:^8} │\n",
                    truncate_str(&peer.name.to_string(), 14),
                    truncate_str(&peer.url, 38),
                    mode
                ));
            }
            table
                .push_str("└────────────────┴────────────────────────────────────────┴──────────┘");
            table
        }
        Err(e) => dim
            .apply_to(format!("(error listing peers: {e})"))
            .to_string(),
    };
    cliclack::note("Configured Peers", peers_content)?;

    // Workspace
    match Darn::open_without_subduction(Path::new(".")) {
        Ok(darn) => {
            let manifest = darn.load_manifest();
            let root_id_str = manifest
                .as_ref()
                .map(|m| sedimentree_id_to_url(m.root_directory_id()))
                .unwrap_or_else(|_| "(error)".to_string());
            let file_count = manifest.as_ref().map(|m| m.len()).unwrap_or(0);

            let workspace_table = format!(
                "\
┌─────────────┬──────────────────────────────────────────────────────────────┐
│ {:^11} │ {:^60} │
├─────────────┼──────────────────────────────────────────────────────────────┤
│ {:<11} │ {:<60} │
│ {:<11} │ {:<60} │
│ {:<11} │ {:<60} │
└─────────────┴──────────────────────────────────────────────────────────────┘",
                "Field",
                "Value",
                "Root",
                truncate_path(&darn.root().display().to_string(), 60),
                "Root Dir ID",
                &root_id_str,
                "Files",
                format!("{file_count} tracked")
            );
            cliclack::note("Workspace", workspace_table)?;

            // Show tracked files if any
            if let Ok(manifest) = manifest {
                if !manifest.is_empty() {
                    let mut files_table = String::new();
                    files_table.push_str(
                        "┌──────────────────────────────────────────┬────────┬─────────────────────┐\n",
                    );
                    files_table.push_str(&format!(
                        "│ {:^40} │ {:^6} │ {:^19} │\n",
                        "Path", "Type", "State"
                    ));
                    files_table.push_str(
                        "├──────────────────────────────────────────┼────────┼─────────────────────┤\n",
                    );

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
                        files_table.push_str(&format!(
                            "│ {:<40} │ {:^6} │ {:^19} │\n",
                            truncate_str(&entry.relative_path.display().to_string(), 40),
                            type_str,
                            state_str
                        ));
                    }
                    files_table.push_str(
                        "└──────────────────────────────────────────┴────────┴─────────────────────┘",
                    );
                    cliclack::note("Tracked Files", files_table)?;
                }
            }
        }
        Err(_) => {
            cliclack::note("Workspace", dim.apply_to("(not in a darn workspace)"))?;
        }
    }

    out.outro("")?;

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
