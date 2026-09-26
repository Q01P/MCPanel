# Changelog

All notable changes to MCPanel are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-09-26

The feature set was chosen from a survey of what people ask of MCP
tooling — see `docs/user-research-2026-09.md`.

### Added

- **Resources and prompts browsers** beside the tools browser:
  `resources/list` and `resources/templates/list` as one list (a template's
  RFC 6570 variables become a form), `resources/read` rendered by content
  type — text, pretty-printed JSON for JSON mime types, blobs summarized
  with the base64 in the raw result; `prompts/list` as a list, declared
  arguments as a form, `prompts/get` rendered as messages. Both follow
  their `list_changed` notifications, land in the shared history, and hand
  the exact request to the raw editor.
- **Restart** button on a running or errored server: stop and start as one
  operation, for the edit → restart → retest loop.
- **"Command not found" that names the command.** Servers are spawned with
  a PATH that merges the login shell's (`nvm`, `uv`, Homebrew…) with the
  app's own, handed down to grandchildren too, and the command is resolved
  before spawn (PATHEXT-aware on Windows). A miss reads
  "command not found: npx — not in any of the N PATH directories MCPanel
  can see" with the fix, instead of `os error 2`.
- **Per-server request timeout** (blank = 30 s): bounds the handshake — a
  cold `npx -y` install routinely exceeds 30 s — and every request without
  its own. The workbench timeout field is blank by default and defers to
  it, showing the server's setting as its placeholder.
- **Restart on crash**, opt-in per server: respawn with exponential backoff
  (1 s, 2 s, 4 s … capped at 30 s, five tries), "restarting in N s
  (attempt k of 5)" on the row meanwhile, a fresh streak after a minute of
  uptime, and a deliberate stop or remove cancels the timer.
- **Server identity on the row** while running: name and version from
  `serverInfo`, the negotiated protocol revision, and the advertised
  capabilities.
- **MCP log messages in the log viewer.** `notifications/message` — the
  logging channel well-behaved servers use instead of stderr — is a third,
  level-tinted stream; dropped notifications show as a gap marker. The
  tools browser re-lists on `notifications/tools/list_changed`, keeping
  the selection when the tool survives.
- **History survives restarts** (cap 50) with a one-click re-run and a
  clear action; the tools browser **remembers the last-typed inputs** per
  server and tool.
- **Context cost and annotations** in the tools browser: a ~tokens
  estimate per tool and for the whole list, long-description warnings at
  500 and 1000 characters, and `readOnlyHint` / `destructiveHint` /
  `idempotentHint` / `openWorldHint` as badges (only the hints a tool sets).
- **Export** the selected servers as a `mcpServers` document (Claude
  Desktop, Claude Code, Cursor, Windsurf) or VS Code's `servers` shape:
  copy it, or save it as a new file — never over an existing one. Secret
  env values are written as `${KEY}` placeholders unless "include secret
  values" is ticked, which resolves them from the keyring under a warning.
- The mock fixture serves resources and prompts, so the browsers can be
  exercised end to end.

- **Tools browser** in the workbench: a running server's `tools/list` is
  shown as a list, a selected tool's `inputSchema` is rendered as a form
  (string, number, integer, boolean, and string-enum properties get typed
  controls; anything else is a JSON field; a schema with no properties is a
  single JSON object field), and `tools/call` fires from it. Coercion is
  strict — `"12abc"` is rejected as a number, `1.5` as an integer — and an
  empty optional field is omitted rather than sent as `""`. Results render
  their text content as text, other content and `structuredContent` as
  labelled JSON, with the raw result one click away; a tool's own `isError`
  is shown distinctly from a JSON-RPC error and from a transport failure.
  `tools/list` pagination via `nextCursor` is followed. The raw JSON-RPC
  editor is the second tab; every tool call is recorded in the shared
  history as the request it amounted to, and **open in editor** hands the
  current call over to it.

- **Import from other MCP clients**: MCPanel now scans the standard config
  locations for Claude Desktop, Claude Code, Cursor, VS Code, and Windsurf,
  and offers the stdio servers it finds for import; a config file at any other
  path can be read by pasting it in. Entries it can't honour (remote `url` /
  `http` / `sse` servers, entries with no command) are listed with the reason
  rather than dropped silently, name clashes are imported as `name (2)`, and
  imported servers are never armed to auto-start.
- Importing moves credentials **out** of plaintext config: environment
  variables whose names look like credentials are written to the OS keyring
  and kept here only as markers. Their values are read from the source file
  backend-side and never cross into the UI. A server whose credentials fail to
  store is rolled back rather than left unable to start.

### Changed

- Schema v3: `request_timeout_s` and `restart_on_crash` columns; older
  databases migrate in place.
- **Node 22+ is now required** to build the frontend (previously Node 20+).
  Node 20 reached end of life in April 2026, and Vitest 5 — which the test
  suite now runs on — does not support it. CI and the release workflow build
  on Node 22.
- Dependency refresh, no behaviour change: Tauri 2.12 (wry 0.57 / tao 0.37),
  keyring 4.2, rusqlite 0.40.2, thiserror 2.0.21, hyper 1.11.1,
  tower-http 0.7.1 on the backend; React 19.3, Vite 8.3, Vitest 5, Biome
  2.5.14, `@tauri-apps/api` / `@tauri-apps/cli` 2.12 on the frontend.
  The Rust MSRV stays at 1.95.

## [0.1.0] - 2026-08-08

First public release.

### Added

- **Server management**: add, edit, and remove local MCP server configurations
  (command, args, env vars), stored in SQLite.
- **Service-style toggles**: starting a server spawns the process and completes
  the MCP `initialize` handshake before it's shown as running; "running" means
  ready for tool calls.
- **Live log streaming**: per-server stdout/stderr, line by line, ANSI escapes
  stripped. Flood-proof by design: 64 KiB line cap, bounded buffering with
  counted drop markers, capped scrollback in the UI.
- **JSON-RPC workbench**: a CodeMirror editor to hand-craft requests, send them
  to a running server, and inspect responses.
- **Process supervision**: servers run in Unix process groups with PDEATHSIG on
  Linux, or Windows Job Objects with kill-on-close, so there are no orphaned
  processes when MCPanel exits or crashes. Graceful stop (2 s grace, then hard kill).
- **Secrets in the OS keyring**: env values marked secret live in the OS
  credential manager (Keychain / Windows Credential Manager / Secret Service),
  are resolved just-in-time at spawn, and never appear in config, events, or
  logs.
- **Hardened local gateway**: the UI talks to the backend over an Axum server
  bound to `127.0.0.1` on an ephemeral port, guarded by a per-launch random
  32-byte bearer token (constant-time comparison), Host-header validation
  against DNS rebinding, and CORS pinned to the app's webview origins.

### Known limitations

- On Unix, if MCPanel itself is SIGKILLed, a reparented grandchild process can
  survive (PDEATHSIG covers direct children only).
- Windows graceful shutdown is compile-verified but untested on real hardware
  and likely degrades to grace-then-terminate.

[0.2.0]: https://github.com/Q01P/mcpanel/releases/tag/v0.2.0
[0.1.0]: https://github.com/Q01P/mcpanel/releases/tag/v0.1.0
