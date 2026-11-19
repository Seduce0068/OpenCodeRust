---
type: Reference
description: PTY 機能の現在の実装先（opencode-server）を案内する。
tags: [pty, terminal, process, server]
---

# PTY (opencode-server)

`opencode-pty` crate は 2026-11-19 に削除された。PTY 機能は `opencode-server` の
`PtyManager` に統合済みで、独立 crate としては存在しない。

## 参照先

- 実装: `crates/opencode-server/src/pty.rs`
- モジュール文書: [opencode-server](opencode-server.md)
- HTTP ルート: `/pty/*`
