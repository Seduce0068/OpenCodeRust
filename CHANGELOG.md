---
type: Reference
description: OpenCodeRust の変更履歴 (Keep a Changelog / SemVer 準拠)。
tags: [changelog, releases]
generated:
  by: agent
  at: "2026-11-17T23:24:33+09:00"
---

# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `opencode-codemode`: RegExp (`fancy-regex` による lookaround、capture group、
  backreference、sticky flag)、URL / URLSearchParams、`String.normalize`
- ADR-0006 (CodeMode 非同期実行の保留)、ADR-0007 (`missing_docs` 一括対応の保留)、
  ADR-0008 (テストカバレッジの baseline と改善方針)
- 未カバー分岐の単体テスト 199 件 (truncation / arity / retry / revert / error /
  models / invalid / codesearch / websearch / todo / glob / grep / ls / batch /
  read / status / apply_patch / bash / edit / multiedit / write / webfetch /
  lsp / plan / tool / external_directory / registry / question / task / skill /
  message_v2 / message / schema / transform / bootstrap / responses / openai /
  anthropic / batch / loader / grep / cli / server / lsp / tui / mcp)、
  テスト総数 594 → 951 件

### Changed

- 依存更新: `notify` 8.2.1 → 8.2.2、`toml` 1.1.8 → 1.2.1、
  `serde_spanned` / `toml_datetime` / `toml_writer`
- ドキュメントを実態に追従:
  [MAPPING.md](MAPPING.md)、[ALIGNMENT.md](ALIGNMENT.md)、
  [opencode-codemode.md](docs/opencode-codemode.md)、
  [opencode-provider.md](docs/opencode-provider.md)、
  [security-audit.md](docs/security-audit.md)
- `cargo-outdated` を必須ツールとして導入し、月次監査を実施
- `cargo-machete` を導入し、未使用依存 66 件を削除 (検出ゼロ)
- 検証を `cargo test --workspace --all-targets` に統一
- `cargo-nextest` / `cargo-tarpaulin` を導入し、カバレッジ 35.98% を計測
- リリースビルド・`cargo audit`・`cargo outdated` を再検証 (脆弱性 0、
  依存は最新、`version` = `OpenCode 0.1.0`)
- 全 feature で check / clippy / test (899 passed) が成功。`cargo nextest
  run --workspace` は 900 passed / 0 skipped

### Fixed

- `opencode-session`: rustdoc の HTML タグ解釈警告 (`Vec<PartInput>`)
- `opencode-cli`: クレートドキュメントの欠落
- 全 Markdown の markdownlint 違反 (検出時 126 件) を解消
- `opencode-tool`: InvalidTool のスキーマ (camelCase) とデシリアライズ
  (snake_case) の不整合を serde alias で解消
- `opencode-tool`: BatchTool のスキーマ (`toolCalls`) とデシリアライズ
  (`tool_calls`) の不整合を serde rename + alias で解消。全ツールの
  スキーマ/引数名を横断監査し、不整合はこの 2 件のみであることを確認
- `opencode-provider` / `opencode-session` に残っていた開発用プレースホルダ
  コメントを削除。未実装マーカーを横断監査し、`litert.rs` の FFI TODO
  (unsafe が必要で未対応) のみが実質的な残作業であることを確認
- `opencode-tool`: `read` ツールの `limit=0` 時に「read beyond line 0」と
  誤表示する問題を修正し、「0 lines read」を明示するよう変更 (L-12)
- `opencode-provider`: Bedrock 署名ヘッダの `parse().unwrap()` を
  `map_err(...)?` に変更し、不正な `session_token` 等で panic せず
  `AuthError` として伝播するよう修正 (M-07)
- `opencode-tool`: `LineTrimmedReplacer` が検索行数の方が多い場合に範囲外
  インデックスで panic する問題を早期 return で回避 (M-28)
- `opencode-tool`: `apply_patch` の巨大な行番号が暗黙に 1 へフォールバック
  する問題を修正し、`InvalidArguments` として伝播 (M-29)
- `opencode-storage`: `list_for_session` の `ORDER BY` に `rowid ASC` を追加し、
  同一 `created_at` のメッセージ順序をキューと同じ規則で安定化 (M-30)
- `opencode-core`: ID 生成の timestamp と counter を単一 Mutex で管理し、
  counter を 12bit マスクしてオーバーフロー時の timestamp 破損を防止 (M-31)

## [0.1.0]

初期実装。詳細は Git 履歴を参照。
