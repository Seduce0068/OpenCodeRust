---
type: Report
title: "OpenCodeRust 全体クロスレビュー報告 (2026-11-17)"
description: >-
  5回の観点別レビューと第6回の重複排除・妥当性検証の結果。
  95件を66グループに統合。
status: Current
tags:
  - cross-review
  - audit
  - findings
generated:
  by: cross-review agent team
  at: "2026-11-17T21:29:22+09:00"
verified:
  by: lead
  at: "2026-11-17T21:29:22+09:00"
---

## 目的と範囲

プロジェクト全体（22 crate、Cargo.toml / Cargo.lock、docs、ADR、CI、opencode.jsonc、
スクリプト）を様々な観点でクロスレビューし、重複と妥当性を吸収して
ルート `todo.md` に追加する。コードの修正は行わない。

## 実施方法

- 第1回 セキュリティ: 入力検証と認証認可、インジェクションとパストラバーサル、
  秘密情報と設定漏洩、依存とサプライチェーン。
- 第2回 アーキテクチャ: クレート境界と依存方向、公開APIとインターフェース設計、
  状態管理と所有権、技術的負債とADR整合。
- 第3回 運用・保守: ログとトレーシング、エラー処理、設定外部化、
  マイグレーションとロールバック。
- 第4回 正確性・並行: ロジックバグ、非同期と並行、タイムアウトとリトライ、
  データ整合性。
- 第5回 テスト・ドキュメント・ビルド: テストカバレッジの空白、
  ドキュメントとコードの乖離、ビルド・CI・lint、パフォーマンス。

各回 4 レビュア、計 20 レビュア（すべて read-only、ファイル変更なし）。
第6回で 95 件を 66 グループへ重複排除し、3 バリデータが現行コードと照合した。
エージェント総数は 25（親を含む）。

### 判定

- 重複グループ化前: 95 件。
- 統合後: 66 グループ（High 26 → 最終 High 14 / Medium 38 / Low 14）。
- 妥当性検証: false-positive 0 件、unconfirmed 0 件。
- 「要確認」: 6 件（中核事実は確認済みだが、影響や因果に推測を含むもの）。
- 既知の先送り（ADR-0006 / ADR-0007 / ADR-0008、`.cargo/audit.toml` の ignore）は
  新規根拠がない限り統合対象から除外した。

### 妥当性検証で修正した主張

- 権限チェックの fail-open: `executor.rs:531-542` が Ask を
  PermissionDenied へ変換し、テストも存在。executor 経由の
  「承認スキップ→全許可」という因果は成立しないため
  「要確認」・Medium に格下げ。`tool.rs` の fail-open と
  `"*": "allow"` は事実。
- Todo 状態の未配線: 「`TodoManager::update` の呼び出し 0 件」は誤りで
  `session/todo.rs:186` のテストから呼ばれている。本番コードでは 0 件。
- パス正規化欠如: 「`is_within_project` は未使用」は誤りで
  `external_directory.rs:39` から呼ばれている。
  コアの文字列比較バイパスは confirmed。
- プラグイン subprocess bridge: 実ファイルは `builtin/plugin-host.ts`
  ではなく `host/plugin-host.ts`。内容自体は confirmed。

## ベースライン検証（Runtime Proof）

2026-11-17 に以下を実行し、すべて exit 0 を確認した。

```bash
cargo check --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

レビューエージェントは読み取りのみで、作業ツリーはレビュー開始前と同じく
変更なし（`git status --short` 空）。

## 発見事項一覧

詳細な根拠・推奨対応はルート `todo.md` の該当 ID を参照。

### High（14 件）

| ID | テーマ |
| --- | --- | --- |
| H-01 | 秘密情報の平文保存（GitHub PAT） |
| H-02 | パス正規化欠如による traversal 誤判定 |
| H-03 | プラグイン subprocess bridge の信頼境界 |
| H-04 | MCP 実行時依存（`npx -y`）の未固定 |
| H-05 | abort ハンドラのキャンセル未実装 |
| H-06 | LLM タイムアウト欠如とリトライ上限未適用 |
| H-07 | マイグレーションのバージョン管理・原子性 |
| H-08 | プラグイン subprocess のデッドロック |
| H-09 | LSP リクエストの無限待機とリソースリーク |
| H-10 | セッション永続化の並行制御（要確認） |
| H-11 | キュー再実行によるレスポンス重複 |
| H-12 | Bearer トークンの平文ログ出力 |
| H-13 | TUI イベントルートのブロッキング HTTP |
| H-14 | 認証情報ファイルのエラー飲み込み |

### Medium（38 件）

| ID | テーマ |
| --- | --- | --- |
| M-01 | 権限チェックの fail-open（要確認） |
| M-02 | 共有型の重複定義と serde 往復 |
| M-03 | 未接続クレートと PTY 責務重複 |
| M-04 | 公開 API の `anyhow::Result` |
| M-05 | Todo 状態の未配線（要確認） |
| M-06 | placeholder エンドポイントの隠蔽 |
| M-07 | HeaderValue パースの panic |
| M-08 | 未知設定キーと env 展開の未検証 |
| M-09 | 認証情報のリテラル設定 |
| M-10 | ローカル LLM の timeout/kill 欠落 |
| M-11 | 重要パスのテストカバレッジ空白 |
| M-12 | 未信頼ツール結果のラップ欠如 |
| M-13 | 動的実行コマンドの権限抽出漏れ |
| M-14 | サーバ認証の警告文と実装の乖離 |
| M-15 | 依存ピン留め不足 |
| M-16 | 依存方向の逆転 |
| M-17 | 内部実装モジュールの過剰な pub |
| M-18 | `SessionOps` の object safety |
| M-19 | 永続化結果の握りつぶし |
| M-20 | 型付き Event Bus の未配線 |
| M-21 | ServerState 外のグローバル static |
| M-22 | ドキュメントの数値ドリフト |
| M-23 | 構造化ログ・スパンの欠如 |
| M-24 | 検索処理のエラー飲み込み |
| M-25 | TUI 許可プロンプトの通知欠如 |
| M-26 | wellknown 定数と env 書き換え |
| M-27 | MCP 操作のタイムアウト未適用 |
| M-28 | `LineTrimmedReplacer` の panic |
| M-29 | `apply_patch` の暗黙フォールバック |
| M-30 | メッセージ順序のタイブレーク欠如 |
| M-31 | ID 生成の順序キー整合性 |
| M-32 | README のバイナリ名乖離 |
| M-33 | OKF フロントマターの乖離 |
| M-34 | CI ゲートの抜け |
| M-35 | TUI file_index の同期走査 |
| M-36 | watch の debounce 仕様矛盾 |
| M-37 | async 内の同期 I/O（要確認） |
| M-38 | ストリーミングのキャンセル（要確認） |

### Low（14 件）

| ID | テーマ |
| --- | --- | --- |
| L-01 | Event Bus のロック保持中コールバック |
| L-02 | MCP list_changed の非対称処理 |
| L-03 | dead_code 抑制と CHANGELOG の矛盾 |
| L-04 | tracing 初期化の未配線 |
| L-05 | workspace lints の継承漏れ |
| L-06 | GET /config の api_key 平文 |
| L-07 | 依存 feature の過剰指定 |
| L-08 | 公開 API ドキュメント欠落（要確認） |
| L-09 | LiteRT 未実装 FFI の露出 |
| L-10 | POST /log 入力の未検証 |
| L-11 | 設定マイグレーションのロールバック欠如 |
| L-12 | read ツールの `limit=0` 誤メッセージ |
| L-13 | RegExp `lastIndex` の parity 欠如 |
| L-14 | 巨大 JSON と履歴の全件アロケーション |

## 秘密情報の取り扱い

`.env:1` に GitHub Personal Access Token が平文で存在する。リポジトリには
未追跡（`git check-ignore` で `.gitignore:19` が該当、`git ls-files` に不在）で、
履歴にもない。トークンの有効性と外部漏洩の有無は未確認。
本報告書および `todo.md` には値を記載していない。

## 採用・却下した指摘

- 採用: 66 グループすべて（false-positive 0 件）。
- 格下げ: 権限 fail-open（High → Medium・要確認）、Todo 未配線（High → Medium）、
  Event Bus ロック順序（Medium → Low）、tracing 初期化（Medium → Low）、
  workspace lints（Medium → Low）など、妥当性検証で因果・影響が限定されたもの。
- 除外: 既知の先送り（ADR-0006 / 0007 / 0008、cargo audit ignore）に新規根拠がない項目、
  SQLite インジェクション（全クエリがバインド済みで痕跡なし）、server 認証の fail-open
  （ADR-0001 通り `auth_token` が常に Some）、PTY spawn のシェル経由
  （CommandBuilder + args で直接実行）。
