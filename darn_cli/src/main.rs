//! # darn
//!
//! Directory-based Automerge Replication Node - a filesystem CLI for CRDT-backed files.

#![forbid(unsafe_code)]
// CLI-specific lint allows
#![allow(clippy::format_push_string)] // Common pattern for building CLI output
#![allow(clippy::large_futures)] // Async CLI commands are naturally large

use std::time::Duration;

use clap::{Parser, Subcommand};
use eyre::Result;
use tracing_subscriber::{EnvFilter, fmt};

mod commands;
mod output;
mod setup;
mod theme;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    // Silent by default, respects RUST_LOG, -v forces debug
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };

    fmt().with_env_filter(filter).init();

    let porcelain = cli.porcelain;
    let out = output::Output::new(porcelain);

    // Apply Catppuccin Mocha theme for all cliclack prompts (skip in porcelain mode)
    if !porcelain {
        theme::apply();
    }

    // Ensure signer exists before running commands
    if !setup::ensure_signer(porcelain)? {
        return Ok(());
    }

    match cli.command {
        Commands::Init {
            path,
            peer,
            peer_name,
            force_immutable,
        } => {
            commands::init(&path, peer.as_deref(), peer_name.as_deref(), force_immutable, out).await
        }
        Commands::Clone { root_id, path } => commands::clone_cmd(&root_id, &path, out).await,
        Commands::Ignore { patterns } => commands::ignore(&patterns, out),
        Commands::Unignore { patterns } => commands::unignore(&patterns, out),
        Commands::Tree => commands::tree(out),
        Commands::Stat { target } => commands::stat(&target, out).await,
        Commands::Sync {
            peer,
            dry_run,
            force,
            force_immutable,
        } => commands::sync_cmd(peer.as_deref(), dry_run, force, force_immutable, out).await,
        Commands::Watch {
            interval,
            no_track,
            force_immutable,
        } => commands::watch(&interval, no_track, force_immutable, out).await,
        Commands::Info => commands::info(out),
        Commands::Peer { command } => match command {
            PeerCommands::Add {
                name,
                websocket,
                iroh,
                relay,
                peer_id,
            } => commands::peer_add(name, websocket, iroh, relay, peer_id, out),
            PeerCommands::List => commands::peer_list(out),
            PeerCommands::Remove { name } => commands::peer_remove(&name, out),
        },
    }
}

/// Directory-based Automerge Replication Node
#[derive(Debug, Parser)]
#[command(name = "darn")]
#[command(version, about, long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Machine-readable output (no spinners, no color, tab-separated)
    #[arg(long, global = true)]
    porcelain: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize a new `darn` workspace
    Init {
        /// Directory to initialize (defaults to current directory)
        #[arg(default_value = ".")]
        path: std::path::PathBuf,

        /// Add a sync server during init (WebSocket URL, e.g. `ws://localhost:9000`)
        #[arg(long)]
        peer: Option<String>,

        /// Name for the peer (defaults to hostname from URL)
        #[arg(long, requires = "peer")]
        peer_name: Option<String>,

        /// Store new text files as immutable strings (LWW, no character merging)
        #[arg(long)]
        force_immutable: bool,
    },

    /// Clone a workspace by root directory ID (syncs from global peers)
    Clone {
        /// Automerge URL or base58 ID - get this from `darn info` on the source workspace
        root_id: String,

        /// Directory to clone into (created if it doesn't exist)
        path: std::path::PathBuf,
    },

    /// Add ignore patterns (excluded from sync)
    Ignore {
        /// Patterns to ignore (gitignore syntax)
        #[arg(required = true)]
        patterns: Vec<String>,
    },

    /// Remove ignore patterns
    Unignore {
        /// Patterns to stop ignoring
        #[arg(required = true)]
        patterns: Vec<String>,
    },

    /// Show tracked files as a tree
    Tree,

    /// Show stats for a tracked file
    Stat {
        /// File path or automerge URL
        target: String,
    },

    /// Sync with peers
    Sync {
        /// Specific peer name to sync with (syncs with all if not specified)
        peer: Option<String>,

        /// Show what would be synced without actually syncing
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation for new file discovery
        #[arg(long, short)]
        force: bool,

        /// Store new text files as immutable strings (LWW, no character merging)
        #[arg(long)]
        force_immutable: bool,
    },

    /// Watch for file changes and auto-sync
    Watch {
        /// Sync interval (e.g., "30s", "5m", "0" for push-only)
        #[arg(long, short, default_value = "60s", value_parser = parse_duration)]
        interval: std::time::Duration,

        /// Disable auto-tracking of new files
        #[arg(long)]
        no_track: bool,

        /// Store new text files as immutable strings (LWW, no character merging)
        #[arg(long)]
        force_immutable: bool,
    },

    /// Show info about global config and current workspace
    Info,

    /// Manage peers
    Peer {
        #[command(subcommand)]
        command: PeerCommands,
    },
}

#[derive(Debug, Subcommand)]
enum PeerCommands {
    /// Add a peer (interactive when flags omitted)
    Add {
        /// Name for this peer (prompted interactively if omitted)
        #[arg(long)]
        name: Option<String>,

        /// WebSocket URL (e.g., `ws://localhost:9000`)
        #[arg(long, conflicts_with = "iroh")]
        websocket: Option<String>,

        /// Iroh node ID (base32 public key)
        #[arg(long, conflicts_with = "websocket")]
        iroh: Option<String>,

        /// Iroh relay URL for NAT traversal (only with --iroh)
        #[arg(long, requires = "iroh")]
        relay: Option<String>,

        /// Peer ID in base58 (optional; if omitted, uses discovery mode)
        #[arg(long)]
        peer_id: Option<String>,
    },

    /// List known peers
    List,

    /// Remove a peer
    Remove {
        /// Name of the peer to remove
        name: String,
    },
}

/// Parse a duration string like "5s", "1m", "500ms", or "0" (for zero duration).
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();

    // Handle "0" as zero duration
    if s == "0" {
        return Ok(Duration::ZERO);
    }

    // Try to parse with suffix
    if let Some(num) = s.strip_suffix("ms") {
        num.parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|e| format!("invalid milliseconds: {e}"))
    } else if let Some(num) = s.strip_suffix('s') {
        num.parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|e| format!("invalid seconds: {e}"))
    } else if let Some(num) = s.strip_suffix('m') {
        num.parse::<u64>()
            .map(|m| Duration::from_secs(m * 60))
            .map_err(|e| format!("invalid minutes: {e}"))
    } else {
        // Default to seconds if no suffix
        s.parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|e| format!("invalid duration (use 5s, 1m, 500ms): {e}"))
    }
}
