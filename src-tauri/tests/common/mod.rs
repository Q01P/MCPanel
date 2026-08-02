//! Helpers shared by the integration suites. Every suite compiles its own
//! copy of this module and uses a subset of it, so unused items are expected
//! per-crate — hence the blanket allow.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

use mcpanel_lib::commands::lifecycle;
use mcpanel_lib::db::{self, NewServer};
use mcpanel_lib::mcp::process::{ManagedChild, ProcessConfig, spawn};
use mcpanel_lib::mcp::protocol::ProtocolHandle;
use mcpanel_lib::mcp::stream::{ChildStreams, StreamEvent, attach};
use mcpanel_lib::server::{AuthToken, Gateway};
use mcpanel_lib::state::{AppState, ServerId};

/// Bound for handshakes and requests that are expected to *succeed*. Cold CI
/// runners can take well over the old 500 ms to spawn a process and get the
/// first response back; tests that expect a timeout pass their own short one.
pub const HAPPY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_mock-mcp-server").to_string()
}

/// Liveness check that dodges zombie false-positives: on Linux, a pid whose
/// /proc stat state is Z (zombie) or X (dead) counts as dead. Other Unix
/// (the macOS CI leg) has no /proc — there `kill(pid, 0)` alone decides.
pub fn alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // State is the first field after the parenthesized comm (which may
        // itself contain spaces or parens).
        let state = stat
            .rfind(')')
            .and_then(|i| stat[i + 1..].trim_start().chars().next());
        !matches!(state, Some('Z') | Some('X') | None)
    }
    #[cfg(not(target_os = "linux"))]
    true
}

/// Poll `cond` for up to 5 s; panic naming `what` on timeout.
pub async fn wait_for<F: Fn() -> bool>(what: &str, cond: F) {
    for _ in 0..100 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn test_state() -> AppState {
    AppState::new(db::open_in_memory().expect("in-memory db"))
}

/// Host the gateway tests bind in the Host header; nothing listens there —
/// requests go through `tower::ServiceExt::oneshot`.
pub const TEST_HOST: &str = "127.0.0.1:4321";

pub fn test_gateway() -> Gateway {
    Gateway {
        token: AuthToken::generate(),
        app: test_state(),
        host: TEST_HOST.into(),
    }
}

/// Config row pointing at the fixture binary.
pub fn fixture_server(name: &str, args: &[&str], auto_start: bool) -> NewServer {
    NewServer {
        name: name.into(),
        command: fixture_path(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: BTreeMap::new(),
        cwd: None,
        auto_start,
    }
}

pub async fn add_fixture(state: &AppState, name: &str, args: &[&str]) -> ServerId {
    add_fixture_auto(state, name, args, false).await
}

pub async fn add_fixture_auto(
    state: &AppState,
    name: &str,
    args: &[&str],
    auto_start: bool,
) -> ServerId {
    lifecycle::add(state, fixture_server(name, args, auto_start))
        .await
        .expect("insert server")
        .id
}

/// Spawn the fixture through the supervisor and attach its stream pipelines.
pub fn spawn_fixture(args: &[&str]) -> (ManagedChild, ChildStreams) {
    let mut managed = spawn(&ProcessConfig {
        command: fixture_path(),
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    })
    .expect("spawn fixture");
    let streams = attach(&mut managed.child).expect("attach streams");
    (managed, streams)
}

/// [`spawn_fixture`] plus the protocol layer; `request_timeout` bounds the
/// handshake and every request. The stderr pipeline is returned too — most
/// callers drop it, the ping test reads the fixture's confirmation from it.
pub fn spawn_connected(
    args: &[&str],
    request_timeout: Duration,
) -> (
    ManagedChild,
    ProtocolHandle,
    tokio::sync::mpsc::Receiver<StreamEvent>,
) {
    let (mut managed, streams) = spawn_fixture(args);
    let stdin = managed.child.stdin.take().expect("piped stdin");
    let handle = mcpanel_lib::mcp::protocol::connect(stdin, streams.stdout, request_timeout);
    (managed, handle, streams.stderr)
}

/// Spawn the fixture with `--spawn-child` through the supervisor and return
/// (managed child, grandchild pid read from the fixture's stdout).
pub async fn spawn_tree() -> (ManagedChild, i32) {
    let mut managed = spawn(&ProcessConfig {
        command: fixture_path(),
        args: vec!["--spawn-child".into()],
        ..Default::default()
    })
    .expect("spawn fixture");

    let stdout = managed.child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let grandchild: i32 = lines
        .next_line()
        .await
        .expect("read grandchild pid")
        .expect("fixture printed a pid line")
        .trim()
        .parse()
        .expect("pid parses");

    (managed, grandchild)
}
