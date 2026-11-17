---
type: Reference
title: "残タスク・機能追加案 調査 最終統合レポート"
description: "5 ラウンドのエージェント調査を重複吸収・妥当性検証済みで統合した結果。"
tags: [investigation, final, roadmap, opencoderust]
generated:
  by: lead
  at: "2026-11-17T21:35:00+09:00"
status: Current
---

## 目的と方法

未知の残タスク・機能追加案を多角的に調査するため、5 ラウンドの観点別調査を
エージェントチーム（延べ 10 エージェント、内 2 件は途中失敗を Lead が代替）
で並列実行し、6 回目の本レポートで重複・妥当性を吸収した。

対象: `OpenCodeRust`（22 クレート、Commit `71e8eef`）
参照 upstream: `/home/koppe/ドキュメント/作業用/opencode` @ `9c7bc2f42f`
（2026-10-31）

| ラウンド | 観点 | 状態 |
| --- | --- | --- |
| 1 | 未実装マーカー / 抑制属性 / 機能サーフェス | 完了 |
| 2 | ドキュメント整合 / テスト・CI ギャップ | 完了 |
| 3 | upstream との機能差の実証 | 完了（新規 12 件） |
| 4 | 機能追加案の創出と精査 | 完了 |
| 5 | 依存・設定スキーマ・セキュリティ | 完了 |
| 6 | 重複吸収・妥当性検証・統合 | 本レポート |

担当エージェント: `r1-markers`、`r1-suppression`、`r1-surface`（失敗、
`r1b-surface` も失敗のため Lead 代替）、`r2-docs`、`r2-quality`、
`r3-upstream`、`r4-ideas`、`r5-audit`。

## 区分 A: 残タスク（実装すべきもの）

重複を吸収し、優先度順に並べた。確度は Lead が直接読み取った
Runtime Proof に基づく。

### P0

- **F-02 `/experimental/*` の placeholder を実処理へ接続**
  - 出典（重複元）: M-05〜M-08, E-02, D-04, D-05, I-01, r3 (G-01)
  - 根拠: `crates/opencode-server/src/routes.rs:4380-4478`、
    upstream `packages/server/src/handlers/generate.ts:20-63`
  - text/image/decisions と一覧・有効/無効の実装が必要

### P1

- **F-26 API パスの `/api` prefix 不一致**（parity 構造問題）
  - 出典: r3 (G-03)、Lead 直接裏取り
  - 根拠: upstream
    `packages/protocol/src/groups/agent.ts:10` は `/api/agent`、
    Rust `crates/opencode-server/src/routes.rs:49` は `/agent`、
    `crates/opencode-server/src/server.rs:932` に prefix 層なし
  - 実害は Rust 側に公式 client が無いため未確認だが、F-02 の前提となる

- **F-07 `Config.keybinds` を TUI へ配線**
  - 出典: SF-01
  - 根拠: `crates/opencode-config/src/schema.rs:13`、
    `crates/opencode-tui/src/context/keybind.rs:147-211`
  - 現在はハードコードされたデフォルトのみで設定が反映されない

- **F-08 未配線 config フィールド**
  - 出典: SF-02, SF-03
  - 根拠: `crates/opencode-config/src/schema.rs:41,50,114,1175-1188`
  - `snapshot` / `autoupdate` / `enterprise` / `experimental`
    （5 サブフィールド）の runtime 参照は 0 件

- **F-16 CI ゲートへ `--all-features` と unsafe lint を追加**
  - 出典: r2-quality, R5-01, Lead 直接裏取り
  - 根拠: `.github/workflows/ci.yml:24-27`、
    `Seduce0068/workflows@v1` の `rust-gates.yml` 実体
  - 共有ワークフローの clippy は `-D warnings` のみで、
    `undocumented_unsafe_blocks` も指定なし

### P2

- **F-03 MCP `list_changed` の非対称処理**（出典: E-03, I-02）
  - 根拠: `crates/opencode-mcp/src/client.rs:625-641`
  - resources / prompts を tools と同様に flag し再取得する

- **F-04 interleaved thinking の no-op**（出典: M-09, SF-04, I-03）
  - 根拠: `crates/opencode-provider/src/transform.rs:297-312,312-340`
  - 実装済みの
    `normalize_messages_with_interleaved_field` を呼び出す配線が必要

- **F-11 SSE transport の config 経路への配線**（出典: D-02）
  - 根拠: `crates/opencode-mcp/src/client.rs:391-456,945`、
    `crates/opencode-server/src/routes.rs:3220-3237`
  - `server_type` を見て `add_sse` を選択する

- **F-12 `McpProtocol::Auto` の probe + fallback 実装**（出典: D-03）
  - 根拠: `crates/opencode-config/src/schema.rs:571-592`
  - 現在は `Legacy` と同じ revision を返す

- **R5-04 upstream 新規設定キー**（出典: r5-audit）
  - 根拠: upstream `packages/schema/src/config.ts:55,61,109`、
    `crates/opencode-config/src/schema.rs:4-124`
  - `timezone` / `policies` / `worktree` が未対応

### P3

- **F-05 未配線・空実装の整理**（出典: S-04, S-05, M-10, SF-05, SF-06,
  I-04, I-05）
  - 根拠: `crates/opencode-session/src/prompt.rs:1961`、
    `crates/opencode-tui/src/components/dialog.rs:28`、
    `crates/opencode-tui/src/components/markdown/parser.rs:1`
  - 未使用性は静的調査のみ。使用確認後に実装または削除

- **F-15 セキュリティ境界・破壊的経路のテスト追加**（出典: r2-quality）
  - 根拠: `crates/opencode-server/src/server.rs:828,883`、
    `crates/opencode-server/src/routes.rs:3334`、
    `crates/opencode-cli/src/main.rs:2744`、
    `crates/opencode-plugin/src/subprocess/auth.rs:87`、
    `crates/opencode-local-llm/src/main.rs:266,292`

- **R5-05 upstream キー名の複数形ズレ**（出典: r5-audit）
  - 根拠: upstream `packages/schema/src/config.ts:39,94,106`、
    `crates/opencode-config/src/loader.rs:1265-1342`
  - `update` / `commands` / `plugins` が捨てられる可能性。Runtime Proof 待ち

## 区分 B: ドキュメント修正（実装は済みだが記載が古い）

- **F-10 OAuth token exchange の「未実装」記載を削除**（出典: D-01）
  - 根拠: `crates/opencode-mcp/src/oauth.rs:359-406`（2026-11-15 実装済み）

- **F-13 「routes が唯一の面」を修正**（出典: D-06）
  - 根拠: `crates/opencode-cli/src/main.rs:264-265,2752-2895`

- **F-14 軽微な不整合**（出典: D-07〜D-11）
  - バイナリ名 `opencode` → `opencoderust`（`README.md:145`）
  - CLI サブコマンド一覧の日付 2026-10-16 を更新
  - TypeScript 型消去を「full parity 未実装」へ限定
    （`crates/opencode-codemode/src/support.rs:192-201`）
  - テスト件数 946 → 951（静的カウント）

- **F-01 CHANGELOG.md:61-63 の「litert.rs のみ」を修正**（出典: D-04）
  - 根拠: `crates/opencode-provider/src/litert.rs:60-71` の他に
    `crates/opencode-server/src/routes.rs:4411-4478` 等がある

- **F-25 ADR-0008 の件数表記を 951 件へ**（出典: D-11）
  - 根拠: `docs/adr/adr-0008-coverage-baseline.md:83`
  - カバレッジ 35.89% と方針は維持

- **F-23b ADR-0006 の前提ファイル名を修正**（出典: r3 (G-02)）
  - 根拠: `docs/adr/adr-0006-codemode-async-deferred.md:35` が参照する
    `async.ts` / `scheduler.ts` / `vm.ts` は本 checkout の全 ref に存在せず、
    実体は upstream `packages/codemode/src/interpreter/runner.ts:142` /
    `promises.ts:19` / `interpreter.ts:419`

- **F-13b cloud sync / share の優先度を下方修正**（出典: r3）
  - 根拠: upstream `packages/function/src/share.ts:161,169,170` で廃止済み

## 区分 C: 機能追加案（新規）

- **F-17 upstream `schedule.*` の段階的移植**
  - 価値 / コスト: 高 / 大
  - 判定: 採用候補（要設計）
  - 根拠: upstream
    `packages/server/src/handlers/schedule.ts:10-67`
  - 妥当性検証: r4-ideas が根拠に挙げた
    `crates/opencode-cli/src/main.rs:4187` は GitHub Actions の
    イベント判定であり誤り。Lead が修正した

- **F-19 upstream `http-recorder` 相当のリプレイテスト基盤**
  - 価値 / コスト: 中 / 中
  - 判定: 条件付き採用（秘密情報マスキング・ローカル限定が必須）
  - 根拠: `MAPPING.md:64`

- **F-09 `lsp` feature の既定有効化を検討**
  - 価値 / コスト: 中 / 中
  - 判定: 保留（設計判断）
  - 根拠: `crates/opencode-tool/Cargo.toml:8-10`、
    `crates/opencode-tool/src/lsp_tool.rs:387-401`

- **r3 新規ギャップ（artifacts / persistent-pty / voice /
  websearch・rpc・migration・form・media / v2 core credential・integration・
  location / native ACP / JS SDK）**
  - 判定: 調査・優先度判断待ち
  - 根拠: `.cache/investigation/round3-upstream.md` の G-05〜G-12
  - voice (G-07) は外部送信を伴うため、採用には別途合意が必要

### 妥当性検証で格下げ・却下した項目

- **F-20（セッション共有 opt-in 化）**: r4-ideas では採用候補だったが、
  既に `config.share` で制御可能
  （`crates/opencode-cli/src/main.rs:1160`）であり、新規機能ではなく
  既定値の設計判断であるため「保留」へ格下げした。
- **F-22（LiteRT FFI / voice）**: LiteRT は `unsafe` 禁止規約
  （`~/.agents/AGENTS.md` セクション15）と矛盾し却下。voice は音声データの
  外部送信を伴い却下。いずれも採用しない。
- **F-05 の実装/削除判断**: 未使用性は静的調査のみであり、実際の使用確認が
  先決。機械的な実装は避ける。

## 区分 D: 既知の保留・正当（変更不要）

- **F-23 ADR-0006（codemode 非同期実行保留）**: 同期 `ToolHost` /
  timeout は文・block 境界のみで現状正確。前提ファイル名のみ修正対象
  （F-23b を参照）。
- **F-24 ADR-0007（missing_docs 5,604 件保留）**: lint allow 0 件、
  CI rustdoc ゲート無しで判断は維持。
- **F-25 ADR-0008（カバレッジ 35.89%、80% 全体目標を採らない）**: 方針維持。
- **F-01 LiteRT FFI TODO**: 既知。FFI は `unsafe` を伴い規約上すぐには
  実装できない。
- **R5-02 unsafe 実コード 0 件**: 検出 6 件はすべてテスト名・文字列リテラル。
- **R5-06 `libsqlite3-sys` の audit ignore**: lock 0.37.0、sqlx 0.9.0 の
  制約 `<0.38.0` で修正版選択不可。
- **R5-08 依存バージョン整合性**: lock 内重複 0、semver 衝突なし。
- **R5-03 `snapshots` / `websearch` / `shell` 未マッピング**:
  `MAPPING.md:80` 記載済み。

## 重複吸収のサマリ

- 延べ検出項目: Round 1 が 19 + 22 + 6 件、Round 2 が 11 + 多数、
  Round 3 が 7 項目 + 新規ギャップ 12 件、Round 4 が 14 案、Round 5 が 9 件。
- 統合後の区分: A 残タスク 13 件、B ドキュメント修正 7 件、C 機能追加案
  5 件（＋保留 4）、D 既知・正当 8 件。
- 主な吸収: experimental placeholder（8 出典 → F-02）、LiteRT FFI
  （5 出典 → F-01）、MCP list_changed（2 出典 → F-03）、interleaved no-op
  （3 出典 → F-04）、未配線・空実装（7 出典 → F-05）、CI ゲート
  （3 出典 → F-16）、codemode 非同期（r2-docs + r3 → F-23, F-23b）。
- 妥当性検証で 2 件を格下げ・却下（F-20 格下げ、F-22 却下）、2 件の根拠を
  修正（F-17 の誤 path、F-23b の ADR 前提ファイル名）、1 件の優先度を
  下方修正（F-13b cloud sync/share）。
- `/api` prefix 不一致（F-26）は 5 ラウンド中 r3 のみが検出した構造問題で、
  多角的調査の意義が確認できた。
- 本レポートの新規発見は `todo.md` の P-01〜P-08 へ反映済み。cross-review
  の H/M/L と重複する項目は既存項目を優先し、P-* には新規発見のみを記録した。

## 検証の限界

- コード変更は行っていない。カバレッジ・テスト・`cargo audit` の再計測は
  未実施（既存 Runtime Proof を引用）。
- CI 共有ワークフローの実体は GitHub API 経由で取得して確認済み
  （`.cache/investigation/lead-verification-ci.md`）。
- 未確認項目は `needs-check` として明示した（R5-05、F-05 の使用状況、
  F-26 の実害）。
- upstream の `async.ts` / `scheduler.ts` / `vm.ts` 不在は本 checkout の
  全 ref で確認したが、ADR-0006 が参照した当時の外部状態は未確認。

## 成果物一覧

- 生データ: `.cache/investigation/round1-markers.md`、
  `round1-suppression.md`、`round1-surface.md`、`round2-docs.md`、
  `round2-quality.md`、`round3-upstream.md`、`round4-ideas.md`、
  `round5-audit.md`
- Lead 直接検証: `lead-verification-ci.md`、`lead-verification-ideas.md`
- 重複マッピング: `ROUND6-DUP-MAP.md`
- 本レポート: `docs/investigation-final-report.md`
