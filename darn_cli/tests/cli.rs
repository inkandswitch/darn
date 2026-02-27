//! CLI smoke tests for `darn`.
//!
//! Each test spawns `darn` as a child process with `DARN_CONFIG_DIR` set to
//! a unique tempdir, providing full isolation from the user's real config.
//!
//! All tests use `--porcelain` mode because `cliclack` requires a TTY and
//! `assert_cmd` pipes stdout/stderr.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

/// Test fixture: an isolated config dir + workspace dir.
struct Fixture {
    config_dir: tempfile::TempDir,
    workspace_dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let config_dir = tempfile::tempdir().expect("create config tempdir");
        let workspace_dir = tempfile::tempdir().expect("create workspace tempdir");

        // Pre-create signer so the CLI doesn't prompt
        let signer_dir = config_dir.path().join("signer");
        std::fs::create_dir_all(&signer_dir).expect("create signer dir");
        darn_core::signer::load_or_generate(&signer_dir).expect("generate test signer");

        Self {
            config_dir,
            workspace_dir,
        }
    }

    /// Build a `Command` for `darn --porcelain` with isolated env.
    fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = cargo_bin_cmd!("darn");
        cmd.env("DARN_CONFIG_DIR", self.config_dir.path());
        cmd.current_dir(self.workspace_dir.path());
        cmd.arg("--porcelain");
        cmd
    }

    /// Workspace root path.
    fn workspace(&self) -> &Path {
        self.workspace_dir.path()
    }

    /// Run `darn init` on this fixture's workspace.
    fn init(&self) {
        self.cmd()
            .args(["init", self.workspace().to_str().expect("utf8")])
            .assert()
            .success();
    }
}

// ==========================================================================
// darn init
// ==========================================================================

#[test]
fn init_succeeds() {
    let f = Fixture::new();
    f.init();

    assert!(f.workspace().join(".darn").is_file());
}

#[test]
fn init_porcelain_output() {
    let f = Fixture::new();

    f.cmd()
        .args(["init", f.workspace().to_str().expect("utf8")])
        .assert()
        .success()
        .stdout(
            predicates::str::contains("root\t").and(predicates::str::contains("root_dir_id\t")),
        );
}

#[test]
fn init_twice_fails() {
    let f = Fixture::new();
    let ws = f.workspace().to_str().expect("utf8").to_string();

    f.cmd().args(["init", &ws]).assert().success();
    f.cmd().args(["init", &ws]).assert().failure();
}

// ==========================================================================
// darn tree (after init)
// ==========================================================================

#[test]
fn tree_empty_workspace() {
    let f = Fixture::new();
    f.init();

    f.cmd().arg("info").assert().success().stdout(
        predicates::str::contains("workspace_root\t")
            .and(predicates::str::contains("root_dir_id\t"))
            .and(predicates::str::contains("tracked_files\t")),
    );
}

// ==========================================================================
// darn ignore / unignore
// ==========================================================================

#[test]
fn ignore_and_unignore() {
    let f = Fixture::new();
    f.init();

    f.cmd()
        .args(["ignore", "*.log"])
        .assert()
        .success()
        .stdout(predicates::str::contains("added\t*.log"));

    f.cmd()
        .args(["ignore", "*.log"])
        .assert()
        .success()
        .stdout(predicates::str::contains("exists\t*.log"));

    f.cmd()
        .args(["unignore", "*.log"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed\t*.log"));

    f.cmd()
        .arg("info")
        .assert()
        .success()
        .stdout(predicates::str::contains("workspace_root\t"));
}

// ==========================================================================
// darn info
// ==========================================================================

#[test]
fn info_shows_workspace_details() {
    let f = Fixture::new();
    f.init();

    f.cmd()
        .arg("info")
        .assert()
        .success()
        .stdout(predicates::str::contains("workspace_root\t"));
}

// ==========================================================================
// darn peer add / list / remove
// ==========================================================================

#[test]
fn peer_lifecycle() {
    let f = Fixture::new();
    f.init();

    f.cmd()
        .args([
            "peer",
            "add",
            "--name",
            "test-relay",
            "--websocket",
            "wss://relay.example.com",
        ])
        .assert()
        .success();

    f.cmd()
        .args(["peer", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("test-relay"));

    f.cmd()
        .args(["peer", "remove", "test-relay"])
        .assert()
        .success();

    f.cmd()
        .args(["peer", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("test-relay").not());
}

// ==========================================================================
// darn sync (without peers — should handle gracefully)
// ==========================================================================

#[test]
fn sync_with_no_peers_succeeds_with_warning() {
    let f = Fixture::new();
    f.init();

    // sync with no peers succeeds (exit 0) but emits a warning
    f.cmd().args(["sync", "--force"]).assert().success();
}

// ==========================================================================
// darn sync --dry-run
// ==========================================================================

#[test]
fn sync_dry_run_no_peers() {
    let f = Fixture::new();
    f.init();

    std::fs::write(f.workspace().join("file.txt"), "content").expect("write file");

    // Dry-run with no peers exits early (success) — untracked files
    // don't appear in the manifest scan, and no peers means no sync plan.
    f.cmd().args(["sync", "--dry-run"]).assert().success();
}

// ==========================================================================
// CLI flags
// ==========================================================================

#[test]
fn help_flag_works() {
    let f = Fixture::new();
    // --help doesn't need --porcelain (it writes to stdout directly)
    let mut cmd = cargo_bin_cmd!("darn");
    cmd.env("DARN_CONFIG_DIR", f.config_dir.path());
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("darn"));
}

#[test]
fn version_flag_works() {
    let f = Fixture::new();
    let mut cmd = cargo_bin_cmd!("darn");
    cmd.env("DARN_CONFIG_DIR", f.config_dir.path());
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains("darn"));
}

#[test]
fn unknown_command_fails() {
    let f = Fixture::new();
    f.cmd().arg("nonexistent").assert().failure();
}

// ==========================================================================
// Full CLI workflow: init → create files → tree → ignore → info
// ==========================================================================

#[test]
fn full_cli_init_and_tree() {
    let f = Fixture::new();
    f.init();

    std::fs::write(f.workspace().join("hello.txt"), "hello").expect("write file");
    std::fs::create_dir_all(f.workspace().join("src")).expect("create dir");
    std::fs::write(f.workspace().join("src/lib.rs"), "pub fn hello() {}").expect("write file");

    f.cmd().args(["ignore", "*.log"]).assert().success();

    // Tree should show no tracked files (files aren't ingested until sync)
    f.cmd()
        .arg("tree")
        .assert()
        .success()
        .stdout(predicates::str::is_empty().or(predicates::str::contains("total\t0")));

    // Info should work
    f.cmd()
        .arg("info")
        .assert()
        .success()
        .stdout(predicates::str::contains("workspace_root\t"));
}
