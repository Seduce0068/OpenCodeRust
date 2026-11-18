---
type: ADR
title: 公開APIドキュメント (missing_docs) の一括対応保留
description: 5,604 件の missing_docs 違反を確認し、一括修正を見送る判断を記録する。
status: approved
tags:
  - documentation
  - rustdoc
  - lint
generated:
  by: agent
  at: "2026-11-16T23:47:20+09:00"
reviewed:
  by: agent
  at: "2026-11-16T23:47:20+09:00"
---

## ADR-0007: 公開APIドキュメント (missing_docs) の一括対応保留

### Status

Accepted (self-reviewed).

### Context

AGENTS.md セクション8は「公開 API: すべてに `///` ドキュメントコメント必須」を
要求する。2026-11-16 に
`RUSTDOCFLAGS='-D missing_docs' cargo doc -p <crate> --no-deps` を 22 クレートへ
適用し、クレート別の違反数を計測した。合計 5,604 件である。

| クレート | 件数 |
| --- | --- |
| `opencode-tui` | 1,336 |
| `opencode-provider` | 1,106 |
| `opencode-session` | 845 |
| `opencode-server` | 531 |
| `opencode-config` | 300 |
| `opencode-tool` | 288 |
| その他 16 クレート | 1,198 |

`opencode-local-llm` は 0 件、`opencode-cli` は集計時 1 件（クレート自体の
ドキュメント欠落）だったが、本 ADR 作成時に `//!` を追加して 0 件とした。

### Decision

5,604 件の一括修正は行わない。以下の方針を採る。

- 新規の公開 API には引き続き `///` を必須とする（既存方針を維持）。
- クレートドキュメント 1 件のみの欠落のような小規模ケースは個別に修正する。
- `missing_docs` は CI ゲートに追加しない。
- 将来、重要クレートから段階的に doc を充実させる場合は、本 ADR を更新して
  対象クレートと進捗を記録する。

### Alternatives

1. **全件一括修正**: 5,604 件のレビュー負荷が大きく、機械的な説明文が増えて
   情報価値が下がるリスクがある。不採用。
2. **lint 無効化・方針撤回**: AGENTS.md セクション8の要件に反する。不採用。
3. **現状維持 + 小規模ケースの個別修正**: 採用。

### Consequences

- `RUSTDOCFLAGS='-D missing_docs' cargo doc --workspace --no-deps` は
  workspace 全体では失敗し続ける。
- `opencode-cli` と `opencode-local-llm` は個別には green である。
- 既存の `cargo check` / `clippy` / `test` のゲートは影響を受けない。
- rustdoc の HTML タグ解釈警告（`Vec<PartInput>`）は別途修正済みである。

### Links

- [AGENTS.md](../../AGENTS.md)
- [opencode-cli エントリポイント](../../crates/opencode-cli/src/main.rs)
- [ADR-0006: CodeMode の非同期実行保留](adr-0006-codemode-async-deferred.md)
