---
type: Todo
title: "OpenCodeRust プロジェクトTODO"
description: "OpenCodeRust 固有のタスク。横断タスクはグローバルTODOへ。"
tags:
  - todo
  - tasks
  - "opencoderust"
generated:
  by: opencode
  at: "2026-11-17T20:11:47+09:00"
reviewed:
  by: cross-review (20 reviewers / 6 validators)
  at: "2026-11-17T21:29:22+09:00"
status: current
---

## 進行中

（なし）

## 未着手

### 優先度: 高

- [ ] **[H-01]** 秘密情報の平文保存（GitHub PAT）
  - `.env:1` に GitHub PAT が平文、`.envrc:1` の dotenv で direnv 自動ロード、copilot-auth.ts
    が `GITHUB_TOKEN` 参照。取り消し再発行し、リポジトリ外の秘密管理先へ移して dotenv を無効化する
- [ ] **[H-03]** プラグイン subprocess bridge のサプライチェーン／信頼境界
  - `loader.rs:334-349` が未指定 npm spec に `*`、`:268-313` で install 実行、
    plugin-host.ts:403 が検証なし import、`$` ヘルパ／認証ヘッダ渡し fetch／Deno `--allow-all`。
    厳密バージョン＋lockfile＋`--ignore-scripts`、許可リスト化と信頼モデル ADR 化
- [ ] **[H-04]** MCP 実行時依存の完全性検証（2 件は固定済み、残りに絞る）
  - `chrome-devtools-mcp` / `tavily-mcp` は Exact 固定済み。未対応は
    `@modelcontextprotocol/server-devtools` と `server-playwright` が公式 npm で
    404（パッケージ不在）のため固定不能。代替（`@playwright/mcp` 等）の選定、
    lockfile 生成、CI npm スキャン対象化）
- [ ] **[H-07]** マイグレーションの完全統一（schema/user_version は対応済み、残りに絞る）
  - `Storage::migrate()` は schema.rs の ALL_MIGRATIONS 経路へ統一済み。未対応は
    SCHEMA_VERSION / user_version の共有（database.rs に pub(crate) の migrate_pool を
    切り出すリファクタ）と runbook 化
- [ ] **[H-10]** セッション永続化の並行制御（トランザクション化は対応済み）
  - 移行と delete+create（`replace_for_session`）は原子化済み。未対応は単一ライタ
    persist キュー、session_repo.update/create とメッセージ置換の同一トランザクション化、
    古い snapshot の遅延 commit による更新喪失（推測のまま）
- [ ] **[H-11]** キュー冪等性の完全化（終端化・相関 ID は対応済み）
  - promote 失敗時 failed 化と assistant への queue_id 付与は完了。未対応は
    セッション永続化と promoted 更新の同一トランザクション化（repository.rs 変更要）と、
    下流での queue_id による重複レスポンス排除。完全な二重課金防止は未検証
- [ ] **[H-13]** TUI イベントルートのブロッキング HTTP と全件再マッピング
  - 事実確認済み（2026-11-18、tui-http-worker）: 同期 HTTP は `ApiClient`（api.rs、
    書き込みスコープ外）にあり 30s timeout。Tick は 2 秒ごとで約 12 箇所の同期呼び出し。
    根本修正には api.rs の非同期化＋サーバの差分 API＋12 箇所の async 化が必要で
    大幅変更。app.rs のみの workaround は UI  timing を変えるため見送り

### 優先度: 中

- [ ] **[M-02]** 共有型の重複定義と serde_json 往復による変換
  - 事実確認済み（2026-11-18、shared-type-worker）: serde 往復で session 側の `usage` /
    `synthetic` / `ignored` が消えることを一時テストで実証。ただし session は types に非依存で、
    型集約には types 側フィールド追加＋session の依存追加＋server.rs の From 置換が
    必要（大幅変更）。推奨: types/message.rs へ3フィールド追加 → session を
    `pub use` 化 → server.rs を明示 From へ（H-10 の非アトミック書き出しと
    同一箇所、同時対応が効率的）
- [ ] **[M-03]** 未接続クレートと責務重複（PTY）による責務分離の形骸化
  - pty／util／watcher／local-llm／codemode の 5 クレートが無依存（codemode は ADR-0006
    で既知の先送り）、server が portable-pty を直接依存し同名 PtyManager を再実装。配線または member 外し、
    server を opencode-pty 利用へ
- [ ] **[M-04]** ライブラリ公開 API の `anyhow::Result`（CommandError は対応済み、残りに絞る）
  - `command/lib.rs` の CommandError 追加と source 保持は完了。未対応は 3 メソッドの
    シグネチャを `Result<_, CommandError>` 化（完全な charter 準拠、破壊的変更）、
    ToolError／ApiError の String のみで `#[source]` なしの問題は他クレートの別タスク
- [ ] **[M-05]** Todo 状態の未配線（ServerState / ToolContext / TUI SSE）（要確認）
  - 本番で `TodoManager::update` 呼び出し 0 件（テストからの呼び出しは存在）、`ToolContext::todo_update`
    が None、`todo.updated` を TUI が非処理。`ServerState` 保持＋コールバック配線＋SSE publish
- [ ] **[M-11]** 重要パスのテストカバレッジ空白（database.rs / mcp_oauth.rs は対応済み）
  - storage/database.rs に 3 件、server/mcp_oauth.rs に 14 件追加済み。未対応は
    mcp/oauth.rs・server/oauth.rs・server/pty.rs と provider 統合テストの
    HTTP／ストリーミング／認証フロー（モック OAuth／callback／PtyManager）
- [ ] **[M-15]** 依存ピン留めの残課題（CI locked と集約 6 件は対応済み）
  - 残り 22 件の workspace バイパス＋9 件の未集約依存は段階的集約が必要。特に serde_json は
    `preserve_order` feature の挙動影響を事前評価。厳密ピン拡充（8/61）は複数バージョン併存
    リスクの精査が必要（Cargo.lock コミット＋CI locked で再現性は担保済み）
  - 別課題: 7 クレートが version.workspace を使わず authors/license 欠如
- [ ] **[M-16]** 依存方向の逆転と ADR／アーキテクチャ方針との矛盾
  - charter の依存方向と逆に permission→plugin、tool→permission/plugin、session→
    provider/tool/mcp/lsp、types が sqlx::Type を derive（循環なし、feature 分割は皆無）。ADR
    改訂またはトレイト注入反転、sqlx derive の storage 側移管
- [ ] **[M-17]** 内部実装モジュールの過剰な pub 露出
  - provider/src/lib.rs が 32 個、tool/src/lib.rs が 24 個の実装モジュールを `pub mod`
    で露出しテストがフルパス参照。`pub(crate) mod` 化しファサード経由で公開
- [ ] **[M-20]** 型付き Event Bus の未配線とイベント系統の分散
  - `Bus::new` はテスト内のみで本番構築 0 件、ServerState は `broadcast<String>`、TUI は
    `mpsc<Event>`、session 側 publish は bus=None で実行されない。単一イベント源として配線するか Bus を削除
- [ ] **[M-21]** ServerState 外のグローバル static（CONFIG_STATE／TUI キュー／実行状態は対応済み）
  - 移行済み: CONFIG_STATE、TUI キュー群、SESSION_RUN_STATUS、PROMPT_LOCKS。
    未対応: PROJECT_METADATA（cwd キーで実害限定的、ハンドラ 4 箇所のシグネチャ変更要）、
    TODO_MANAGER / PTY_MANAGER（M-05 / M-20 と連携した配線が必要）
- [ ] **[M-34]** CI ゲートの抜け（all-features・unsafe lint・依存監査・スクリプト）
  - `ci.yml:21-27` がデフォルト feature のみ、lsp feature の 591→899 差分未検証、unsafe
    lint／cargo outdated／Renovate／`scripts/*.rs` が対象外。`extra_gates` と
    `clippy_args` 追加、`unsafe_code = "deny"`、Renovate 設定
- [ ] **[M-37]** async ハンドラ／ツール内の同期ファイル I/O（要確認）
  - 事実確認済み（2026-11-18、grep-io-worker）: opencode-grep は同期ライブラリで tokio 非依存、
    問題の実体は `opencode-tool/src/grep_tool.rs:140-195`（async 内で WalkDir/File::open）と
    `server/routes.rs` の std::fs 混在箇所。レイテンシ悪化は未計測。
    推奨: 走査部分を所有値クロージャへ切り出し `spawn_blocking(...).await` で逃がす
- [ ] **[M-38]** ストリーミング応答のタスク寿命（切断検知は対応済み）
  - `tx.closed()` を select で監視する協調 break を実装済み（AbortHandle は循环的リーク
    リスクで不採択）。未対応は spawn の JoinHandle 保持、初期 `chat_stream()` 呼び出し中の
    切断検知、provider 実装側の上流生成キャンセル（routes.rs 外・未検証）

### 優先度: 低

- [ ] **[L-01]** Event Bus のロック保持中コールバックと publish 順序
  - `bus.rs:57-62` が read guard 保持中にコールバック同期実行（write lock からの呼び出しでデッドロックしうる推測）、
    status.rs は更新前に publish。ロック外実行と更新後 publish
- [ ] **[L-02]** MCP list_changed 通知の非対称な処理
  - `client.rs:622-642` が tools のみフラグを立て resources／prompts は debug ログのみ、
    MAPPING.md に記載なし。フラグ追加または MAPPING.md へ明記
- [ ] **[L-03]** 未配線コードの dead_code 抑制と CHANGELOG の矛盾
  - `prompt.rs:1961-1962` が定義のみ、`markdown/parser.rs` が 363 行全体を抑制、
    CHANGELOG.md:61-63 の「のみ」と矛盾。配線または削除し CHANGELOG を修正
- [ ] **[L-04]** tracing 初期化の未配線（level 反映は対応済み、残りに絞る）
  - `build_filter(level)` で logLevel 反映は実装済み。未対応は `logging.rs` の
    WorkerGuard 早期ドロップ、rolling 未使用、`init_tracing` 呼び出し 0 件、
    CLI が `fmt::init()` で `config.log_level` 未参照。Guard 延伸＋rolling＋呼出配線
- [ ] **[L-07]** 依存 feature の過剰指定
  - `Cargo.toml:49,83` が tokio／hyper に `full`、opencode-lsp が重複指定、plugin
    の使用は限定的（ビルド面増大は推測）。 使用中の feature のみに絞る
- [ ] **[L-08]** 公開 API ドキュメント欠落（ADR-0007 既知の先送り）（要確認）
  - 5,604 件を見送る ADR-0007 の一方、types／core が `//!` なし、19 クレートの lib.rs
    先頭もなし（「矛盾」は解釈次第）。段階対象として types／core から `///`／`//!` を付与し ADR 進捗表を更新
- [ ] **[L-09]** LiteRT プロバイダの未実装 FFI の設定面への露出
  - `litert.rs:60-72` が TODO のままエラー、bootstrap が `LITERT_MODEL_PATH` で登録しモデルを広告、
    MAPPING.md に記載なし。feature gate するか MAPPING.md へ追加
- [ ] **[L-14]** 巨大 JSON 処理とメッセージ履歴の全件アロケーション
  - `routes.rs:1103-1118` が全件 collect、`:1316-1317` が全履歴 clone（O(n) は推測）。
    ページネーション／差分取得と Arc による copy-on-write

### 優先度: 高（残タスク調査で追加）

- [ ] **[P-01]** API パスの `/api` prefix 不一致（parity 構造問題）
  - upstream の HTTP パス 126 種はすべて `/api/...`（例 `packages/protocol/src/groups/agent.ts:10`
    の `/api/agent`）。Rust は `crates/opencode-server/src/routes.rs:49`
    で `/agent` 等を直置きし、
    `server.rs:932` にも prefix 層なし。公式 client をそのまま接続できない。実害は Rust 側に
    JS client が無いため未確認だが、G-01 の generate 接続の前提となる。範囲と互換方針を ADR 化
- [ ] **[P-02]** `/experimental/generate` の契約が upstream と完全に不一致
  - Rust は `{prompt?, context?}` → `{operation, status, message, ...}` の placeholder
    (`routes.rs:4423,4440,4462`)。upstream は `/api/experimental/generate/text` /
    `/image` / `/decisions` に実 handler（`packages/server/src/handlers/generate.ts:20-63`）と
    契約（`packages/protocol/src/groups/generate.ts:25-73`）あり。M-06 の具体化。
    text/image/decisions を実装し、一覧・有効/無効 (`routes.rs:4411-4421`) も実処理へ

### 優先度: 中（残タスク調査で追加）

- [ ] **[P-03]** 未配線の設定フィールド群
  - `Config.keybinds` (`opencode-config/src/schema.rs:13`) が TUI の `KeybindRegistry::new()`
    (`opencode-tui/src/context/keybind.rs:147-211`) へ一切渡されずハードコード固定。
    同一ファイルの `snapshot` / `autoupdate` / `enterprise` / `experimental` の 4 フィールドと、
    `ExperimentalConfig` の 5 サブフィールド (`schema.rs:1175-1188`) も runtime 参照 0 件。
    配線するか、未対応をドキュメントへ明記
- [ ] **[P-04]** interleaved thinking の正規化が no-op
  - `opencode-provider/src/transform.rs:303-308` の
    `normalize_interleaved_field` は空本体だが
    呼び出し元 `:297` は生存。実処理は
    `normalize_messages_with_interleaved_field` (`:312`) に
    あるが re-export のみで呼び出し 0 件。model の `interleaved` 設定を runtime へ反映する配線
- [ ] **[P-05]** MCP protocol negotiation の `Auto` が probe せず常に legacy
  - `McpProtocol::Auto` はコメントで probe + fallback を謳うが
    `revision()` (`opencode-config/src/schema.rs:571-592`) は `Legacy` と同じ
    `"2025-11-25"` を返す。MAPPING.md:96-97 の「protocol 未実装」は stale だが、
    Auto の実挙動は未実装。probe 実装またはドキュメント修正
- [ ] **[P-06]** upstream 新規設定キー `timezone` / `policies` / `worktree` が未対応
  - upstream `packages/schema/src/config.ts:55,61,109`
    （追加 2026-09〜10、MAPPING baseline より後）。
    Rust `Config` (`opencode-config/src/schema.rs:4-124`) に同名字段なし。
    `update` / `commands` / `plugins` の複数形キー (`config.ts:39,94,106`) も単数形フィールドと
    不一致で捨てられる可能性（要 Runtime Proof）
- [ ] **[P-07]** upstream 2026-08〜10 追加グループの parity ギャップ
  - artifacts (G-05)、persistent-pty (G-06)、websearch/rpc/migration/form/media (G-08)、
    v2 core credential/integration/location (G-09)、native ACP (G-11)、JS SDK (G-12)。
    出典は `docs/investigation-final-report.md` と `.cache/investigation/round3-upstream.md`。
    優先度判断は未実施。voice (G-07) は外部送信を伴うため採用には別途合意が必要

### 優先度: 低（残タスク調査で追加）

- [ ] **[P-08]** ADR-0006 の前提ファイル名が実在しない
  - `docs/adr/adr-0006-codemode-async-deferred.md:35` が参照する
    `packages/codemode/src/async.ts` / `scheduler.ts` / `vm.ts` は本 checkout の HEAD・
    `origin/dev`・全 ref に存在せず、実体は `interpreter/runner.ts:142` /
    `promises.ts:19` / `interpreter.ts:419` の job queue + Promise reaction 基盤。
    判断（同期維持）は upstream 進展で再検証対象。リンクと事実関係を修正

## 完了

- [x] `todo.md` をプロジェクト内へ配置（2026-11-17）
- [x] プロジェクト全体クロスレビュー 5 回＋第 6 回統合（2026-11-17）
- [x] 残タスク・機能追加案の調査 5 回＋第 6 回重複吸収（2026-11-17）
- [x] **[L-12]** read ツールの `limit=0` 時に「0 lines read」を返すよう修正（2026-11-17）
- [x] **[M-07]** Bedrock 署名の HeaderValue パースを `map_err(...)?` でエラー伝播に修正（2026-11-17）
- [x] **[M-28]** `LineTrimmedReplacer` の範囲外インデックスを早期 return で回避（2026-11-17）
- [x] **[M-29]** `apply_patch` の行番号オーバーフローを `InvalidArguments` としてエラー化（2026-11-17）
- [x] **[M-30]** メッセージ順序に `rowid ASC` タイブレークを追加しキューと統一（2026-11-17）
- [x] **[M-31]** ID 生成を単一 Mutex + 12bit カウンタに変更し timestamp 破損を防止（2026-11-17）
- [x] **[M-32]** README のバイナリ名を `opencoderust` に修正（2026-11-17）
- [x] **[H-12]** 認証トークンのログ出力をマスク（先头4文字＋長さ）に変更（2026-11-17）
- [x] **[M-24]** 検索処理の WalkDir / ファイル読み取りエラーを伝播（2026-11-17）
- [x] **[M-26]** auth.json 由来の環境変数書き換えにキー検証を追加（2026-11-17）
- [x] **[M-25]** TUI 許可応答の送信失敗をトーストで通知（2026-11-18）
- [x] **[M-14]** サーバ起動時の認証警告を実態（ランダム Bearer / NO_AUTH）に修正（2026-11-18）
- [x] **[L-10]** POST /log の service/message/extra をサニタイズ（2026-11-18）
- [x] **[L-06]** GET/PATCH /config の provider api_key をマスク（2026-11-18）
- [x] **[L-13]** RegExp lastIndex を i64 で保持し負数を JS と同じく保持（2026-11-18）
- [x] **[L-05]** 全クレートへ `[lints] workspace = true` を継承（2026-11-18、lints-worker）
- [x] **[L-11]** レガシー設定マイグレーションで `.bak` 退避を追加（2026-11-18、migration-worker）
- [x] **[M-21/P0]** CONFIG_STATE を ServerState.config へ移行（2026-11-18、state-worker）
- [x] **[M-35]** file_index 検索を Top-K ヒープへ（2026-11-18、index-worker）
- [x] **[M-36]** watch debounce を leading-edge 合体で実装（2026-11-18、watch-worker）
- [x] **[M-21/P0-②]** TUI キュー群を ServerState フィールドへ移行（2026-11-18、queue-worker）
- [x] **[M-27]** MCP `read_resource` に catalog timeout を適用（2026-11-18、mcp-timeout-worker）
- [x] **[L-04 一部]** `build_filter(level)` で logLevel を反映（2026-11-18、logging-worker）
- [x] **[H-09]** LSP の実 timeout・pending 削除・子プロセス回収を実装（2026-11-18、lsp-worker）
- [x] **[H-06]** LLM リトライ上限と 300s ストリーム timeout を適用（2026-11-18、llm-timeout-worker）
- [x] **[M-22]** ALIGNMENT を 974 件／36.27% へ更新（2026-11-18、docs-worker）
- [x] **[H-05]** prompt タスクの AbortHandle 登録と cancel/Idle 配線（2026-11-18、abort-worker）
- [x] **[H-10 一部]** マイグレーションを単一トランザクション化＋metadata 判定を PRAGMA 化（2026-11-18、storage-worker）
- [x] **[M-10]** local-llm サブプロセスを async 化＋timeout/kill（2026-11-18、local-llm-worker）
- [x] **[H-02]** パス判定を canonicalize + コンポーネント比較へ変更（2026-11-18、path-worker）
- [x] **[M-13]** 動的実行検出時は常に承認要求へフォールバック（2026-11-18、bash-worker）
- [x] **[M-04 一部]** CommandError で source チェーン保持（2026-11-18、command-error-worker）
- [x] **[H-07]** マイグレーションに PRAGMA user_version を記録（2026-11-18、migration-version-worker）
- [x] **[M-12]** 未信頼ツール結果を変換点で system-reminder ラップ（2026-11-18、prompt-wrap-worker）
- [x] **[M-09]** opencode.jsonc の認証キーを env 参照へ（2026-11-18、config-key-worker）
- [x] **[H-14]** 認証情報のエラーを try_* API と状態アクセサで明示
  （2026-11-18、auth-error-worker）
- [x] **[M-19]** compaction 永続化失敗を伝播、TodoManager は warn 明記
  （2026-11-18、persist-worker）
- [x] **[M-06]** /experimental/* 4  endpoint を 501 化
  （2026-11-18、placeholder-worker + 親）
- [x] **[H-04 一部]** npx 依存 2 件を Exact 固定（2026-11-18、mcp-pin-worker）
- [x] **[M-18]** SessionOps の静的ディスパッチ意図を明記（2026-11-18、sessionops-worker）
- [x] **[M-33]** ADR の date と TERMUX_BUILD verified を OKF 準拠へ（2026-11-18、okf-worker）
- [x] **[M-08]** 未知キーと未定義 env を警告化（値は非記録）（2026-11-18、config-key2-worker）
- [x] **[M-23]** request/session id の構造化スパンを導入（2026-11-18、span-worker）
- [x] **[H-08]** plugin subprocess の JoinHandle abort とロック timeout（2026-11-18、plugin-lock-worker）
- [x] **[M-01]** fail-open 因果は否定、root とテストで明記（2026-11-18、permission-worker）
- [x] **[M-15 一部]** CI locked 明示＋workspace 集約 6 件（2026-11-18、deps-worker）
- [x] **[H-14]** 認証永続化失敗を HTTP 500/400 へ反映（2026-11-18、auth-wire-worker）
- [x] **[M-34 一部]** workspace lints に unsafe_code = deny（2026-11-18、unsafe-lint-worker）
- [x] **[M-21 一部]** SESSION_RUN_STATUS / PROMPT_LOCKS を state 化（2026-11-18、static-migrate-worker）
- [x] **[M-11 一部]** database.rs に rollback/commit/分離テスト追加（2026-11-18、db-test-worker）
- [x] **[H-07]** Storage::migrate を schema.rs の ALL_MIGRATIONS へ統一（2026-11-18、storage-path-worker）
- [x] **[H-11]** promote 失敗時 failed 化と assistant へ queue_id 付与（2026-11-18、queue-idem-worker）
- [x] **[M-33]** ADR の status/generated と TERMUX_BUILD title を OKF 準拠化
  （2026-11-18、okf2-worker）
- [x] **[H-10]** replace_for_session で delete+create を原子化＋server.rs へ配線
  （2026-11-18、atomic-helper-worker + 親）
- [x] **[M-38]** SSE 切断を tx.closed() で検知し早期停止
  （2026-11-18、stream-cancel-worker）
- [x] **[M-11 一部]** mcp_oauth.rs に 14 テスト追加（2026-11-18、oauth-test-worker）

## メモ

- グローバルTODO: `~/.config/opencode/todo.md`（複数プロジェクト横断のタスク用）。
- クロスレビュー方法: 5 回 × 4 観点 = 20 レビュア（read-only）＋第 6 回で重複排除とコード照合（6 バリデータ）。95 件 → 66
  グループ（High 14 / Medium 38 / Low 14）、false-positive／unconfirmed 0 件、「要確認」6 件。
- 妥当性検証で修正した主張: executor の Ask→Deny 変換（M-01）、`session/todo.rs:186`
  のテスト呼び出し（M-05）、`external_directory.rs:39` からの `is_within_project` 呼び出し（H-02）、
  plugin-host.ts の実パスは `host/`（H-03）。
- 秘密情報は値を記録しない。`.env:1` の PAT は未追跡・履歴なしで、有効性と漏洩の有無は未確認。
- 詳細な根拠・検証ログ: `docs/cross-review-report-2026-11-17.md`。
- 残タスク調査の詳細: `docs/investigation-final-report.md`（延べ 10 エージェント、
  重複吸収後に P-01〜P-08 を追加）。cross-review の H/M/L と重複する項目
  （experimental placeholder・MCP list_changed・未配線コード・LiteRT・CI ゲート・
  ドキュメント数値）は既存項目を優先し、P-* は新規発見のみを記録した。
- M-21 調査（2026-11-18、static-auditor）: ServerState 外 static 10 種を洗い出し。
  P0 は CONFIG_STATE と TUI キュー群の state 化（CONFIG_STATE は後始動インスタンスが
  設定を上書きする共有バグ）、P1 は SESSION_RUN_STATUS/PROMPT_LOCKS・PTY_MANAGER・
  TODO_MANAGER（M-20 と連携）、MODEL_VARIANT_LOOKUP は不変キャッシュで移行不要。
- ベースライン検証（2026-11-17）: `cargo check --all-targets` /
  `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace --all-targets` すべて exit 0。レビューエージェントはコードを変更していない。
