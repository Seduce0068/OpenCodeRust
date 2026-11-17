---
type: Reference
description: MCP client、transport、tool sync、OAuth 管理を説明する。
tags: [mcp, oauth, integration, crate]
---

# opencode-mcp

`opencode-mcp` implements the MCP (Model Context Protocol) client stack,
with support for multiple transports and OAuth.

## Responsibilities

- MCP client connection and session maintenance
- Tool sync and registration
- OAuth auth state management
- SSE/HTTP/stdio transport abstraction

## Module structure

- `client.rs` – Client and registry
- `tool.rs` – MCP tool wrapper
- `oauth.rs` / `auth.rs` – OAuth and auth flow
- `transport.rs` – Transport layer (HTTP/SSE/Stdio)
- `protocol.rs` – JSON-RPC protocol structures

## Key exports

- `McpClient` / `McpClientRegistry`
- `McpToolRegistry`
- `McpOAuthManager` / `OAuthRegistry`
- `MCP_TOOLS_CHANGED_EVENT`

## Use cases

- CLI `opencoderust mcp ...`
- Server `/mcp/*` routes
- Tool/session dynamic tool extension

## Timeouts

`McpTimeouts` holds per-operation timeouts in milliseconds:

- `startup_ms` – transport connect and `initialize` handshake (default 30s)
- `catalog_ms` – `tools/list` discovery and `resources/read` (default 30s)
- `execution_ms` – `tools/call` (default 12h)

The defaults follow the OpenCode MCP schema; see
[MCP servers](https://opencode.ai/docs/mcp-servers/) for the reference
behaviour. `McpConfig::timeout` (the global `mcp.timeout`) and a server's own
`timeout` are resolved per key by `McpTimeoutConfig::resolve`.

`McpClient::with_timeouts` applies them; `with_timeout(ms)` sets all three.
A timeout surfaces as `McpClientError::Timeout`.

`resources/read` uses the progress-aware request helper with `catalog_ms`, so a
server that never replies no longer blocks the calling task indefinitely.
A `notifications/progress` message extends the deadline by another `catalog_ms`.

## Validation

```bash
cargo check -p opencode-mcp
cargo test -p opencode-mcp
```
