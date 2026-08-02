pub mod commands;
pub mod db;
pub mod error;
pub mod mcp;
pub mod secrets;
pub mod server;
pub mod state;

use tauri::Manager;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
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
            let app_state = state::AppState::new(conn);
            app.manage(app_state.clone());

            let token = server::AuthToken::generate();
            app.manage(token.clone());
            tauri::async_runtime::spawn(server::serve(server::Gateway {
                token,
                app: app_state,
            }));
            info!(target: "app", "MCPanel starting; gateway spawning on {}", server::GATEWAY_ADDR);
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running MCPanel");
}
