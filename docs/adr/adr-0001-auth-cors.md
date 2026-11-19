---
type: ADR
title: HTTP認証必須化とCORS強化
status: approved
date: "2026-01-10T00:00:00+09:00"
deciders:
  - OpenCodeRust Team
tags:
  - security
  - server
  - auth
generated:
  by: OpenCodeRust Team
  at: "2026-01-10T00:00:00+09:00"
---

## ADR-0001: HTTP認証必須化とCORS強化

## Context

OpenCodeRust サーバモード（`opencoderust serve`）はローカル開発用だが、以下のリスクがあった：

- 認証なしで任意のコード実行・ファイル読取り可能
- CORS 設定が緩く、`http://localhost.attacker.com` 等からのアクセス許容
- 本番・共有環境での利用時に重大なセキュリティホール

## Decision

### 1. Bearer トークン認証必須化

- 起動時に 32 バイト乱数トークン生成、ログ出力
- `Authorization: Bearer <token>` ヘッダー必須
- 環境変数 `OPENCODE_NO_AUTH` で明示的に無効化可能（開発用のみ）:
  - 無効化とみなす値は `1` / `true` / `yes` のみ（大小文字無視・前後空白除去）
  - 未設定・空文字・`0` / `false`・不正値は認証有効（fail-closed）

### 2. CORS 厳格化（Origin のホスト・スキーム完全一致）

- ホストとスキームは完全一致必須。loopback はポートを任意とし、
  ポート省略も許可する意図的な緩和を適用
  - 理由: ブラウザはスキームの既定ポート（`http` は 80、`https` は 443）を
    Origin から省略するため、ポート完全一致では正規の loopback Origin を
    誤拒否していた。セキュリティ境界はホスト・スキームの完全一致に置き、
    リモートホストはポートの有無にかかわらず許可しない
- 許可オリジン:
  - loopback: `localhost` / `127.0.0.1` / `[::1]` を `http` / `https` で、
    ポートあり・なし両方（例: `http://localhost:3000`, `http://localhost`）
  - `tauri://localhost` / `http://tauri.localhost` / `https://tauri.localhost`
  - `https://opencode.ai` / `https://www.opencode.ai`
  - 設定追加分（完全一致）
- `http://localhost.attacker.com` 等のサブドメイン攻撃を遮断

### 3. 許可メソッド・ヘッダー明示

- Methods: `GET, POST, PUT, PATCH, DELETE`
- Headers: `Authorization, Content-Type, Accept, x-opencode-directory, x-request-id`
- 追加ヘッダの理由:
  - `x-opencode-directory`: cross-origin の session create が作業ディレクトリを
    指定するために送る
  - `x-request-id`: フロントエンドから渡す相関ログ ID
  - `AllowHeaders::mirror_request()` は要求ヘッダを無条件に許可するため、
    許可ヘッダは明示的に列挙する方式を採用

## Implementation

- `crates/opencode-server/src/server.rs`:
  - `generate_auth_token()` / `parse_no_auth()` / `auth_enabled_from_env()`
  - `ServerState.auth_enabled` に起動時確定値を保持し、`auth_middleware()` は
    リクエストごとに環境変数を再読しない
  - `set_cors_whitelist()` / `is_allowed_origin()` /
    `is_extra_allowed_origin_exact()` と `cors_layer()` の
    `AllowOrigin::predicate` でオリジン判定
  - `cors_allow_headers()` で許可ヘッダ 5 種を明示列挙
- `crates/opencode-cli/src/main.rs`:
  - `set_cors_whitelist()` で設定追加の許可オリジンを登録

## Consequences

- 既存スクリプトはトークン取得・ヘッダー付与必要
- `OPENCODE_NO_AUTH` による無効化は非推奨・本番禁止を明記
- Tauri/Web 連携は既存オリジン維持で互換性確保

## Links

- [ADR-0000: プロジェクト憲章](20260115-rust-reimplementation-charter.md)
- 実装: `server.rs:72-101, 491-539, 1030-1209, 1259-1294`
