//! Config store: rusqlite (bundled), hand-rolled `PRAGMA user_version`
//! migrations — no ORM. All functions are synchronous over a `&Connection`;
//! callers go through `spawn_blocking` per the concurrency rules.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, params};

use crate::error::{AppError, AppResult};
use crate::state::ServerId;

/// One statement batch per schema version; `user_version` tracks how many
/// have been applied. Append-only — never edit a shipped migration.
const MIGRATIONS: &[&str] = &[
    // v1: the servers table.
    "CREATE TABLE servers (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        command    TEXT NOT NULL,
        args       TEXT NOT NULL DEFAULT '[]',
        env        TEXT NOT NULL DEFAULT '{}',
        cwd        TEXT,
        auto_start INTEGER NOT NULL DEFAULT 0
    );",
    // v2: AUTOINCREMENT so deleted ids are never reused — keyring entries
    // are keyed by id, and a recreated server must not inherit a previous
    // server's credentials. SQLite can't add AUTOINCREMENT in place, so the
    // table is rebuilt; explicit-id inserts seed sqlite_sequence to max(id).
    "CREATE TABLE servers_new (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        name       TEXT NOT NULL UNIQUE,
        command    TEXT NOT NULL,
        args       TEXT NOT NULL DEFAULT '[]',
        env        TEXT NOT NULL DEFAULT '{}',
        cwd        TEXT,
        auto_start INTEGER NOT NULL DEFAULT 0
    );
    INSERT INTO servers_new (id, name, command, args, env, cwd, auto_start)
        SELECT id, name, command, args, env, cwd, auto_start FROM servers;
    DROP TABLE servers;
    ALTER TABLE servers_new RENAME TO servers;",
];

/// An environment value as stored in config. Secrets are only ever a marker:
/// the real value lives in the OS credential manager under
/// `mcpanel/<id>/<key>` and is resolved just-in-time at spawn (T9) —
/// never written to config, DB, or logs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvValue {
    Plain { value: String },
    Secret,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, EnvValue>,
    pub cwd: Option<String>,
    pub auto_start: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerRecord {
    pub id: ServerId,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, EnvValue>,
    pub cwd: Option<String>,
    pub auto_start: bool,
}

/// Open (creating if needed) and migrate the config database.
pub fn open(path: &Path) -> AppResult<Connection> {
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Fresh in-memory database, migrated — tests and ephemeral use.
pub fn open_in_memory() -> AppResult<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> AppResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> AppResult<()> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, batch) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(batch)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// Surface the `servers.name` UNIQUE violation as a `Conflict` the UI can
/// present ("name taken") instead of a 500 leaking raw SQL text.
fn map_name_conflict(err: rusqlite::Error, name: &str) -> AppError {
    if let rusqlite::Error::SqliteFailure(code, Some(message)) = &err
        && code.code == rusqlite::ErrorCode::ConstraintViolation
        && message.contains("servers.name")
    {
        return AppError::Conflict(format!("a server named \"{name}\" already exists"));
    }
    err.into()
}

pub fn insert_server(conn: &Connection, new: &NewServer) -> AppResult<ServerRecord> {
    conn.execute(
        "INSERT INTO servers (name, command, args, env, cwd, auto_start)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            new.name,
            new.command,
            serde_json::to_string(&new.args)?,
            serde_json::to_string(&new.env)?,
            new.cwd,
            new.auto_start,
        ],
    )
    .map_err(|e| map_name_conflict(e, &new.name))?;
    get_server(conn, conn.last_insert_rowid())
}

pub fn get_server(conn: &Connection, id: ServerId) -> AppResult<ServerRecord> {
    conn.query_row(
        "SELECT id, name, command, args, env, cwd, auto_start FROM servers WHERE id = ?1",
        params![id],
        row_to_record,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => AppError::ServerNotFound(id.to_string()),
        other => other.into(),
    })?
}

pub fn list_servers(conn: &Connection) -> AppResult<Vec<ServerRecord>> {
    let mut statement = conn.prepare(
        "SELECT id, name, command, args, env, cwd, auto_start FROM servers ORDER BY name",
    )?;
    let rows = statement.query_map([], row_to_record)?;
    let mut servers = Vec::new();
    for row in rows {
        servers.push(row??);
    }
    Ok(servers)
}

pub fn update_server(conn: &Connection, record: &ServerRecord) -> AppResult<()> {
    let changed = conn
        .execute(
            "UPDATE servers
         SET name = ?2, command = ?3, args = ?4, env = ?5, cwd = ?6, auto_start = ?7
         WHERE id = ?1",
            params![
                record.id,
                record.name,
                record.command,
                serde_json::to_string(&record.args)?,
                serde_json::to_string(&record.env)?,
                record.cwd,
                record.auto_start,
            ],
        )
        .map_err(|e| map_name_conflict(e, &record.name))?;
    if changed == 0 {
        return Err(AppError::ServerNotFound(record.id.to_string()));
    }
    Ok(())
}

pub fn delete_server(conn: &Connection, id: ServerId) -> AppResult<()> {
    let changed = conn.execute("DELETE FROM servers WHERE id = ?1", params![id])?;
    if changed == 0 {
        return Err(AppError::ServerNotFound(id.to_string()));
    }
    Ok(())
}

/// Row mapper. JSON parse failures surface as `AppResult` (not a rusqlite
/// error) so a corrupted row is reported precisely.
fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppResult<ServerRecord>> {
    let args: String = row.get(3)?;
    let env: String = row.get(4)?;
    Ok((|| {
        Ok(ServerRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            command: row.get(2)?,
            args: serde_json::from_str(&args)?,
            env: serde_json::from_str(&env)?,
            cwd: row.get(5)?,
            auto_start: row.get(6)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NewServer {
        NewServer {
            name: "echo".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "some-mcp-server".into()],
            env: BTreeMap::from([
                (
                    "PLAIN".into(),
                    EnvValue::Plain {
                        value: "visible".into(),
                    },
                ),
                ("API_KEY".into(), EnvValue::Secret),
            ]),
            cwd: Some("/tmp".into()),
            auto_start: true,
        }
    }

    #[test]
    fn crud_round_trip() {
        let conn = open_in_memory().unwrap();

        let created = insert_server(&conn, &sample()).unwrap();
        assert_eq!(get_server(&conn, created.id).unwrap(), created);
        assert_eq!(list_servers(&conn).unwrap(), vec![created.clone()]);

        let mut updated = created.clone();
        updated.command = "uvx".into();
        updated.auto_start = false;
        updated.env.insert("EXTRA".into(), EnvValue::Secret);
        update_server(&conn, &updated).unwrap();
        assert_eq!(get_server(&conn, created.id).unwrap(), updated);

        delete_server(&conn, created.id).unwrap();
        assert!(matches!(
            get_server(&conn, created.id),
            Err(AppError::ServerNotFound(_))
        ));
        assert!(matches!(
            update_server(&conn, &updated),
            Err(AppError::ServerNotFound(_))
        ));
        assert!(matches!(
            delete_server(&conn, created.id),
            Err(AppError::ServerNotFound(_))
        ));
    }

    #[test]
    fn duplicate_names_surface_as_conflict() {
        let conn = open_in_memory().unwrap();
        let first = insert_server(&conn, &sample()).unwrap();
        assert!(matches!(
            insert_server(&conn, &sample()),
            Err(AppError::Conflict(_))
        ));

        // Renaming onto a taken name conflicts the same way.
        let mut second = insert_server(
            &conn,
            &NewServer {
                name: "other".into(),
                ..sample()
            },
        )
        .unwrap();
        second.name = first.name;
        assert!(matches!(
            update_server(&conn, &second),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn secret_env_values_store_no_plaintext() {
        let conn = open_in_memory().unwrap();
        let created = insert_server(&conn, &sample()).unwrap();

        let raw_env: String = conn
            .query_row(
                "SELECT env FROM servers WHERE id = ?1",
                params![created.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(raw_env.contains(r#""API_KEY":{"kind":"secret"}"#));
        assert!(!raw_env.to_lowercase().contains("hunter2"), "sanity");
        assert_eq!(
            created.env.get("API_KEY"),
            Some(&EnvValue::Secret),
            "secret round-trips as a marker only"
        );
    }

    #[test]
    fn deleted_ids_are_never_reused() {
        let conn = open_in_memory().unwrap();

        let first = insert_server(&conn, &sample()).unwrap();
        delete_server(&conn, first.id).unwrap();

        let second = insert_server(&conn, &sample()).unwrap();
        assert!(
            second.id > first.id,
            "id {} reused after delete (was {})",
            second.id,
            first.id
        );
    }

    #[test]
    fn v1_db_migrates_to_v2_preserving_rows_and_sequence() {
        let path = std::env::temp_dir().join(format!(
            "mcpanel-db-migrate-test-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        {
            // A database exactly as a v1 install left it, with a row at id 7.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO servers (id, name, command) VALUES (7, 'legacy', 'echo')",
                [],
            )
            .unwrap();
        }

        let conn = open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let record = get_server(&conn, 7).unwrap();
        assert_eq!(record.name, "legacy");

        // The INSERT..SELECT must have seeded sqlite_sequence: deleting the
        // max row and reinserting continues past it instead of reusing 7.
        delete_server(&conn, 7).unwrap();
        let recreated = insert_server(&conn, &sample()).unwrap();
        assert_eq!(recreated.id, 8);

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn migrations_apply_once_and_persist() {
        let path =
            std::env::temp_dir().join(format!("mcpanel-db-test-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);

        {
            let conn = open(&path).unwrap();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(version, MIGRATIONS.len() as i64);
            insert_server(&conn, &sample()).unwrap();
        }
        {
            // Reopen: migrations must not rerun, data must survive.
            let conn = open(&path).unwrap();
            assert_eq!(list_servers(&conn).unwrap().len(), 1);
        }

        let _ = std::fs::remove_file(&path);
    }
}
