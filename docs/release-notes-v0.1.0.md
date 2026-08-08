# MCPanel v0.1.0

First public release.

MCPanel is a lightweight desktop app for managing local MCP (Model Context Protocol) servers: **"Postman for MCP."** Flip a server on like a service, watch its logs stream live, and fire hand-crafted JSON-RPC requests at it from a built-in workbench. No Electron, no bundled runtime, just a ~7 MB native binary (Tauri + Rust).

## Highlights

- **Toggles that tell the truth**: a server shows as *running* only after the MCP `initialize` handshake completes, so "running" means ready for tool calls.
- **Flood-proof live logs**: stdout/stderr streamed line by line, ANSI stripped; a server logging thousands of lines per second can't freeze the UI (oversized lines capped, overflow counted and reported as dropped).
- **JSON-RPC workbench**: hand-craft requests in a CodeMirror editor, send, inspect the response.
- **No orphaned processes**: Unix process groups + PDEATHSIG, Windows Job Objects with kill-on-close. If MCPanel dies, its servers die with it.
- **Secrets done right**: API keys live in the OS credential manager, are resolved just-in-time at spawn, and never touch config files, events, or logs.
- **Hardened local gateway**: loopback-only ephemeral port, per-launch random 32-byte bearer token compared in constant time, Host-header validation against DNS rebinding, CORS pinned to the app's own origins.

## Install

Downloads for macOS (Apple Silicon & Intel), Windows, and Linux (deb / rpm / AppImage) are attached below.

**These builds are unsigned**, so your OS will complain on first launch: macOS calls the app "damaged," Windows shows a SmartScreen warning. Both are expected; the [install section of the README](https://github.com/Q01P/mcpanel#install) has the two-line fix for each.

## Known limitations

- On Unix, if MCPanel itself is SIGKILLed, a reparented grandchild process can survive (PDEATHSIG covers direct children only). Normal exits and crashes are fully covered.
- Windows graceful shutdown is compile-verified but untested on real hardware and likely degrades to grace-then-terminate.

**Windows testers wanted:** if you can run MCPanel on a real Windows box, [open an issue](https://github.com/Q01P/mcpanel/issues) with what you find, especially around stopping servers.

Thanks to everyone who tried the early builds and reported back. Bug reports and PRs welcome; see [CONTRIBUTING.md](https://github.com/Q01P/mcpanel/blob/main/CONTRIBUTING.md).
