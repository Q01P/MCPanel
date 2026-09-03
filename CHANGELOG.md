# Changelog

All notable changes to MCPanel are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Homebrew cask.** The repository doubles as a tap
  (`brew tap q01p/mcpanel https://github.com/Q01P/MCPanel` then
  `brew install --cask q01p/mcpanel/mcpanel`). The cask clears the quarantine
  flag at install time, so unsigned builds open without the "damaged" dialog.
- **winget manifests** under `packaging/winget`, in the layout the community
  repository expects, ready to submit.
- **Packaging sync workflow.** Publishing a GitHub release opens a pull
  request that updates the cask and winget manifests from the release's own
  asset digests and the MSI's ProductCode (`scripts/update-packaging.mjs`).
- **Signing-ready release workflow.** The macOS legs sign and notarize the
  app when the Apple signing secrets are configured, and build unsigned
  exactly as before when they are not.

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

[0.1.0]: https://github.com/Q01P/mcpanel/releases/tag/v0.1.0
