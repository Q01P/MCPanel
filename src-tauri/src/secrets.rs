//! Secret env values live in the OS credential manager (Keychain /
//! Credential Manager / Secret Service) under `mcpanel/<id>/<key>` — never
//! in config, DB, or logs. Config and DB only ever hold the
//! [`EnvValue::Secret`] marker; the real value is resolved just-in-time at
//! spawn.
//!
//! Entries are keyed by server *id*, not name: names are mutable (renames
//! must not orphan credentials) and ids are never reused (AUTOINCREMENT,
//! schema v2) so a recreated server cannot inherit a deleted server's
//! secrets. Installs that stored entries under the legacy `<name>/<key>`
//! account are moved by [`migrate_name_keyed_secrets`] at startup. The
//! keyring cannot be enumerated, so entries orphaned *before* that scheme
//! change (by renames or removals under the old keying) stay unreachable. A
//! server literally named e.g. `"5"` could collide with an id-keyed account
//! under the legacy scheme — accepted as a theoretical ambiguity.
//!
//! Redaction rule: functions here take and return values, but nothing in
//! this crate may ever put a secret value into a tracing event, an error
//! message, or an [`AppEvent`](crate::state::AppEvent). Log keys, not
//! values.
//!
//! keyring is a synchronous API — call these from `spawn_blocking`, never
//! from a runtime thread.

use std::collections::HashMap;

use crate::db::{EnvValue, ServerRecord};
use crate::error::AppResult;
use crate::state::ServerId;

const SERVICE: &str = "mcpanel";

fn entry(account: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, account)?)
}

fn account(id: ServerId, key: &str) -> String {
    format!("{id}/{key}")
}

pub fn store_secret(id: ServerId, key: &str, value: &str) -> AppResult<()> {
    entry(&account(id, key))?.set_password(value)?;
    Ok(())
}

pub fn get_secret(id: ServerId, key: &str) -> AppResult<String> {
    Ok(entry(&account(id, key))?.get_password()?)
}

/// Deleting an already-absent credential is not an error — the desired end
/// state is reached either way.
pub fn delete_secret(id: ServerId, key: &str) -> AppResult<()> {
    delete_account(&account(id, key))
}

fn delete_account(account: &str) -> AppResult<()> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(other) => Err(other.into()),
    }
}

/// Just-in-time env resolution at spawn: plain values pass through, secret
/// markers are fetched from the credential store. A missing secret fails the
/// start — a server must never silently launch without credentials it was
/// configured to have.
pub fn resolve_env(record: &ServerRecord) -> AppResult<HashMap<String, String>> {
    record
        .env
        .iter()
        .map(|(key, value)| match value {
            EnvValue::Plain { value } => Ok((key.clone(), value.clone())),
            EnvValue::Secret => get_secret(record.id, key).map(|resolved| (key.clone(), resolved)),
        })
        .collect()
}

/// Best-effort removal of every secret-marked key for a server being
/// deleted. Per-key failures are logged (key only) and skipped — a flaky
/// credential store must not block `remove_server`; ids are never reused, so
/// a leftover entry is unreachable garbage, not a security hole.
pub fn delete_server_secrets(record: &ServerRecord) {
    let keys: Vec<&String> = record
        .env
        .iter()
        .filter(|(_, value)| **value == EnvValue::Secret)
        .map(|(key, _)| key)
        .collect();
    delete_keys_best_effort(record.id, keys);
}

/// Best-effort delete of the given keys' entries; per-key failures are
/// logged (key only) and skipped.
pub fn delete_keys_best_effort<'a>(id: ServerId, keys: impl IntoIterator<Item = &'a String>) {
    for key in keys {
        if let Err(error) = delete_secret(id, key) {
            tracing::warn!(
                target: "app::secrets",
                server_id = id,
                key,
                %error,
                "failed to delete keyring entry"
            );
        }
    }
}

/// One-time move of legacy name-keyed entries (`<name>/<key>`) to the id
/// scheme. Idempotent and best-effort: reruns find the id entry already
/// present (or nothing legacy to move) and cost one probe per secret key.
/// Runs synchronously at startup, strictly before anything can resolve env.
pub fn migrate_name_keyed_secrets(records: &[ServerRecord]) {
    for record in records {
        for (key, value) in &record.env {
            if *value != EnvValue::Secret {
                continue;
            }
            let legacy = format!("{}/{}", record.name, key);
            if let Err(error) = migrate_one(record.id, key, &legacy) {
                tracing::warn!(
                    target: "app::secrets",
                    server_id = record.id,
                    key,
                    %error,
                    "failed to migrate legacy keyring entry"
                );
            }
        }
    }
}

fn migrate_one(id: ServerId, key: &str, legacy: &str) -> AppResult<()> {
    if get_secret(id, key).is_ok() {
        // Already migrated; clear any stale legacy copy.
        return delete_account(legacy);
    }
    match entry(legacy)?.get_password() {
        Ok(value) => {
            store_secret(id, key, &value)?;
            delete_account(legacy)
        }
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(other) => Err(other.into()),
    }
}
