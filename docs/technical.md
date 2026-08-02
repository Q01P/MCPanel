# MCPanel — Technical Reference

MCPanel is a lightweight desktop app for managing local MCP (Model Context
Protocol) servers — "Postman for MCP." This document is the full technical
reference for the implementation. The binding design spec lives in
[`project.md`](../project.md); this file describes what is actually built and
how the pieces fit.

- Shell: **Tauri v2** (system webview — no Electron, no Node backend)
- Backend: **pure Rust** (tokio), single crate in `src-tauri/`
- Frontend: **React 19 + Vite 8 + TypeScript + Zustand**, CodeMirror 6
- Storage: **SQLite** (rusqlite, bundled) + **OS credential manager** for secrets
- UI ↔ backend streaming: token-guarded **Axum gateway** on `127.0.0.1:6789`
- Release binary: ~7.0 MB (budget: < 15 MB)

---

## 1. Architecture overview

```
┌────────────────────────── MCPanel process ──────────────────────────┐
│                                                                     │
│  ┌─ webview (React) ─┐        ┌──────── Rust backend ────────────┐  │
│  │ server list       │ invoke │ commands/  (Tauri IPC)           │  │
│  │ log viewer        │───────►│   └─ lifecycle orchestration     │  │
│  │ JSON workbench    │        │ state.rs   AppState              │  │
│  │                   │  SSE   │   ├─ DashMap registry            │  │
│  │ EventSource ◄─────┼────────┤   ├─ SQLite (Mutex<Connection>)  │  │
│  │ fetch POST /mcp ──┼───────►│   └─ broadcast<AppEvent>         │  │
│  └───────────────────┘        │ server/    Axum gateway :6789    │  │
│                               │ mcp/       process/stream/proto  │  │
│                               │ db/        config store          │  │
│                               │ secrets.rs OS credential store   │  │
│                               └───────────┬──────────────────────┘  │
└───────────────────────────────────────────┼─────────────────────────┘
                                            │ spawn (process group /
                                            ▼        Job Object)
                                   MCP server child processes
                                   (stdin/stdout = JSON-RPC, stderr = logs)
```

Two transport paths reach the webview:

1. **Tauri IPC** (`invoke`) — request/response commands (CRUD, start/stop,
   secrets, gateway credentials).
2. **The local HTTP gateway** — `GET /sse` for the continuous event stream
   (status changes, log lines) and `POST /mcp/{server_id}` for workbench
   JSON-RPC forwarding. The gateway exists because `EventSource` gives the
   webview a clean, backpressure-aware streaming channel.

### Module layout (`src-tauri/src/`)

| Path              | Responsibility |
| ----------------- | -------------- |
| `lib.rs`          | tracing init, DB open, state/token management, gateway spawn, Tauri builder |
| `main.rs`         | thin entry → `mcpanel_lib::run()` |
| `error.rs`        | `AppError` — the single error funnel |
| `state.rs`        | `AppState`: registry + DB handle + event broadcast |
| `commands/`       | Tauri IPC commands + lifecycle orchestration |
| `mcp/process.rs`  | spawn/reap with orphan prevention |
| `mcp/stream.rs`   | stdout/stderr line pipelines |
| `mcp/protocol.rs` | JSON-RPC correlation + MCP handshake |
| `server/`         | Axum gateway |
| `db/`             | rusqlite config store + migrations |
| `secrets.rs`      | keyring integration |
| `bin/`            | test fixtures (`mock-mcp-server`, `orphan-harness`) |

---

## 2. Error funnel (`error.rs`)

Everything funnels into `AppError` (thiserror). Infrastructure failures
arrive via `#[from]`; domain failures are explicit variants. `anyhow` is
dev-dependencies only — production code never uses it.

`AppError` serializes for Tauri commands as a stable shape the frontend
matches on:

```json
{ "code": "server_not_found", "message": "server not found: 3" }
```

| Variant             | `code`              | HTTP status (gateway) | Source |
| ------------------- | ------------------- | --------------------- | ------ |
| `ServerNotFound`    | `server_not_found`  | 404                   | unknown id / not running |
| `Handshake`         | `handshake`         | 502                   | MCP handshake failures |
| `Timeout`           | `timeout`           | 504                   | request/handshake/stop timeouts |
| `Unauthorized`      | `unauthorized`      | 401                   | bad/missing gateway token |
| `Rpc {code, message}` | `rpc`             | 502                   | JSON-RPC error responses |
| `ConnectionClosed`  | `connection_closed` | 502                   | server stdout EOF |
| `Io(std::io::Error)` | `io`               | 500                   | `#[from]` |
| `Db(rusqlite::Error)` | `db`              | 500                   | `#[from]` |
| `Json(serde_json::Error)` | `json`        | 400                   | `#[from]` |
| `Keyring(keyring::Error)` | `keyring`     | 500                   | `#[from]` |

Status-mapping rationale: from the gateway's perspective the child MCP server
is the *upstream*, so its timeouts are 504 and its failures 502.

---

## 3. State (`state.rs`)

`AppState` is cheaply clonable; everything inside is shared:

- `registry: Arc<DashMap<ServerId, ServerEntry>>` — holds **live servers
  only**. An id absent from the registry reads as `Stopped`. `ServerEntry`
  carries the current `ServerStatus` and, once the handshake succeeds, a
  `RunningServer`.
- `db: Arc<Mutex<rusqlite::Connection>>` — rusqlite is sync and no pool crate
  is in the pinned set, so a single mutex-guarded connection is used, always
  behind `spawn_blocking` (`AppState::with_db`). Never lock it on a runtime
  thread.
- `events: broadcast::Sender<AppEvent>` (capacity **1024**) — the UI fan-out.
  Lagged subscribers drop stale events (surfaced as a `lagged` SSE marker)
  instead of back-pressuring producers.

### State machine

```
Stopped ──start──► Starting ──spawned──► Initializing ──handshake ok──► Running
   ▲                  │                        │                          │
   │                  └──── any failure ───────┴──► Errored ◄── crash ────┘
   └───────────── stop (from Running or Errored) ─────────────────────────┘
```

- `try_begin_start(id)` atomically claims the right to start: succeeds from
  absent (Stopped) or `Errored`, fails while any start/run is in flight —
  concurrent toggles cannot double-spawn.
- The **exit waiter** task owns the `Child`, reaps it, and settles the final
  status: `Stopped` if a stop was requested (a `stopping` flag is set before
  signalling), `Errored { "server exited unexpectedly (…)" }` otherwise.
- Tool calls are only possible in `Running` — the gateway 404s any server
  without an installed runtime.

`RunningServer` (all cheaply clonable): `pid`, `client: McpClient`,
`kill: KillHandle`, `handshake: ServerHandshake`, `stopping: Arc<AtomicBool>`,
`exited: watch::Receiver<bool>`.

### `AppEvent` (the UI event schema)

Serialized as tagged JSON; this is the exact wire format on `/sse`:

```json
{ "type": "status_changed", "server_id": 3, "status": { "state": "running" } }
{ "type": "status_changed", "server_id": 3,
  "status": { "state": "errored", "message": "handshake timeout" } }
{ "type": "log",     "server_id": 3, "stream": "stderr", "line": "listening…" }
{ "type": "log_gap", "server_id": 3, "stream": "stdout", "dropped": 412 }
{ "type": "notification", "server_id": 3, "payload": { "jsonrpc": "2.0", "method": "…" } }
```

Log lines are `Arc<str>` end-to-end — fan-out across threads never clones the
payload (serde's `rc` feature stays off via a `serialize_with` helper).

---

## 4. Process supervision (`mcp/process.rs`)

Servers are spawned with `tokio::process::Command` directly (deliberately
**not** `tauri-plugin-shell`), stdio fully piped, `kill_on_drop(true)` as
belt-and-braces.

**Orphan prevention is mandatory** — if MCPanel dies, every server it spawned
dies with it:

- **Unix:** the child calls `setpgid(0, 0)` in `pre_exec` *and* the parent
  mirrors `setpgid(pid, pid)` right after spawn — whichever runs first wins,
  closing the fork/exec race where a signal could target a group that doesn't
  exist yet. The whole tree is signalled with `kill(-pgid, …)` so
  grandchildren (`npx` → `node`) die too.
- **Linux extra:** `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec` — direct
  children die even if MCPanel is SIGKILLed and no cleanup code runs.
  Caveat: PDEATHSIG binds to the spawning **thread**; only spawn from
  long-lived runtime threads (never `spawn_blocking` workers).
- **Windows:** a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
  assigned from `child.raw_handle()`; `CREATE_NEW_PROCESS_GROUP` enables
  CTRL_BREAK delivery; `TerminateJobObject` covers the tree. Written per
  spec, compile-verified by CI's windows leg.

`KillHandle` is clonable and deliberately separate from the `Child`: the exit
waiter owns the `Child` for `wait()`; stop paths only need the handle.

**Graceful shutdown sequence** (`ManagedChild::shutdown` / `lifecycle::stop`):
SIGTERM (Unix) / CTRL_BREAK (Windows) to the tree → wait `SHUTDOWN_GRACE`
(**2 s**) → `kill_now()` (SIGKILL / TerminateJobObject) → reap.

---

## 5. Stream pipelines (`mcp/stream.rs`)

Two async pump tasks per child (stdout, stderr), each feeding a **bounded**
mpsc channel (`CHANNEL_CAPACITY = 1024` events per stream).

- **Framing:** a custom `CappedLines` decoder — *not* `LinesCodec*` — with a
  hard single-line cap (`MAX_LINE_LENGTH = 64 KiB`). An oversized line is a
  normal decoder *item* (discarded and counted), never a decode error,
  because tokio-util 0.7's `FramedRead` permanently terminates a stream after
  any codec error (`has_errored`); with `LinesCodec` one hostile line would
  kill the whole log pipeline. CRLF-tolerant; invalid UTF-8 is replaced
  lossily (these are logs, not the protocol layer).
- **Flood-proofing:** pumps never await channel space. A full channel means
  the line is dropped and counted — a flooding server can stall neither the
  pipeline nor the app.
- **Drop accounting:** nothing vanishes silently. Losses (backpressure +
  oversize discards) accumulate and surface as an in-order
  `StreamEvent::Dropped(n)` gap marker (flushed before the next delivered
  line, and finally at EOF). The invariant `delivered + dropped == produced`
  is tested.
- **Sanitizing:** `strip_ansi` removes CSI/OSC/two-char escape sequences and
  stray control bytes (tab survives), borrowing when the line is already
  clean. `json_candidate` returns the slice from the first `{` for salvaging
  garbage-prefixed NDJSON.

---

## 6. MCP protocol (`mcp/protocol.rs`)

stdio transport: **stdout is the protocol channel; stderr is logs.**

`connect(stdin, stdout_events, request_timeout)` spawns:

- a **writer task** owning stdin (newline-framed frames, flushed per write);
- a **router task** consuming the pumped stdout lines.

### Routing rules (per stdout line)

1. Parses as a JSON object containing `"jsonrpc"` — directly or after
   `json_candidate` salvage → dispatcher:
   - has `id` + (`result` | `error`) → **response**: resolves the pending
     oneshot for that id; JSON-RPC errors become `AppError::Rpc`.
   - has `id` + `method` → **server→client request** (e.g. `roots/list`):
     politely answered `-32601` so the server isn't left waiting.
   - has `method`, no `id` → **notification**: forwarded to a bounded
     advisory channel → `AppEvent::Notification`.
2. Anything else — servers misbehave — falls back into the **log buffer**
   (with the same drop accounting) instead of being lost.

### Correlation

Monotonic `AtomicI64` ids → oneshot senders in a `DashMap`. Every request has
a per-request timeout (default **30 s**; injectable) that removes its pending
entry. On stdout EOF the router fails **all** in-flight requests with
`ConnectionClosed` and flips a `closed` flag so subsequent requests fail fast
instead of timing out against a dead server.

### Handshake (Rust-owned)

```
MCPanel                          server
   │ initialize {protocolVersion: "2025-06-18",
   │             capabilities: {}, clientInfo}     ──►
   │ ◄── result {protocolVersion, capabilities, serverInfo}
   │ notifications/initialized                     ──►
   └─ status → Running; capabilities + serverInfo kept for the UI
```

Only after `notifications/initialized` may the UI issue tool calls. Handshake
failure or timeout ⇒ the process tree is torn down (`kill_now` + reap) and
the server is marked `Errored`.

---

## 7. HTTP gateway (`server/`)

Axum bound to `GATEWAY_ADDR = 127.0.0.1:6789` — the loopback bind is what
rejects non-local clients.

**Auth:** a random 32-byte bearer token, hex-encoded, generated **in memory
at startup** (ring `SystemRandom`), never persisted or logged. Comparison is
constant-time (hand-rolled XOR-accumulate fold with `black_box`; ring's
`verify_slices_are_equal` is deprecated in 0.17.14 with no replacement).
Every route requires it: `Authorization: Bearer <token>` or — because
`EventSource` cannot set headers — a `?token=` query parameter. The webview
obtains it via the `gateway_info` IPC command.

### `GET /sse`

Server-Sent Events stream: one `ready` event, then every `AppEvent`
pre-serialized to JSON as `event: app`, with keep-alives. A subscriber that
falls behind the broadcast channel receives

```json
{ "type": "lagged", "missed": 42 }
```

instead of silently missing events.

### `POST /mcp/{server_id}`

Forwards JSON-RPC to a **running** server. Body:

```json
{ "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {} }
```

- The gateway **re-correlates**: the child sees MCPanel's own monotonic ids;
  the caller's `id` is echoed back in the envelope.
- With `id`: awaits the correlated response →
  `{"jsonrpc":"2.0","id":7,"result":…}`. JSON-RPC-level errors come back as
  proper **error envelopes** (`{"error":{"code":-32601,…}}`, HTTP 200) so the
  workbench can inspect them; transport-level failures surface as HTTP errors
  per the table in §2.
- Without `id`: fire-and-forget notification → `{"accepted": true}`.
- Not running → 404. Malformed body → axum's 400/422 rejection.
- **30 s `tower::timeout`** middleware wraps the route (→ 504).

---

## 8. Config store (`db/`)

rusqlite (bundled — SQLite compiled in, no system dependency), opened in the
Tauri app-data dir as `mcpanel.sqlite` with WAL, foreign keys, and a 5 s busy
timeout.

**Migrations:** hand-rolled `PRAGMA user_version` — an append-only array of
statement batches; each unapplied batch runs in its own transaction that
bumps `user_version` on commit. No ORM. Never edit a shipped migration.

Schema (v1):

```sql
CREATE TABLE servers (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    command    TEXT NOT NULL,
    args       TEXT NOT NULL DEFAULT '[]',   -- JSON array of strings
    env        TEXT NOT NULL DEFAULT '{}',   -- JSON object: key → EnvValue
    cwd        TEXT,
    auto_start INTEGER NOT NULL DEFAULT 0
);
```

`EnvValue` JSON:

```json
{ "kind": "plain", "value": "visible" }
{ "kind": "secret" }
```

`Secret` is a bare marker with **no payload field** — a secret's value
structurally cannot reach the DB. CRUD: `insert_server`, `get_server`,
`list_servers` (name-ordered), `update_server`, `delete_server`; missing ids
→ `ServerNotFound`, constraint hits → `Db`, corrupt JSON columns → `Json`.

---

## 9. Secrets (`secrets.rs`)

Secret env values live in the OS credential manager (Keychain / Windows
Credential Manager / Secret Service) via **keyring v4** (default features
auto-select the platform store):

- Entry layout: service `mcpanel`, account `<server-name>/<KEY>`.
- **Just-in-time resolution:** `resolve_env(record)` runs at spawn, inside
  `spawn_blocking` (keyring is sync). Plain values pass through; secret
  markers are fetched. A missing secret **fails the start** — a server never
  silently launches without credentials it was configured to have.
- Resolved values exist only in the child's spawn config — never in state,
  DB, `AppEvent`s, or logs.
- **Redaction rule:** nothing in the crate may put a secret value into a
  tracing event, error message, or UI event. Log keys, not values (command
  spans `skip(value)`).
- Deleting an absent credential is treated as success (the desired end state
  is reached either way).

---

## 10. Tauri IPC commands (`commands/`)

All commands are async (never block the UI thread), start with
`info!(target: "app::commands", …)`, and carry tracing spans. Logic lives in
`commands/lifecycle.rs` as plain functions over `AppState` (fully testable
without a webview); the `#[tauri::command]` wrappers are thin.

| Command | Args | Returns | Notes |
| ------- | ---- | ------- | ----- |
| `list_servers` | — | `ServerOverview[]` | DB records joined with live status |
| `add_server` | `new: NewServer` | `ServerRecord` | id assigned by SQLite |
| `update_server` | `record: ServerRecord` | — | applies on next start |
| `remove_server` | `id` | — | stops first if running, then deletes |
| `start_server` | `id` | — | idempotent while starting/running |
| `stop_server` | `id` | — | graceful → grace 2 s → hard kill; clears `Errored` |
| `set_server_secret` | `id, key, value` | — | value → keyring; DB gets only the marker |
| `delete_server_secret` | `id, key` | — | removes credential + marker |
| `gateway_info` | — | `{url, token}` | gateway address + bearer token |

`ServerOverview` = `ServerRecord` (flattened) + `status: ServerStatus`.

### Start sequence (lifecycle)

```
try_begin_start (atomic guard, broadcasts Starting)
  → load record (spawn_blocking)
  → resolve env incl. secrets (spawn_blocking)     ── failure → Errored
  → spawn (process group / Job Object)             ── failure → Errored
  → attach stream pumps, connect protocol
  → broadcast Initializing
  → spawn log/notification fan-out tasks           (logs flow from here on)
  → handshake                                      ── failure → kill tree, Errored
  → install RunningServer handles, broadcast Running
  → spawn exit waiter (owns Child)
```

Unknown ids roll back to absent (no ghost `Errored` entry).

---

## 11. Frontend (repo-root `src/`)

| File | Responsibility |
| ---- | -------------- |
| `types.ts` | TS mirrors of the backend serde shapes (1:1) |
| `api.ts` | `invoke` wrappers + `{code,message}` error describer |
| `events.ts` | SSE subscription (`gateway_info` → `EventSource`, 2 s auto-reconnect) |
| `store.ts` | `usePanel` — server list, statuses, actions |
| `logs.ts` | `useLogs` — per-server ring buffers, batching, lagged counter |
| `workbench.ts` | `useWorkbench` — request body, templates, history, send |
| `components/ServerList.tsx` | rows: badge, toggle switch, logs/remove buttons |
| `components/LogViewer.tsx` | virtualized log pane |
| `components/Workbench.tsx` | CodeMirror editor + response pane + history |
| `components/AddServerForm.tsx` | minimal add form |

Design decisions worth knowing:

- **Statuses come from the backend, not optimism.** Toggles call
  `start/stop`; badge changes arrive via `status_changed` SSE events — the
  UI says "running" only when the handshake actually completed.
- **Log ingestion is batched** (100 ms flush) into per-server ring buffers
  capped at **5000** entries — a chatty server produces bounded re-renders.
- **Virtualization is hand-rolled** (fixed 20 px rows, overscan 10, spacer
  divs) — no list library, keeping the dependency set minimal. Follow-tail
  pins to the bottom, disengages on scroll-up, re-engages at bottom or via
  the follow button.
- `log_gap` events render as centered italic dividers; the stream-global
  `lagged` marker accumulates into a header warning.
- **Workbench** targets running servers only (selection self-corrects when a
  target stops), validates JSON client-side, POSTs to the gateway with the
  bearer token, pretty-prints responses, and keeps the last 20 requests as
  clickable history. CodeMirror 6 via `@uiw/react-codemirror` (not Monaco).

---

## 12. Testing

All backend behavior is driven through the **`mock-mcp-server`** fixture
(`src/bin/mock-mcp-server.rs`) — a tiny stdio binary speaking just enough
MCP. Default mode answers `initialize`, `tools/list`, `ping`, answers unknown
methods `-32601`, and exits on stdin EOF.

| Flag | Behavior | Exercises |
| ---- | -------- | --------- |
| `--spam` | floods stdout as fast as possible | flood-proofing, channel bounds |
| `--spawn-child` | spawns an idle grandchild, prints its pid | whole-tree kill |
| `--no-handshake` | never answers anything | handshake timeout → Errored |
| `--garbage` | non-JSON + control bytes on stdout before serving | garbage-tolerant routing |
| `--ansi` | ANSI-colored stderr | ANSI stripping |
| `--notify` | emits a notification after `initialized` | notification fan-out |
| `--idle` | (internal) sleep forever | the grandchild mode |

### Suites (`src-tauri/tests/`)

- `process_tree.rs` — the flagship orphan tests: controlled shutdown kills
  the whole tree (grandchild included; liveness via `/proc/<pid>/stat`
  treating `Z`/`X` as dead to dodge zombie false-positives); `kill_now`
  kills the tree; and the crash half — `orphan-harness` spawns a server
  through the real supervisor, the test **SIGKILLs the harness** (no cleanup
  runs), and the server dies anyway via PDEATHSIG.
- `streams.rs` — ANSI stripping, garbage tolerance, 300 ms flood stays
  bounded with a positive drop count.
- `protocol.rs` — handshake happy path/timeout, `-32601` surfacing, clean
  `ConnectionClosed` after stop, notification fan-out, garbage→log fallback.
- `lifecycle.rs` — full state-machine walk (broadcast order asserted),
  idempotent start, crash → `Errored(unexpectedly)`, handshake teardown,
  unknown-id start, remove-while-running.
- `gateway.rs` — a real fixture driven end-to-end over the HTTP surface.
- `secrets.rs` — keyring round-trips (skip cleanly when no OS store is
  reachable); unresolvable secret fails the start regardless.
- Unit tests live beside their modules (error shape, token auth, capped
  decoder, drop accounting, DB CRUD/migrations, frame parsing).

```bash
export PATH="$HOME/.cargo/bin:$PATH"           # cargo is user-local
cargo test   --manifest-path src-tauri/Cargo.toml            # all
cargo test <name> --manifest-path src-tauri/Cargo.toml       # single
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

Do **not** use `cargo test --release`: the release profile sets
`panic = "abort"`, which breaks `#[should_panic]`-style tests.

---

## 13. Build, dev, release

### Fresh clone

```bash
npm ci && npm run build     # REQUIRED before any cargo command:
                            # tauri::generate_context!() embeds dist/ and
                            # icons/ at compile time
cargo check --manifest-path src-tauri/Cargo.toml
```

### Dev loop

```bash
npm run tauri dev           # vite on :1420 + cargo run (default-run = mcpanel)
```

The crate has extra `[[bin]]` targets (test fixtures), so `default-run =
"mcpanel"` in `Cargo.toml` is what keeps bare `cargo run` unambiguous.

### Dependency policy (binding)

Versions in `src-tauri/Cargo.toml` and `package.json` were verified against
crates.io / npm on **2026-07-27**. Caret ranges + committed lockfiles are the
effective pins. Never "downgrade" to remembered versions; re-verify current
stable before adding any dep (crates.io API requires a `User-Agent` header).
`edition = "2024"`, `rust-version = "1.85"`.

### Release profile

`opt-level = "s"`, `lto = true`, `codegen-units = 1`, `strip = true`,
`panic = "abort"` → ~7.0 MB binary on Linux.

### CI (`.github/workflows/ci.yml`)

3-OS matrix (ubuntu / macos / windows): Linux webkit deps → `npm ci && npm
run build` → clippy `-D warnings` → debug-profile tests → release build →
**size budget gate** (< 15 MB, fails the job). The windows leg is what
compile-verifies the Job-Object code path.

### Release (`.github/workflows/release.yml`)

Tag `v*` → `tauri-action` builds bundles for Linux, Windows, and macOS
(arm64 + x86_64) and opens a draft GitHub release.

---

## 14. Constants reference

| Constant | Value | Where |
| -------- | ----- | ----- |
| Gateway address | `127.0.0.1:6789` | `server::GATEWAY_ADDR` |
| Gateway POST timeout | 30 s | `server::POST_TIMEOUT` |
| Auth token | 32 random bytes, hex | `server::AuthToken` |
| Request/handshake timeout | 30 s default | `protocol::DEFAULT_REQUEST_TIMEOUT` |
| Advertised MCP version | `2025-06-18` | `protocol::PROTOCOL_VERSION` |
| Shutdown grace | 2 s | `process::SHUTDOWN_GRACE` |
| Kill confirm timeout | 5 s | `lifecycle::KILL_CONFIRM_TIMEOUT` |
| Line cap | 64 KiB | `stream::MAX_LINE_LENGTH` |
| Stream channel capacity | 1024/stream | `stream::CHANNEL_CAPACITY` |
| Event broadcast capacity | 1024 | `state` |
| Notification channel | 256 | `protocol` |
| UI log ring buffer | 5000/server | `src/logs.ts` |
| UI log flush batch | 100 ms | `src/logs.ts` |
| Workbench history | 20 entries | `src/workbench.ts` |
| Binary size budget | < 15 MB | CI gate |
| Vite dev port | 1420 | `vite.config.ts` |
