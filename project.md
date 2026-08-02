# MCPanel — Project Reference

> **MCPanel** is a lightweight desktop app for managing local MCP (Model Context
> Protocol) servers — think **"Postman for MCP."**

MCP servers are the small stdio-based programs that give AI clients (Claude
Desktop, IDEs, agents) access to tools. Today, developers babysit them with raw
terminals, hand-edited JSON configs, and zero visibility into what they're
doing. MCPanel gives them a control panel instead.

- **Owner:** Oussema Taleb ([@Q01P](https://github.com/Q01P), taleb@xseth.com)
- **Repository:** https://github.com/Q01P/mcpanel
- **License:** MIT
- **App identifier:** `com.xseth.mcpanel`
- **Binary size target:** **< 15 MB** release binary

---

## 1. What the user gets

| Feature | Behavior |
| --- | --- |
| **Service-style toggles** | Each configured server toggles on/off like a service. Flipping one on spawns the process *and* transparently performs the MCP `initialize` handshake — when the UI says "running," the server is genuinely ready for tool calls. |
| **Live log streaming** | stdout/stderr of every server, parsed line-by-line, ANSI junk stripped, flood-proof even at thousands of lines per second. |
| **JSON workbench** | CodeMirror-based editor to hand-craft JSON-RPC requests, fire them at a running server, and inspect responses — the "Postman" part. |
| **No orphaned processes, ever** | If MCPanel dies or crashes, every server it spawned dies with it (Windows Job Objects / Unix process groups). |
| **Sane secrets handling** | Server API keys live in the OS credential manager (Keychain / Credential Manager / Secret Service), never in plaintext config files. |

## 2. Tech stack

Deliberately minimalist — **no Electron, no Node/Python backend**:

| Piece | Tech |
| --- | --- |
| Shell | Tauri v2 (system webview) |
| Backend | Pure Rust: tokio process orchestration |
| UI ↔ backend streaming | Token-guarded local Axum gateway on `127.0.0.1:6789` (SSE + JSON-RPC forwarding) |
| Config storage | SQLite (rusqlite, bundled) |
| Secrets | `keyring` v4 → OS credential store |
| Frontend | React 19 + Vite 8 + TypeScript + Zustand (not scaffolded yet) |
| JSON editor | `@uiw/react-codemirror` (CodeMirror 6 — **not Monaco**) |

### Dependency policy (binding)

Versions in `src-tauri/Cargo.toml` were **verified against crates.io/npm on
2026-07-27** — never "downgrade" them to training-data versions, and re-verify
current stable before adding new deps. Notable pins:

- `tauri 2.11.5`, `tauri-build 2.6.3`, `@tauri-apps/cli 2.11.x`
- `tokio 1.53.1`, `tokio-util 0.7.19` (codec), `tokio-stream 0.1.19`
- `axum 0.8.9`, `hyper 1.11.0`, `tower 0.5.3` (timeout)
- `rusqlite 0.40.1` (bundled), `dashmap 6.2.1`, `bytes 1.12.1`
- `keyring 4.1.5` — **v4**: default features auto-select the platform store;
  the v3 `linux-native`/`sync-secret-service` feature flags no longer exist
- `thiserror 2.0.19`, `tracing 0.1.44`, `tracing-subscriber 0.3.23`
- `windows-sys 0.61.2`, `libc 0.2.189`, `ring 0.17.14`
- `edition = "2024"` (newest Rust edition — do not change), `rust-version = "1.85"`
- `anyhow` is **dev-dependencies only** — tests may use it, production code
  never does; everything funnels into `AppError`

## 3. Architecture (owner's spec — binding)

Single crate in `src-tauri/`; planned module layout under `src-tauri/src/`:

```
src-tauri/src/
├── lib.rs          # tracing init, Axum gateway startup, Tauri builder
├── main.rs         # thin entry → mcpanel_lib::run()
├── error.rs        # AppError (exists) — the single error funnel
├── state.rs        # AppState: Arc of DashMap registry + SQLite pool + sender channels
├── commands/       # Tauri IPC commands
├── mcp/
│   ├── process.rs  # spawn/reap (process groups / Job Objects)
│   ├── stream.rs   # stdout/stderr parsing (LinesCodec, ANSI strip)
│   └── protocol.rs # JSON-RPC + initialize handshake
├── server/         # Axum gateway
└── db/             # rusqlite config store
```

### Process orchestration

- Spawn MCP servers with `tokio::process::Command` **directly** — deliberately
  NOT `tauri-plugin-shell`.
- **Orphan prevention is mandatory:** Unix process groups via `CommandExt`;
  Windows Job Objects via `windows-sys` tied to the main process
  (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
- Graceful shutdown = SIGTERM / CTRL_BREAK, wait 2 s, then SIGKILL.

Proven details from the prototype (keep when implementing):

- Child calls `setpgid(0, 0)` in `pre_exec`; the parent *also* calls
  `setpgid(pid, pid)` right after spawn — whichever runs first wins, closing
  the fork/exec race where you could signal a group that doesn't exist yet.
- On Linux additionally set `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec`, so
  direct children die even if MCPanel is SIGKILLed and no cleanup code runs.
  (Caveat: PDEATHSIG fires when the spawning *thread* dies — only spawn from
  long-lived runtime threads.)
- Kill the whole tree with `kill(-pgid, …)` so grandchildren (`npx` → `node`!)
  die too; on Windows `TerminateJobObject` covers the tree.
- Keep a cloneable kill handle (pgid / `Arc<JobObject>`) separate from the
  `Child`, because the exit-waiter task owns the `Child` for `wait()`.
- `kill_on_drop(true)` as belt-and-braces for normal drop paths.

### Streams

- Two async tasks per child (stdout/stderr) using `tokio_util::codec::LinesCodec`.
- Strip ANSI codes and garbage bytes before the first `{` of NDJSON.
- **Bounded MPSC channels (1024 lines/process); drop stale lines under
  pressure** — the panel must never choke on a flooding server.
- Cap single-line length (prototype used 64 KB) so one endless line can't
  balloon memory; bounded channels alone don't cover that case.
- Log data across threads: use `Arc<str>` / `bytes::Bytes`, **not cloned
  Strings**.

### MCP protocol (stdio transport)

- **stdout is the protocol channel; stderr is logs.** Route stdout lines:
  valid JSON-RPC (object with `"jsonrpc"`) → dispatcher; anything else —
  servers misbehave — falls back into the log buffer instead of being lost.
- Rust owns the handshake: on server start, send `initialize`, await the
  response, send `notifications/initialized` — only then may the UI issue
  tool calls. State machine: `Stopped → Starting → Initializing → Running /
  Errored`.
- Advertised protocol version: `2025-06-18`. Capture `capabilities` +
  `serverInfo` from the handshake for the UI.
- Request/response correlation: monotonic i64 ids → oneshot channels, with a
  per-request timeout; fail all in-flight requests when stdout reaches EOF.
- Server→client requests (e.g. `roots/list`): reply `-32601` politely so the
  server isn't left waiting. Server notifications fan out to the UI stream.
- Handshake timeout ⇒ tear the process tree down and mark the server Errored.

### HTTP gateway

- Axum on `127.0.0.1:6789`; reject non-loopback connections.
- Random bearer token generated **in memory at startup**, required on every
  route (query param for `GET /sse` since `EventSource` can't set headers).
- `GET /sse` — server state changes + log lines for the UI.
- `POST /mcp/{server_id}` — forwards JSON-RPC to the child's stdin
  (axum 0.8 = `/{param}` route syntax).
- 30 s timeout middleware on POSTs (`tower` timeout).

### Errors

Everything funnels into `AppError` in `src-tauri/src/error.rs` (exists):
thiserror enum, manual `Serialize` impl for Tauri commands, `IntoResponse`
for Axum handlers. Infrastructure failures arrive via `#[from]`, domain
failures via explicit variants (`ServerNotFound`, `Handshake`, `Timeout`,
`Unauthorized`, …).

### Concurrency rules

- `tokio::spawn` for async I/O; `spawn_blocking` for SQLite writes.
- Never block the UI thread — **all Tauri commands async**.
- Every `tauri::command` starts with `info!(target: "app::commands", ...)`
  and uses tracing spans.
- High-frequency emits to the UI: prefer `Emitter::emit_str_to`
  (pre-serialized JSON).

### Database

- rusqlite (bundled), hand-rolled `PRAGMA user_version` migrations — no ORM.
- `servers` table: name, command, args (JSON), env (JSON), cwd, auto-start.
- Env values marked secret store only a *reference*; the real value lives in
  the OS credential manager under `mcpanel/<server>/<key>` and is resolved
  just-in-time at spawn. Never written to config, DB, or logs.

## 4. Testing strategy

All backend behavior is tested against a **`mock-mcp-server` fixture** — a
tiny stdio binary speaking just enough MCP, with failure-mode flags:

| Flag | Behavior | Exercises |
| --- | --- | --- |
| *(none)* | answers `initialize`, `tools/list`, `ping`; exits on stdin EOF | happy path |
| `--spam` | floods stdout as fast as possible | flood-proofing, ring buffer bounds |
| `--spawn-child` | spawns an idle grandchild, prints its pid | whole-tree kill |
| `--no-handshake` | never answers `initialize` | handshake timeout → Errored |
| `--garbage` | prints non-JSON to stdout before serving | garbage-tolerant stdout routing |
| `--ansi` | ANSI-colored stderr | ANSI stripping |
| `--notify` | emits a notification after `initialized` | server-notification fan-out |

Flagship tests (both proven in the prototype):

1. **Controlled-shutdown half:** stopping a server kills its whole tree,
   grandchildren included (assert pids dead; on Linux read `/proc/<pid>/stat`
   and treat `Z`/`X` as dead to dodge zombie false-positives).
2. **Crash half:** a harness binary spawns a server through the supervisor,
   the test **SIGKILLs the harness** (no cleanup code runs), and asserts the
   server dies anyway (PDEATHSIG).

Plus: handshake happy path / timeout, JSON-RPC error surfacing, requests
failing cleanly after stop, log flood boundedness, line-length capping.

## 5. Roadmap

Work is delivered **one scoped task at a time, with owner review between
tasks** — do not scaffold ahead of the agreed task.

- [x] **T0** — Backend scaffold: pinned `Cargo.toml`, `error.rs` (`AppError`),
      stub `lib.rs`/`main.rs`/`build.rs`/`tauri.conf.json`
- [x] **T1** — Real `lib.rs`/`main.rs`: tracing init, Axum gateway skeleton
      (token, `/sse` stub), Tauri builder
- [x] **T2** — `state.rs`: AppState (DashMap registry + SQLite pool + channels)
- [x] **T3** — `mcp/process.rs`: spawn/reap with orphan prevention + fixture
      + orphan tests
- [x] **T4** — `mcp/stream.rs`: line pipelines (capped custom codec), ANSI
      strip, bounded channels
- [x] **T5** — `mcp/protocol.rs`: JSON-RPC correlation + initialize handshake
- [x] **T6** — `db/`: rusqlite store, migrations, server CRUD
- [x] **T7** — `commands/`: Tauri IPC (list/add/update/remove/start/stop)
- [x] **T8** — `server/`: full gateway (`/sse`, `/mcp/{server_id}`, timeouts)
- [ ] **T9** — Secrets: keyring integration, just-in-time resolution, redaction
      ← **next**
- [ ] **T10** — Frontend: Vite scaffold, server list + toggles + status badges
- [ ] **T11** — Frontend: virtualized log viewer (follow-tail, dropped-lines
      marker)
- [ ] **T12** — Frontend: JSON workbench (CodeMirror 6, templates, history)
- [ ] **T13** — Packaging: bundling on, icons, CI (3-OS matrix), release
      workflow, size budget check

## 6. Development

Cargo is user-local — if it's not on PATH: `export PATH="$HOME/.cargo/bin:$PATH"`.

```bash
cargo check  --manifest-path src-tauri/Cargo.toml   # type-check backend
cargo test   --manifest-path src-tauri/Cargo.toml   # run tests (single: cargo test <name>)
cargo clippy --manifest-path src-tauri/Cargo.toml   # lint
```

Frontend / `tauri dev` commands land once `package.json` exists
(`npm create tauri-app` conventions).

Linux build prerequisites (already installed on the dev machine):
`libwebkit2gtk-4.1-dev build-essential libxdo-dev libssl-dev
libayatana-appindicator3-dev librsvg2-dev`

Release profile (already configured): `opt-level = "s"`, `lto = true`,
`codegen-units = 1`, `strip = true`, `panic = "abort"`.

## 7. History note

An earlier cloud prototyping session built milestones M0–M2 (workspace layout,
supervisor, MCP client, 17 passing tests) but was **never pushed**; that
environment is gone. Its architecture diverged from the owner's spec (Cargo
workspace, edition 2021, older deps), so this repo remains canonical. The
*proven behaviors* from that prototype — supervisor semantics, MCP stdout
routing, the mock-server fixture, and the orphan-test design — are recorded in
§3–§4 above and get re-implemented here, inside this repo's architecture, as
tasks T3–T5.
