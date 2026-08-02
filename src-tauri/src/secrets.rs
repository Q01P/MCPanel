//! Secret env values live in the OS credential manager (Keychain /
//! Credential Manager / Secret Service) under `mcpanel/<server>/<key>` —
//! never in config, DB, or logs. Config and DB only ever hold the
//! [`EnvValue::Secret`] marker; the real value is resolved just-in-time at
//! spawn.
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

const SERVICE: &str = "mcpanel";

fn entry(server_name: &str, key: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(
        SERVICE,
        &format!("{server_name}/{key}"),
    )?)
}

pub fn store_secret(server_name: &str, key: &str, value: &str) -> AppResult<()> {
    entry(server_name, key)?.set_password(value)?;
    Ok(())
}

pub fn get_secret(server_name: &str, key: &str) -> AppResult<String> {
    Ok(entry(server_name, key)?.get_password()?)
}

/// Deleting an already-absent credential is not an error — the desired end
/// state is reached either way.
pub fn delete_secret(server_name: &str, key: &str) -> AppResult<()> {
    match entry(server_name, key)?.delete_credential() {
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
            EnvValue::Secret => {
                get_secret(&record.name, key).map(|resolved| (key.clone(), resolved))
            }
        })
        .collect()
}
