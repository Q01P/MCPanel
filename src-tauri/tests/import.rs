//! Import suite: config files on disk → servers in the DB.
//!
//! Deliberately free of `common` (and so of its Unix-only process helpers):
//! nothing here spawns a child, so this suite compiles and runs on Windows
//! too. Secret *storage* needs a live credential store and is opt-in exactly
//! as in `secrets.rs`; the paths that don't touch the keyring run everywhere.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use mcpanel_lib::db::{self, EnvValue};
use mcpanel_lib::import;
use mcpanel_lib::state::AppState;

fn test_state() -> AppState {
    AppState::new(db::open_in_memory().expect("in-memory db"))
}

/// Scratch directory unique per test, cleaned up by [`Scratch`]'s drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mcpanel-import-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn write(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).expect("write config");
        path.display().to_string()
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Opt-in *and* verified, exactly as in `secrets.rs`: the env var alone
/// would turn a machine without a working store into a failure rather than a
/// skip (a headless container has no Secret Service), and probing without
/// the env var can hang a macOS runner on a keychain prompt.
fn keyring_available() -> bool {
    if std::env::var_os("MCPANEL_TEST_KEYRING").is_none() {
        return false;
    }
    match mcpanel_lib::secrets::store_secret(-1, "PROBE", "1") {
        Ok(()) => {
            let _ = mcpanel_lib::secrets::delete_secret(-1, "PROBE");
            true
        }
        Err(_) => false,
    }
}

#[tokio::test]
async fn imports_selected_servers_and_leaves_the_rest() {
    let scratch = Scratch::new();
    let path = scratch.write(
        "claude_desktop_config.json",
        r#"{"mcpServers":{
            "files":{"command":"npx","args":["-y","@mcp/fs","/tmp"],"env":{"LOG":"debug"}},
            "memory":{"command":"uvx","args":["mcp-memory"]}
        }}"#,
    );
    let state = test_state();

    let outcome = import::import(&state, path, vec!["files".into()])
        .await
        .expect("import runs");
    assert_eq!(outcome.failed.len(), 0, "{:?}", outcome.failed);
    assert_eq!(outcome.imported.len(), 1);
    assert_eq!(outcome.imported[0].name, "files");

    let servers = state.with_db(db::list_servers).await.expect("list");
    assert_eq!(servers.len(), 1, "unselected entries must not be imported");
    let files = &servers[0];
    assert_eq!(files.command, "npx");
    assert_eq!(files.args, ["-y", "@mcp/fs", "/tmp"]);
    assert_eq!(
        files.env["LOG"],
        EnvValue::Plain {
            value: "debug".into()
        }
    );
    assert!(
        !files.auto_start,
        "import must never arm a server to start on launch"
    );
}

#[tokio::test]
async fn a_name_collision_is_suffixed_not_rejected() {
    let scratch = Scratch::new();
    let config = r#"{"mcpServers":{"files":{"command":"npx"}}}"#;
    let path = scratch.write("mcp.json", config);
    let state = test_state();

    for expected in ["files", "files (2)", "files (3)"] {
        let outcome = import::import(&state, path.clone(), vec!["files".into()])
            .await
            .expect("import runs");
        assert_eq!(outcome.failed.len(), 0, "{:?}", outcome.failed);
        assert_eq!(outcome.imported[0].name, expected);
        // The source name is reported unchanged, so the UI can be honest
        // about the rename.
        assert_eq!(outcome.imported[0].source_name, "files");
    }
    assert_eq!(state.with_db(db::list_servers).await.unwrap().len(), 3);
}

#[tokio::test]
async fn one_failure_does_not_abort_the_run() {
    let scratch = Scratch::new();
    // The middle entry's cwd does not exist: validation rejects it, and the
    // entries either side must still land.
    let path = scratch.write(
        "mcp.json",
        r#"{"mcpServers":{
            "a":{"command":"npx"},
            "b":{"command":"npx","cwd":"/nonexistent/mcpanel/test/dir"},
            "c":{"command":"npx"}
        }}"#,
    );
    let state = test_state();

    let outcome = import::import(
        &state,
        path,
        vec!["a".into(), "b".into(), "c".into(), "ghost".into()],
    )
    .await
    .expect("import runs");

    let imported: Vec<_> = outcome.imported.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(imported, ["a", "c"]);
    assert_eq!(outcome.failed.len(), 2);
    assert!(
        outcome
            .failed
            .iter()
            .any(|f| f.name == "b" && f.reason.contains("working directory"))
    );
    assert!(
        outcome
            .failed
            .iter()
            .any(|f| f.name == "ghost" && f.reason.contains("no longer present"))
    );
}

#[tokio::test]
async fn reading_a_named_file_reports_conflicts_and_skips() {
    let scratch = Scratch::new();
    let path = scratch.write(
        "mcp.json",
        r#"{"mcpServers":{
            "files":{"command":"npx"},
            "hosted":{"url":"https://example.test/mcp"}
        }}"#,
    );
    let state = test_state();
    import::import(&state, path.clone(), vec!["files".into()])
        .await
        .expect("first import");

    let discovered = import::read_file(&state, path).await.expect("read file");
    assert_eq!(discovered.servers.len(), 1);
    assert!(
        discovered.servers[0].conflicts,
        "an already-imported name must be flagged"
    );
    assert_eq!(discovered.skipped.len(), 1);
    assert!(discovered.skipped[0].reason.contains("stdio only"));
}

#[tokio::test]
async fn a_missing_or_malformed_file_is_an_error_when_named_explicitly() {
    let scratch = Scratch::new();
    let state = test_state();

    let missing = scratch.path().join("absent.json").display().to_string();
    assert_eq!(
        import::read_file(&state, missing).await.unwrap_err().code(),
        "io"
    );

    let junk = scratch.write("junk.json", "{ not json");
    assert_eq!(
        import::read_file(&state, junk).await.unwrap_err().code(),
        "json"
    );
}

#[tokio::test]
async fn credentials_move_into_the_keyring_not_the_database() {
    if !keyring_available() {
        eprintln!("skipping: set MCPANEL_TEST_KEYRING=1 to exercise the credential store");
        return;
    }
    let scratch = Scratch::new();
    let path = scratch.write(
        "mcp.json",
        r#"{"mcpServers":{"gh":{"command":"npx","env":{
            "GITHUB_TOKEN":"ghp_live_value",
            "LOG_LEVEL":"debug"
        }}}}"#,
    );
    let state = test_state();

    let outcome = import::import(&state, path, vec!["gh".into()])
        .await
        .expect("import runs");
    assert_eq!(outcome.failed.len(), 0, "{:?}", outcome.failed);
    assert_eq!(outcome.imported[0].secrets_stored, 1);

    let record = state
        .with_db(db::list_servers)
        .await
        .expect("list")
        .remove(0);
    assert_eq!(record.env["GITHUB_TOKEN"], EnvValue::Secret);
    assert_eq!(
        record.env["LOG_LEVEL"],
        EnvValue::Plain {
            value: "debug".into()
        }
    );
    // The token itself must be nowhere in the persisted row.
    let serialized = serde_json::to_string(&record).expect("serialize record");
    assert!(!serialized.contains("ghp_live_value"));

    let resolved = mcpanel_lib::secrets::resolve_env(&record).expect("resolve env");
    assert_eq!(resolved["GITHUB_TOKEN"], "ghp_live_value");

    mcpanel_lib::secrets::delete_secret(record.id, "GITHUB_TOKEN").expect("cleanup");
}
