//! Secrets tests. Store round-trips need a live OS credential store (Secret
//! Service on Linux); when none is reachable those tests skip rather than
//! fail. The missing-secret start-failure path needs no store at all — a
//! missing/unreachable credential must fail the start either way.
//!
//! Keyring entries are keyed by server id; tests that talk to the store
//! directly use negative ids, which real servers (AUTOINCREMENT, starting at
//! 1) can never have — no collision with a user's actual entries.

#![cfg(unix)]

mod common;

use std::collections::BTreeMap;

use common::test_state;
use mcpanel_lib::commands::lifecycle;
use mcpanel_lib::db::{self, EnvValue, NewServer, ServerRecord};
use mcpanel_lib::error::AppError;
use mcpanel_lib::secrets;
use mcpanel_lib::state::{AppState, ServerStatus};

/// Detect a usable credential store; skip (not fail) store-dependent tests
/// on headless machines.
fn store_available() -> bool {
    match secrets::store_secret(-1, "PROBE", "1") {
        Ok(()) => {
            let _ = secrets::delete_secret(-1, "PROBE");
            true
        }
        Err(_) => false,
    }
}

async fn add_with_secret_marker(state: &AppState, name: &str, key: &str) -> ServerRecord {
    lifecycle::add(
        state,
        NewServer {
            name: name.into(),
            command: "true".into(),
            args: vec![],
            env: BTreeMap::from([(key.into(), EnvValue::Secret)]),
            cwd: None,
            auto_start: false,
        },
    )
    .await
    .expect("add server")
}

#[test]
fn store_get_delete_round_trip() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    secrets::store_secret(-100, "API_KEY", "hunter2").expect("store");
    assert_eq!(secrets::get_secret(-100, "API_KEY").expect("get"), "hunter2");

    secrets::delete_secret(-100, "API_KEY").expect("delete");
    assert!(secrets::get_secret(-100, "API_KEY").is_err());
    // Deleting again is fine — absent is the desired end state.
    secrets::delete_secret(-100, "API_KEY").expect("idempotent delete");
}

#[test]
fn resolve_env_mixes_plain_and_secret() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    secrets::store_secret(-101, "TOKEN", "s3cr3t").expect("store");
    let record = ServerRecord {
        id: -101,
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

    secrets::delete_secret(-101, "TOKEN").expect("cleanup");
}

/// A server configured with a secret that can't be resolved must not launch.
/// This holds with or without a credential store (NoEntry vs platform
/// failure — both are errors).
#[tokio::test]
async fn start_fails_when_secret_is_unresolvable() {
    let state = test_state();
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

/// Renaming a server must not orphan its credentials — entries are keyed by
/// id, so the rename is invisible to the keyring.
#[tokio::test]
async fn secrets_survive_rename() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    let state = test_state();
    let record = add_with_secret_marker(&state, "rename-me", "API_KEY").await;
    lifecycle::set_secret(&state, record.id, "API_KEY".into(), "hunter2".into())
        .await
        .expect("set secret");

    let mut renamed = record.clone();
    renamed.name = "renamed".into();
    lifecycle::update(&state, renamed).await.expect("rename");

    assert_eq!(
        secrets::get_secret(record.id, "API_KEY").expect("still reachable"),
        "hunter2"
    );
    let id = record.id;
    let refreshed = state
        .with_db(move |conn| db::get_server(conn, id))
        .await
        .expect("get renamed");
    assert_eq!(
        secrets::resolve_env(&refreshed).expect("resolve")["API_KEY"],
        "hunter2"
    );

    lifecycle::remove(&state, record.id).await.expect("cleanup");
}

/// Removing a server deletes its keyring entries along with the config row.
#[tokio::test]
async fn remove_deletes_keyring_entries() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    let state = test_state();
    let record = add_with_secret_marker(&state, "doomed-secret", "API_KEY").await;
    lifecycle::set_secret(&state, record.id, "API_KEY".into(), "hunter2".into())
        .await
        .expect("set secret");

    lifecycle::remove(&state, record.id).await.expect("remove");
    assert!(
        secrets::get_secret(record.id, "API_KEY").is_err(),
        "keyring entry must be gone"
    );
}

/// A server recreated under a deleted server's name gets a fresh id
/// (AUTOINCREMENT) and must not resolve the old server's credentials.
#[tokio::test]
async fn recreated_server_does_not_inherit_secrets() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    let state = test_state();
    let first = add_with_secret_marker(&state, "reborn", "API_KEY").await;
    lifecycle::set_secret(&state, first.id, "API_KEY".into(), "hunter2".into())
        .await
        .expect("set secret");
    lifecycle::remove(&state, first.id).await.expect("remove");

    let second = add_with_secret_marker(&state, "reborn", "API_KEY").await;
    assert_ne!(second.id, first.id, "ids must never be reused");
    let err = secrets::resolve_env(&second).expect_err("no inherited credential");
    assert!(matches!(err, AppError::Keyring(_)), "got: {err:?}");
}

/// Legacy name-keyed entries move to the id scheme exactly once; reruns are
/// no-ops.
#[tokio::test]
async fn startup_migration_moves_name_keyed_entries_idempotently() {
    if !store_available() {
        eprintln!("skipping: no OS credential store available");
        return;
    }

    let state = test_state();
    let record = add_with_secret_marker(&state, "mcpanel-test-legacy", "API_KEY").await;
    // Plant the entry the way a pre-migration install would have stored it.
    keyring::Entry::new("mcpanel", "mcpanel-test-legacy/API_KEY")
        .expect("legacy entry")
        .set_password("hunter2")
        .expect("plant legacy secret");

    let records = vec![record.clone()];
    secrets::migrate_name_keyed_secrets(&records);
    assert_eq!(
        secrets::get_secret(record.id, "API_KEY").expect("migrated"),
        "hunter2"
    );
    assert!(
        matches!(
            keyring::Entry::new("mcpanel", "mcpanel-test-legacy/API_KEY")
                .expect("legacy entry")
                .get_password(),
            Err(keyring::Error::NoEntry)
        ),
        "legacy entry must be deleted"
    );

    // Rerun: still the same value, still no legacy entry.
    secrets::migrate_name_keyed_secrets(&records);
    assert_eq!(
        secrets::get_secret(record.id, "API_KEY").expect("still migrated"),
        "hunter2"
    );

    lifecycle::remove(&state, record.id).await.expect("cleanup");
}
