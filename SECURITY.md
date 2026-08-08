# Security Policy

## Supported versions

Only the [latest release](https://github.com/Q01P/mcpanel/releases/latest) is supported. Older versions receive no fixes; upgrade before reporting.

## Reporting a vulnerability

**Do not open a public issue for security problems.** Email **taleb@xseth.com** with:

- what you found and where (file/endpoint/component),
- steps to reproduce or a proof of concept,
- the version or commit you tested.

You'll get an acknowledgment within a few days. Please give me a chance to ship a fix before any public disclosure; I'll coordinate timing with you and credit you in the release notes unless you'd rather stay anonymous.

## Scope

Most interesting areas, roughly in order:

- **The local HTTP gateway**: authentication (bearer token, constant-time comparison), Host-header validation, CORS policy, and anything that would let a browser page or another local process reach it without the token.
- **Secret handling**: anything that causes a secret value to land in config, the database, logs, events, or error messages instead of staying in the OS keyring.
- **Process supervision**: sandbox-adjacent issues in how server processes are spawned, supervised, and torn down (process groups, PDEATHSIG, Windows Job Objects).

The overall design is summarized in the [security model section of the README](README.md#security-model).

Out of scope: vulnerabilities in the MCP servers you configure MCPanel to run. MCPanel executes the commands you give it, with the privileges of your user, by design.
