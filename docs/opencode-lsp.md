---
type: Reference
description: Language Server Protocol client と server registry を説明する。
tags: [lsp, code-intelligence, crate]
---

# opencode-lsp

`opencode-lsp` provides Language Server Protocol client support for code
intelligence and editing assistance.

## Responsibilities

- Start and manage LSP subprocesses
- Handle JSON-RPC requests, responses, and notifications
- Maintain client state and event stream
- Manage multi-language server registry

## Key types

- `LspClient`
- `LspClientRegistry`
- `LspServerConfig`
- `LspEvent`
- `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcNotification`
- `LspError`

## Use cases

- `opencode-tool` LSP tools (requires `lsp` feature)
- TUI / Server LSP status and debugging

## Development notes

- Each JSON-RPC request is bounded by a real 30s timeout (`REQUEST_TIMEOUT` in
  `lib.rs`). On timeout, cancellation, or write failure the pending entry is
  removed, so it cannot leak. Retries are not implemented.
- The stdin mutex is released before waiting for a response, so a slow request
  does not block other requests or notifications.
- `LspClient` retains the server `Child` and the stdout reader `JoinHandle`;
  `Drop` aborts the reader and kills the child so no orphan process is left.
- Keep URI/path conversion consistent
- Consider backpressure and capacity for event broadcast channels

## Validation

```bash
cargo check -p opencode-lsp
cargo test -p opencode-lsp
```
