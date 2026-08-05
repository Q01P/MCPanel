# MCPanel — Full Reference

> **MCPanel** is a lightweight desktop app for managing local MCP (Model
> Context Protocol) servers — "Postman for MCP." This document consolidates
> the binding spec (`project.md`), the implementation reference
> (`docs/technical.md`), and the verified state of the codebase as of
> 2026-08-05 into a single file.

- **Owner:** Oussema Taleb ([@Q01P](https://github.com/Q01P), taleb@xseth.com)
- **Repository:** https://github.com/Q01P/mcpanel — MIT license
- **App identifier:** `com.xseth.mcpanel`
- **Release binary:** ~7.0 MB on Linux (budget: < 15 MB, CI-gated)
- **Status:** roadmap T0–T13 complete, plus the post-T13 hardening round
  I1–I9. One known open defect (gateway CORS, §12).

---

## 1. What the app does

| Feature | Behavior |
| --- | --- |
| **Service-style toggles** | Each configured server toggles on/off. Flipping on spawns the process *and* performs the MCP `initialize` handshake — "running" means genuinely ready for tool calls. |
| **Live log streaming** | stdout/stderr per server, line-parsed, ANSI-stripped, flood-proof at thousands of lines/second. |
| **JSON workbench** | CodeMirror editor to hand-craft JSON-RPC requests, fire them at a running server, inspect responses. |
| **No orphaned processes** | If MCPanel dies, every server it spawned dies with it (Unix process groups + PDEATHSIG / Windows Job Objects). |
| **Sane secrets** | API keys live in the OS credential manager (Keychain / Credential Manager / Secret Service), never in plaintext config. |

## 2. Tech stack & dependency policy

No Electron, no Node/Python backend:

| Piece | Tech |
| --- | --- |
| Shell | Tauri v2 (system webview) |
| Backend | Pure Rust (tokio), single crate `src-tauri/` (lib `mcpanel_lib` + thin `main.rs`) |
| UI ↔ backend streaming | Token-guarded Axum gateway on an ephemeral loopback port (SSE + JSON-RPC forwarding) |
| Config storage | SQLite (rusqlite, bundled) |
| Secrets | `keyring` v4 → OS credential store |
| Frontend | React 19 + Vite 8 + TypeScript + Zustand, repo-root `src/` |
| JSON editor | `@uiw/react-codemirror` (CodeMirror 6 — not Monaco) |

**Dependency policy (binding):** versions in `src-tauri/Cargo.toml` and
`package.json` were verified against crates.io/npm on 2026-07-27 (frontend
dev tooling 2026-08-03). Caret ranges + committed lockfiles are the effective
pins. Never downgrade to remembered versions; verify current stable before
adding any dep (the crates.io API requires a `User-Agent` header). Notable:

- `tauri 2.11.5`, `tauri-build 2.6.3`, `@tauri-apps/cli 2.11.x`
- `tokio 1.53.1`, `tokio-util 0.7.19`, `tokio-stream 0.1.19`
- `axum 0.8.9`, `hyper 1.11.0`, `tower 0.5.3`
- `rusqlite 0.40.1` (bundled), `dashmap 6.2.1`, `bytes 1.12.1`
- `keyring 4.1.5` (v4: default features auto-select the platform store)
- `thiserror 2.0.19`, `tracing 0.1.44`, `windows-sys 0.61.2`, `ring 0.17.14`
- `edition = "2024"` (do not change); `rust-version = "1.95"` — the
  compile-verified floor (keyring 4.1.5 declares 1.88; `libsqlite3-sys
  0.38.1` needs `cfg_select!`, stable since 1.95; verified failing on 1.94,
  passing on 1.95)
- `anyhow` is **dev-dependencies only** — production code funnels everything
  into `AppError`

## 3. Architecture overview

```
┌────────────────────────── MCPanel process ──────────────────────────┐
│  ┌─ webview (React) ─┐        ┌──────── Rust backend ────────────┐  │
│  │ server list       │ invoke │ commands/  (Tauri IPC)           │  │
│  │ log viewer        │───────►│   └─ lifecycle orchestration     │  │
│  │ JSON workbench    │        │ state.rs   AppState              │  │
│  │                   │  SSE   │   ├─ DashMap registry            │  │
│  │ EventSource ◄─────┼────────┤   ├─ SQLite (Mutex<Connection>)  │  │
│  │ fetch POST /mcp ──┼───────►│   └─ broadcast<AppEvent>         │  │
│  └───────────────────┘        │ server/    Axum gateway :0       │  │
│                               │ mcp/       process/stream/proto  │  │
│                               │ db/        config store          │  │
│                               │ secrets.rs OS credential store   │  │
│                               └───────────┬──────────────────────┘  │
└───────────────────────────────────────────┼─────────────────────────┘
                                            ▼ spawn (pgroup/Job Object)
                                   MCP server child processes
                                   (stdout = JSON-RPC, stderr = logs)
```

Two transports reach the webview: **Tauri IPC** (`invoke`) for
request/response commands, and the **HTTP gateway** for `GET /sse`
(continuous status/log/notification stream) and `POST /mcp/{server_id}`
(workbench forwarding).

### Module layout (`src-tauri/src/`)

| Path | Responsibility |
| --- | --- |
| `lib.rs` | tracing init, DB open, state/token, gateway spawn, Tauri builder |
| `main.rs` | thin entry → `mcpanel_lib::run()` |
| `error.rs` | `AppError` — the single error funnel |
| `state.rs` | `AppState`: registry + DB handle + event broadcast |
| `commands/` | Tauri IPC commands + `lifecycle.rs` orchestration |
| `mcp/process.rs` | spawn/reap with orphan prevention |
| `mcp/stream.rs` | stdout/stderr line pipelines |
| `mcp/protocol.rs` | JSON-RPC correlation + MCP handshake |
| `server/` | Axum gateway |
| `db/` | rusqlite config store + migrations |
| `secrets.rs` | keyring integration |
| `bin/` | test fixtures (`mock-mcp-server`, `orphan-harness`) |

## 4. Errors (`error.rs`)

`AppError` (thiserror): infrastructure failures via `#[from]`, domain
failures as explicit variants. Serializes for Tauri commands as
`{ "code": "...", "message": "..." }`; implements `IntoResponse` for Axum.

| Variant | `code` | Gateway HTTP | Source |
| --- | --- | --- | --- |
| `ServerNotFound` | `server_not_found` | 404 | unknown id / not running |
| `Handshake` | `handshake` | 502 | handshake failures |
| `Timeout` | `timeout` | 504 | request/handshake/stop timeouts |
| `Unauthorized` | `unauthorized` | 401 | bad/missing token or Host |
| `Rpc {code,message}` | `rpc` | 502 | JSON-RPC error responses |
| `ConnectionClosed` | `connection_closed` | 502 | server stdout EOF |
| `InvalidInput` / `Conflict` / `Internal` | — | 400 / 409 / 500 | validation, races |
| `Io` / `Db` / `Json` / `Keyring` | `io`/`db`/`json`/`keyring` | 500/500/502/500 | `#[from]` |

The child MCP server is the gateway's *upstream*: its timeouts map to 504,
its failures to 502. A `Json` error from a handler means the child's output
failed to (de)serialize (malformed request bodies are rejected by axum
before handlers run).

## 5. State (`state.rs`)

`AppState` is cheaply clonable:

- `registry: Arc<DashMap<ServerId, ServerEntry>>` — live servers only; an
  absent id reads as `Stopped`. Entries hold `ServerStatus`, a `StartHandle`
  (cancel token + settled watch) during starts, and a `RunningServer` after
  the handshake.
- `db: Arc<Mutex<rusqlite::Connection>>` — single connection, always used
  behind `spawn_blocking` (`AppState::with_db`).
- `events: broadcast::Sender<AppEvent>` (capacity 1024) — lagged subscribers
  drop stale events (surfaced as a `lagged` SSE marker) instead of
  back-pressuring producers.

### State machine

```
Stopped ──start──► Starting ──spawned──► Initializing ──handshake ok──► Running
   ▲                  │                        │                          │
   │                  └──── any failure ───────┴──► Errored ◄── crash ────┘
   └────────── stop (from any non-Stopped state, incl. cancel) ───────────┘
```

- `try_begin_start(id)` atomically claims the start (succeeds from absent or
  `Errored`); concurrent toggles cannot double-spawn.
- **Starts are cancellable:** `stop`/`remove` during Starting/Initializing
  cancels the token, kills anything spawned, settles Stopped (bounded 10 s
  wait). `try_install_runtime` promotes to Running atomically and fails if
  cancelled meanwhile — a late handshake can never resurrect a removed server.
- The exit-waiter task owns the `Child`, reaps it, and settles final status:
  `Stopped` if a stop was requested, `Errored("server exited unexpectedly")`
  otherwise.

### `AppEvent` wire format (SSE `event: app`)

```json
{ "type": "status_changed", "server_id": 3, "status": { "state": "running" } }
{ "type": "log",     "server_id": 3, "stream": "stderr", "line": "listening…" }
{ "type": "log_gap", "server_id": 3, "stream": "stdout", "dropped": 412 }
{ "type": "notification", "server_id": 3, "payload": { "jsonrpc": "2.0", "method": "…" } }
{ "type": "notification_gap", "server_id": 3, "dropped": 144 }
{ "type": "lagged", "missed": 42 }
```

Log lines are `Arc<str>` end-to-end — fan-out never clones the payload.

## 6. Process supervision (`mcp/process.rs`)

Spawned with `tokio::process::Command` directly (not `tauri-plugin-shell`),
stdio piped, `kill_on_drop(true)` as belt-and-braces.

- **Unix:** child calls `setpgid(0,0)` in `pre_exec` *and* the parent mirrors
  `setpgid(pid,pid)` after spawn (closes the fork/exec race). Tree kill =
  `kill(-pgid, …)` so grandchildren (`npx` → `node`) die too.
- **Linux extra:** `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec` — direct
  children die even if MCPanel is SIGKILLed. PDEATHSIG binds to the spawning
  *thread*: only spawn from long-lived runtime threads.
- **Windows:** Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` from
  `child.raw_handle()`; `CREATE_NEW_PROCESS_GROUP` for CTRL_BREAK;
  `TerminateJobObject` covers the tree. Compile-verified by CI only.
- **Graceful shutdown:** SIGTERM / CTRL_BREAK → 2 s grace
  (`SHUTDOWN_GRACE`) → SIGKILL / TerminateJobObject → reap.
- `KillHandle` is clonable and separate from the `Child` (the exit waiter
  owns the `Child` for `wait()`).

Accepted residual gaps:

- **Unix + supervisor SIGKILL:** `kill(-pgid)` never runs and PDEATHSIG only
  covers direct children — a reparented grandchild survives. No watchdog
  helper by design; Windows genuinely covers this case.
- **Windows graceful shutdown unverified on real hardware:** a GUI-subsystem
  app shares no console, so the CTRL_BREAK phase is likely a no-op and stops
  degrade to grace-then-terminate.

## 7. Stream pipelines (`mcp/stream.rs`)

Two pump tasks per child (stdout, stderr) feeding bounded mpsc channels
(1024 events/stream).

- **Framing:** custom `CappedLines` decoder — deliberately *not*
  `LinesCodec` — with a 64 KiB single-line cap. Oversized lines are normal
  decoder items (discarded + counted), never decode errors, because
  tokio-util 0.7's `FramedRead` permanently terminates after any codec error.
  CRLF-tolerant; invalid UTF-8 replaced lossily.
- **Flood-proofing:** pumps never await channel space — full channel means
  drop + count.
- **Drop accounting:** losses surface as in-order `Dropped(n)` gap markers;
  invariant `delivered + dropped == produced` is tested.
- **Sanitizing:** `strip_ansi` removes CSI/OSC/two-char escapes and control
  bytes (tab survives), borrowing when clean; `json_candidate` salvages
  garbage-prefixed NDJSON from the first `{`.

## 8. MCP protocol (`mcp/protocol.rs`)

stdio transport: **stdout is the protocol channel; stderr is logs.**
`connect()` spawns a writer task (owns stdin) and a router task.

Routing per stdout line: JSON object with `"jsonrpc"` (directly or after
salvage) → dispatcher — responses resolve pending oneshots (JSON-RPC errors
become `AppError::Rpc`); server→client `ping` gets an empty result; other
server requests get `-32601`; notifications go to a bounded advisory channel
(256) with overflow counted and flushed as `notification_gap`. Anything else
falls back into the log buffer.

- **Correlation:** monotonic `AtomicI64` ids → oneshot senders in a DashMap;
  per-request timeout (default 30 s, per-call overridable). On stdout EOF all
  in-flight requests fail with `ConnectionClosed` and a `closed` flag makes
  later requests fail fast.
- **Handshake (Rust-owned):** send `initialize` (advertising `2025-06-18`),
  validate the returned `protocolVersion` against `2025-06-18` / `2025-03-26`
  / `2024-11-05` *before* sending `notifications/initialized`; anything else
  (or missing) fails the handshake → tree torn down → `Errored`.
  `capabilities` + `serverInfo` are kept for the UI.

## 9. HTTP gateway (`server/`)

- Axum on `127.0.0.1:0` — bound synchronously in `setup` (bind failure aborts
  launch loudly); real address handed out via the `gateway_info` command. No
  fixed port: second instances coexist; no local page can fingerprint the app.
- **Auth:** random 32-byte hex bearer token, in-memory only (ring
  `SystemRandom`), constant-time comparison. Required on every route:
  `Authorization: Bearer <token>`; `?token=` fallback on `GET /sse` only
  (`EventSource` cannot set headers). Nothing logs request URIs.
- **Host validation** before the token check on every route: Host must be the
  bound address or its `localhost` spelling — DNS-rebinding defense.
- `GET /sse`: one `ready` event, then every `AppEvent` pre-serialized as
  `event: app`, with keep-alives; a lagging subscriber gets
  `{"type":"lagged","missed":n}`.
- `POST /mcp/{server_id}`: forwards JSON-RPC to a **running** server. The
  gateway re-correlates (child sees MCPanel's ids; caller's `id` echoed
  back). With `id` → awaited response; JSON-RPC errors return as proper error
  envelopes with HTTP 200; transport failures as HTTP errors (§4 table).
  Without `id` → `{"accepted": true}`. Not running → 404. Timeout via
  `?timeout_s=` clamped 1–300 s (default 30); `tower` timeout layer is a
  310 s backstop strictly above the cap.

## 10. Config store (`db/`) and secrets (`secrets.rs`)

SQLite via rusqlite (bundled), at the Tauri app-data dir as `mcpanel.sqlite`
(WAL, foreign keys, 5 s busy timeout). Hand-rolled `PRAGMA user_version`
migrations — append-only batches, each in its own transaction. Schema v2:

```sql
CREATE TABLE servers (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,   -- ids never reused
    name       TEXT NOT NULL UNIQUE,
    command    TEXT NOT NULL,
    args       TEXT NOT NULL DEFAULT '[]',   -- JSON array of strings
    env        TEXT NOT NULL DEFAULT '{}',   -- JSON object: key → EnvValue
    cwd        TEXT,
    auto_start INTEGER NOT NULL DEFAULT 0
);
```

`EnvValue` is `{"kind":"plain","value":…}` or `{"kind":"secret"}` — the
secret marker has **no payload field**, so a secret value structurally cannot
reach the DB.

Secrets live in the OS credential store, service `mcpanel`, account
`<server-id>/<KEY>` — keyed by id (names are mutable; ids never recur).
Legacy name-keyed entries migrate once at startup (idempotent, off the
resolve path). `remove_server` deletes entries with the row; `update_server`
sweeps entries for dropped markers. **Just-in-time resolution** at spawn
inside `spawn_blocking`; a missing secret fails the start. Resolved values
exist only in the child's spawn config — never in state, DB, events, or
logs. Redaction rule: log keys, never values.

## 11. Tauri IPC commands (`commands/`)

All async, all start with `info!(target: "app::commands", …)`, logic in
`commands/lifecycle.rs` as plain functions over `AppState`.

| Command | Args | Returns | Notes |
| --- | --- | --- | --- |
| `list_servers` | — | `ServerOverview[]` | DB records + live status |
| `add_server` | `new: NewServer` | `ServerRecord` | id assigned by SQLite |
| `update_server` | `record` | — | applies next start; sweeps dropped secret markers |
| `remove_server` | `id` | — | stop (cancelling in-flight start) → keyring entries → row |
| `start_server` | `id` | — | idempotent while starting/running |
| `stop_server` | `id` | — | cancels in-flight start; graceful → 2 s → hard kill; clears Errored |
| `set_server_secret` | `id, key, value` | — | value → keyring; DB gets the marker |
| `delete_server_secret` | `id, key` | — | removes credential + marker |
| `gateway_info` | — | `{url, token}` | bound address + bearer token |

Start sequence: `try_begin_start` → load record → resolve env (secrets) →
spawn → attach pumps + protocol → Initializing → fan-out tasks → handshake
(select! on cancel token) → `try_install_runtime` (→ Running) → exit waiter.
Every failure edge lands in `Errored`; every cancel edge kills what was
spawned and lands in `Stopped`. At launch `start_auto_servers` sweeps
auto-start records concurrently.

## 12. Frontend (repo-root `src/`)

| File | Responsibility |
| --- | --- |
| `types.ts` | 1:1 TS mirrors of backend serde shapes |
| `api.ts` | `invoke` wrappers + `{code,message}` error describer |
| `events.ts` | SSE subscription (`gateway_info` → `EventSource`, 2 s reconnect; `ready` triggers resync) |
| `store.ts` | `usePanel` — server list, statuses, actions, edit state |
| `logs.ts` | `useLogs` — per-server ring buffers (5000 cap), 100 ms batching, gap/lagged markers, removal tombstones |
| `workbench.ts` | `useWorkbench` — body, templates, history (20), send |
| `envRows.ts` | pure row ↔ env-map conversion (secret values never serialize) |
| `components/` | `ServerList`, `LogViewer` (hand-rolled virtualization), `Workbench`, `ServerForm`, `ErrorBoundary` |

Design decisions: statuses come from the backend, not optimism (badges change
on `status_changed` events; the toggle stays enabled during starts so a hung
start can be cancelled). Every SSE `ready` and every `lagged` marker
refetches `list_servers` (debounced 250 ms) — the list is the authority.
Removed ids are tombstoned so late events can't resurrect buffers. Secrets
ride `set_server_secret`, never the config payload.

### Known defect — gateway CORS (found in the 2026-08-05 live smoke test)

The gateway has **no CORS layer**: responses carry no
`Access-Control-Allow-Origin`, and preflight `OPTIONS /mcp/{id}` returns 405.
Consequence in the real webview (cross-origin: page origin
`http://localhost:1420` in dev / `tauri://localhost` in prod vs gateway
origin `http://127.0.0.1:<port>`):

- the `EventSource` SSE stream fails → no live status/log/notification
  updates (a started server keeps showing STOPPED until a reload resyncs via
  `list_servers` over IPC);
- the workbench POST (whose `Authorization` header forces a preflight) fails
  with "request failed: Load failed".

The test pyramid cannot see this: Rust gateway tests speak raw hyper (no
browser CORS enforcement) and frontend tests mock `fetch`. Fix direction: a
CORS layer on the Axum router allowing the exact dev/prod origins plus
preflight handling (new dep — verify per the dependency policy). Secondary
nit found alongside: an unauthenticated malformed POST gets 422 from the
`Json` extractor before `authorize()` runs — auth should precede body
parsing. Both pending owner scoping.

## 13. Testing

### Fixture bins (`src-tauri/src/bin/`)

`mock-mcp-server` — tiny stdio binary speaking just enough MCP: answers
`initialize`, `tools/list`, `ping`, `-32601` for unknown methods, exits on
stdin EOF. Failure-mode flags:

| Flag | Behavior | Exercises |
| --- | --- | --- |
| `--spam` | floods stdout | flood-proofing, channel bounds |
| `--spawn-child` | idle grandchild, prints pid | whole-tree kill |
| `--no-handshake` | never answers | handshake timeout → Errored |
| `--garbage` | non-JSON on stdout first | garbage-tolerant routing |
| `--ansi` | ANSI-colored stderr | ANSI stripping |
| `--notify` | one notification after `initialized` | notification fan-out |
| `--notify-flood` | 400 notifications | drop accounting |
| `--wrong-version` | bogus protocolVersion | unsupported-version disconnect |
| `--ping-client` | server→client ping, pong on stderr | liveness reply |
| `--idle` | sleep forever | the grandchild mode |

`orphan-harness` — spawns a server through the real supervisor and parks;
the crash-half flagship test SIGKILLs it and asserts the server dies anyway
(PDEATHSIG).

### Test map (counts verified by a full run on 2026-08-05 — all green)

**Rust: 65 tests** (29 unit + 36 integration, all `--locked`, debug profile):

| Suite | Tests | Covers |
| --- | --- | --- |
| `tests/lifecycle.rs` | 11 | state-machine walk, crash → Errored, handshake teardown, event fan-out, add/remove validation, stop-cancels-start, remove-while-starting, concurrent start+remove, auto-start sweep |
| `tests/protocol.rs` | 9 | handshake happy/timeout, `-32601` surfacing, clean failure after stop, notification fan-out + overflow gap, server-ping reply, wrong-version disconnect, garbage fallback |
| `tests/secrets.rs` | 8 | keyring round-trips, rename/remove/recreate semantics, name→id migration, unresolvable secret fails start |
| `tests/process_tree.rs` | 3 | flagship orphan tests: controlled-shutdown tree kill, kill-now, SIGKILL-the-supervisor PDEATHSIG (Linux-only) |
| `tests/streams.rs` | 3 | ANSI strip, garbage bytes, flood bounded with positive drop count |
| `tests/gateway.rs` | 2 | end-to-end HTTP forwarding, error envelopes, 404 after stop and crash |
| unit (`mod tests`) | 29 | `server/` 10, `db/` 6, `state.rs` 6, `mcp/stream.rs` 4, `error.rs` 2, `mcp/protocol.rs` 1 |

**Keyring gate:** 7 of the 8 `secrets.rs` tests self-skip (pass silently
with an eprintln) unless `MCPANEL_TEST_KEYRING=1` is set *and* a probe
round-trip succeeds (`store_available()`, `tests/secrets.rs:29`) — probing
macOS CI hangs on a keychain prompt. Store-facing tests use negative ids so
they can't touch real entries. CI never runs this leg; run it locally:

```bash
MCPANEL_TEST_KEYRING=1 cargo test --locked --manifest-path src-tauri/Cargo.toml
```

**Frontend: 34 vitest tests** (happy-dom):

| File | Tests | Covers |
| --- | --- | --- |
| `store.test.ts` | 9 | `applyEvent` patches, mutation success/failure, load-failure vs empty, debounced resync |
| `logs.test.ts` | 8 | flush batching, ring cap, gap/lagged markers, bucket drop, resurrection guard |
| `workbench.test.ts` | 6 | timeout clamp (1–300), template validity, send guards |
| `envRows.test.ts` | 5 | env row ↔ config mapping, secret markers, blank keys |
| `api.test.ts` | 3 | `describeError` shapes |
| `events.test.ts` | 3 | SSE frame parsing, defensive failures |

Shared Rust helpers in `tests/common/mod.rs` (`alive()` with Linux `/proc`
zombie handling, `wait_for`, fixture builders) — never re-roll them per
suite. **Never `cargo test --release`** (`panic = "abort"` breaks
`#[should_panic]`).

## 14. Development

Cargo is user-local: `export PATH="$HOME/.cargo/bin:$PATH"`.

```bash
# fresh clone — REQUIRED before any cargo command:
npm ci && npm run build        # generate_context!() embeds dist/ + icons/

cargo check  --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test   --manifest-path src-tauri/Cargo.toml            # single: cargo test <name> …

npm run tauri dev   # vite on :1420 + cargo run (default-run = mcpanel)
npm test            # vitest
npm run lint        # biome (a11y rules on)
npm run typecheck   # tsc --noEmit
```

Linux build prerequisites: `libwebkit2gtk-4.1-dev build-essential libxdo-dev
libssl-dev libayatana-appindicator3-dev librsvg2-dev`.

Release profile: `opt-level = "s"`, `lto = true`, `codegen-units = 1`,
`strip = true`, `panic = "abort"`.

**Workflow rule (binding):** one scoped roadmap task at a time, owner review
between tasks; never scaffold ahead.

## 15. CI & release

`.github/workflows/ci.yml` — three jobs, everything `--locked`:

- **test** — 3-OS matrix (ubuntu/macos/windows), 45 min timeout, Node 20:
  webkit deps (Linux) → `npm ci && npm run build` → `fmt --check` (Linux) →
  clippy `-D warnings` → debug tests → release build + <15 MB size gate
  (Linux leg). The windows leg compile-verifies the Job-Object path
  (integration suites are `#![cfg(unix)]`).
- **frontend** (ubuntu) — biome lint → `tsc --noEmit` → vitest.
- **msrv** (ubuntu) — `cargo check` on 1.95.

`.github/workflows/release.yml` — tag `v*` → check job (clippy + full tests)
gates → `tauri-action` builds Linux/Windows/macOS (arm64 + x86_64) bundles →
draft GitHub release. Each leg runs `cargo fetch --locked`.

## 16. Constants reference

| Constant | Value | Where |
| --- | --- | --- |
| Gateway address | `127.0.0.1:<ephemeral>` | `server::bind` → `GatewayAddr` |
| Forward timeout cap | 300 s (`?timeout_s=`) | `server::MAX_FORWARD_TIMEOUT` |
| Gateway POST backstop | 310 s | `server::GATEWAY_BACKSTOP_TIMEOUT` |
| Auth token | 32 random bytes, hex | `server::AuthToken` |
| Request/handshake timeout | 30 s default | `protocol::DEFAULT_REQUEST_TIMEOUT` |
| Advertised MCP version | `2025-06-18` | `protocol::PROTOCOL_VERSION` |
| Accepted MCP versions | 2025-06-18 / 2025-03-26 / 2024-11-05 | `protocol::SUPPORTED_PROTOCOL_VERSIONS` |
| Shutdown grace | 2 s | `process::SHUTDOWN_GRACE` |
| Kill confirm timeout | 5 s | `lifecycle::KILL_CONFIRM_TIMEOUT` |
| Cancel settle timeout | 10 s | `lifecycle::CANCEL_SETTLE_TIMEOUT` |
| Line cap | 64 KiB | `stream::MAX_LINE_LENGTH` |
| Stream channel capacity | 1024/stream | `stream::CHANNEL_CAPACITY` |
| Event broadcast capacity | 1024 | `state` |
| Notification channel | 256 | `protocol` |
| UI log ring buffer | 5000/server | `src/logs.ts` |
| UI log flush batch | 100 ms | `src/logs.ts` |
| Workbench history | 20 entries | `src/workbench.ts` |
| Binary size budget | < 15 MB | CI gate |
| Vite dev port | 1420 | `vite.config.ts` |

## 17. History note

An earlier cloud prototyping session built milestones M0–M2 (workspace
layout, supervisor, MCP client, 17 passing tests) but was never pushed; that
environment is gone and its architecture diverged from the spec (Cargo
workspace, edition 2021, older deps). This repo is canonical; the proven
behaviors — supervisor semantics, stdout routing, the fixture design, the
orphan tests — were re-implemented here as tasks T3–T5.
