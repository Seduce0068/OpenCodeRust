---
type: Reference
description: OpenCodeRust の変更履歴 (Keep a Changelog / SemVer 準拠)。
tags: [changelog, releases]
generated:
  by: agent
  at: "2026-11-22T08:23:15+09:00"
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
  テスト総数 594 → 3045 件（all-features は 3044 件）

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
- `cargo-nextest` / `cargo-tarpaulin` を導入し、カバレッジ 49.19% を計測
  （前回 37.63% から 49.19% へ向上、+11.56pt の大幅増）。「計測揺らぎ」の
  説明は直近の 49.20% → 49.19% → 49.19% の横ばいなど、ADR-0008 の該当
  箇所に限定する
- リリースビルド・`cargo audit`・`cargo outdated` を再検証 (脆弱性 0、
  依存は最新、`version` = `OpenCode 0.1.0`)
- 全 feature で check / clippy / test (899 passed) が成功。`cargo nextest
  run --workspace` は 900 passed / 0 skipped

### Fixed

- `tui`: セッション一覧 API に directory/roots/start フィルタを露出
  （swarm-coding R1-R6）
- `server`: ストリーム異常終了時の run_status を Idle に復帰（後続 Busy 保護）
  （swarm-coding R1-R6）
- `session`: 明示開始された compaction の Compacting 状態を終端化
  （swarm-coding R1-R6）
- `watcher`: unwatch 失敗時も監視登録とデバウンス表を整合させる
  （swarm-coding R1-R6）
- `agent`: 永続化サブセッションの subagent 深さを保持
  （swarm-coding R1-R6）
- `tui`: シェル実行失敗時の optimistic メッセージ残存を解消
  （swarm-coding R1-R6）
- `config` / `tool` / `session` / `core` / `codemode`: 境界値 panic の解消
  （1文字引用符・全角空白・u32/u64 オーバーフロー・巨大年）
  （swarm-coding R1-R6）
- `server`: 信頼プロキシ設定時のみ X-Forwarded-For を尊重（opt-in・fail-closed）
  （swarm-coding R1-R6）
- `server`: limit クランプの `x-limit-clamped` 通知と CORS expose
  （swarm-coding R1-R6）
- `mcp`: OAuth フローの TTL 回収（放置された verifier/state を解放）
  （swarm-coding R1-R6）
- `server`: plugin loader を per-instance 化（process-global を削除）
  （swarm-coding R1-R6）
- `server`: MCP OAuth manager を per-instance 化（process-global を削除）
  （swarm-coding R1-R6）
- `server`: message part 追加の冪等化（任意 ID 受け付け・再送重複を防止）
  （swarm-coding R1-R6）
- `server`: `/provider` に `providers` エイリアスを追加しキーを統一
  （swarm-coding R1-R6）
- `session`: レガシー SessionRow がライフサイクル status を保持
  （swarm-coding R1-R6）
- `config`: 未定義 env 参照の opt-in strict モード（コメント内は除外）
  （swarm-coding R1-R6）
- `tui`: セッション削除の 404 を冪等な既削除として扱う
  （swarm-coding R1-R6）
- `config` / `cli`: wellknown 環境変数ブロックリストの拡張と真値判定の統一
  （swarm-coding R1-R6）
- `agent`: 空モデル ID を type 付きエラーとして早期失敗させる
  （swarm-coding R1-R6）
- `config`: provider リストの明示的空配列によるクリアを可能化
  （swarm-coding R1-R6）
- `provider` / `mcp`: retryable 判定の type フォールバックと SSE パース失敗の伝播
  （swarm-coding R1-R6）
- `cli`: `--project` 指定時に対象プロジェクトの logLevel で tracing を初期化
  （swarm-coding R1-R6）
- `config`: wellknown 検証パイプラインの共有とキー名・URL の redact
  （swarm-coding R1-R6）
- `provider` / `config`: usage デコード失敗の可視化とディレクトリ走査の決定的化
  （swarm-coding R1-R6）
- `config` / `docs`: legacy mode 移行での明示 mode 保持と README 設定源の網羅
  （swarm-coding R1-R6）
- `mcp` / `provider`: tools_changed 復元と Anthropic error イベントの昇格
  （swarm-coding R1-R6）
- `lsp` / `tui`: close_document と一時ファイル RAII の追加（リソース回収）
  （swarm-coding R1-R6）
- `server`: 終端 PTY セッションの回収（zombie 化・リソース残存を防止）
  （swarm-coding R1-R6）
- `session` / `plugin`: リソース累積の解消（writer_lock / clients / trusted_scripts）
  （swarm-coding R1-R6）
- `server` / `tui`: 単一メッセージ取得の wire を一覧取得と統一
  （swarm-coding R1-R6）
- `server` / `mcp`: TUI レスポンスキューと MCP ログの容量上限（無限成長を防止）
  （swarm-coding R1-R6）
- `tui`: compact / revert / fork の API 契約不一致を修正（操作の失敗を解消）
  （swarm-coding R1-R6）
- `server` / `plugin`: ビルトイン認証プラグインのロード失敗を fail-closed 化
  （swarm-coding R1-R6）
- `provider`: 認証キーローテーションを永続化（失敗時ロールバック）
  （swarm-coding R1-R6）
- `cli`: `config --check` で設定を明示検証（値・パス非露出、cwd 失敗を区別）
  （swarm-coding R1-R6）
- `server`: graceful shutdown 時のプロンプト abort とキュー終端化（再起動時の二重実行防止）
  （swarm-coding R1-R6）
- `tui`: APP_VERSION_DATE を 2026.11.21 へ同期（固定ラベル方針明記）
  （swarm-coding R1-R6）
- `docs`: 不正設定時の挙動と次のアクションを README に明記
  （swarm-coding R1-R6）
- `server` / `tui`: MCP 同期失敗をヘッダで伝え、ボディの既存ワイヤ形状を維持
  （swarm-coding R1-R6）
- `plugin` / `server`: plugin init の競合可視化と世代採番、session sync の
  追加・更新のみ化と明示削除配線（swarm-coding R1-R6）
- `config`: 入れ子キー誤記と不正値の警告（ユーザー定義名は `*` でマスク）
  （swarm-coding R1-R6）
- `config`: 設定エラーから値・ファイルパスを除去し安全なフィールドパスのみ保持
  （swarm-coding R1-R6）
- `config` / `core` / `watcher`: テストの env 直列化・時間依存を解消
  （swarm-coding R1-R6）
- `types` / `session` / `server`: AgentColor 正規化、diffs 既定値、PtyStatus 集約
  （swarm-coding R1-R6）
- `storage`: セッション権限 JSON の二重エンコードを修正（PATCH 後の CorruptData を解消）
  （swarm-coding R1-R6）
- `cli`: 設定ロード失敗時の値非依存警告（初期化前後どちらも到達可能）
  （swarm-coding R1-R6）
- `config` / `tui`: provider 衝突と frontmatter 失敗の警告、入力スレッド join
  （swarm-coding R1-R6）
- `plugin` / `config`: shutdown RPC 失敗時 kill フォールバック、非推奨キーと
  明示 CONFIG 不在の警告（swarm-coding R1-R6）
- `server` / `cli`: graceful shutdown、終了時 session flush、設定警告 replay
  （swarm-coding R1-R6）
- `config` / `tool`: 移行バックアップの順序保証、マスク形式と timeout の明記
  （swarm-coding R1-R6）
- `refactor`: 定数の単一情報源化（OUTPUT_TOKEN_MAX / デフォルト URL / 診断上限 /
  出力上限）、SSE バックオフの文書修正（swarm-coding R1-R6）
- `provider` / `server` / `storage`: 認証の永続化整合性、MCP ログアウト、
  models.dev 境界、キュー metadata の fail-loud（swarm-coding R1-R6）
- `cli`: レガシー設定移行を起動時一度きりの設計意図へ復元、失敗時の
  詳細出力を抑制（swarm-coding R1-R6）
- `server`: 完了ログの request_id 明示、variant lookup の無効化と非ブロック化
  （swarm-coding R1-R6）
- `cli` / `docs`: 認証状態の起動時固定明記、設定反映タイミングと readiness
  記述の実装追随（swarm-coding R1-R6）
- `server` / `cli`: readiness 失敗 reason の構造化、TUI env の RAII 復元
  （swarm-coding R1-R6）
- `server`: plugin loader 重複登録の warn、redact の camelCase/OAuth キー対応
  （swarm-coding R1-R6）
- `server` / `cli` / `docs`: ProviderError の相関 ID、永続化失敗の構造化ログ、
  dogfood ドキュメント修正（swarm-coding R1-R6）
- `provider`: OpenAI 互換ストリームの UTF-8 境界安全化（swarm-coding R1-R6）
- `tui`: Markdown 表の端末幅クランプを本番描画経路へ配線、flat 表示の幅遵守
  （swarm-coding R1-R6）
- `plugin` / `types` / `storage` / `session`: Plugin の async_trait 統一、
  Todo 型の単一情報源化（swarm-coding R1-R6）
- `mcp` / `config` / `tui`: progress hard deadline、transport 直列化、OAuth
  並行フロー分離、ratatui feature 絞り込み（swarm-coding R1-R6）
- `provider` / `session` / `plugin`: ストリーム切断即検知、フック task の
  Drop 自動 cleanup と個別 timeout（swarm-coding R1-R6）
- `tui`: フッタ/sidebar/toast の表示幅計算、centered の境界値（swarm-coding R1-R6）
- `tool` / `plugin` / `mcp`: hook データの再帰 redact、URL userinfo 除去、
  npm install の排水と孤立 crate の意図明記（swarm-coding R1-R6）
- `config` / `storage` / `provider` / `session`: .opencode 設定階層の維持、
  schema 非公開化、ストリーム retryable 保持（swarm-coding R1-R6）
- `cli` / `config` / `mcp` / `server`: 設定 fingerprint の完全化、protocol 正規化、
  セッション更新の coalescing（swarm-coding R1-R6）
- `watcher` / `config` / `tool` / `agent`: デバウンス/キャッシュの回収、
  サブエージェント深さ上限と伝播、シェル出力の上限（swarm-coding R1-R6）
- `storage` / `mcp` / `server`: session.metadata と total_cost の往復、v3 移行、
  auth 権限の fail-closed、破損値の error/warn 非漏洩（swarm-coding R1-R6）
- `config` / `types` / `session` / `server`: config.env 配線、Todo/AgentColor の
  serde 対称化、glob 衝突解消、連鎖 env 置換（swarm-coding R1-R6）
- `provider` / `config` / `server` / ADR: リモート JSON の 8 MiB 上限、
  OAuth ログの ring buffer、ドキュメントの現状追随（swarm-coding R1-R6）
- `config` / `server` / `agent` / `session` / ADR: timezone/agents のマージ、
  PATCH 後の再 bootstrap と secret マスク、API 境界の明確化、
  ADR と実装の整合（swarm-coding R1-R6）
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
- `opencode-server`: `SESSION_RUN_STATUS` / `PROMPT_LOCKS` を `ServerState` フィールドへ
  移行し、インスタンス間で実行状態が共有される問題を解消（分離テスト追加）(M-21)
- `opencode-storage`: DB マイグレーションの rollback / commit / in_memory 分離を
  検証するテストを 3 件追加 (M-11)
- `opencode-storage`: `Storage::migrate()` を schema.rs の `ALL_MIGRATIONS` 経路へ
  統一し、別スキーマ問題を解消（冪等テスト追加）(H-07)
- `opencode-server`: キュー promote 失敗時に `failed` へフォールバックし、
  新規 assistant メッセージへ `queue_id` を付与して再実行重複を抑制 (H-11)
- docs: ADR 10 件の status を OKF enum (`approved`) へ、generated を by/at
  構造へ修正、`TERMUX_BUILD.md` に title 追加と本文 ## 化 (M-33)
- `opencode-server`: SSE ストリームでクライアント切断を `tx.closed()` で検知し
  プロバイダー消費を早期停止（協調 break、外部契約は不変）(M-38)
- `opencode-storage`: `MessageRepository::replace_for_session` を追加し、
  delete+create を単一トランザクション化（server.rs へ配線）(H-10)
- `opencode-server`: mcp_oauth.rs に 14 件のテストを追加
  （OAuth 判定／エラー経路／CSRF state、ダミー値のみ使用）(M-11)
- `opencode-tool`: grep_tool の走査を所有値クロージャ＋`spawn_blocking` へ逃がし、
  async 実行中の tokio ワーカー占有を回避（出力契約は不変）(M-37)
- `opencode-core`: Event Bus のコールバックを Arc スナップショット化し、ロック
  解放後に呼び出すよう変更（順序保証なしの性质は不変）(L-01)
- `MAPPING.md`: MCP list_changed が tools のみ対応で resources/prompts は
  debug ログのみであることを明記 (L-02)
- Cargo: version/edition/authors/license を直書きしていた 7 クレートを workspace
  継承へ統一（値は不変、Cargo.lock の実依存変更なし）(M-15)
- `opencode-cli`: 起動時の `fmt::init()` を `opencode_util::init_tracing` へ置換し、
  `config.logLevel` を実効化（RUST_LOG より優先、実 CLI で確認）(L-04)
- `opencode-session`/`opencode-tui`: 未配線の `process_response` と自作 markdown
  パーサ 6 関数（計約 400 行）と dead_code 抑制を削除 (L-03)
- `opencode-storage`: `migrate_pool` を切り出し `Storage::migrate` と `Database`
  の 2 経路を user_version 記録まで完全統一 (H-07)
- `opencode-server`: ProviderAuthError の Display / source / From 変換を検証する
  テスト 7 件を追加（ダミー値のみ）(M-11)
- `opencode-types`/`opencode-core`: クレートドキュメントと主要 pub mod の `///`
  を追加、ADR-0007 に段階進捗を記録 (L-08)
- `opencode-server`: pty.rs に 6 件のテストを追加（PTY spawn なし、ダミー値のみ）(M-11)
- `opencode-mcp`: oauth.rs に 8 件のテストを追加（CSRF state 一意性、設定検証、
  authorize URL 組み立て。外部通信なし）(M-11)
- docs: 残り 12 クレートの lib.rs に `//!` モジュールドキュメントを追加 (L-08 第2段)
- `opencode-provider`: models/bootstrap の純粋変換ロジックに 11 テストを追加
  （ダミー値のみ、HTTP/ストリーミングなし）(M-11)
- `opencode-command`: 公開 3 メソッドを `Result<_, CommandError>` 化し、charter
  の「anyhow は境界のみ」に準拠（破壊的変更、CLI の `?` は From で互換）(M-04)
- docs: opencode-types/core の個別 pub 型へ `///` を 280 行追加し `missing_docs`
  残 276 件を 0 件へ解消 (L-08 第3段)
- `opencode-provider`: 未実装の LiteRT を bootstrap で登録しないようにし、
  `LITERT_MODEL_PATH` 等が無視されることを警告（回帰テスト追加）(L-09)
- `opencode-types`: 未使用の `SessionID` の `sqlx::Type` derive を削除
  （`SessionStatus` は storage が実使用のため維持）(M-16 一部)
- MAPPING.md / docs/opencode-provider.md: LiteRT 未実装の現状を明記 (L-09)
- `opencode-watcher`: debounce のパス独立性と window=0 の対照テスト 2 件を追加
  （5 回連続 pass でフレーキ確認）(M-11)
- `opencode-codemode`: ToolError の構造化データ保持と JSON null/不在の区別など
  4 テストを追加 (M-11)
- `opencode-server`: stream_message の冗長な全履歴 clone を廃止し、ロック保持中に
  provider 用メッセージへ直接変換（provider へ渡る履歴は不変）(L-14)
- `opencode-mcp`: protocol.rs に 17 テストを追加し、`listChanged` / `isError` /
  ContentBlock の `mimeType` の camelCase wire format を修正・固定 (M-11)
- `opencode-plugin`: subprocess/protocol.rs に RPC エンベロープの serde 境界
  8 テストを追加（subprocess spawn なし）(M-11)
- `opencode-tui`: context/keybind.rs に 17 テスト（LeaderKeyState の状態遷移、
  Display 書式、BackTab 差、10 回連続 pass）、command.rs に 10 テストを追加 (M-11)
- `opencode-agent`: message.rs に 7 テストを追加（コンストラクタのフィールド
  検証、serde 往復、Conversation ビルダー）(M-11)
- `opencode-tui`: event.rs に 12 テストを追加（EventBus 配送、clone 共有、
  全 StateChange variant の Clone/Debug。serde derive 不在は明記）(M-11)
- provider/tool: 外部フルパス参照ゼロの 7 モジュールを `pub(crate) mod` へ
  縮小（ファサード公開経路は不変、dead_code 発生箇所は見送り）(M-17 一部)
- `opencode-tui`: context/session_context.rs に 11 テストを追加
  （データ型の serde 往復、内部型は snake_case で自己整合）(M-11)
- tool/provider: ToolError と ProviderError に source 付きバリアントを追加
  （加法変更、既存パターンに倣う。呼び出し元置換は未実施）(M-04 一部)
- `opencode-server`: `PROJECT_METADATA` を `ServerState` フィールドへ移行し、
  4 ハンドラへ State extractor を追加（ルート登録は不変、分離テスト追加）(M-21)
- `opencode-storage`: schema.rs の DDL 定数を文字列レベルで検証する 7 テストを追加 (M-11)
- `opencode-mcp`: tool.rs のレジストリ（登録/検索/削除/サーバー別フィルタ）8 テストを追加 (M-11)
- `opencode-provider`: tools.rs のツール変換・tool_choice・serde wire format 17 テストを追加 (M-11)
- `opencode-provider`: no-op の `normalize_interleaved_field` と呼び出しを削除（挙動不変）(P-04)
- MAPPING.md: 未配線の設定フィールド群（keybinds / snapshot / autoupdate /
  enterprise / experimental と ExperimentalConfig 6 サブフィールド）を明記 (P-03)
- `opencode-config`: McpProtocol::Auto が probe 未実装で Legacy と同じ revision を
  送る実挙動を doc/MAPPING へ明記し回帰テストを追加 (P-05)
- docs/adr/adr-0006: 存在しない上流ファイル名の参照を削除し、調査報告に依存する
  旨を明記（同期維持の判断は不変）(P-08)
- `opencode-util`: build_filter の level 優先／RUST_LOG fallback を検証する
  3 テストを追加（環境変数を変更せずフレーキ回避）(M-11)
- `opencode-config`: upstream の `timezone` キーを追加し、他 5 キーの未対応を
  MAPPING.md に明記（型不一致のため alias 見送り、権威 checkout を特定）(P-06)
- `opencode-core`: bus.rs に 8 テストを追加（L-01 の中核であるコールバック内
  subscribe のデッドロック回避を timeout 付きで検証）(L-01 一部)
- `opencode-plugin`: subprocess/mod.rs に 13 種フック名変換の 2 テストを追加 (M-11)
- `opencode-types`: identifier/session/project/message に 16 テストを追加
  （94.12% へ到達）。serde wire format の不整合 4 点は修正せず報告 (M-11)
- `opencode-plugin`: subprocess/auth.rs に 10 テストを追加（実際の認証情報・
  subprocess spawn なし、ダミー値のみ）(M-11)
- `opencode-tool`: edit/mod.rs に 7 テストを追加（unified diff・trim_diff・
  改行正規化、既存 replacers テストと非重複）(M-11)
- `opencode-provider`: message.rs に 14 テストを追加（serde 往復・wire format・
  deserialize_content フォールバック。`ChatRequest.provider_options` の
  snake_case は意図的設計と確認）(M-11)
- `opencode-plugin`: subprocess/runtime.rs に 6 テストを追加（JS ランタイムの
  コマンド/引数、PATH 検索の整合性。外部プロセス起動なし）(M-11)
- convert.rs は未配線スタブであることを再確認（PromptPart 未定義のため配線即
  compile error）。テスト追加を見送り差分ゼロ (M-11)
- `opencode-server`: TODO_MANAGER / PTY_MANAGER を ServerState フィールドへ移行
  （PTY は 6 ハンドラに State extractor 追加、分離テスト 2 件）(M-21)
- `opencode-provider`: google.rs に 8 テスト、mistral.rs / deepseek.rs に 12 テスト
  を追加（モデルカタログ、convert_request、SSE パース。実 API キーなし）(M-11)
- `opencode-provider`: azure.rs / groq.rs / xai.rs に 23 テストを追加
  （プロバイダ固有のモデル・capability・Azure URL 構築、実 API キーなし）(M-11)
- `opencode-provider`: together.rs / perplexity.rs / cerebras.rs に 19 テストを追加
  （各プロバイダ固有のモデル ID・capability・コスト、実 API キーなし）(M-11)
- `opencode-provider`: cohere.rs / deepinfra.rs / openrouter.rs に 26 テストを追加
  （swarm-coding R2 の 3 エージェント並列実装、R4 独立レビュー指摘 0 件、実キーなし）(M-11)
- `opencode-tui`: router / branding / app/state / slash_command / hooks に 45 テストを
  追加（swarm-coding R1-R6）。R4 が検出した `navigate()` の history 重複バグを
  修正し、`go_back()` が正しい前ルートへ戻るよう修正 (M-11)
- `opencode-tui`: Router::default() を new() 委譲へ変更し history 不整合を解消。
  terminal.rs にタイトル整形の純粋関数 4 テスト、todo_item/tool_call に 24 テスト
  を追加（swarm-coding R1-R6、R5 で Buffer/TodoStatus のコンパイルエラーを修正）(M-11)
- `opencode-tui`: border(8) / markdown parser(9) / status(3) / help(8) / alert(12)
  に 40 テストを追加（swarm-coding R1-R6、R5 で unused_assignments を修正）(M-11)
- `opencode-tui`: confirm(13) / session_rename(20) / tag(28) / session_text(9) に
  70 テストを追加（swarm-coding R1-R6、R5 で tag の選択前提を修正）(M-11)
- `opencode-tui`: fork(18) / prompt_stash(27) / code_block(13) / command_palette(30)
  に 88 テストを追加（swarm-coding R1-R6）(M-11)
- `opencode-tui`: session_list(37) / model_select(32) / theme_list(30) /
  agent_select(21) に 120 テストを追加（swarm-coding R1-R6、R5 で backspace の
  1 文字削除という期待値を修正）(M-11)
- `opencode-tui`: session_export(22) / timeline(19) / skill_list(27) / subagent(22)
  に 90 テストを追加（swarm-coding R1-R6、backspace / 空アイテム時のクランプは
  実装から期待値を導出）(M-11)
- `opencode-tui`: mcp(28) / provider(24) / permission(20) に 72 テストを追加
  （swarm-coding R1-R6、R5 で `let mut` 宣言漏れを修正）(M-11)
- `opencode-tui`: dialog(6) / message(12) / home(9) / markdown renderer(12) に
  39 テストを追加（swarm-coding R1-R6、home は OPENCODE_STATE_DIR を一時 dir へ
  切替えて実ユーザー状態を保護）(M-11)
- `opencode-tui`: api(13) / context/app_context(14) / clipboard(7) に 34 テストを
  追加（swarm-coding R1-R6、clipboard は OSC52 生成を純粋関数へ抽出）(M-11)
- `opencode-server` / `opencode-tui`: list_messages の snake_case 契約を双方向で
  固定する 3 テストを追加（sessionID / createdAt は別 endpoint の別契約と確認）(M-02)
- `opencode-tool`: 文字列化していた下位エラー 33 箇所を `execution_with_source`
  へ置換し `source()` チェーンを保持（メッセージ文言は不変、非対象は据え置き）(M-04)
- `opencode-provider`: 20 ファイル 62 箇所を `api_error_with_source` /
  `network_error_with_source` へ置換（旧パターン残存 0、リトライ判定も同一）(M-04)
- `opencode-tool`: multiedit に権限確認、apply_patch Move の移送先検証、
  WebFetch の SSRF 対策（6to4/Teredo 含む）を追加（swarm-coding R1-R6、
  R4 のリグレッション/IPv6 抜け穴指摘を R5 で修正）(H-15/M-54/H-24)
- `provider` / `config` / `core` / `mcp`: リトライ判定の不一致、env 展開の
  JSON 文字列エスケープ、MCP initialized の notification 送信、FileSystemImpl の
  パス検証を修正（swarm-coding R1-R6、R4 の timeout/後方互換指摘を R5 で修正）
  (H-16/H-17/H-18/H-20)
- `mcp` / `agent` / `provider` / `lsp`: RFC 8707 resource パラメータ、サブセッションの
  disabled_tools 継承、Anthropic SSE の状態付きパーサ（tool id 対応）、LSP の
  Content-Length フレーミングを追加（swarm-coding R1-R6、R5 で型/`mut`/clippy 修正）
  (H-19/H-21/H-22/H-23/H-25)
- `server` / `tool` / `tui`: PTY の status 遷移と child 回収、UTF-8 境界を尊重する
  スライス、キーバインド登録と bool 応答の厳密化（swarm-coding R1-R6、
  R4 の status 順序/キー表示指摘を R5 で修正）(M-43/M-51/M-52/M-53)
- `server` / `session` / `tui`: 未実装 14 ハンドラの 501 化と汎用メッセージ、
  retry-after の clamp、復元時 total_cost のゼロ化、クライアント側の unsupported
  処理（swarm-coding R1-R6、R4 の誤表示/UI 退化指摘を R5 で修正）
  (M-44/M-45/M-50)
- `cli` / `server` / `tui`: 統計の Cost unavailable 表示、残り 5 ハンドラの 501 化、
  TUI 旧レスポンス型の削除（swarm-coding R1-R6、R4 findings 0 件）(M-45 フォロー)
- `agent`: tool_result の ID/name/is_error 保持と tool_use の会話伝播
  （swarm-coding R1-R6、R4 の tool_use 不在/空 ID 指摘を R5 で修正）(M-48)
- `agent` / `provider` / `session`: permission pattern の明示引数化（メタ文字・
  traversal・ルール順序のバイパス对策を含む）、未使用 retry ヘルパー削除、
  FreeUsageLimit の非リトライ化（swarm-coding R1-R6、R4 high 3 件を R5 で修正）
  (M-47/M-49/M-50)
- `Cargo.toml`: 未使用 workspace 依存 11 件（hyper / rustls / rustls-webpki / h2 /
  time / tower / dashmap 等）を削除し cargo 警告 11→0（swarm-coding R1-R6）(L-07 一部)
- `Cargo.toml` / `plugin` / `lsp` / `watcher`: tokio `full` を使用中の9 feature へ
  削減（workspace = true で継承、個別ビルド 5 クレートも確認）(L-07)
- `server`: list_messages に後方互換な limit/offset pagination を追加
  （Query デシリアライズ経路のテストを含む、swarm-coding R1-R6、R4 指摘を R5 で修正）
  (L-14)
- `server` / `MAPPING.md` / `Cargo.toml`: /api エイリアス、/stream の位置づけ明記、
  残り 20 件の workspace 依存集約（swarm-coding R1-R6、R4 の誤記/行番号/
  false green 指摘を R5 で修正）(P-01/M-46/L-07)
- `ci.yml` / `server` / `routes.rs`: CI に all-features・unsafe lint ゲートを追加、
  http_request スパン完成（/api 対応・status/latency/outcome・request-id 検証）、
  find 系再帰走査の spawn_blocking 化（swarm-coding R1-R6、R4 findings 0 件）
  (M-34/M-37/L-04)
- `server`: read_file と git status の同期 I/O を tokio 非同期化
  （swarm-coding R1-R6、R4 findings 0 件）(M-37 一部)
- `core` / `session` / `mcp`: 未配線の core Bus と全 publish 配線を削除
  （Server `broadcast<String>` を単一イベント源とし SSE 契約は不変、
  swarm-coding R1-R6、R4 findings 0 件）(M-20)
- `server` / `provider` / `tool`: /stream の初期切断検知を追加（切断時future drop＋
  永続化）、transform/registry を pub(crate) 化（swarm-coding R1-R6、
  R4 の永続化回帰指摘を R5 で修正）(M-38/M-17)
- `server` / `Cargo.toml`: list_files / git status の非同期化と残り 3 依存の
  workspace 集約（swarm-coding R1-R6、R4 の next_entry Err 挙動指摘を R5 で修正）
  (M-37/M-15)
- `provider` / `tool`: Anthropic SSE の行バッファと UTF-8 分割対応（実 HTTP モック
  テスト付き）、tool 側 WalkDir の spawn_blocking 化（swarm-coding R1-R6、
  R4 の UTF-8 境界/NO_PROXY 指摘を R5 で修正）(M-11/M-37)
- `session` / `server` / `tui`: Todo 状態を最小配線（todowrite/todoread の false
  success を解消、todo.updated broadcast と TUI 再同期、swarm-coding R1-R6、
  R4 の session_id 誤検知は却下、再同期指摘を R5 で修正）(M-05)
- `permission` / `tool`: BashArity を tool 内へ移動し tool→permission 依存を解消、
  2 トークン入力の範囲外スライス panic を修正（swarm-coding R1-R6、
  R4 の panic 指摘を R5 で修正）(M-16)
- `cli` / `util`: TUI セッション中のみログをファイルへ出力し stdout 描画干渉を解消、
  WorkerGuard を main ローカルで保持して終了時フラッシュを保証（swarm-coding
  R1-R6、R4 の static 保持/コメント不一致指摘を R5 で修正）(L-04)
- `renovate.json`: 依存更新検知を Renovate へ委譲する最小設定を追加
  （App 有効化は外部サービス操作のためユーザー判断として残す）(M-34)
- `provider`: 全 16 アダプタの SSE ストリームを共通 SseLineBuffer ベースへ変更し、
  チャンク境界・UTF-8 途中分割・終端行の取りこぼしを解消（swarm-coding R1-R6、
  4 エージェント並列、R4 の混在/flush 指摘を R5 で修正）(M-11)
- `session`: revert 経路のデータ損失を修正（checkout 失敗の種別判定、ls-tree 再確認、
  worktree 外パスの除外、unrevert の restore 失敗伝播。swarm-coding R1-R6、
  R4 の ParentDir/remove_file 指摘を R5 で修正）(H-26/H-27)
- `provider` / `tool` / `server`: Responses SSE の生バイト化、エラープレビューの
  UTF-8 境界丸め、write の存在判定、find_files の type フィルタ（swarm-coding
  R1-R6、R4 の成功パスアロケ指摘を R5 で修正）(M-56/M-57/M-58/M-59)
- `server` / `session` / `tool`: worktree の既存ブランチ判定、cleanup の
  辞書順 ID 比較、edit の存在判定を修正（swarm-coding R1-R6、
  R4 の UUID/try_exists 指摘を R5 で修正）(M-60/M-64/M-67)
- `session` / `storage` / `watcher`: TodoManager の安定 ID・トランザクション置換・
  セッション単位 writer 直列化、SQLite FK 有効化と破損データ fail-loud、
  FileWatcher の複数パス監視（swarm-coding R1-R6、R4 の並行レース指摘を R5 で修正）
  (M-61/M-62/M-63/M-65/M-66)
- `tool` / `provider` / `server`: 入力引数の厳密検証、デッドコード削除、
  delete_message の NotFound、CORS localhost の port 省略許可（swarm-coding R1-R6、
  R4 の UTF-8 境界/スキーマ/オーバーフロー指摘を R5 で修正）
  (L-10/L-11/L-12/L-13/L-14b/L-15/L-17)
- `server` / `core` / `session`: キューリトライの指数バックオフと終端化、
  GitWatcher のデッドバリアント削除、LLM ストリームの切断即検知（swarm-coding
  R1-R6、R4 の命名不一致指摘を R5 で修正）(L-16/L-18/L-19)
- `storage` / `server`: Storage::new の FK cascade 有効化、MCP OAuth callback の
  state 必須化（CSRF バイパス対応）。P-04 は現状維持として定義を再設定
  （swarm-coding R1-R6）(M-11 残り / FK 統一 / P-04)
- `mcp` / `server`: 保存済み Bearer トークンを per-server OAuth manager から
  HTTP/SSE 接続へ注入（トークン誤送信防止）、ログアウト時の manager 削除、
  discovery の実 HTTP テスト 7 件（swarm-coding R1-R6）(M-11 残り)
- `provider`: Bedrock ストリームを生バイト保持＋JSON 値ベース解析へ変更し、
  UTF-8 境界分割・固定オフセット panic・空 TextDelta を解消（swarm-coding
  R1-R6）(M-55)
- `tui`: 常時 501 を返す skill/lsp 終点の 5 秒ポーリングを廃止し、
  ダイアログオープン時のみの同期へ変更 (H-13 一部)
- `plugin`: npm spec の厳密化（未指定/dist-tag/range 拒否、scoped 名保持）、
  `--ignore-scripts`、import 先の検証と file:// URL 正規化（swarm-coding
  R1-R6、R4 の Windows/エンコード/名検証指摘を R5 で修正）(H-03 一部)
- `workspace`: 参照 0 の orphan クレート `opencode-pty` を member 外し・削除
  （server の PtyManager は現状維持）(M-03 一部)
- `cli`: load_config の 11 回の再パースを cwd キーのキャッシュへ集約（L-04 一部）
- `session` / `tool`: タイトル/要約の UTF-8 境界 panic と apply_patch の
  stale patch 破壊を修正（swarm-coding R1-R6）(discovery high 2 件)
- `tool` / `provider`: モデルパッチの `*** Update File:` / `*** Move To:` を
  正しく解釈し宣言行数を検証、OpenRouter のヘッダ unwrap を排除
  （swarm-coding R1-R6、R4 のカウント/互換性指摘を R5 で修正）
- `server` / `tool`: Broker pending の残留と false success、detached worktree の
  除外、glob/grep の外部 symlink 権限すり抜けを修正（swarm-coding R1-R6）
- `tool` / `server`: read/ls/skill の外部 symlink 権限すり抜け、edit の replace_all
  型丸め、read の metadata エラー丸め、PTY cursor 境界値を修正し引数パーサを共通化
  （swarm-coding R1-R6、R4 の DRY 指摘を R5 で修正）
- `util` / `provider` / `server` / `session`: glob_up の symlink、SseLineBuffer の
  上限、limit クエリのクランプと list_filtered への適用（swarm-coding R1-R6、
  R4 の下流未適用指摘を R5 で修正）
- `permission` / `mcp` / `plugin`: plugin hook の deny 最優先化、MCP OAuth ストアの
  0600 atomic 書き込み、キャッシュ依存の再検証（swarm-coding R1-R6、
  R4 の Windows テスト指摘を R5 で修正）
- `docs` / `server` / `ci`: opencode-pty 削除後のドキュメント追随、
  OPENCODE_NO_AUTH の fail-open 判定修正、all-features の test/clippy ゲート追加
  （swarm-coding R1-R6、R4 の未検証指摘を R5 でローカル実行）
- `cli` / `tui` / `config`: attach パスワードの Bearer 配線、CLI の fail-open 判定、
  well-known トークン再注入、認証通知の一本化、with_auth_token の panic 排除
  （swarm-coding R1-R6、R4 の残存指摘を R5 で修正）
- `cli` / `tool` / `util` / `storage` / `mcp`: review+discovery の high 11 件を修正
  （/model 実切替、run flags 必須化、Bash 動的実行ガード、WebFetch DNS 固定と URL マスク、
  ログ 0600/フォールバック/エスケープ、SessionRow fail-loud、summary 往復、
  AgentRegistry fail-closed、Content-Length 上限。swarm-coding R1-R6、
  R4 の stdin ガード/URL 露出指摘を R5 で修正）
- `tool` / `mcp` / `cli` / `server` / `session`: レスポンス上限の逐次強制、
  PATCH summary の永続化、権限 pattern の redaction、SSE 切断検出を修正
  （swarm-coding R1-R6、R4 の 4 件を R5 で修正）
- `server` / `config`: CORS カスタムヘッダ、401 共通スキーマ、認証 fail-closed、
  worktree 非同期化、load_global 読み取り専用化、plugin:[] の置換永続化
  （swarm-coding R1-R6、R4 の 2 件を R5 で修正）
- `server` / `config`: 認証トークンの定数時間比較と起動時固定化、
  instructions:[] の presence 永続化（swarm-coding R1-R6）
- `storage` / `server`: 起動時のセッション一覧のみ破損行をスキップし、
  個別取得の fail-loud を維持（swarm-coding R1-R6）
- `storage` / `config`: sessions.metadata 列と ALTER マイグレーションを追加し、
  wellknown テストの env 競合を解消（swarm-coding R1-R6）
- `provider` / `tui` / `codemode` / `plugin` / `session`: 構造化 HTTP エラー、
  Timeout 付き Client、ReDoS 対策、入力安全性、Retry-After 配線
  （swarm-coding R1-R6）
- `provider`: OAuth 期限検知の本番配線、レスポンスヘッダの共通サニタイズ
  （swarm-coding R1-R6）
- `session`: ストリーミング経路の構造化エラー伝播を堅牢化（Retry-After 到達、
  誤分類防止、allowlist 再適用、生ペイロード漏洩防止）（swarm-coding R1-R6）
- `tui` / `plugin` / `local-llm`: ダイアログのクリック判定、書記素境界、
  モーダルの修飾キー、RPC バージョニング、出力上限と入力検証（swarm-coding R1-R6）
- `tui` / `docs`: SlashCommandPopup のクリックと ADR の実装整合（swarm-coding R1-R6）
- `tool` / `ci`: Bash の env 検証とプロセスグループ kill、WebSearch 上限、
  audit 定期実行・MSRV・release/unsafe ゲート（swarm-coding R1-R6）
- `server` / `session` / `agent` / `docs`: 観測性と整合性の強化、バックアップ Runbook
  （swarm-coding R1-R6）
- `tui` / `provider` / `plugin`: スクロール境界、retry 判定の統一、OAuth フロー ID
  （swarm-coding R1-R6）
- `cli` / `session` / `docs`: JSON 契約と clear の実配線、セッション保全、運用文書
  （swarm-coding R1-R6）
- `plugin` / `provider`: キャッシュの symlink 防御と SHA-256 検証、
  モデル解決の決定性、host の EOF 終了（swarm-coding R1-R6）
- `provider` / `local-llm` / `codemode`: キー永続化の分離、 Bedrock 認証警告、
  MCP 並行上限、グローバル登録の一意化（swarm-coding R1-R6）
- `server` / `tool` / `tui` / `codemode`: キー rotation API、LazyLock への移行、
  21 crate 追随、RegExp/URL/normalize の差分コーパス（swarm-coding R1-R6）
- `server` / `session`: permission Always のセッションスコープ保持、TLS を rustls 統一
  （swarm-coding R1-R6）
- `server` / `renovate`: CORS 許可リストの ServerState 分離、依存グループ化
  （swarm-coding R1-R6）
- `config` / `server` / `tui` / `plugin`: env fail-closed、500 の redaction、
  stream 直列化とキュー回収、権限 Enter 既定 Deny、trigger のロック解放
  （swarm-coding R1-R6）
- `config` / `storage` / `cli`: 設定の 0600 atomic 永続化と jsonc/json 整合、
  OPENCODE_DATA_DIR による DB パス解決（swarm-coding R1-R6）
- `config` / `tui` / `tool` / `mcp`: コメント判定と完全エスケープ、UX 文言、
  HTTP エラーの redaction、Weak ロックと SSE リスナ回収（swarm-coding R1-R6）
- `watcher` / `session` / `server` / `provider`: テストの恒真・環境依存を解消、
  start の原子的化と MCP OAuth の env 復元（swarm-coding R1-R6）
- `types` / `provider` / `agent` / `mcp` / `tui`: wire 互換、秘密の redaction、
  fail-loud、依存ゲート更新、ダイアログの折り返し（swarm-coding R1-R6）
- `session` / `mcp` / `tui`: stream エラーの redact、wire 名の serde 集約、
  OAuth 本線の fail-loud、折り返しとクリック位置の同期（swarm-coding R1-R6）
- `util` / `server`: ログの一意名と世代保持、liveness/readiness の分離
  （swarm-coding R1-R6）
- `server` / `storage` / `cli` / `util`: liveness の認証除外、O(1) readiness、
  ログ削除失敗の可視化（swarm-coding R1-R6）

## [0.1.0]

初期実装。詳細は Git 履歴を参照。
