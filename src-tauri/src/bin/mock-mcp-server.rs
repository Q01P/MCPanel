//! Test fixture: a tiny stdio binary speaking just enough MCP, with
//! failure-mode flags (spec §4).
//!
//! Default: answers `initialize`, `tools/list`, `ping`; exits on stdin EOF.
//! `--spam`         floods stdout as fast as possible
//! `--spawn-child`  spawns an idle grandchild, prints its pid
//! `--no-handshake` never answers `initialize`
//! `--garbage`      prints non-JSON to stdout before serving
//! `--ansi`         ANSI-colored stderr
//! `--notify`       emits a notification after `initialized`
//! `--idle`         (internal) sleep forever — the grandchild mode

use std::io::{BufRead, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--idle") {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    if has("--spawn-child") {
        let exe = std::env::current_exe().expect("current_exe");
        // Deliberately never reaped: the grandchild must outlive this process
        // unless the supervisor's tree kill works — that's what's under test.
        #[allow(clippy::zombie_processes)]
        let child = std::process::Command::new(exe)
            .arg("--idle")
            .spawn()
            .expect("spawn idle grandchild");
        println!("{}", child.id());
        std::io::stdout().flush().expect("flush pid");
    }

    if has("--ansi") {
        eprintln!("\x1b[1;31merror:\x1b[0m \x1b[32mall good actually\x1b[0m \x1b[4munderlined\x1b[0m");
    }

    if has("--garbage") {
        println!("mock-mcp-server booting...");
        println!("\u{1}\u{2}\u{7f} pre-JSON garbage bytes");
        std::io::stdout().flush().expect("flush garbage");
    }

    if has("--spam") {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let mut n: u64 = 0;
        loop {
            n += 1;
            if writeln!(lock, "{{\"spam\":{n}}}").is_err() {
                return; // stdout closed — supervisor is gone
            }
        }
    }

    serve(has("--no-handshake"), has("--notify"));
}

fn respond(id: &serde_json::Value, result: serde_json::Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    writeln!(
        lock,
        "{}",
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
    )
    .expect("write response");
    lock.flush().expect("flush response");
}

fn serve(no_handshake: bool, notify: bool) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        if no_handshake {
            continue; // read forever, answer nothing
        }
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned();
        let method = message
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or_default();

        match (method, id) {
            ("initialize", Some(id)) => respond(
                &id,
                serde_json::json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mock-mcp-server", "version": "0.1.0" },
                }),
            ),
            ("notifications/initialized", _) => {
                if notify {
                    let stdout = std::io::stdout();
                    let mut lock = stdout.lock();
                    writeln!(
                        lock,
                        "{}",
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/message",
                            "params": { "level": "info", "data": "hello from mock" },
                        })
                    )
                    .expect("write notification");
                    lock.flush().expect("flush notification");
                }
            }
            ("tools/list", Some(id)) => respond(
                &id,
                serde_json::json!({
                    "tools": [{
                        "name": "echo",
                        "description": "echoes its input",
                        "inputSchema": { "type": "object" },
                    }],
                }),
            ),
            ("ping", Some(id)) => respond(&id, serde_json::json!({})),
            (_, Some(id)) => {
                let stdout = std::io::stdout();
                let mut lock = stdout.lock();
                writeln!(
                    lock,
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": "method not found" },
                    })
                )
                .expect("write error");
                lock.flush().expect("flush error");
            }
            (_, None) => {} // unknown notification — ignore
        }
    }
    // stdin EOF → clean exit
}
