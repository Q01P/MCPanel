# Changelog

All notable changes to MCPanel are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-08

First public release.

### Added

- **Server management**: add, edit, and remove local MCP server configurations
  (command, args, env vars), stored in SQLite.
- **Service-style toggles**: starting a server spawns the process and completes
  the MCP `initialize` handshake before it's shown as running — "running" means
  ready for tool calls.
- **Live log streaming**: per-server stdout/stderr, line by line, ANSI escapes
  stripped. Flood-proof by design: 64 KiB line cap, bounded buffering with
  counted drop markers, capped scrollback in the UI.
- **JSON-RPC workbench**: a CodeMirror editor to hand-craft requests, send them
  to a running server, and inspect responses.
- **Process supervision**: servers run in Unix process groups with PDEATHSIG on
  Linux, or Windows Job Objects with kill-on-close — no orphaned processes when
  MCPanel exits or crashes. Graceful stop (2 s grace, then hard kill).
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
