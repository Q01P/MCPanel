//! Import server definitions from other MCP clients' config files.
//!
//! Every MCP client stores its servers in essentially the same JSON shape: a
//! map of name → `{command, args, env}` under `mcpServers` (Claude Desktop,
//! Claude Code, Cursor, Windsurf) or `servers` (VS Code). This module finds
//! those files, parses them into [`ImportCandidate`]s, and applies the
//! selected ones as MCPanel servers.
//!
//! Two rules shape the design:
//!
//! **Secret values never cross the IPC boundary.** A preview reports
//! credential-looking keys by *name* only ([`ImportCandidate::secret_keys`]);
//! the values stay in [`ParsedServer`], which is deliberately not
//! `Serialize`. Import re-reads the file on this side and hands those values
//! straight to the OS keyring, so an API key in a foreign config never
//! reaches the webview, an event, or our DB — it only ever moves from their
//! plaintext file into the credential store.
//!
//! **Nothing is silently rewritten.** An entry we cannot honour is reported
//! as a [`SkippedEntry`] with the reason, and anything survivable but
//! surprising (an unresolved `${input:…}` placeholder, an `envFile`) becomes
//! a note on the candidate. Config we don't understand is never guessed at.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::commands::lifecycle;
use crate::db::{self, EnvValue, NewServer};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, ServerId};

/// A config file we found and could read, with what it offers.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DiscoveredConfig {
    /// Which client this file belongs to, for display ("Claude Desktop").
    pub client: String,
    pub path: String,
    pub servers: Vec<ImportCandidate>,
    pub skipped: Vec<SkippedEntry>,
}

/// One importable server, as shown in the preview. Carries no secret values.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ImportCandidate {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Plain environment variables, shown as-is.
    pub env: BTreeMap<String, String>,
    /// Keys classified as credentials: names only. Their values are withheld
    /// here and go from the file straight to the keyring at import.
    pub secret_keys: Vec<String>,
    /// Non-blocking warnings worth showing next to the entry.
    pub notes: Vec<String>,
    /// A server with this name already exists; importing will suffix it.
    pub conflicts: bool,
}

/// An entry we found but cannot import, and why.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SkippedEntry {
    pub name: String,
    pub reason: String,
}

/// A parsed entry: the previewable candidate plus the secret values held
/// back from it. Not `Serialize` — that is the type-level guard keeping
/// credentials off the IPC boundary.
#[derive(Clone, Debug)]
struct ParsedServer {
    candidate: ImportCandidate,
    secrets: BTreeMap<String, String>,
}

/// Everything one config file yielded.
#[derive(Clone, Debug)]
pub struct ParsedConfig {
    servers: Vec<ParsedServer>,
    skipped: Vec<SkippedEntry>,
}

impl ParsedConfig {
    pub fn candidates(&self) -> Vec<ImportCandidate> {
        self.servers.iter().map(|s| s.candidate.clone()).collect()
    }

    pub fn skipped(&self) -> &[SkippedEntry] {
        &self.skipped
    }

    /// Nothing to offer — neither importable nor explained. Discovery drops
    /// these files rather than listing an empty source.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.skipped.is_empty()
    }
}

/// Key segments that mark an environment variable as a credential. Matched
/// per segment (split on non-alphanumerics), so `OPENAI_API_KEY` matches on
/// `KEY` while `MONKEY_MODE` does not.
///
/// The bias is deliberately toward over-classifying: a false positive merely
/// routes a harmless value through the keyring, while a false negative
/// leaves a real token sitting in the config DB.
const SECRET_SEGMENTS: &[&str] = &[
    "KEY",
    "KEYS",
    "APIKEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "CREDENTIALS",
    "AUTH",
    "PAT",
];

fn is_secret_key(key: &str) -> bool {
    key.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|segment| {
            let upper = segment.to_ascii_uppercase();
            SECRET_SEGMENTS.contains(&upper.as_str())
        })
}

/// `${input:api-key}` (VS Code) and `${env:FOO}` resolve inside the client
/// that wrote them; we have no way to expand them.
fn has_placeholder(value: &str) -> bool {
    value.contains("${")
}

/// JSON scalars become strings; objects, arrays, and null do not.
fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Parse one client config file's text.
///
/// Errors only when the file is not JSON or holds no server table at all —
/// individual bad entries become [`SkippedEntry`]s so one broken server
/// never costs the user the rest of the file.
pub fn parse_config(text: &str) -> AppResult<ParsedConfig> {
    let root: Value = serde_json::from_str(text)?;
    // `mcpServers` is the near-universal key; VS Code uses `servers`.
    let table = root
        .get("mcpServers")
        .or_else(|| root.get("servers"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppError::InvalidInput(
                "not an MCP client config: no \"mcpServers\" or \"servers\" object".into(),
            )
        })?;

    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    for (name, entry) in table {
        if name.trim().is_empty() {
            skipped.push(SkippedEntry {
                name: "(unnamed)".into(),
                reason: "entry has an empty name".into(),
            });
            continue;
        }
        match parse_entry(name, entry) {
            Ok(parsed) => servers.push(parsed),
            Err(reason) => skipped.push(SkippedEntry {
                name: name.clone(),
                reason,
            }),
        }
    }
    Ok(ParsedConfig { servers, skipped })
}

/// One `name → {…}` entry. `Err` carries the user-facing reason it can't be
/// imported.
fn parse_entry(name: &str, entry: &Value) -> Result<ParsedServer, String> {
    let object = entry
        .as_object()
        .ok_or_else(|| "entry is not an object".to_string())?;

    // Remote servers are recognised and named explicitly rather than falling
    // through a generic "no command" — the distinction is the whole reason
    // they can't be imported yet.
    if let Some(url) = object.get("url").and_then(Value::as_str) {
        return Err(format!("remote server ({url}) — MCPanel speaks stdio only"));
    }
    if let Some(transport) = object
        .get("type")
        .or_else(|| object.get("transport"))
        .and_then(Value::as_str)
        && !transport.eq_ignore_ascii_case("stdio")
    {
        return Err(format!("{transport} transport — MCPanel speaks stdio only"));
    }

    let command = object
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| "no command".to_string())?
        .to_string();

    let mut args = Vec::new();
    if let Some(raw) = object.get("args") {
        let list = raw
            .as_array()
            .ok_or_else(|| "\"args\" is not an array".to_string())?;
        for value in list {
            args.push(
                scalar_to_string(value)
                    .ok_or_else(|| "\"args\" contains a non-scalar value".to_string())?,
            );
        }
    }

    let mut env = BTreeMap::new();
    let mut secrets = BTreeMap::new();
    let mut secret_keys = Vec::new();
    let mut notes = Vec::new();
    if let Some(raw) = object.get("env") {
        let map = raw
            .as_object()
            .ok_or_else(|| "\"env\" is not an object".to_string())?;
        for (key, value) in map {
            if key.trim().is_empty() {
                return Err("\"env\" has an empty variable name".into());
            }
            let value =
                scalar_to_string(value).ok_or_else(|| format!("env {key} is not a string"))?;
            // A placeholder is never a usable credential, so it stays a
            // visible plain value the user can fix rather than becoming an
            // opaque keyring entry holding literal "${input:key}".
            if has_placeholder(&value) {
                notes.push(format!(
                    "{key} holds an unresolved placeholder — set its real value after importing"
                ));
                env.insert(key.clone(), value);
            } else if is_secret_key(key) && !value.is_empty() {
                secret_keys.push(key.clone());
                secrets.insert(key.clone(), value);
            } else {
                env.insert(key.clone(), value);
            }
        }
    }
    if object.contains_key("envFile") {
        notes.push("envFile is not supported — add those variables by hand".into());
    }

    let cwd = object
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string);

    Ok(ParsedServer {
        candidate: ImportCandidate {
            name: name.to_string(),
            command,
            args,
            cwd,
            env,
            secret_keys,
            notes,
            conflicts: false,
        },
        secrets,
    })
}

/// Where each client keeps its config, given the user's home directory (and
/// on Windows, `%APPDATA%`). Pure and parameterized so tests can point it at
/// a scratch directory.
pub fn candidate_paths(home: &Path, appdata: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let mut paths = Vec::new();

    // Claude Desktop and VS Code live in the per-OS application data dir.
    #[cfg(target_os = "macos")]
    {
        let support = home.join("Library").join("Application Support");
        paths.push((
            "Claude Desktop",
            support.join("Claude").join("claude_desktop_config.json"),
        ));
        paths.push((
            "VS Code",
            support.join("Code").join("User").join("mcp.json"),
        ));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = appdata {
            paths.push((
                "Claude Desktop",
                appdata.join("Claude").join("claude_desktop_config.json"),
            ));
            paths.push((
                "VS Code",
                appdata.join("Code").join("User").join("mcp.json"),
            ));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let config = home.join(".config");
        paths.push((
            "Claude Desktop",
            config.join("Claude").join("claude_desktop_config.json"),
        ));
        paths.push(("VS Code", config.join("Code").join("User").join("mcp.json")));
    }
    let _ = appdata;

    // The rest are home-relative on every platform.
    paths.push(("Claude Code", home.join(".claude.json")));
    paths.push(("Cursor", home.join(".cursor").join("mcp.json")));
    paths.push((
        "Windsurf",
        home.join(".codeium")
            .join("windsurf")
            .join("mcp_config.json"),
    ));
    paths
}

fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Read and parse every known config location. Files that are absent,
/// unreadable, or not MCP configs are skipped silently: discovery is a
/// best-effort sweep across other people's applications, and a stray
/// `~/.claude.json` without servers is normal, not an error worth surfacing.
fn scan(home: &Path, appdata: Option<&Path>) -> Vec<(String, String, ParsedConfig)> {
    let mut found = Vec::new();
    for (client, path) in candidate_paths(home, appdata) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match parse_config(&text) {
            Ok(parsed) if !parsed.is_empty() => {
                found.push((client.to_string(), path.display().to_string(), parsed));
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    target: "app::import",
                    path = %path.display(),
                    %error,
                    "skipping unparseable config"
                );
            }
        }
    }
    found
}

/// Mark candidates whose name is already taken so the UI can warn before
/// importing renames them.
fn mark_conflicts(candidates: &mut [ImportCandidate], existing: &BTreeSet<String>) {
    for candidate in candidates {
        candidate.conflicts = existing.contains(&candidate.name);
    }
}

/// Sweep the known client config locations.
pub async fn discover(state: &AppState) -> AppResult<Vec<DiscoveredConfig>> {
    let Some(home) = home_dir() else {
        return Ok(Vec::new());
    };
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    // Filesystem reads across several directories — off the runtime thread.
    let found = crate::state::blocking(move || Ok(scan(&home, appdata.as_deref()))).await?;

    let existing = existing_names(state).await?;
    Ok(found
        .into_iter()
        .map(|(client, path, parsed)| {
            let mut servers = parsed.candidates();
            mark_conflicts(&mut servers, &existing);
            DiscoveredConfig {
                client,
                path,
                servers,
                skipped: parsed.skipped,
            }
        })
        .collect())
}

/// Parse one file the user named explicitly. Unlike [`discover`], every
/// failure is reported: they pointed at this path on purpose.
pub async fn read_file(state: &AppState, path: String) -> AppResult<DiscoveredConfig> {
    let parsed = parse_path(path.clone()).await?;
    let existing = existing_names(state).await?;
    let mut servers = parsed.candidates();
    mark_conflicts(&mut servers, &existing);
    Ok(DiscoveredConfig {
        client: "file".into(),
        path,
        servers,
        skipped: parsed.skipped,
    })
}

async fn parse_path(path: String) -> AppResult<ParsedConfig> {
    crate::state::blocking(move || {
        let text = std::fs::read_to_string(&path)?;
        parse_config(&text)
    })
    .await
}

async fn existing_names(state: &AppState) -> AppResult<BTreeSet<String>> {
    Ok(state
        .with_db(db::list_servers)
        .await?
        .into_iter()
        .map(|record| record.name)
        .collect())
}

/// What one import run did. Per-entry results, because a partial import is
/// the normal outcome: one bad `cwd` must not cost the user the other five.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ImportOutcome {
    pub imported: Vec<ImportedServer>,
    pub failed: Vec<FailedImport>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ImportedServer {
    pub id: ServerId,
    /// The name it was created under — may be suffixed to dodge a conflict.
    pub name: String,
    /// The name in the source file, so the UI can show a rename honestly.
    pub source_name: String,
    pub secrets_stored: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FailedImport {
    pub name: String,
    pub reason: String,
}

/// First free `name`, `name (2)`, `name (3)`… Terminates: `taken` is finite.
fn unique_name(base: &str, taken: &BTreeSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unbounded sequence always clears a finite set")
}

/// Import the named servers from `path`.
///
/// The file is re-read here rather than trusting a payload from the webview:
/// that keeps secret values on this side of IPC, and means we act on what
/// the file says *now*. An entry that vanished since discovery is reported
/// as failed rather than guessed at.
pub async fn import(
    state: &AppState,
    path: String,
    names: Vec<String>,
) -> AppResult<ImportOutcome> {
    let parsed = parse_path(path.clone()).await?;
    let mut by_name: BTreeMap<&str, &ParsedServer> = BTreeMap::new();
    for server in &parsed.servers {
        by_name.insert(server.candidate.name.as_str(), server);
    }

    // Held across the whole run so the name snapshot below can't be raced by
    // a concurrent edit. Safe to hold: nothing called here takes it again.
    let _config = state.lock_config().await;
    let mut taken = existing_names(state).await?;

    let mut outcome = ImportOutcome::default();
    for requested in names {
        let Some(server) = by_name.get(requested.as_str()) else {
            outcome.failed.push(FailedImport {
                name: requested,
                reason: "no longer present in this file".into(),
            });
            continue;
        };
        let name = unique_name(&requested, &taken);
        match apply(state, server, name.clone()).await {
            Ok(id) => {
                taken.insert(name.clone());
                outcome.imported.push(ImportedServer {
                    id,
                    name,
                    source_name: requested,
                    secrets_stored: server.secrets.len(),
                });
            }
            Err(error) => outcome.failed.push(FailedImport {
                name: requested,
                reason: error.to_string(),
            }),
        }
    }
    tracing::info!(
        target: "app::import",
        path = %path,
        imported = outcome.imported.len(),
        failed = outcome.failed.len(),
        "import finished"
    );
    Ok(outcome)
}

/// Create one server and move its credentials into the keyring.
///
/// All-or-nothing: a server whose secrets didn't land would fail every start
/// with a missing-credential error, so a store failure rolls the row back
/// instead of leaving that behind.
async fn apply(state: &AppState, server: &ParsedServer, name: String) -> AppResult<ServerId> {
    let candidate = &server.candidate;
    let mut env: BTreeMap<String, EnvValue> = candidate
        .env
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                EnvValue::Plain {
                    value: value.clone(),
                },
            )
        })
        .collect();
    for key in &candidate.secret_keys {
        env.insert(key.clone(), EnvValue::Secret);
    }

    let record = lifecycle::add(
        state,
        NewServer {
            name,
            command: candidate.command.clone(),
            args: candidate.args.clone(),
            env,
            cwd: candidate.cwd.clone(),
            // Importing must never start processes on the user's behalf.
            auto_start: false,
        },
    )
    .await?;

    let id = record.id;
    let secrets = server.secrets.clone();
    let stored = crate::state::blocking(move || {
        for (key, value) in &secrets {
            crate::secrets::store_secret(id, key, value)?;
        }
        Ok(())
    })
    .await;
    if let Err(error) = stored {
        // Best-effort rollback; `remove` also clears any secrets that did
        // land. If it fails too, the original error is the useful one.
        let _ = lifecycle::remove(state, id).await;
        return Err(error);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ParsedConfig {
        parse_config(text).expect("parses")
    }

    fn only(text: &str) -> ParsedServer {
        let parsed = parse(text);
        assert_eq!(parsed.skipped, vec![], "expected no skipped entries");
        assert_eq!(parsed.servers.len(), 1, "expected exactly one server");
        parsed.servers.into_iter().next().unwrap()
    }

    #[test]
    fn reads_the_claude_desktop_shape() {
        let server =
            only(r#"{"mcpServers":{"files":{"command":"npx","args":["-y","@mcp/fs","/tmp"]}}}"#);
        assert_eq!(server.candidate.name, "files");
        assert_eq!(server.candidate.command, "npx");
        assert_eq!(server.candidate.args, ["-y", "@mcp/fs", "/tmp"]);
        assert_eq!(server.candidate.cwd, None);
    }

    #[test]
    fn reads_the_vs_code_servers_key() {
        let server = only(r#"{"servers":{"files":{"command":"npx","type":"stdio"}}}"#);
        assert_eq!(server.candidate.name, "files");
    }

    #[test]
    fn rejects_a_file_with_no_server_table() {
        let error = parse_config(r#"{"theme":"dark"}"#).unwrap_err();
        assert_eq!(error.code(), "invalid_input");
    }

    #[test]
    fn rejects_text_that_is_not_json() {
        assert_eq!(parse_config("not json").unwrap_err().code(), "json");
    }

    #[test]
    fn skips_remote_servers_by_url_and_by_type() {
        let parsed = parse(
            r#"{"mcpServers":{
                "remote":{"url":"https://example.test/mcp"},
                "streamed":{"type":"http","command":"ignored"},
                "sse":{"transport":"sse","command":"ignored"},
                "local":{"command":"npx"}
            }}"#,
        );
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].candidate.name, "local");
        let reasons: BTreeMap<_, _> = parsed
            .skipped
            .iter()
            .map(|entry| (entry.name.as_str(), entry.reason.as_str()))
            .collect();
        assert!(reasons["remote"].contains("remote server"));
        assert!(reasons["streamed"].contains("http transport"));
        assert!(reasons["sse"].contains("sse transport"));
    }

    #[test]
    fn stdio_type_is_honoured_not_skipped() {
        assert_eq!(
            only(r#"{"mcpServers":{"a":{"type":"STDIO","command":"npx"}}}"#)
                .candidate
                .command,
            "npx"
        );
    }

    #[test]
    fn one_bad_entry_never_costs_the_others() {
        let parsed = parse(
            r#"{"mcpServers":{
                "broken":{"args":["--flag"]},
                "alsobroken":"just a string",
                "fine":{"command":"npx"}
            }}"#,
        );
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.skipped.len(), 2);
        assert!(parsed.skipped.iter().any(|e| e.reason == "no command"));
    }

    #[test]
    fn coerces_scalar_args_and_env_values() {
        let server =
            only(r#"{"mcpServers":{"a":{"command":"x","args":[8080,true],"env":{"PORT":3000}}}}"#);
        assert_eq!(server.candidate.args, ["8080", "true"]);
        assert_eq!(server.candidate.env["PORT"], "3000");
    }

    #[test]
    fn rejects_structured_args_rather_than_flattening_them() {
        let parsed = parse(r#"{"mcpServers":{"a":{"command":"x","args":[{"k":"v"}]}}}"#);
        assert_eq!(parsed.servers.len(), 0);
        assert!(parsed.skipped[0].reason.contains("non-scalar"));
    }

    #[test]
    fn credential_keys_are_withheld_from_the_preview() {
        let server = only(
            r#"{"mcpServers":{"a":{"command":"x","env":{
                "OPENAI_API_KEY":"sk-live-secret",
                "GITHUB_TOKEN":"ghp_secret",
                "LOG_LEVEL":"debug"
            }}}}"#,
        );
        assert_eq!(
            server.candidate.secret_keys,
            ["GITHUB_TOKEN", "OPENAI_API_KEY"]
        );
        assert_eq!(
            server.candidate.env.keys().collect::<Vec<_>>(),
            ["LOG_LEVEL"],
            "secret values must not appear in the preview"
        );
        assert_eq!(server.secrets["OPENAI_API_KEY"], "sk-live-secret");

        let preview = serde_json::to_string(&server.candidate).unwrap();
        assert!(!preview.contains("sk-live-secret"));
        assert!(!preview.contains("ghp_secret"));
    }

    #[test]
    fn classifies_by_segment_not_substring() {
        assert!(is_secret_key("OPENAI_API_KEY"));
        assert!(is_secret_key("token"));
        assert!(is_secret_key("service-auth-pat"));
        assert!(!is_secret_key("MONKEY_MODE"));
        assert!(!is_secret_key("PATH"));
        assert!(!is_secret_key("KEYBOARD_LAYOUT"));
        assert!(!is_secret_key("LOG_LEVEL"));
    }

    #[test]
    fn an_empty_credential_value_stays_a_plain_placeholder() {
        let server = only(r#"{"mcpServers":{"a":{"command":"x","env":{"API_KEY":""}}}}"#);
        assert!(server.candidate.secret_keys.is_empty());
        assert_eq!(server.candidate.env["API_KEY"], "");
    }

    #[test]
    fn placeholders_stay_visible_instead_of_entering_the_keyring() {
        let server =
            only(r#"{"mcpServers":{"a":{"command":"x","env":{"API_KEY":"${input:api-key}"}}}}"#);
        assert!(server.secrets.is_empty());
        assert_eq!(server.candidate.env["API_KEY"], "${input:api-key}");
        assert!(server.candidate.notes[0].contains("placeholder"));
    }

    #[test]
    fn notes_an_unsupported_env_file() {
        let server = only(r#"{"mcpServers":{"a":{"command":"x","envFile":".env"}}}"#);
        assert!(server.candidate.notes[0].contains("envFile"));
    }

    #[test]
    fn unique_name_suffixes_only_on_collision() {
        let taken: BTreeSet<String> = ["files".into(), "files (2)".into()].into_iter().collect();
        assert_eq!(unique_name("other", &taken), "other");
        assert_eq!(unique_name("files", &taken), "files (3)");
    }

    /// Discovery actually reads the paths [`candidate_paths`] advertises:
    /// planted into a scratch home, a config comes back parsed; a sibling
    /// file with no server table is passed over without complaint.
    #[test]
    fn scan_finds_planted_configs_and_ignores_unrelated_files() {
        let home = std::env::temp_dir().join(format!("mcpanel-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let paths = candidate_paths(&home, None);
        let plant = |client: &str, contents: &str| {
            let (_, path) = paths.iter().find(|(c, _)| *c == client).expect(client);
            std::fs::create_dir_all(path.parent().unwrap()).expect("scratch dirs");
            std::fs::write(path, contents).expect("plant config");
        };
        plant("Cursor", r#"{"mcpServers":{"files":{"command":"npx"}}}"#);
        // A real `~/.claude.json` routinely has no MCP servers at all.
        plant("Claude Code", r#"{"theme":"dark"}"#);

        let found = scan(&home, None);
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(found.len(), 1, "only the file with servers is offered");
        let (client, _path, parsed) = &found[0];
        assert_eq!(client, "Cursor");
        assert_eq!(parsed.candidates()[0].name, "files");
    }

    #[test]
    fn conflicts_are_flagged_against_existing_names() {
        let stub = |name: &str| ImportCandidate {
            name: name.into(),
            command: "npx".into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            secret_keys: Vec::new(),
            notes: Vec::new(),
            conflicts: false,
        };
        let mut candidates = [stub("files"), stub("memory")];
        let existing = ["files".to_string()].into_iter().collect();
        mark_conflicts(&mut candidates, &existing);
        assert!(candidates[0].conflicts);
        assert!(!candidates[1].conflicts);
    }

    #[test]
    fn every_platform_offers_the_home_relative_clients() {
        let paths = candidate_paths(Path::new("/home/u"), Some(Path::new("/appdata")));
        let clients: Vec<_> = paths.iter().map(|(client, _)| *client).collect();
        for expected in [
            "Claude Desktop",
            "Claude Code",
            "Cursor",
            "VS Code",
            "Windsurf",
        ] {
            assert!(clients.contains(&expected), "missing {expected}");
        }
        let cursor = paths.iter().find(|(c, _)| *c == "Cursor").unwrap();
        assert!(cursor.1.ends_with("mcp.json"));
        assert!(cursor.1.starts_with("/home/u"));
    }
}
