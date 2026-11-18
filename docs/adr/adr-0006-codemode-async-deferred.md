---
type: ADR
title: CodeMode の非同期実行保留と段階的移植方針
description: 同期インタプリタの現行設計を維持し、真の非同期実行と host 中断 timeout を別タスクとして扱う判断を記録する。
status: approved
tags:
  - codemode
  - async
  - architecture
generated:
  by: agent
  at: "2026-11-16T22:55:25+09:00"
reviewed:
  by: agent
  at: "2026-11-18T16:27:27+09:00"
---

## ADR-0006: CodeMode の非同期実行保留と段階的移植方針

### Status

Accepted (self-reviewed). 現行の同期設計を維持し、非同期対応は別タスクとして扱う。

### Context

`opencode-codemode` は同期再帰評価器である。`ToolHost::invoke` は同期トレイトで、
`execute_code` / `Interpreter::run` / `execute` も同期関数である。
`ExecutionLimits::timeout_ms` は wall-clock deadline を文・block 境界で確認するが、
同期 host 呼び出しの途中では中断できない (`interpreter.rs:415-420`、
`limits.rs:16-23`)。

2026-11-16 時点で `execute_code` の呼び出し元は `opencode-codemode` クレート内の
テストと parity corpus のみであり、他クレートへの API 波及はない。

upstream `anomalyco/opencode` の codemode については、2026-11-16 時点の調査
([investigation-final-report.md](../investigation-final-report.md)) で、
`runtime.ts` が `interpreter/` 群へ再構成され、job queue + Promise reaction の
基盤へ進展したと報告されている。報告は実体を
`packages/codemode/src/interpreter/runner.ts:142` (job queue と `parked` frame)、
`promises.ts:19` (thenable adopt / reaction)、`interpreter.ts:419` (`await`) とする。

ただし本 checkout には上流ソースが無く (`references/` も存在しない)、
`async.ts` / `scheduler.ts` / `vm.ts` は HEAD・全 ref に存在しない。
したがって上記のファイル名・行番号はローカルで直接確認できず、調査報告の
記載に依存する。ローカルで確認できるのは、現行 Rust 実装が同期再帰評価器
(再帰評価、`interpreter.rs` 13,837 行) であり、手本とされた baseline `941e71d`
を調査報告が挙げていることまでである。本 ADR の判断 (同期維持) は維持する。

### Decision

真の非同期実行、tool 呼び出しの同時実行制御、同期 host 呼び出しを中断する
timeout は、現行の同期インタプリタでは実装しない。以下を現状維持とする。

- `ToolHost` は同期トレイト、`execute_code` は同期関数のまま。
- wall-clock timeout は文・block 境界で適用する (既存実装を維持)。
- Promise 組み合わせと `await` は同期的に settle する既存セマンティクスを維持。

非同期対応が必要になった場合は、upstream の job queue + Promise reaction 方式を
手本にした移行を新規 ADR/RFC で設計し、本 ADR を `Deprecated` にして後継へ
リンクする。

### Alternatives

1. **`async-trait` による async 化**: `ToolHost::invoke` と `execute_code`、
   評価器の全 `evaluate_*` を async 化する。呼び出し元は限定的だが、13,837 行の
   全面改修になり、upstream のマイクロタスク順序や unhandled rejection 診断を
   同期 Promise モデルでは再現できない。不採用。
2. **host 呼び出しのスレッド化**: 同期 host を別スレッドで動かし channel で待つ。
   timeout でスレッドを安全に強制終了できず、tool 権限・data 境界・`tool_calls`
   記録の整合性が崩れる。不採用。
3. **現状維持 + 明示的な制約記録**: 既存機能を維持し、制約を
   [opencode-codemode.md](../opencode-codemode.md) と
   [MAPPING.md](../../MAPPING.md) に明記する。採用。

### Consequences

- `docs/opencode-codemode.md` の「未実装」節に非同期実行と host 中断 timeout が
  記載されたままになる (2026-11-16 時点で記載済み)。
- 既存の parse / 構文検査 / data 境界 / limits / stdlib / 同期 tool 境界は変更しない。
- `cargo check --all-targets` と `cargo test --workspace` の既存 green を維持する。
- 将来 job queue 方式へ移行する際は、本 ADR の判断を撤回する根拠 (性能要求、
  host tool の実時間性、upstream parity の必須化など) を新 ADR に明記する。

### Links

- [opencode-codemode モジュール設計](../opencode-codemode.md)
- [公式 OpenCode とのマッピング](../../MAPPING.md)
- [調査最終報告 (F-23b)](../investigation-final-report.md)
- upstream の実体 (本 checkout には同梱されず、調査報告の記載に依存):
  `packages/codemode/src/interpreter/runner.ts:142` / `promises.ts:19` /
  `interpreter.ts:419`
