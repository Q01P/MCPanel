//! Export server definitions in the `mcpServers` shape every MCP client
//! reads — the inverse of [`crate::import`], and the answer to "I set this
//! up in MCPanel, now make Claude Desktop / Cursor / VS Code see it".
//!
//! The one hard rule mirrors the importer's: **secret values leave the
//! keyring only on explicit request.** The default export writes a
//! `${KEY}` placeholder where a credential belongs, so the text can be
//! pasted or committed without leaking anything; `include_secrets` resolves
//! them backend-side and the caller (the UI) must have asked the user
//! first. Even then the values go into the returned text or the named
//! file — never into an event or a log line.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::db::{self, EnvValue, ServerRecord};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, ServerId};

/// Which client's file layout to produce. They differ only in the root
/// key (`servers` for VS Code) and whether a `type: "stdio"` tag is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flavor {
    /// Claude Desktop, Claude Code, Cursor, Windsurf: `{"mcpServers": {…}}`.
    McpServers,
    /// VS Code `mcp.json`: `{"servers": {… "type": "stdio"}}`.
    VsCode,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ExportRequest {
    pub ids: Vec<ServerId>,
    pub flavor: Flavor,
    /// Resolve secret env values from the keyring into the output. The UI
    /// only sets this after an explicit confirmation.
    #[serde(default)]
    pub include_secrets: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportOutcome {
    pub text: String,
    /// Names of the secret env keys that were written as placeholders
    /// (empty when `include_secrets` was set or there were none).
    pub placeholders: Vec<String>,
}

/// One server as a client config entry. `secrets` maps key → resolved value
/// for the keys to inline; any secret key not in it becomes a placeholder.
pub fn entry_for(
    record: &ServerRecord,
    flavor: Flavor,
    secrets: &BTreeMap<String, String>,
    placeholders: &mut Vec<String>,
) -> Value {
    let mut entry = Map::new();
    if flavor == Flavor::VsCode {
        entry.insert("type".into(), json!("stdio"));
    }
    entry.insert("command".into(), json!(record.command));
    if !record.args.is_empty() {
        entry.insert("args".into(), json!(record.args));
    }
    if !record.env.is_empty() {
        let mut env = Map::new();
        for (key, value) in &record.env {
            let rendered = match value {
                EnvValue::Plain { value } => value.clone(),
                EnvValue::Secret => match secrets.get(key) {
                    Some(resolved) => resolved.clone(),
                    None => {
                        placeholders.push(format!("{}/{key}", record.name));
                        format!("${{{key}}}")
                    }
                },
            };
            env.insert(key.clone(), json!(rendered));
        }
        entry.insert("env".into(), Value::Object(env));
    }
    if let Some(cwd) = &record.cwd {
        entry.insert("cwd".into(), json!(cwd));
    }
    Value::Object(entry)
}

/// The whole document for `records`, in `flavor`, pretty-printed.
pub fn render(
    records: &[ServerRecord],
    flavor: Flavor,
    secrets: &BTreeMap<ServerId, BTreeMap<String, String>>,
) -> ExportOutcome {
    let empty = BTreeMap::new();
    let mut placeholders = Vec::new();
    let mut table = Map::new();
    for record in records {
        let resolved = secrets.get(&record.id).unwrap_or(&empty);
        table.insert(
            record.name.clone(),
            entry_for(record, flavor, resolved, &mut placeholders),
        );
    }
    let root_key = match flavor {
        Flavor::McpServers => "mcpServers",
        Flavor::VsCode => "servers",
    };
    let document = json!({ root_key: Value::Object(table) });
    ExportOutcome {
        text: serde_json::to_string_pretty(&document).unwrap_or_default() + "\n",
        placeholders,
    }
}

/// Build the export text for the requested servers.
pub async fn export(state: &AppState, request: ExportRequest) -> AppResult<ExportOutcome> {
    if request.ids.is_empty() {
        return Err(AppError::InvalidInput("nothing selected to export".into()));
    }
    let ids = request.ids.clone();
    let records = state
        .with_db(move |conn| {
            ids.iter()
                .map(|id| db::get_server(conn, *id))
                .collect::<AppResult<Vec<_>>>()
        })
        .await?;

    let mut secrets = BTreeMap::new();
    if request.include_secrets {
        let to_resolve = records.clone();
        secrets = crate::state::blocking(move || {
            let mut all = BTreeMap::new();
            for record in &to_resolve {
                let mut resolved = BTreeMap::new();
                for (key, value) in &record.env {
                    if *value == EnvValue::Secret {
                        resolved.insert(key.clone(), crate::secrets::get_secret(record.id, key)?);
                    }
                }
                all.insert(record.id, resolved);
            }
            Ok(all)
        })
        .await?;
    }
    Ok(render(&records, request.flavor, &secrets))
}

/// Write `text` to `path`, refusing to overwrite: an export must never
/// clobber a client's live config (the user merges by hand, or names a
/// fresh file). The parent directory must already exist.
pub async fn write_new_file(path: String, text: String) -> AppResult<()> {
    crate::state::blocking(move || {
        let path = Path::new(&path);
        if path.exists() {
            return Err(AppError::Conflict(format!(
                "{} already exists — export refuses to overwrite; pick a new file name",
                path.display()
            )));
        }
        match path.parent() {
            Some(parent) if parent.as_os_str().is_empty() || parent.is_dir() => {}
            _ => {
                return Err(AppError::InvalidInput(format!(
                    "the directory for {} does not exist",
                    path.display()
                )));
            }
        }
        std::fs::write(path, text)?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str) -> ServerRecord {
        ServerRecord {
            id: 5,
            name: name.into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@mcp/fs".into()],
            env: BTreeMap::from([
                (
                    "LOG_LEVEL".into(),
                    EnvValue::Plain {
                        value: "debug".into(),
                    },
                ),
                ("API_KEY".into(), EnvValue::Secret),
            ]),
            cwd: Some("/work".into()),
            auto_start: true,
            request_timeout_s: Some(60),
            restart_on_crash: true,
        }
    }

    #[test]
    fn placeholders_stand_in_for_secrets_by_default() {
        let outcome = render(&[record("files")], Flavor::McpServers, &BTreeMap::new());
        let doc: Value = serde_json::from_str(&outcome.text).unwrap();
        let entry = &doc["mcpServers"]["files"];
        assert_eq!(entry["command"], "npx");
        assert_eq!(entry["args"], json!(["-y", "@mcp/fs"]));
        assert_eq!(entry["env"]["LOG_LEVEL"], "debug");
        assert_eq!(entry["env"]["API_KEY"], "${API_KEY}");
        assert_eq!(entry["cwd"], "/work");
        assert!(entry.get("type").is_none());
        // MCPanel-only settings never leak into another client's file.
        assert!(entry.get("auto_start").is_none());
        assert!(entry.get("request_timeout_s").is_none());
        assert_eq!(outcome.placeholders, ["files/API_KEY"]);
    }

    #[test]
    fn resolved_secrets_are_inlined_only_when_supplied() {
        let secrets = BTreeMap::from([(
            5,
            BTreeMap::from([("API_KEY".to_string(), "sk-1".to_string())]),
        )]);
        let outcome = render(&[record("files")], Flavor::McpServers, &secrets);
        assert!(outcome.text.contains("\"API_KEY\": \"sk-1\""));
        assert!(outcome.placeholders.is_empty());
    }

    #[test]
    fn vs_code_flavor_uses_servers_and_tags_stdio() {
        let outcome = render(&[record("files")], Flavor::VsCode, &BTreeMap::new());
        let doc: Value = serde_json::from_str(&outcome.text).unwrap();
        assert_eq!(doc["servers"]["files"]["type"], "stdio");
        assert!(doc.get("mcpServers").is_none());
    }

    #[test]
    fn empty_args_and_env_are_omitted() {
        let mut bare = record("bare");
        bare.args.clear();
        bare.env.clear();
        bare.cwd = None;
        let outcome = render(&[bare], Flavor::McpServers, &BTreeMap::new());
        let doc: Value = serde_json::from_str(&outcome.text).unwrap();
        assert_eq!(doc["mcpServers"]["bare"], json!({ "command": "npx" }));
    }

    /// The export round-trips through the importer: what we write, we can
    /// read back — the two modules agree on the shape.
    #[test]
    fn export_is_importable() {
        let outcome = render(&[record("files")], Flavor::McpServers, &BTreeMap::new());
        let parsed = crate::import::parse_config(&outcome.text).expect("importable");
        let candidates = parsed.candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "files");
        assert_eq!(candidates[0].args, ["-y", "@mcp/fs"]);
        // The placeholder is flagged, not smuggled into the keyring.
        assert!(candidates[0].notes[0].contains("placeholder"));
    }

    #[tokio::test]
    async fn write_new_file_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("mcpanel-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");

        write_new_file(path.display().to_string(), "{}\n".into())
            .await
            .expect("first write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");

        let again = write_new_file(path.display().to_string(), "{\"x\":1}".into()).await;
        assert!(matches!(again, Err(AppError::Conflict(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n", "untouched");

        let nowhere = write_new_file(
            dir.join("no/such/dir/x.json").display().to_string(),
            "".into(),
        )
        .await;
        assert!(matches!(nowhere, Err(AppError::InvalidInput(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
