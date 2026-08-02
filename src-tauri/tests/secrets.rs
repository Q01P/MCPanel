//! Secrets tests. Store round-trips need a live OS credential store (Secret
//! Service on Linux); when none is reachable those tests skip rather than
//! fail. The missing-secret start-failure path needs no store at all — a
//! missing/unreachable credential must fail the start either way.

#![cfg(unix)]

use std::collections::BTreeMap;

use mcpanel_lib::commands::lifecycle;
use mcpanel_lib::db::{self, EnvValue, NewServer, ServerRecord};
use mcpanel_lib::error::AppError;
use mcpanel_lib::secrets;
use mcpanel_lib::state::{AppState, ServerStatus};

/// Detect a usable credential store; skip (not fail) store-dependent tests
/// on headless machines.
fn store_available() -> bool {
    match secrets::store_secret("mcpanel-test-probe", "PROBE", "1") {
        Ok(()) => {
            let _ = secrets::delete_secret("mcpanel-test-probe", "PROBE");
            true
        }
        Err(_) => false,
    }
}

#[test]
fn store_get_delete_round_trip() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    secrets::store_secret("mcpanel-test", "API_KEY", "hunter2").expect("store");
    assert_eq!(
        secrets::get_secret("mcpanel-test", "API_KEY").expect("get"),
        "hunter2"
    );

    secrets::delete_secret("mcpanel-test", "API_KEY").expect("delete");
    assert!(secrets::get_secret("mcpanel-test", "API_KEY").is_err());
    // Deleting again is fine — absent is the desired end state.
    secrets::delete_secret("mcpanel-test", "API_KEY").expect("idempotent delete");
}

#[test]
fn resolve_env_mixes_plain_and_secret() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    secrets::store_secret("mcpanel-test-resolve", "TOKEN", "s3cr3t").expect("store");
    let record = ServerRecord {
        id: 1,
        name: "mcpanel-test-resolve".into(),
        command: "true".into(),
        args: vec![],
        env: BTreeMap::from([
            ("PLAIN".into(), EnvValue::Plain { value: "visible".into() }),
            ("TOKEN".into(), EnvValue::Secret),
        ]),
        cwd: None,
        auto_start: false,
    };

    let resolved = secrets::resolve_env(&record).expect("resolve");
    assert_eq!(resolved["PLAIN"], "visible");
    assert_eq!(resolved["TOKEN"], "s3cr3t");

    secrets::delete_secret("mcpanel-test-resolve", "TOKEN").expect("cleanup");
}

/// A server configured with a secret that can't be resolved must not launch.
/// This holds with or without a credential store (NoEntry vs platform
/// failure — both are errors).
#[tokio::test]
async fn start_fails_when_secret_is_unresolvable() {
    let state = AppState::new(db::open_in_memory().expect("db"));
    let id = lifecycle::add(
        &state,
        NewServer {
            name: "mcpanel-test-missing-secret".into(),
            command: env!("CARGO_BIN_EXE_mock-mcp-server").into(),
            args: vec![],
            env: BTreeMap::from([("MISSING".into(), EnvValue::Secret)]),
            cwd: None,
            auto_start: false,
        },
    )
    .await
    .expect("add")
    .id;

    let err = lifecycle::start(&state, id).await.expect_err("must fail");
    assert!(matches!(err, AppError::Keyring(_)), "got: {err:?}");
    assert!(matches!(state.status(id), ServerStatus::Errored { .. }));
}
