//! Test fixture: a tiny stdio binary speaking just enough MCP, with
//! failure-mode flags (spec §4).
//!
//! Default: answers `initialize`, `tools/list`, `ping`; exits on stdin EOF.
//! `--spam`          floods stdout as fast as possible
//! `--spawn-child`   spawns an idle grandchild, prints its pid
//! `--no-handshake`  never answers `initialize`
//! `--wrong-version` answers `initialize` with a bogus protocolVersion
//! `--garbage`       prints non-JSON to stdout before serving
//! `--ansi`          ANSI-colored stderr
//! `--notify`        emits a notification after `initialized`
//! `--notify-flood`  emits 400 notifications after `initialized`
//! `--ping-client`   sends a server→client ping after `initialized`; prints
//!                   "client answered ping" to stderr on an empty result
//! `--idle`          (internal) sleep forever — the grandchild mode

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
        eprintln!(
            "\x1b[1;31merror:\x1b[0m \x1b[32mall good actually\x1b[0m \x1b[4munderlined\x1b[0m"
        );
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

    serve(Flags {
        no_handshake: has("--no-handshake"),
        wrong_version: has("--wrong-version"),
        notify: has("--notify"),
        notify_flood: has("--notify-flood"),
        ping_client: has("--ping-client"),
    });
}

struct Flags {
    no_handshake: bool,
    wrong_version: bool,
    notify: bool,
    notify_flood: bool,
    ping_client: bool,
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

fn serve(flags: Flags) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        if flags.no_handshake {
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

        // The pong for our own server→client ping (string id, no method).
        if method.is_empty() && message.get("result").is_some() {
            if message.get("id").and_then(|i| i.as_str()) == Some("srv-ping-1") {
                eprintln!("client answered ping");
            }
            continue;
        }

        match (method, id) {
            ("initialize", Some(id)) => respond(
                &id,
                serde_json::json!({
                    "protocolVersion": if flags.wrong_version { "1999-01-01" } else { "2025-06-18" },
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mock-mcp-server", "version": "0.1.0" },
                }),
            ),
            ("notifications/initialized", _) => {
                let notification = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/message",
                    "params": { "level": "info", "data": "hello from mock" },
                });
                if flags.notify {
                    let stdout = std::io::stdout();
                    let mut lock = stdout.lock();
                    writeln!(lock, "{notification}").expect("write notification");
                    lock.flush().expect("flush notification");
                }
                if flags.notify_flood {
                    let stdout = std::io::stdout();
                    let mut lock = stdout.lock();
                    for _ in 0..400 {
                        writeln!(lock, "{notification}").expect("write notification");
                    }
                    lock.flush().expect("flush flood");
                }
                if flags.ping_client {
                    let stdout = std::io::stdout();
                    let mut lock = stdout.lock();
                    writeln!(
                        lock,
                        "{}",
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": "srv-ping-1",
                            "method": "ping",
                        })
                    )
                    .expect("write server ping");
                    lock.flush().expect("flush server ping");
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
