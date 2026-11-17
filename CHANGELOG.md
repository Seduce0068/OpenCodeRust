---
type: Reference
description: OpenCodeRust の変更履歴 (Keep a Changelog / SemVer 準拠)。
tags: [changelog, releases]
generated:
  by: agent
  at: "2026-11-18T07:28:37+09:00"
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
  テスト総数 594 → 1011 件

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
- `cargo-nextest` / `cargo-tarpaulin` を導入し、カバレッジ 36.80% を計測
  （前回 36.81% から計測揺らぎの範囲で微減）
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
- `README.md`: CLI バイナリ名の記載を `opencode` から実際の `opencoderust` へ修正 (M-32)
- `opencode-server`: 認証トークンの平文ログ出力をマスク（先头4文字と長さのみ）に変更 (H-12)
- `opencode-grep`: WalkDir / ファイル読み取りエラーを握りつぶさず `Err` として伝播 (M-24)
- `opencode-config`: auth.json 由来の `set_var` にキー形式検証と危険キー
  (`PATH` / `LD_PRELOAD` 等) の拒否を追加 (M-26)
- `opencode-tui`: 許可プロンプトの送信失敗をトーストで通知し、question 応答と
  一貫したエラー扱いに変更 (M-25)
- `opencode-cli`: サーバ起動時の認証警告を実態（ランダム Bearer トークン /
  `OPENCODE_NO_AUTH`）に合わせて修正 (M-14)
- `opencode-server`: `POST /log` の service/message/extra から制御文字を除去し
  長さを制限してログ汚染を防止 (L-10)
- `opencode-server`: `GET/PATCH /config` が provider の `api_key` を平文で返す
  問題を修正し、`***` にマスクして返す (L-06)
- `opencode-codemode`: RegExp の `lastIndex` を `i64` で保持し、負数代入が
  0 に潰れず JS と同じ値を返すよう修正 (L-13)
- 全 21 クレートの `Cargo.toml` へ `[lints] workspace = true` を追加し、
  workspace の deny 設定を一括継承 (L-05)
- `opencode-config`: レガシー TOML マイグレーション時に元ファイルを `config.bak`
  へ退避し、コピー失敗時は元を保持するよう修正 (L-11)
- `opencode-server`: `CONFIG_STATE` 静的設定を `ServerState.config` へ移行し、
  複数インスタンス間で設定が上書きされる問題を修正（分離テスト追加）(M-21 P0)
- `opencode-tui`: `FileIndex::search` を Top-K ヒープへ変更し、全候補の clone と
  O(n log n) ソートを回避（顺序は既存と同一）(M-35)
- `opencode-watcher`: `debounce_ms` を実際にパス単位の合体へ配線（leading-edge、
  回帰テスト追加）(M-36)
- `opencode-server`: TUI リクエスト/レスポンスキューと Notify を `ServerState` フィールド
  へ移行し、インスタンス間で共有される問題を解消（分離テスト追加）(M-21 P0-②)
- `opencode-mcp`: `read_resource` を `send_request_with_progress_timeout` へ切替え、
  `catalog_ms` タイムアウトを適用（無応答サーバーでのハングを解消）(M-27)
- `opencode-util`: `build_filter(level)` を追加し `init_tracing` の level 引数を
  実際のログレベルへ反映（呼び出し側配線は未対応）(L-04 一部)
- `opencode-lsp`: stdin ロックを await 前に解放し、30s 実タイムアウト・pending
  確実削除・Drop 時の子プロセス/reader 回収を実装 (H-09)
- `opencode-session`: LLM リトライに上限（`retries` 既定 3）と 300s ストリーム
  タイムアウトを適用し、retry delay を saturating 計算へ変更 (H-06)
- `ALIGNMENT.md`: テスト件数/カバレッジを 974 件／36.27% へ更新し ADR-0008 へリンク (M-22)
- `opencode-server`: 実行中 prompt の `AbortHandle` を `ServerState` で保持し、abort 時に
  キャンセルと Idle 更新を配線（外部契约 `aborted: true` は維持）(H-05)
- `opencode-storage`: マイグレーションを単一トランザクション化し、metadata 列の有無を
  PRAGMA で構造的に判定（エラー文字列依存を解消）(H-10 一部)
- `opencode-local-llm`: ollama 実行を tokio 非同期化し、timeout 時 kill を実装
  （既定 300s、`OPENCODE_LOCAL_LLM_TIMEOUT_SECS` で上書き可）(M-10)
- `opencode-tool`: パス判定を canonicalize + コンポーネント単位比較へ変更し、
  `..`／兄弟ディレクトリ／symlink escape の誤判定を修正 (H-02)
- `opencode-tool`: eval/source/コマンド置換等の動的実行を検出した場合に常に
  承認要求へフォールバックし、`always allow` を生成しないように修正 (M-13)
- `opencode-command`: `CommandError` ドメインエラーを追加し、下位エラーを
  source チェーンで保持（公開シグネチャは anyhow::Result を維持）(M-04)
- `opencode-storage`: マイグレーションに `PRAGMA user_version` を記録し、
  2 回目以降はスキップ（既存 DB の互換性もテスト）(H-07)
- `opencode-session`: 未信頼ツール結果をプロンプト変換点で system-reminder
  ラップし、二重ラップを回避（既存契約は維持）(M-12)
- `opencode.jsonc`: 認証キーのプレースホルダを `{env:ANTHROPIC_API_KEY}` へ変更 (M-09)
- `opencode-provider`: 認証情報の読み込み/永続化エラーを `try_*` API と状態アクセサで
  明示（トークン値をログへ出さず、API 互換を維持）(H-14)
- `opencode-session`: compaction の永続化失敗を `?` で伝播、TodoManager は warn
  と best-effort 契約を明記 (M-19)
- `opencode-server`: `/experimental/*` 4 エンドポイントを 501 Not Implemented へ変更し、
  未実装の `accepted` レスポンスを廃止 (M-06)
- `opencode.jsonc`: npx 実行時依存 2 件を Exact バージョン固定 (H-04 一部。
  残る 2 件は公式 npm にパッケージ自体が存在しないため据え置き、報告済み)
- `opencode-session`: `SessionOps` が静的ディスパッチ専用であることを doc comment
  に明記し、Future Send の扱いを記録 (M-18)
- docs: ADR 4 件の date に RFC3339 オフセットを付与、`TERMUX_BUILD.md` の
  verified を by/at 構造へ修正 (M-33)
- `opencode-config`: 未知トップレベル設定キーと未定義 `{env:VAR}` を警告化
  （キー名／変数名のみ記録、起動継続と値の非漏洩を維持）(M-08)
- `opencode-server`: request id と session id を伝播する構造化スパンを
  ミドルウェアで導入（トークン／クエリ／ボディは属性に含めない）(M-23)
- `opencode-plugin`: subprocess ストリームの JoinHandle を保持し失敗経路で abort、
  call のロック取得にも timeout を適用してデッドロックを解消 (H-08)
- `opencode-tool`/`opencode-session`: 権限 fail-open の因果が上位ゲート
  （executor の Ask→Deny／broker の Reject）で否定されたことを root とテストで明記 (M-01)
- Cargo: CI に `locked: true` を明示し、workspace 依存を 6 件集約（バイパス 28→22）(M-15 一部)
- `opencode-server`: 認証情報の永続化失敗を `try_*` API 経由で HTTP 500/400 へ反映し、
  初回起動の未存在ファイルは空認証へフォールバック（キー値は非ログ）(H-14)
- Cargo: `[workspace.lints.rust]` に `unsafe_code = "deny"` を追加し、全クレートで
  unsafe Rust を禁止 (M-34 一部)

## [0.1.0]

初期実装。詳細は Git 履歴を参照。
