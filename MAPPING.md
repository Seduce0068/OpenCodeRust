---
type: Reference
title: "公式 OpenCode と Rust 実装のマッピング"
description: "upstream packages と OpenCodeRust crate の対応、欠落領域、命名差を記録する。"
status: Current
tags:
  - mapping
  - compatibility
  - roadmap
verified:
  by: dsh
  at: "2026-11-16T23:51:46+09:00"
---

## 文書の位置付け

upstream checkout (`github.com/anomalyco/opencode` @ `941e71d`、2026-08-10)
に対する 2026-10-18 時点の対応調査である。掲載した対応と欠落は checkout の
path で確認した事実に基づく。Rust 側の実装状況は workspace とテストにより
2026-11-16 時点で再確認した。旧版の初期計画 (specs/ 前提) は git 履歴を
参照。フェーズ計画は [ALIGNMENT.md](ALIGNMENT.md)。

## upstream の構造前提

upstream は v1 monolith (`packages/opencode/src/*`) と v2 engine
(`packages/core/src/*`) の二層で、`packages/` は 32 ディレクトリ
(baseline `941e71d` 時点)。Rust 側の crate 分割は v1 の subsystem 構成と
概ね 1:1 で対応する。

2026-11-16 に `dev` の `packages/` を確認したところ 34 ディレクトリで、
`ai` / `app` / `data` / `desktop` / `identity` などが追加されている。
これらとの対応調査は未実施。

## 対応 (高確度)

| upstream | Rust 側 |
| --- | --- |
| `opencode/src/{session,tool,agent,...}` | 同名 `opencode-*` crate |
| `packages/cli` | `opencode-cli` |
| `packages/server` | `opencode-server` |
| `packages/tui` | `opencode-tui` (別実装: ratatui) |
| `packages/llm` | `opencode-provider` |
| `packages/schema` | `opencode-types` |
| `packages/plugin` | `opencode-plugin` |
| `packages/core` (v2 engine) | `opencode-core` ほか (責務別) |

v1 `opencode/src` の subsystem (`agent`、`session`、`tool`、`permission`、
`pty`、`lsp`、`mcp`、`storage`、`config`、`command`) は同名 crate に存在する。

## 欠落・未分化の領域

未実装または対応層が未分化の主な領域 (重要性順):

1. `codemode` — parser基盤、対応構文検査、値境界、実行制限、同期評価器、
   stdlib (Date/Map/Set含む)、同期tool境界、期限監視、実行API
   (`opencode-codemode`)。非同期実行と host呼び出しを中断する timeout は
   未実装。RegExp (`fancy-regex` による lookaround 対応)、URL /
   URLSearchParams、`String.normalize` は実装済み
   (2026-11-16 時点、commit `bf8c62a` ほか)。
2. `protocol` / `client` / `httpapi-codegen` — 権威 HTTP 契約と生成
   client 層が未分化 (Rust は `opencode-server` の routes が唯一の面)。
3. `sdk` / `sdk-next` — JS SDK がない。
4. `function` (cloud sync) と v1 `opencode/src/sync` — 同期基盤がない。
5. `http-recorder` — HTTP/WS リプレイによるテスト基盤がない。
6. `opencode/src/acp` — Rust は外部 ACP ブリッジ起動のみ (HTTP fallback)。
7. v2 `core` の policy / location / integration / credential /
   observability 層 — 対応 crate がない。
8. `/experimental/*` (analyze / generate / refactor / test) — Rust サーバーでは
   未実装であり、2026-11-18 以降は 501 Not Implemented を返す
   (`opencode-server` の routes)。以前の `"accepted"` placeholder レスポンスは廃止。

## 設定スキーマの追随状況 (2026-11-15)

現行の公式グローバル設定は `providers` (複数形)、`permissions` のルール配列、
`mcp.servers` を持つ形式へ移行している。Rust 側の対応状況:

| 項目 | 状態 |
| --- | --- |
| `permissions` ルール配列 | 受理済み (`PermissionsConfig`、旧 string 配列も受理) |
| `providers` (複数形) | マッピング済み (`provider` へ正規化) |
| `mcp.servers` | マッピング済み (config 層で `mcp` 直下へ展開) |
| `mcp.timeout` (グローバル既定) | 受理済み (`McpConfig::timeout`) |
| `snapshots` / `websearch` / `shell` | 未マッピング (未知キーとして無視) |

`providers` は `package` / `settings` / `headers` / `compatibility` を
`provider` の `npm` / `base_url` / `api_key` / `options` へ変換する。
`providers.*.models.*` の `limit` (context / output) と `capabilities`
(tools / reasoning / attachment / modalities) も runtime へ反映する。

`mcp.servers` は `disabled` → `enabled` 反転、`command` (string / argv)、
`headers` / `environment` / `cwd` / `oauth: false` を変換する。remote は
streamable HTTP で接続し、config の `headers` を送信する。401 は
`needs_auth`、その他は `failed` として扱う。local は Content-Length framing
で stdio 接続する。`timeout` (integer ms、または
`{startup, catalog, execution}` ms) は startup / catalog / execution の
各操作タイムアウトへ反映する。既定は公式仕様どおり startup 30s /
catalog 30s / execution 12h で、グローバル `mcp.timeout` を既定とし
server の `timeout` がキー単位で上書きする。
SSE transport、OAuth の token exchange、MCP オプションの `codemode` /
`protocol` / `pool` は未実装。
未マッピングのキーも parse を失敗させない。

## 命名差

- upstream `core` = v2 engine 全体、Rust `opencode-core` = bus / fs /
  git / id / repository。同名だが責務が異なる。
- 共有型は upstream `schema` ↔ Rust `opencode-types`。
- Rust 独自の crate 名: `opencode-grep`、`opencode-watcher`、
  `opencode-pty`、`opencode-command` (upstream は `core/src` の module)。
- `opencode-local-llm` は upstream に対応物がない (Ollama 用 stdio MCP)。
- バイナリ名は `opencoderust` (upstream は `opencode`)。

## 更新手順

upstream を更新したら、本表の commit 記録を更新し、対応と欠落の状態を
checkout の path で再確認する。
