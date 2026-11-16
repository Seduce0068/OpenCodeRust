---
type: Reference
description: OpenCodeRust の変更履歴 (Keep a Changelog / SemVer 準拠)。
tags: [changelog, releases]
generated:
  by: agent
  at: "2026-11-16T23:49:08+09:00"
---

# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `opencode-codemode`: RegExp (`fancy-regex` による lookaround、capture group、
  backreference、sticky flag)、URL / URLSearchParams、`String.normalize`
- ADR-0006 (CodeMode 非同期実行の保留)、ADR-0007 (`missing_docs` 一括対応の保留)

### Changed

- 依存更新: `notify` 8.2.1 → 8.2.2、`toml` 1.1.8 → 1.2.1、
  `serde_spanned` / `toml_datetime` / `toml_writer`
- ドキュメントを実態に追従:
  [MAPPING.md](MAPPING.md)、[ALIGNMENT.md](ALIGNMENT.md)、
  [opencode-codemode.md](docs/opencode-codemode.md)、
  [opencode-provider.md](docs/opencode-provider.md)、
  [security-audit.md](docs/security-audit.md)
- `cargo-outdated` を必須ツールとして導入し、月次監査を実施

### Fixed

- `opencode-session`: rustdoc の HTML タグ解釈警告 (`Vec<PartInput>`)
- `opencode-cli`: クレートドキュメントの欠落
- 全 Markdown の markdownlint 違反 (検出時 126 件) を解消

## [0.1.0]

初期実装。詳細は Git 履歴を参照。
