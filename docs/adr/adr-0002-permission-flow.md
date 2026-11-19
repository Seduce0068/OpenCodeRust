---
type: ADR
title: ツール実行権限フローの配線
status: approved
date: "2026-01-12T00:00:00+09:00"
deciders:
  - OpenCodeRust Team
tags:
  - security
  - server
  - prompt
  - permission
generated:
  by: OpenCodeRust Team
  at: "2026-01-12T00:00:00+09:00"
---

## ADR-0002: ツール実行権限フローの配線

## Context

ツール実行前のユーザー許可フローが未実装だった。TypeScript版では
`PermissionBroker` パターンで実現されており、Rust版でも同等の機能が必要。

## Decision

### 1. PermissionBroker 実装（サーバ側）

- `oneshot::channel` で保留リクエスト管理
- `ask(info) -> PermissionResponse` / `reply(id, response) -> bool` /
  `list() -> Vec<Info>`
- イベントバス経由で `permission.requested` / `permission.replied` ブロードキャスト

### 2. ServerState 統合

- `permission_broker: Arc<PermissionBroker>` 追加
- `new_with_storage()` で初期化

### 3. ルート接続

- `GET /permission` → `broker.list()`
- `POST /permission/{id}/reply` → `broker.reply()`
  （`routes.rs` の `permission_routes()` が `/{id}/reply` を定義し、
  TUI の `api.rs::reply_permission()` が同パスへ POST する）

### 4. プロンプト側コールバック配線（`opencode-session`）

- `PermissionCallback` 型定義: `Arc<dyn Fn(PermissionRequest) -> ...>`
  (`Pin<Box<dyn Future<Output = Result<(), ToolError>> + Send>>` を返す
  `Send + Sync` クロージャ)
- `IntoCallback` トレイトで `Option<Arc<...>>` → `Box<dyn Fn(...)>` 変換
- `SessionPrompt::prompt_with_update_hook` に `permission_callback` 引数追加
- `tool_context.with_ask(callback)` で注入

### 5. サーバ側コールバック生成（`routes.rs:session_prompt`）

- `PermissionRequestInfo` 構築（ツール名・パターン・メタデータ・alwaysリスト）
- `broker.ask()` で応答待機
- `PermissionResponse` を `ToolError::PermissionDenied` に変換

### 6. TUI 側統合

- `api.rs`: `reply_permission()` メソッド追加
- `app.rs`: 権限 UI で `y/Enter→once`, `n/Esc→reject`, `a→always`

## Consequences

- ツール実行前に必ずユーザー確認を行う
- `always` 選択は永続化されず、現呼出しのみ permissive として扱う
  （`PermissionResponse::Always` は callback で `Ok(())` に変換されるだけで、
  以降のリクエストを自動許可する状態は残らない）。永続化の配線は将来課題
- TUI/HTTP 両方で統一 UX
- `OPENCODE_NO_AUTH=1` でも権限フローは独立動作

## Links

- [ADR-0000: プロジェクト憲章](20260115-rust-reimplementation-charter.md)
- 実装: `server.rs:62-70, 129-231`（Broker と PendingEntryGuard）,
  `routes.rs:1994-2022, 3995-4032`（コールバック生成と `/permission/{id}/reply`）,
  `prompt.rs:1502-1510`（`with_ask` 注入）
