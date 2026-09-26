# What users ask of a local MCP server manager (September 2026)

A survey of GitHub issues on MCP Inspector and the major MCP clients,
purpose-built manager tools, and community threads, ranked by how many
independent sources ask for the same thing. It is the rationale for the
0.2.0 feature set and the roadmap after it.

| # | Need | Sources | 0.2.0 |
|---|------|---------|-------|
| 1 | Per-tool enable/disable; show each server's context-token cost | ~12 | token estimate + long-description flags in the tools browser |
| 2 | Restart / hot-reload without restarting the client; honour `list_changed`; keep inputs across restarts | ~11 | restart button, `list_changed` re-lists, remembered tool inputs |
| 3 | Explain *why* a start failed (command not on the GUI PATH, `spawn npx ENOENT`, handshake timeout) | ~10 | login-shell PATH, command preflight, handshake shown on the row, MCP log messages in the viewer |
| 4 | Write config back to clients, not just import | ~9 | export as `mcpServers` JSON |
| 5 | Remote transports (Streamable HTTP / SSE) with OAuth | ~10 | roadmap |
| 6 | Health, crash counters, auto-restart with backoff | ~8 | auto-restart with backoff |
| 7 | Profiles / presets of servers per task | ~8 | roadmap |
| 8 | Persistent history, replay, remembered inputs | ~7 | history survives restarts; re-run |
| 9 | Resources and prompts browsers; elicitation / roots | ~8 | resources + prompts tabs |
| 10 | Per-server request timeout | ~6 | per-server timeout (handshake and default request) |
| 11 | Transparent stdio proxy to see what the real client sends | ~9 | roadmap |
| 12 | Tool annotation badges; tool-description drift detection | ~5 | annotation badges |

Representative evidence:

- Restart: anthropics/claude-code#54136 ("the current workaround is Cmd+Q
  + reopen Claude Desktop"); modelcontextprotocol/inspector#609 ("I have
  to re-fill in the tool input parameters each time the server
  re-connects").
- Diagnostics: anthropics/claude-code#49133 ("a server can 'fail' in two
  completely invisible ways before a single tool call is ever made");
  `spawn npx ENOENT` is called "the single most common MCP setup error".
- Token cost: modelcontextprotocol/modelcontextprotocol#2808 ("20 MCP
  tools registered has 10,000 fewer tokens available for actual work");
  inspector#523 asked for description-length warnings.
- Timeouts: inspector#142; anthropics/claude-code#63379.
- Sync to clients: xjeway/mcp-manager, EnjoyableWork/mcp-sync,
  tylergraydev/claude-code-tool-manager all exist to solve config drift.
- Resources and prompts: every comparable client (wong2/mcp-cli,
  mcp-use/inspector, Docker MCP gateway) offers them.
- Health: anthropics/claude-code#84363 wants a client that "automatically
  re-spawns the server process with retry logic".
