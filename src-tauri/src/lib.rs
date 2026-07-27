pub mod error;
pub mod server;

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
        .setup(|_app| {
            let token = server::AuthToken::generate();
            tauri::async_runtime::spawn(server::serve(token));
            info!(target: "app", "MCPanel starting; gateway spawning on {}", server::GATEWAY_ADDR);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running MCPanel");
}
