//! Tauri IPC commands — thin async wrappers over [`lifecycle`]; no logic of
//! their own beyond the mandated entry log + span.

pub mod lifecycle;

use tauri::State;
use tracing::info;

use crate::db::{NewServer, ServerRecord};
use crate::error::AppResult;
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
pub async fn add_server(
    state: State<'_, AppState>,
    new: NewServer,
) -> AppResult<ServerRecord> {
    info!(target: "app::commands", name = %new.name, "add_server");
    lifecycle::add(&state, new).await
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(state, record))]
pub async fn update_server(
    state: State<'_, AppState>,
    record: ServerRecord,
) -> AppResult<()> {
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

/// How the webview reaches the gateway; the token is handed over IPC only —
/// never logged, never persisted.
#[derive(serde::Serialize)]
pub struct GatewayInfo {
    pub url: String,
    pub token: String,
}

#[tauri::command]
#[tracing::instrument(target = "app::commands", skip(token))]
pub async fn gateway_info(
    token: State<'_, crate::server::AuthToken>,
) -> AppResult<GatewayInfo> {
    info!(target: "app::commands", "gateway_info");
    Ok(GatewayInfo {
        url: format!("http://{}", crate::server::GATEWAY_ADDR),
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
