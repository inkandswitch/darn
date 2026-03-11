//! Integration tests for `darn_core`.
//!
//! Each test creates an isolated environment with its own `DARN_CONFIG_DIR`
//! and workspace tempdir, so tests don't interfere with each other or the
//! user's real `~/.config/darn/`.

// Integration tests need `set_var` for test isolation — production code still
// has `#![forbid(unsafe_code)]` in `lib.rs`.
#![allow(unsafe_code)]
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use darn_core::{
    darn::{Darn, NotAWorkspace},
    file::state::FileState,
    ignore,
    staged_update::StagedUpdate,
    workspace::id::WorkspaceId,
};
use testresult::TestResult;

/// Serializes access to `DARN_CONFIG_DIR` across tests in this binary.
///
/// `std::env::set_var` is process-global, so concurrent tests that set
/// different values would race. This mutex ensures one test at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// An isolated test environment for darn operations.
///
/// Sets `DARN_CONFIG_DIR` to a private tempdir on construction and restores
/// the previous value on drop.
struct TestEnv {
    /// Fake global config directory (`DARN_CONFIG_DIR`).
    _config_dir: tempfile::TempDir,

    /// Workspace root directory (where `.darn` lives).
    workspace_dir: tempfile::TempDir,

    /// Previous value of `DARN_CONFIG_DIR` (restored on drop).
    prev_config: Option<String>,
}

impl TestEnv {
    /// Create a fresh isolated environment.
    ///
    /// # Panics
    ///
    /// Panics if tempdirs cannot be created.
    fn new() -> Self {
        let config_dir = tempfile::tempdir().expect("create config tempdir");
        let workspace_dir = tempfile::tempdir().expect("create workspace tempdir");

        let prev_config = std::env::var("DARN_CONFIG_DIR").ok();

        // SAFETY: serialized by ENV_LOCK — only one test mutates the env at a time.
        unsafe {
            std::env::set_var("DARN_CONFIG_DIR", config_dir.path());
        }

        // Ensure the signer directory exists (Darn::init needs it)
        let signer_dir = config_dir.path().join("signer");
        std::fs::create_dir_all(&signer_dir).expect("create signer dir");
        darn_core::signer::load_or_generate(&signer_dir).expect("generate test signer");

        Self {
            _config_dir: config_dir,
            workspace_dir,
            prev_config,
        }
    }

    /// Workspace root path.
    fn workspace(&self) -> &Path {
        self.workspace_dir.path()
    }

    /// Initialize a workspace and return the initialized handle.
    fn init(&self) -> darn_core::darn::InitializedDarn {
        Darn::init(self.workspace()).expect("init workspace")
    }

    /// Open the workspace with full Subduction (async).
    async fn open(&self) -> Darn {
        Darn::open(self.workspace()).await.expect("open workspace")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            match &self.prev_config {
                Some(val) => std::env::set_var("DARN_CONFIG_DIR", val),
                None => std::env::remove_var("DARN_CONFIG_DIR"),
            }
        }
    }
}

/// Run a test body with an isolated `TestEnv`, holding the `ENV_LOCK`.
fn with_env<F, R>(f: F) -> R
where
    F: FnOnce(&TestEnv) -> R,
{
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let env = TestEnv::new();
    f(&env)
}

/// Async variant of `with_env`.
///
/// Intentionally holds the std Mutex across the await to serialize env var access.
/// This is correct here because these tests are inherently sequential (shared
/// process-global `DARN_CONFIG_DIR`), and tokio's single-threaded test runtime
/// won't deadlock.
#[allow(clippy::await_holding_lock)]
async fn with_env_async<F, Fut, R>(f: F) -> R
where
    F: FnOnce(TestEnv) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let env = TestEnv::new();
    f(env).await
}

// ==========================================================================
// Workspace initialization
// ==========================================================================

#[test]
fn init_creates_workspace() -> TestResult {
    with_env(|env| {
        let ws = env.init();

        // .darn marker file should exist
        assert!(env.workspace().join(".darn").is_file());

        // Root should match workspace path (canonicalize to handle /tmp → /private/tmp on macOS)
        let canonical = env.workspace().canonicalize()?;
        assert_eq!(ws.root(), canonical);

        // Centralized storage should exist
        assert!(ws.layout().storage_dir().is_dir());
        assert!(ws.manifest_path().is_file());

        Ok(())
    })
}

#[test]
fn init_twice_fails() -> TestResult {
    with_env(|env| {
        env.init();
        let result = Darn::init(env.workspace());
        assert!(result.is_err());
        Ok(())
    })
}

#[test]
fn init_creates_default_ignore_patterns() -> TestResult {
    with_env(|env| {
        env.init();
        let patterns = ignore::list_patterns(env.workspace())?;

        assert!(
            patterns.iter().any(|p| p == ".git/"),
            "should include .git/ in default patterns"
        );
        assert!(
            patterns.iter().any(|p| p == ".darn-staging-*/"),
            "should include .darn-staging-*/ in default patterns"
        );

        Ok(())
    })
}

#[test]
fn open_without_subduction_after_init() -> TestResult {
    with_env(|env| {
        env.init();

        let ws = Darn::open_without_subduction(env.workspace())?;
        let canonical = env.workspace().canonicalize()?;
        assert_eq!(ws.root(), canonical);
        assert!(ws.storage_dir().is_dir());

        Ok(())
    })
}

#[test]
fn open_from_subdirectory() -> TestResult {
    with_env(|env| {
        env.init();

        let subdir = env.workspace().join("a/b/c");
        std::fs::create_dir_all(&subdir)?;

        let ws = Darn::open_without_subduction(&subdir)?;
        let canonical = env.workspace().canonicalize()?;
        assert_eq!(ws.root(), canonical);

        Ok(())
    })
}

#[test]
fn open_nonexistent_workspace_fails() -> TestResult {
    with_env(|env| {
        let result = Darn::open_without_subduction(env.workspace());
        assert!(result.is_err());
        Ok(())
    })
}

#[test]
fn find_root_from_nested_directory() -> TestResult {
    with_env(|env| {
        env.init();

        let subdir = env.workspace().join("a").join("b").join("c");
        std::fs::create_dir_all(&subdir)?;

        let root = Darn::find_root(&subdir)?;
        assert!(root.join(".darn").is_file());

        Ok(())
    })
}

#[test]
fn find_root_not_found() -> TestResult {
    with_env(|env| {
        let result = Darn::find_root(env.workspace());
        assert!(matches!(result, Err(NotAWorkspace)));

        Ok(())
    })
}

#[test]
fn centralized_storage_paths() -> TestResult {
    with_env(|env| {
        env.init();
        let ws = Darn::open_without_subduction(env.workspace())?;

        let workspace_dir = ws.layout().workspace_dir();
        assert!(
            ws.storage_dir().starts_with(&workspace_dir),
            "storage should be under workspace dir"
        );
        assert!(
            ws.manifest_path().starts_with(&workspace_dir),
            "manifest should be under workspace dir"
        );

        Ok(())
    })
}

#[test]
fn root_is_absolute_and_exists() -> TestResult {
    with_env(|env| {
        let ws = env.init();
        assert!(ws.root().is_absolute());
        assert!(ws.root().is_dir());

        Ok(())
    })
}

#[test]
fn config_has_workspace_id() -> TestResult {
    with_env(|env| {
        let ws = env.init();

        let canonical = env.workspace().canonicalize()?;
        let expected_id = WorkspaceId::from_path(&canonical);
        assert_eq!(ws.config().id, expected_id);

        Ok(())
    })
}

// ==========================================================================
// Ignore pattern management
// ==========================================================================

#[test]
fn add_and_remove_ignore_pattern() -> TestResult {
    with_env(|env| {
        env.init();
        let root = env.workspace();

        let added = ignore::add_pattern(root, "*.log")?;
        assert!(added, "pattern should be added");

        let patterns = ignore::list_patterns(root)?;
        assert!(patterns.contains(&"*.log".to_string()));

        // Adding the same pattern again should return false
        let added_again = ignore::add_pattern(root, "*.log")?;
        assert!(!added_again, "duplicate pattern should not be added");

        // Remove the pattern
        let removed = ignore::remove_pattern(root, "*.log")?;
        assert!(removed);

        let patterns = ignore::list_patterns(root)?;
        assert!(!patterns.contains(&"*.log".to_string()));

        Ok(())
    })
}

#[test]
fn darn_file_is_always_ignored() -> TestResult {
    with_env(|env| {
        env.init();

        let rules = darn_core::ignore::IgnoreRules::from_workspace_root(env.workspace())?;
        assert!(rules.is_ignored(Path::new(".darn"), false));

        Ok(())
    })
}

#[test]
fn staging_dir_is_ignored() -> TestResult {
    with_env(|env| {
        env.init();

        let rules = darn_core::ignore::IgnoreRules::from_workspace_root(env.workspace())?;
        assert!(
            rules.is_ignored(Path::new(".darn-staging-abc123"), true),
            "staging directory should be ignored"
        );

        Ok(())
    })
}

// ==========================================================================
// Manifest and file tracking
// ==========================================================================

#[test]
fn fresh_manifest_is_empty() -> TestResult {
    with_env(|env| {
        env.init();
        let ws = Darn::open_without_subduction(env.workspace())?;
        let manifest = ws.load_manifest()?;

        assert_eq!(manifest.iter().count(), 0);

        Ok(())
    })
}

// ==========================================================================
// File discovery (scan_new_files — sync, no Subduction needed)
// ==========================================================================

#[tokio::test]
async fn scan_discovers_new_files() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let manifest = darn.load_manifest()?;

        std::fs::write(env.workspace().join("hello.txt"), "hello")?;
        std::fs::write(env.workspace().join("world.txt"), "world")?;

        let new_files = darn.scan_new_files(&manifest)?;
        assert_eq!(new_files.len(), 2);

        let names: Vec<_> = new_files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"hello.txt".to_string()));
        assert!(names.contains(&"world.txt".to_string()));

        Ok(())
    })
    .await
}

#[tokio::test]
async fn scan_ignores_hidden_files() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let manifest = darn.load_manifest()?;

        std::fs::write(env.workspace().join(".hidden"), "secret")?;
        std::fs::write(env.workspace().join("visible.txt"), "hello")?;

        let new_files = darn.scan_new_files(&manifest)?;
        assert_eq!(new_files.len(), 1);
        assert!(new_files[0].ends_with("visible.txt"));

        Ok(())
    })
    .await
}

#[tokio::test]
async fn scan_respects_ignore_patterns() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let manifest = darn.load_manifest()?;

        ignore::add_pattern(env.workspace(), "*.log")?;

        std::fs::write(env.workspace().join("app.log"), "log data")?;
        std::fs::write(env.workspace().join("app.txt"), "text data")?;

        let new_files = darn.scan_new_files(&manifest)?;
        assert_eq!(new_files.len(), 1);
        assert!(new_files[0].ends_with("app.txt"));

        Ok(())
    })
    .await
}

#[tokio::test]
async fn scan_discovers_nested_files() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let manifest = darn.load_manifest()?;

        std::fs::create_dir_all(env.workspace().join("src/utils"))?;
        std::fs::write(env.workspace().join("src/main.rs"), "fn main() {}")?;
        std::fs::write(
            env.workspace().join("src/utils/helpers.rs"),
            "pub fn help() {}",
        )?;

        let new_files = darn.scan_new_files(&manifest)?;
        assert_eq!(new_files.len(), 2);

        Ok(())
    })
    .await
}

// ==========================================================================
// File ingestion (requires async + Subduction)
// ==========================================================================

#[tokio::test]
async fn ingest_and_track_files() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        std::fs::write(env.workspace().join("readme.txt"), "hello world")?;
        std::fs::write(env.workspace().join("data.bin"), vec![0u8, 1, 2, 3])?;

        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 2);

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 2);
        assert!(result.errors.is_empty());

        assert_eq!(manifest.iter().count(), 2);

        let readme = manifest
            .get_by_path(Path::new("readme.txt"))
            .ok_or("readme should be tracked")?;
        assert_eq!(readme.state(env.workspace()), FileState::Clean);

        let data = manifest
            .get_by_path(Path::new("data.bin"))
            .ok_or("data.bin should be tracked")?;
        assert_eq!(data.state(env.workspace()), FileState::Clean);

        // Save and reload to verify persistence
        darn.save_manifest(&manifest)?;
        let reloaded = darn.load_manifest()?;
        assert_eq!(reloaded.iter().count(), 2);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn ingest_skips_ignored_via_scan() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        ignore::add_pattern(env.workspace(), "*.tmp")?;

        std::fs::write(env.workspace().join("keep.txt"), "keep")?;
        std::fs::write(env.workspace().join("skip.tmp"), "skip")?;

        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 1);

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 1);

        assert!(manifest.get_by_path(Path::new("keep.txt")).is_some());
        assert!(manifest.get_by_path(Path::new("skip.tmp")).is_none());

        Ok(())
    })
    .await
}

// ==========================================================================
// File refresh (detect local modifications)
// ==========================================================================

#[tokio::test]
async fn refresh_detects_modified_file() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // Create and ingest
        std::fs::write(env.workspace().join("file.txt"), "original")?;
        let paths = darn.scan_new_files(&manifest)?;
        let cancel = tokio_util::sync::CancellationToken::new();
        darn.ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;

        let entry = manifest
            .get_by_path(Path::new("file.txt"))
            .ok_or("file.txt should be tracked")?;
        assert_eq!(entry.state(env.workspace()), FileState::Clean);

        // Modify the file
        std::fs::write(env.workspace().join("file.txt"), "modified")?;

        let entry = manifest
            .get_by_path(Path::new("file.txt"))
            .ok_or("file.txt should be tracked after modify")?;
        assert_eq!(entry.state(env.workspace()), FileState::Modified);

        // Refresh should pick it up
        let diff = darn.refresh_all(&mut manifest).await;
        assert_eq!(diff.updated.len(), 1);
        assert!(diff.errors.is_empty());

        // After refresh, should be clean again
        let entry = manifest
            .get_by_path(Path::new("file.txt"))
            .ok_or("file.txt should be tracked after refresh")?;
        assert_eq!(entry.state(env.workspace()), FileState::Clean);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn refresh_detects_missing_file() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        std::fs::write(env.workspace().join("doomed.txt"), "bye")?;
        let paths = darn.scan_new_files(&manifest)?;
        let cancel = tokio_util::sync::CancellationToken::new();
        darn.ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;

        std::fs::remove_file(env.workspace().join("doomed.txt"))?;

        let entry = manifest
            .get_by_path(Path::new("doomed.txt"))
            .ok_or("doomed.txt should be tracked")?;
        assert_eq!(entry.state(env.workspace()), FileState::Missing);

        Ok(())
    })
    .await
}

// ==========================================================================
// Staged updates (end-to-end stage + commit)
// ==========================================================================

#[tokio::test]
async fn staged_update_creates_files_atomically() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let _darn = env.open().await;
        let mut manifest = darn_core::manifest::Manifest::new();

        let mut staged = StagedUpdate::new(env.workspace())?;

        let files = vec![
            ("a/first.txt", "content one"),
            ("a/second.txt", "content two"),
            ("b/third.txt", "content three"),
        ];

        for (path, content) in &files {
            let name = Path::new(path)
                .file_name()
                .ok_or("path should have file name")?
                .to_str()
                .ok_or("file name should be utf8")?;
            let file = darn_core::file::File::text(name, *content);
            let id = darn_core::generate_sedimentree_id();
            let digest = sedimentree_core::crypto::digest::Digest::force_from_bytes([0u8; 32]);

            staged.stage_create(
                &file,
                PathBuf::from(path),
                id,
                darn_core::file::file_type::FileType::Text,
                digest,
            )?;
        }

        // Before commit: no files in workspace (except .darn)
        assert!(!env.workspace().join("a/first.txt").exists());
        assert!(!env.workspace().join("b/third.txt").exists());

        // Commit
        let result = staged.commit(&mut manifest).await?;
        assert!(result.errors.is_empty());
        assert_eq!(result.created.len(), 3);

        // After commit: all files present
        for (path, content) in &files {
            let full = env.workspace().join(path);
            assert!(full.exists(), "file {path} should exist after commit");
            assert_eq!(std::fs::read_to_string(&full)?, *content);
        }

        assert_eq!(manifest.iter().count(), 3);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn staged_update_handles_mixed_creates_and_deletes() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // First, ingest a file normally
        std::fs::write(env.workspace().join("old.txt"), "old content")?;
        let paths = darn.scan_new_files(&manifest)?;
        let cancel = tokio_util::sync::CancellationToken::new();
        darn.ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;

        let old_entry = manifest
            .get_by_path(Path::new("old.txt"))
            .ok_or("old.txt should be tracked")?;
        let old_id = old_entry.sedimentree_id;

        // Now stage: create a new file + delete the old one
        let mut staged = StagedUpdate::new(env.workspace())?;

        let new_file = darn_core::file::File::text("new.txt", "new content");
        let new_id = darn_core::generate_sedimentree_id();
        let digest = sedimentree_core::crypto::digest::Digest::force_from_bytes([0u8; 32]);

        staged.stage_create(
            &new_file,
            PathBuf::from("new.txt"),
            new_id,
            darn_core::file::file_type::FileType::Text,
            digest,
        )?;
        staged.stage_delete(PathBuf::from("old.txt"), old_id);

        let result = staged.commit(&mut manifest).await?;
        assert!(result.errors.is_empty());
        assert_eq!(result.created.len(), 1);
        assert_eq!(result.deleted.len(), 1);

        assert!(env.workspace().join("new.txt").exists());
        assert!(!env.workspace().join("old.txt").exists());

        assert!(manifest.get_by_path(Path::new("new.txt")).is_some());
        assert!(manifest.get_by_path(Path::new("old.txt")).is_none());

        Ok(())
    })
    .await
}

// ==========================================================================
// Peer management (no network needed)
// ==========================================================================

#[test]
fn peer_add_list_remove() -> TestResult {
    with_env(|_env| {
        use darn_core::peer::{Peer, PeerAddress, PeerName, add_peer, list_peers, remove_peer};

        let peers = list_peers()?;
        assert!(peers.is_empty());

        let name = PeerName::new("test-relay")?;
        let addr = PeerAddress::websocket("wss://relay.example.com".to_string());
        let peer = Peer::discover(name.clone(), addr);
        add_peer(&peer)?;

        let peers = list_peers()?;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, name);

        let removed = remove_peer(&name)?;
        assert!(removed);

        let peers = list_peers()?;
        assert!(peers.is_empty());

        Ok(())
    })
}

// ==========================================================================
// Full workflow: init → create files → discover → ingest → modify → refresh
// ==========================================================================

#[tokio::test]
async fn full_local_workflow() -> TestResult {
    with_env_async(|env| async move {
        // 1. Init workspace
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // 2. Create project files
        std::fs::create_dir_all(env.workspace().join("src"))?;
        std::fs::write(env.workspace().join("README.md"), "# My Project")?;
        std::fs::write(env.workspace().join("src/main.rs"), "fn main() {}")?;
        std::fs::write(env.workspace().join("debug.log"), "log data")?;

        // 3. Add ignore pattern for logs
        ignore::add_pattern(env.workspace(), "*.log")?;

        // 4. Discover new files (should skip .log)
        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 2, "expected 2 files, got: {paths:?}");
        assert!(
            paths.iter().any(|p| p.ends_with("README.md")),
            "expected README.md in {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("main.rs")),
            "should find main.rs in {paths:?}"
        );

        // 5. Ingest discovered files
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 2);
        assert!(result.errors.is_empty());
        assert!(!result.cancelled);

        // 6. Verify all files are clean
        for entry in manifest.iter() {
            assert_eq!(entry.state(env.workspace()), FileState::Clean);
        }

        // 7. Modify a file
        std::fs::write(
            env.workspace().join("README.md"),
            "# My Project\n\nUpdated!",
        )?;

        let readme = manifest
            .get_by_path(Path::new("README.md"))
            .ok_or("README.md should be tracked")?;
        assert_eq!(readme.state(env.workspace()), FileState::Modified);

        // 8. Refresh
        let diff = darn.refresh_all(&mut manifest).await;
        assert_eq!(diff.updated.len(), 1);
        assert!(diff.errors.is_empty());

        // 9. After refresh, everything clean again
        for entry in manifest.iter() {
            assert_eq!(entry.state(env.workspace()), FileState::Clean);
        }

        // 10. Save and verify persistence
        darn.save_manifest(&manifest)?;
        let reloaded = darn.load_manifest()?;
        assert_eq!(reloaded.iter().count(), 2);

        Ok(())
    })
    .await
}

// ==========================================================================
// Root directory document contains both files and folders
// ==========================================================================

#[tokio::test]
async fn root_dir_doc_contains_root_level_files() -> TestResult {
    use darn_core::directory::Directory;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // Create root-level files AND subdirectory files
        std::fs::create_dir_all(env.workspace().join("dist"))?;
        std::fs::write(env.workspace().join("package.json"), r#"{"name":"test"}"#)?;
        std::fs::write(
            env.workspace().join("tsconfig.json"),
            r#"{"compilerOptions":{}}"#,
        )?;
        std::fs::write(env.workspace().join("dist/index.js"), "console.log('hi')")?;
        std::fs::write(
            env.workspace().join("dist/style.css"),
            "body { color: red }",
        )?;

        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 4, "expected 4 files, got: {paths:?}");

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 4);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Now read the root directory Automerge document and verify its contents
        let root_dir_id = manifest.root_directory_id();
        let root_doc = darn_core::sedimentree::load_document(darn.subduction(), root_dir_id)
            .await?
            .expect("root directory document should exist");

        let root_dir = Directory::from_automerge(&root_doc)?;

        // Root dir should contain: dist (folder), package.json (file), tsconfig.json (file)
        let entry_names: Vec<String> = root_dir.entries.iter().map(|e| e.name.clone()).collect();

        assert!(
            root_dir.get("dist").is_some(),
            "root dir should contain 'dist' folder. entries: {entry_names:?}"
        );
        assert!(
            root_dir.get("package.json").is_some(),
            "root dir should contain 'package.json'. entries: {entry_names:?}"
        );
        assert!(
            root_dir.get("tsconfig.json").is_some(),
            "root dir should contain 'tsconfig.json'. entries: {entry_names:?}"
        );

        // Verify entry types
        let dist_entry = root_dir.get("dist").expect("dist should exist");
        assert_eq!(
            dist_entry.entry_type,
            darn_core::directory::entry::EntryType::Folder
        );

        let pkg_entry = root_dir
            .get("package.json")
            .expect("package.json should exist");
        assert_eq!(
            pkg_entry.entry_type,
            darn_core::directory::entry::EntryType::File
        );

        Ok(())
    })
    .await
}

// ==========================================================================
// Attribute-based file type classification during ingestion
// ==========================================================================

#[tokio::test]
async fn dist_files_ingested_as_immutable() -> TestResult {
    use darn_core::file::file_type::FileType;

    with_env_async(|env| async move {
        env.init();

        // Add dist/** to immutable patterns in .darn config
        let mut config = darn_core::dotfile::DarnConfig::load(env.workspace())?;
        config.attributes.immutable.push("dist/**".to_string());
        config.save(env.workspace())?;

        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // Create dist/ files and a src/ file
        std::fs::create_dir_all(env.workspace().join("dist"))?;
        std::fs::write(env.workspace().join("dist/tool.js"), "console.log('tool')")?;
        std::fs::write(env.workspace().join("dist/tool.css"), "body { color: red }")?;
        std::fs::write(
            env.workspace().join("dist/chunk-ABC123.js"),
            "export const x = 1",
        )?;
        std::fs::create_dir_all(env.workspace().join("src"))?;
        std::fs::write(env.workspace().join("src/main.ts"), "const x: number = 1")?;
        std::fs::write(env.workspace().join("package.json"), r#"{"name":"test"}"#)?;

        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 5, "expected 5 files, got: {paths:?}");

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 5);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // dist/ files must be Immutable
        let tool_js = manifest
            .get_by_path(Path::new("dist/tool.js"))
            .expect("dist/tool.js should be tracked");
        assert_eq!(
            tool_js.file_type,
            FileType::Immutable,
            "dist/tool.js should be immutable (dist/** pattern)"
        );

        let tool_css = manifest
            .get_by_path(Path::new("dist/tool.css"))
            .expect("dist/tool.css should be tracked");
        assert_eq!(
            tool_css.file_type,
            FileType::Immutable,
            "dist/tool.css should be immutable (dist/** pattern)"
        );

        let chunk = manifest
            .get_by_path(Path::new("dist/chunk-ABC123.js"))
            .expect("dist/chunk-ABC123.js should be tracked");
        assert_eq!(
            chunk.file_type,
            FileType::Immutable,
            "dist/chunk-ABC123.js should be immutable (dist/** pattern)"
        );

        // src/ files must NOT be Immutable
        let main_ts = manifest
            .get_by_path(Path::new("src/main.ts"))
            .expect("src/main.ts should be tracked");
        assert_eq!(
            main_ts.file_type,
            FileType::Text,
            "src/main.ts should be text (auto-detected)"
        );

        // Root-level files must NOT be Immutable (unless matched by other patterns)
        let pkg = manifest
            .get_by_path(Path::new("package.json"))
            .expect("package.json should be tracked");
        assert_eq!(
            pkg.file_type,
            FileType::Text,
            "package.json should be text (auto-detected)"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
async fn default_immutable_patterns_applied_during_ingestion() -> TestResult {
    use darn_core::file::file_type::FileType;

    with_env_async(|env| async move {
        env.init();

        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // Create files that match default immutable patterns
        std::fs::write(
            env.workspace().join("package-lock.json"),
            r#"{"lockfileVersion":3}"#,
        )?;
        std::fs::write(
            env.workspace().join("app.js.map"),
            r#"{"version":3,"mappings":"AAAA"}"#,
        )?;
        std::fs::write(env.workspace().join("bundle.min.js"), "var a=1;")?;
        // And a regular file that should auto-detect as text
        std::fs::write(env.workspace().join("README.md"), "# Hello")?;

        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 4);

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 4);

        let lock = manifest
            .get_by_path(Path::new("package-lock.json"))
            .expect("package-lock.json should be tracked");
        assert_eq!(
            lock.file_type,
            FileType::Immutable,
            "package-lock.json should be immutable (default pattern)"
        );

        let sourcemap = manifest
            .get_by_path(Path::new("app.js.map"))
            .expect("app.js.map should be tracked");
        assert_eq!(
            sourcemap.file_type,
            FileType::Immutable,
            "app.js.map should be immutable (default pattern)"
        );

        let minified = manifest
            .get_by_path(Path::new("bundle.min.js"))
            .expect("bundle.min.js should be tracked");
        assert_eq!(
            minified.file_type,
            FileType::Immutable,
            "bundle.min.js should be immutable (default pattern)"
        );

        let readme = manifest
            .get_by_path(Path::new("README.md"))
            .expect("README.md should be tracked");
        assert_eq!(
            readme.file_type,
            FileType::Text,
            "README.md should be text (auto-detected)"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
async fn nested_dist_files_ingested_as_immutable() -> TestResult {
    use darn_core::file::file_type::FileType;

    with_env_async(|env| async move {
        env.init();

        // Add dist/** to immutable patterns
        let mut config = darn_core::dotfile::DarnConfig::load(env.workspace())?;
        config.attributes.immutable.push("dist/**".to_string());
        config.save(env.workspace())?;

        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        // Create deeply nested dist files
        std::fs::create_dir_all(env.workspace().join("dist/assets/fonts"))?;
        std::fs::write(env.workspace().join("dist/assets/chunk-XYZ.js"), "// chunk")?;
        std::fs::write(
            env.workspace().join("dist/assets/fonts/inter.woff2"),
            vec![0u8; 100],
        )?;
        std::fs::write(env.workspace().join("dist/index.html"), "<html></html>")?;

        let paths = darn.scan_new_files(&manifest)?;
        assert_eq!(paths.len(), 3);

        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;
        assert_eq!(result.new_files.len(), 3);
        assert!(result.errors.is_empty());

        let nested_js = manifest
            .get_by_path(Path::new("dist/assets/chunk-XYZ.js"))
            .expect("nested dist js should be tracked");
        assert_eq!(
            nested_js.file_type,
            FileType::Immutable,
            "dist/assets/chunk-XYZ.js should be immutable"
        );

        // Binary file in dist/ — should still be binary since it's not valid UTF-8
        let font = manifest
            .get_by_path(Path::new("dist/assets/fonts/inter.woff2"))
            .expect("font should be tracked");
        assert_eq!(
            font.file_type,
            FileType::Immutable,
            "dist/assets/fonts/inter.woff2: immutable rule takes priority over auto-detect"
        );

        let html = manifest
            .get_by_path(Path::new("dist/index.html"))
            .expect("html should be tracked");
        assert_eq!(
            html.file_type,
            FileType::Immutable,
            "dist/index.html should be immutable"
        );

        Ok(())
    })
    .await
}

// ==========================================================================
// Sedimentree store / load roundtrip
// ==========================================================================

#[tokio::test]
async fn sedimentree_store_load_roundtrip() -> TestResult {
    use darn_core::file::File;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let original = File::text("hello.txt", "Hello, world!");
        let mut am_doc = original.to_automerge()?;

        let id = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id, &mut am_doc).await?;

        let loaded_doc = darn_core::sedimentree::load_document(darn.subduction(), id)
            .await?
            .expect("document should exist after store");

        let loaded_file = File::from_automerge(&loaded_doc)?;
        assert_eq!(loaded_file.content, original.content);
        assert_eq!(loaded_file.name, original.name);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn sedimentree_store_load_binary_roundtrip() -> TestResult {
    use darn_core::file::File;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let binary_data: Vec<u8> = (0..=255).collect();
        let original = File::binary("data.bin", binary_data);
        let mut am_doc = original.to_automerge()?;

        let id = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id, &mut am_doc).await?;

        let loaded_doc = darn_core::sedimentree::load_document(darn.subduction(), id)
            .await?
            .expect("document should exist");

        let loaded_file = File::from_automerge(&loaded_doc)?;
        assert_eq!(loaded_file.content, original.content);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn sedimentree_store_load_immutable_roundtrip() -> TestResult {
    use darn_core::file::File;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let original = File::immutable("Cargo.lock", "[[package]]\nname = \"darn\"");
        let mut am_doc = original.to_automerge()?;

        let id = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id, &mut am_doc).await?;

        let loaded_doc = darn_core::sedimentree::load_document(darn.subduction(), id)
            .await?
            .expect("document should exist");

        let loaded_file = File::from_automerge(&loaded_doc)?;
        assert_eq!(loaded_file.content, original.content);

        Ok(())
    })
    .await
}

#[tokio::test]
async fn sedimentree_load_nonexistent_returns_none() -> TestResult {
    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let id = darn_core::generate_sedimentree_id();
        let result = darn_core::sedimentree::load_document(darn.subduction(), id).await?;
        assert!(
            result.is_none(),
            "loading a never-stored ID should return None"
        );

        Ok(())
    })
    .await
}

// ==========================================================================
// Sedimentree compute_digest determinism
// ==========================================================================

#[tokio::test]
async fn sedimentree_compute_digest_deterministic() -> TestResult {
    use darn_core::file::File;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let doc = File::text("test.txt", "deterministic content");
        let mut am_doc = doc.to_automerge()?;

        let id = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id, &mut am_doc).await?;

        let digest1 = darn_core::sedimentree::compute_digest(darn.subduction(), id).await?;
        let digest2 = darn_core::sedimentree::compute_digest(darn.subduction(), id).await?;

        assert_eq!(digest1, digest2, "digest must be deterministic");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn sedimentree_compute_digest_differs_for_different_content() -> TestResult {
    use darn_core::file::File;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let doc_a = File::text("a.txt", "content A");
        let mut am_a = doc_a.to_automerge()?;
        let id_a = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id_a, &mut am_a).await?;

        let doc_b = File::text("b.txt", "content B");
        let mut am_b = doc_b.to_automerge()?;
        let id_b = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id_b, &mut am_b).await?;

        let digest_a = darn_core::sedimentree::compute_digest(darn.subduction(), id_a).await?;
        let digest_b = darn_core::sedimentree::compute_digest(darn.subduction(), id_b).await?;

        assert_ne!(
            digest_a, digest_b,
            "different content should produce different digests"
        );

        Ok(())
    })
    .await
}

#[tokio::test]
async fn sedimentree_add_changes_stores_incremental() -> TestResult {
    use automerge::{ReadDoc, transaction::Transactable};
    use darn_core::file::File;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;

        let doc = File::text("incremental.txt", "version 1");
        let mut am_doc = doc.to_automerge()?;
        let id = darn_core::generate_sedimentree_id();
        darn_core::sedimentree::store_document(darn.subduction(), id, &mut am_doc).await?;

        // Make an incremental change
        let heads_before: Vec<_> = am_doc.get_heads().into_iter().collect();
        am_doc
            .transact::<_, _, automerge::AutomergeError>(|tx| {
                let (_, content_id) = tx.get(automerge::ROOT, "content")?.expect("content field");
                let old_len = tx.text(&content_id)?.chars().count();
                tx.splice_text(
                    &content_id,
                    0,
                    old_len.try_into().unwrap_or(isize::MAX),
                    "version 2",
                )?;
                Ok(())
            })
            .map_err(|f| f.error)?;

        let count =
            darn_core::sedimentree::add_changes(darn.subduction(), id, &mut am_doc, &heads_before)
                .await?;
        assert_eq!(count, 1, "should have stored exactly 1 new change");

        let loaded = darn_core::sedimentree::load_document(darn.subduction(), id)
            .await?
            .expect("should exist");
        let loaded_file = File::from_automerge(&loaded)?;
        assert_eq!(
            loaded_file.content,
            darn_core::file::content::Content::Text("version 2".into())
        );

        Ok(())
    })
    .await
}

// ==========================================================================
// Discovery: scan tolerates unreadable entries
// ==========================================================================

#[cfg(unix)]
#[tokio::test]
async fn scan_tolerates_unreadable_file() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let manifest = darn.load_manifest()?;

        std::fs::write(env.workspace().join("readable.txt"), "hello")?;
        let unreadable = env.workspace().join("unreadable.txt");
        std::fs::write(&unreadable, "secret")?;
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))?;

        // scan_new_files only lists candidates — it doesn't read content
        let paths = darn.scan_new_files(&manifest)?;
        assert!(
            paths.iter().any(|p| p.ends_with("readable.txt")),
            "readable file should be discovered"
        );

        // Restore permissions for cleanup
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644))?;

        Ok(())
    })
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn ingest_reports_errors_for_unreadable_files() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    with_env_async(|env| async move {
        env.init();
        let darn = env.open().await;
        let mut manifest = darn.load_manifest()?;

        std::fs::write(env.workspace().join("good.txt"), "readable")?;
        let bad = env.workspace().join("bad.txt");
        std::fs::write(&bad, "unreadable")?;
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000))?;

        let paths = darn.scan_new_files(&manifest)?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = darn
            .ingest_files(paths, &mut manifest, false, |_| {}, &cancel)
            .await?;

        assert!(
            result.new_files.iter().any(|p| p.ends_with("good.txt")),
            "readable file should be ingested"
        );
        assert!(
            result.errors.iter().any(|(p, _)| p.ends_with("bad.txt")),
            "unreadable file should produce an error, errors: {:?}",
            result.errors
        );

        // Restore permissions for cleanup
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644))?;

        Ok(())
    })
    .await
}
