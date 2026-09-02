pub mod commands;
pub mod db;
pub mod error;
pub mod import;
pub mod mcp;
pub mod secrets;
pub mod server;
pub mod state;

use tauri::Manager;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let conn = db::open(&data_dir.join("mcpanel.sqlite"))?;
            let records = db::list_servers(&conn)?;
            let app_state = state::AppState::new(conn);
            app.manage(app_state.clone());

            // Ephemeral port; a failure here aborts the launch loudly rather
            // than running with the webview pointed at a dead gateway.
            let (listener, addr) = server::bind()?;
            let auto_state = app_state.clone();
            let token = server::AuthToken::generate();
            app.manage(token.clone());
            app.manage(server::GatewayAddr(addr));
            tauri::async_runtime::spawn(server::serve(
                server::Gateway {
                    token,
                    app: app_state,
                    host: addr.to_string(),
                },
                listener,
            ));
            // Keyring probes are synchronous and can stall on a locked
            // credential store — never on the setup thread, or the window
            // waits on the OS keychain. Migration runs on the blocking pool,
            // then the auto-start sweep (which may resolve migrated secrets)
            // follows in the same task; failures surface per-server as
            // Errored, never as a launch failure.
            tauri::async_runtime::spawn(async move {
                let _ = state::blocking(move || {
                    secrets::migrate_name_keyed_secrets(&records);
                    Ok(())
                })
                .await;
                commands::lifecycle::start_auto_servers(&auto_state).await;
            });
            info!(target: "app", "MCPanel starting; gateway bound on {addr}");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_servers,
            commands::add_server,
            commands::update_server,
            commands::remove_server,
            commands::start_server,
            commands::stop_server,
            commands::set_server_secret,
            commands::delete_server_secret,
            commands::gateway_info,
            commands::discover_imports,
            commands::read_import_config,
            commands::import_servers,
        ])
        .run(tauri::generate_context!())
        .expect("error while running MCPanel");
}
