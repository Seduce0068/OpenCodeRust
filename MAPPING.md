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
  at: "2026-11-19T07:33:31+09:00"
---

## 文書の位置付け

upstream checkout (`github.com/anomalyco/opencode` @ `941e71d`、2026-08-10)
に対する 2026-10-18 時点の対応調査である。掲載した対応と欠落は checkout の
path で確認した事実に基づく。Rust 側の実装状況は workspace とテストにより
2026-11-16 および 2026-11-19 時点で再確認した。旧版の初期計画
(specs/ 前提) は git 履歴を参照。フェーズ計画は [ALIGNMENT.md](ALIGNMENT.md)。

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
9. LiteRT-LM プロバイダ — ネイティブ FFI が未リンクで推論不可。2026-11-18 以降は
   `bootstrap.rs` が `litert` を登録せず、`LITERT_MODEL_PATH` / `LITERT_BACKEND`
   は無視される（feature gate なし、FFI 実装後に再有効化が必要）。

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
`pool` は未実装。`protocol` は parse され、`revision()` の返す値が
`initialize` の `protocolVersion` として MCP クライアントへ渡る
(`opencode-server/src/routes.rs` → `opencode-mcp` の `with_protocol_version`)。
ただし `McpProtocol::Auto` の probe/fallback (2026-07-28 を試して
legacy へ戻す挙動) は未実装で、`revision()` は `Legacy` と同じ
`"2025-11-25"` を返す。新 revision を使うには `"2026-07-28"` を
明示指定する必要がある。クライアントは `initialize` 応答の
`protocolVersion` を参照せず、再 negotiation も行わない。
`notifications/tools/list_changed` を受信すると tools を再ロードし
`mcp.tools.changed` を発行する。`notifications/resources/list_changed` と
`notifications/prompts/list_changed` は debug ログのみで、再ロード／
イベント発行は未実装（capability も `tools.listChanged` のみを宣言）。
未マッピングのキーも parse を失敗させない。

### upstream 新規キーとキー名ズレ (2026-11-18, P-06)

調査元は `.cache/investigation/round5-audit.md` が特定した upstream checkout
`/home/koppe/ドキュメント/作業用/opencode` (branch `permission-path-match`、
HEAD `9c7bc2f42f`、2026-10-31) の `packages/schema/src/config.ts`。
MAPPING baseline (`941e71d`) とは異なる checkout である点に注意。

| upstream キー | upstream 型 | Rust 側の状況 |
| --- | --- | --- |
| `timezone` (`:55`) | `String?` | 追加済み (`Option<String>`) |
| `policies` (`:61`) | `ConfigPolicy.Info[]` | 未対応 (破棄) |
| `worktree` (`:109`) | `ConfigWorktree.Info` | 未対応 (破棄) |
| `update` (`:39`) | 3 リテラル | 未対応 (`autoupdate` 不一致) |
| `commands` (`:94`) | `Record<String, Cmd.Info>` | 未対応 (alias せず) |
| `plugins` (`:106`) | string または object[] | 未対応 (`Vec<String>` 不一致) |

`timezone` 以外はすべて未知キーとして warn のうえ破棄される。`plugins` の
object 要素は `{package: String, options?: Record<String, unknown>}`、
`update` の 3 リテラルは `disable` / `notify` / `auto` である。

追加コミットは `timezone` が `4585c316c3` (2026-10-28)、`policies` が
`f576da508d` (2026-10-13)、`worktree` が `c30285c148` (2026-09-04) で、
いずれも MAPPING baseline より後 (git log で確認)。

`timezone` は upstream の記述が "IANA time zone for schedules" で型・意味が
自明なため、`Config.timezone: Option<String>` として追加し
`known_config_keys()` にも登録した。runtime 消費箇所は現時点で 0 件。

`policies` は同 checkout でトップレベルへ昇格しており、要素は
`{action, resource, effect}` (`action` は `provider.use` / `tool.use` /
`integration.connect` / `integration.use` / `mcp.use` / `skill.use` /
`plugin.use`、`effect` は `allow` / `deny`) と、legacy resource 変換を伴う。
Rust の `permissions` (tool permission rules) とは意味が異なるため流用しない。
`worktree` は `{directory: String}` (非空・trim) のみ。両者は構造を確認済みだが
runtime 配線は本タスクの scope 外のため、フィールド追加は行わず未対応とする。

alias を見送った理由:

- `update`: Rust の `autoupdate` は `bool` または `String` の untagged enum
  (`AutoUpdateMode`)。upstream の 3 リテラルをそのまま受けると `disable` も
  `notify` も `Notify(String)` として解釈され意味が変わる。
- `plugins`: upstream は string または `{package, options}` オブジェクトの
  配列。`plugin: Vec<String>` に alias すると object 要素で parse が失敗し、
  「起動を止めない」互換方針に反する。
- `commands`: 型は近いが upstream の `subagent` 等を落とすため、ここでは
  alias を追加せず現状を明記するのみとする。

Runtime Proof: `schema.rs` の
`parses_timezone_and_discards_unmapped_upstream_keys` により、上記 6 キーを
含む JSON が parse 成功し、`timezone` のみ保持されて他は破棄されることを
確認済み。loader は未知キー名のみ warn し、値はログに出さない
(`crates/opencode-config/src/loader.rs:538-548`)。

### 未配線の設定フィールド (2026-11-18)

`Config` (`crates/opencode-config/src/schema.rs`) の下記フィールドは
serde による parse、`known_config_keys()` での既知キー扱い、`DeepMerge`
でのマージには参加する。しかし 2026-11-18 時点で、これらを読み取る
runtime 消費箇所は schema.rs 外に 0 件である (grep による事実確認)。
設定ファイルに記述しても起動は失敗しないが、挙動には反映されない。

- `keybinds` (`KeybindsConfig`) — TUI の `KeybindRegistry::new()`
  (`opencode-tui/src/context/keybind.rs:148`) が `register_defaults()`
  でキーをハードコードし、`AppContext::new()`
  (`opencode-tui/src/context/app_context.rs:132`) も引数なしで生成する。
  `Config.keybinds` を渡す経路はない。
- `snapshot` (`Option<bool>`) — schema.rs 外の参照 0 件。
- `autoupdate` (`AutoUpdateMode`) — schema.rs 外の参照 0 件。
- `enterprise` (`EnterpriseConfig`) — schema.rs 外の参照 0 件。
- `experimental` (`ExperimentalConfig`) — schema.rs 外の参照 0 件。

`ExperimentalConfig` のサブフィールド 6 件も同様に未配線である:
`disable_paste_summary`、`batch_tool`、`open_telemetry` (alias
`openTelemetry`)、`primary_tools`、`continue_loop_on_deny`、`mcp_timeout`。
いずれも schema.rs の定義と `DeepMerge` 実装以外に参照がない。

混同注意: セッション／revert の `snapshot` (`Option<String>` の git
snapshot ハッシュ)、provider 側の `enable_experimental` /
`experimental_over_200k`、MCP の `mcp.timeout` は別フィールドであり、
上記 config キーの消費箇所ではない。

配線そのものは本タスクの scope 外のため、ここでは現状を明記するのみとする。

## 命名差

- upstream `core` = v2 engine 全体、Rust `opencode-core` = bus / fs /
  git / id / repository。同名だが責務が異なる。
- 共有型は upstream `schema` ↔ Rust `opencode-types`。
- Rust 独自の crate 名: `opencode-grep`、`opencode-watcher`、
  `opencode-pty`、`opencode-command` (upstream は `core/src` の module)。
- `opencode-local-llm` は upstream に対応物がない (Ollama 用 stdio MCP)。
- バイナリ名は `opencoderust` (upstream は `opencode`)。

## /session/{id}/stream の位置付け (2026-11-19, M-46)

`opencode-server` には `POST /session/{id}/stream` と
`POST /session/{id}/prompt` の 2 つの応答経路がある。外観は似るが契約と
内部実装は異なり、互いに置き換えられない。以下は `routes.rs` の
`stream_message` と `session_prompt` / `spawn_prompt_execution` を
2026-11-19 に確認した結果である。

### /stream はエージェント・ツールなしの生 chat_stream

`stream_message` は `SendMessageRequest` (`content` / `model` / `variant` /
`stream`) を受け取り、`opencode_provider::ChatRequest` を `system: None`、
`tools: None` で構築して `provider.chat_stream()` を直接呼ぶ。agent も
`StreamProcessor` も介在せず、プロバイダーの生のストリームを SSE へ中継する
だけである。ユーザーメッセージと assistant プレースホルダの追加、完了時の
テキスト／tool_call の反映と永続化は行うが、ツール実行・権限確認・
question 応答・agent system prompt の注入は行わない。

### /prompt との主な差異

`session_prompt` は JSON を返し、実行は `spawn_prompt_execution` が
バックグラウンドで行う。主な差異は次のとおり。

| 観点 | /stream | /prompt |
| --- | --- | --- |
| 応答契約 | SSE | JSON + broadcast |
| system prompt | なし | provider + agent + env |
| ツール / 権限 | なし | registry + broker |
| エージェント | なし | 既定 `build` + AgentParams |
| 実行系 | 直接 `chat_stream` | `SessionPrompt` + hook |
| キャンセル | 切断検知のみ | abort + キュー |
| 永続化 | 完了時に反映 | 完了時 + queue 遷移 |

補足:

- 応答契約: `/stream` は `message_start` / `message_delta` / `message_end`
  に加え tool_call / usage / error の SSE を返す。`/prompt` は JSON 応答の
  後、update_hook から `session.updated` (`source: prompt.stream`) と
  完了時の同イベント (`source: prompt.final`) を broadcast する。
- 実行系: `/prompt` は `SessionPrompt::prompt_with_update_hook` が `loop_inner`
  を駆動し `provider.chat_stream()` を直接消費する。retry / compaction /
  abort は `loop_inner` 内で処理され、`llm.rs` の `StreamProcessor` は
  本番配線されていない (参照はテストモジュールのみ)。`/stream` は
  `loop_inner` を通さない。
- キャンセル: `/stream` はクライアント切断を `tx.closed()` で検知して
  生成を止めるが、`/prompt/abort` には登録しない。`/prompt` は
  `/prompt/abort` が AbortHandle を保持し、durable input のキューも処理する。
- 永続化: `/prompt` は `sessions.update` と queue 状態遷移
  (`mark_as_promoted` / `mark_as_failed`) まで行う。

この差は実装上の細部ではなく応答品質と課金に直結する。`/stream` には
system prompt とツール定義が渡らないため応答品質が `/prompt` と異なり、
トークン消費と請求額も一致しない。両者を同じ経路として扱わないこと。

### 新規利用の案内

新規クライアントは `POST /session/{id}/prompt` と
`GET /event` (`global_event_stream` / `event_stream`) の組み合わせを使う。
`/stream` は生の `chat_stream` が要る既存利用向けの経路で、エージェント機能を
期待する用途には適さない。経路統合は破壊的変更 (5〜7 ファイル、見積りは
speculative) になるため M-46 では見送り、現状維持と本節での明記を選んだ。
M-38 で AbortHandle 保持が見送り済みである点も変更リスクの一因である。

## 更新手順

upstream を更新したら、本表の commit 記録を更新し、対応と欠落の状態を
checkout の path で再確認する。
