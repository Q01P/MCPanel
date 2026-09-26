//! Tauri IPC commands — thin async wrappers over [`lifecycle`]; no logic of
//! their own beyond the mandated entry log + span.

pub mod lifecycle;

use tauri::State;
use tracing::info;

use crate::db::{NewServer, ServerRecord};
use crate::error::AppResult;
use crate::import::{DiscoveredConfig, ImportOutcome};
use crate::state::{AppState, ServerId};

use lifecycle::ServerOverview;

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn list_servers(state: State<'_, AppState>) -> AppResult<Vec<ServerOverview>> {
    info!(target: "app::commands", "list_servers");
    lifecycle::list(&state).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state, new))]
pub async fn add_server(state: State<'_, AppState>, new: NewServer) -> AppResult<ServerRecord> {
    info!(target: "app::commands", name = %new.name, "add_server");
    lifecycle::add(&state, new).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state, record))]
pub async fn update_server(state: State<'_, AppState>, record: ServerRecord) -> AppResult<()> {
    info!(target: "app::commands", id = record.id, "update_server");
    lifecycle::update(&state, record).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn remove_server(state: State<'_, AppState>, id: ServerId) -> AppResult<()> {
    info!(target: "app::commands", id, "remove_server");
    lifecycle::remove(&state, id).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn start_server(state: State<'_, AppState>, id: ServerId) -> AppResult<()> {
    info!(target: "app::commands", id, "start_server");
    lifecycle::start(&state, id).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn stop_server(state: State<'_, AppState>, id: ServerId) -> AppResult<()> {
    info!(target: "app::commands", id, "stop_server");
    lifecycle::stop(&state, id).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn restart_server(state: State<'_, AppState>, id: ServerId) -> AppResult<()> {
    info!(target: "app::commands", id, "restart_server");
    lifecycle::restart(&state, id).await
}

/// How the webview reaches the gateway; the token is handed over IPC only —
/// never logged, never persisted.
#[derive(serde::Serialize)]
pub struct GatewayInfo {
    pub url: String,
    pub token: String,
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(token, addr))]
pub async fn gateway_info(
    token: State<'_, crate::server::AuthToken>,
    addr: State<'_, crate::server::GatewayAddr>,
) -> AppResult<GatewayInfo> {
    info!(target: "app::commands", "gateway_info");
    Ok(GatewayInfo {
        url: format!("http://{}", addr.0),
        token: token.expose().to_string(),
    })
}

// Secret values are redacted by construction: never logged (key only,
// `skip(value)`), never echoed back, never written to the DB.

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state, value))]
pub async fn set_server_secret(
    state: State<'_, AppState>,
    id: ServerId,
    key: String,
    value: String,
) -> AppResult<()> {
    info!(target: "app::commands", id, key = %key, "set_server_secret");
    lifecycle::set_secret(&state, id, key, value).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn delete_server_secret(
    state: State<'_, AppState>,
    id: ServerId,
    key: String,
) -> AppResult<()> {
    info!(target: "app::commands", id, key = %key, "delete_server_secret");
    lifecycle::delete_secret(&state, id, key).await
}

// Import from other MCP clients' config files. Discovery and preview never
// carry secret values; `import_servers` re-reads the file backend-side and
// moves credentials straight into the OS keyring (see `crate::import`).

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn discover_imports(state: State<'_, AppState>) -> AppResult<Vec<DiscoveredConfig>> {
    info!(target: "app::commands", "discover_imports");
    crate::import::discover(&state).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state))]
pub async fn read_import_config(
    state: State<'_, AppState>,
    path: String,
) -> AppResult<DiscoveredConfig> {
    info!(target: "app::commands", path = %path, "read_import_config");
    crate::import::read_file(&state, path).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state, names))]
pub async fn import_servers(
    state: State<'_, AppState>,
    path: String,
    names: Vec<String>,
) -> AppResult<ImportOutcome> {
    info!(target: "app::commands", path = %path, count = names.len(), "import_servers");
    crate::import::import(&state, path, names).await
}

// Export to other MCP clients' config shape. Secret values stay in the
// keyring unless the request says otherwise (the UI asks first); the
// resolved text is returned over IPC and never logged.

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state, request))]
pub async fn export_servers(
    state: State<'_, AppState>,
    request: crate::export::ExportRequest,
) -> AppResult<crate::export::ExportOutcome> {
    info!(
        target: "app::commands",
        count = request.ids.len(),
        include_secrets = request.include_secrets,
        "export_servers"
    );
    crate::export::export(&state, request).await
}

/// Save export text to a new file — never over an existing one.
#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(text))]
pub async fn write_export_file(path: String, text: String) -> AppResult<()> {
    info!(target: "app::commands", path = %path, "write_export_file");
    crate::export::write_new_file(path, text).await
}
