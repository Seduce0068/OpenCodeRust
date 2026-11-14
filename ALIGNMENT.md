---
type: Plan
title: "official 移植・整合計画"
description: "公式 OpenCode と OpenCodeRust の機能整合の現況と残作業。"
status: Active
tags:
  - alignment
  - roadmap
  - compatibility
verified:
  by: dsh
  at: "2026-11-15T02:10:55+09:00"
---

## 目標

`OpenCodeRust` (Rust) の性能と安定性を維持しつつ、公式 OpenCode
(TypeScript) の仕様、プロトコル、機能セットに整合させる。

## 戦略

1. Rust ベースの核心エンジン (core crate) を定義し、公式仕様に基づく
   モジュール化を行う。
2. 機能ごとに公式 OpenCode のアーキテクチャ (packages/*) を Rust で
   実装・移植する。
3. 公式仕様を Rust コードの実装基準とする。

## 移行フェーズの現況

下表は 2026-11-15 時点で workspace とテストにより確認した範囲。

| フェーズ | 状態 |
| --- | --- |
| Phase 1: 仕様理解と Rust マッピング策定 | 完了 |
| Phase 2: コア機能 (session/storage) の同期 | 主要実装済み |
| Phase 3: エージェント機能の実装と検証 | 進行中 |

根拠と残作業:

- Phase 1: `MAPPING.md` と `docs/architecture.md` が現行構成を記述済み。
- Phase 2: `opencode-session` / `opencode-storage` に主要実装がある
  (ADR-0004)。queue と通常 prompt は server の共通 executor を使い、
  状態遷移は `InputRepository` に集約済み。残る制約 (admission と
  snapshot の非同一 transaction、単一 process 前提) は
  [DESIGN_RUNNER.md](DESIGN_RUNNER.md) に記録。
- Phase 3: `opencode-agent` / `opencode-tool` は実装・テスト済み
  (workspace テスト 564 件 green、2026-11-15 実行)。差分照合の初版は
  [MAPPING.md](MAPPING.md) に記録済み。未移植機能の実装が残作業。

## 直近の更新 (2026-11-15)

現行公式設定スキーマへの追随と、CI・監査の復旧を行った。

- グローバル設定が新スキーマ (`providers` / `permissions` ルール配列 /
  `mcp.servers`) へ移行したため、config loader で受理・正規化するように
  した ([MAPPING.md](MAPPING.md) の「設定スキーマの追随状況」)。
- `providers.*.models.*` の `limit` / `capabilities` を runtime へ反映。
- remote MCP (streamable HTTP) の接続を実装。config の `headers` を送信し、
  401 は `needs_auth`、その他の失敗は `failed` として扱う。
- 依存監査: `pulldown-cmark` を 0.13.6 へ更新。`libsqlite3-sys` は sqlx の
  依存制約で修正版を適用できないため、根拠付きで ignore
  ([docs/security-audit.md](docs/security-audit.md))。
- CI: アカウント移転に伴い共通ワークフロー参照を `Seduce0068/workflows` へ
  更新。

残作業は `cwd` / per-operation timeout の反映、SSE transport、OAuth の
token exchange、codemode の未実装群 ([MAPPING.md](MAPPING.md) の欠落領域)。

## 参照元

- 公式リポジトリ: `github.com/anomalyco/opencode` の local checkout
  (`~/projects/opencode-official`、参照時 commit `941e71d`、2026-08-10)。
- Rust 開発指針: [AGENTS.md](AGENTS.md)、
  [docs/architecture.md](docs/architecture.md)。

## 記録ルール

公式との機能差を更新するときは、比較対象の upstream commit を記録し、
実装・テストで確認した事実と提案を分けて書く
([MAPPING.md](MAPPING.md) と同一方針)。
