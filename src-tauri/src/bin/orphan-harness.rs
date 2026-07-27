//! Crash-half orphan-test harness (spec §4, flagship test 2): spawns a server
//! through the real supervisor, prints its pid, then parks forever. The test
//! SIGKILLs this process — no cleanup code runs — and asserts the server dies
//! anyway (PDEATHSIG on Linux).

use std::io::Write;

use mcpanel_lib::mcp::process::{ProcessConfig, spawn};

fn main() {
    let fixture = std::env::args()
        .nth(1)
        .expect("usage: orphan-harness <server-binary>");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");

    // block_on keeps the spawning thread = main thread, so PDEATHSIG is tied
    // to the whole harness dying.
    runtime.block_on(async {
        let managed = spawn(&ProcessConfig {
            command: fixture,
            ..Default::default()
        })
        .expect("spawn server through supervisor");

        println!("{}", managed.pid);
        std::io::stdout().flush().expect("flush pid");

        // Keep `managed` (and its kill_on_drop child) alive until we're killed.
        std::future::pending::<()>().await;
        drop(managed); // unreachable; silences the unused-variable lint
    });
}
