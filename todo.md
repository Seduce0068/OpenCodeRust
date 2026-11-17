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
- [ ] **[H-02]** パス正規化欠如による project 外・traversal パスの誤判定
  - `tool.rs:666-673` が文字列 `starts_with` で判定し `..` や兄弟ディレクトリを project 内と誤判定、
    `read.rs:490-495` の canonicalize と不整合。両方を `Path::canonicalize`
    後にコンポーネント単位で比較し、失敗時は external 扱い
- [ ] **[H-03]** プラグイン subprocess bridge のサプライチェーン／信頼境界
  - `loader.rs:334-349` が未指定 npm spec に `*`、`:268-313` で install 実行、
    plugin-host.ts:403 が検証なし import、`$` ヘルパ／認証ヘッダ渡し fetch／Deno `--allow-all`。
    厳密バージョン＋lockfile＋`--ignore-scripts`、許可リスト化と信頼モデル ADR 化
- [ ] **[H-04]** MCP 実行時依存（`npx -y`）のバージョン固定・完全性検証欠如
  - `opencode.jsonc:99-145` の 4 件が未固定、`transport.rs:40` がそのまま実行、cargo audit は
    Rust のみ。Exact バージョン固定＋lockfile＋CI の npm スキャン対象化
- [ ] **[H-05]** abort ハンドラのキャンセル未実装と孤児タスク
  - `routes.rs:2121-2141` が存在確認のみで `aborted:true` を返し、cancel 呼び出し 0 件・spawn
    ハンドル未保持・ADR-0005 と矛盾。`ServerState` に session_id → AbortHandle レジストリを追加し
    cancel／Idle 更新を配線
- [ ] **[H-06]** LLM リクエストのタイムアウト欠如とリトライ上限未適用
  - `llm.rs:309-357` が無上限ループ、`RetryState::max_attempts` 未使用、adapter に timeout
    なし、`retry.rs` の delay が非 saturating。`.timeout()` と試行上限を強制し `saturating_mul`
    で計算
- [ ] **[H-07]** マイグレーションのバージョン管理・原子性・経路統一の欠如
  - `database.rs:100-120` が単純ループでバージョン記録なし、metadata ALTER をエラー文字列で冪等判定、
    複数文がトランザクション外、`Storage::migrate()` が別スキーマ経路。`sqlx::migrate!`／`user_version`
    ＋単一トランザクション＋経路統一＋runbook 化
- [ ] **[H-08]** プラグイン subprocess のロック保持中 await によるデッドロック
  - `client.rs:288` の rpc_lock ガードを spawn タスクへ移し `:301` で stdout ロック保持、`:394`
    のタイムアウト後も JoinHandle 未保持で `:428` が永久ブロック。AbortHandle を保持して abort するか
    rpc_lock 保持区間を短縮
- [ ] **[H-09]** LSP リクエストの無限待機とリソースリーク
  - `lib.rs:280-301` が stdin ロック保持中に `rx.await`、sender がドロップされず実タイムアウトなし、
    Child／JoinHandle 未保持（`docs/opencode-lsp.md:35` と矛盾）。`tokio::time::timeout` と
    pending 削除、Drop 時 kill／abort
- [ ] **[H-10]** セッション永続化のトランザクション境界と並行制御の欠如（要確認）
  - `server.rs:553-588` がロック解放後に非アトミック書き出し、古い snapshot の遅延 commit で更新喪失は推測、
    delete＋1 件ずつ create が途中失敗で欠損。単一ライタの persist キュー＋`Database::transaction` で原子化
- [ ] **[H-11]** キュー再実行による assistant レスポンス重複と冪等性欠如
  - promoted 更新失敗はログのみでキューが processing のまま残り起動時に再実行、assistant 側に冪等性なし（課金重複は推測）。
    `queue_id` を assistant にも相関 ID 保持するか promoted 更新を同一トランザクション化
- [ ] **[H-12]** Bearer トークンの平文ログ出力
  - `server.rs:925-926,958-960` が 32 バイト乱数トークンを全桁 `tracing::info!` 出力、
    ファイル出力と組み合わさりディスク永続化。ADR-0001 を見直しマスク出力またはローカル TTY のみに制限
- [ ] **[H-13]** TUI イベントルートのブロッキング HTTP と全件再マッピング
  - `app.rs:3186-3215` が 16ms ループから `reqwest::blocking`（timeout
    30s）で同期取得し全件再マッピング。非同期 `reqwest::Client`＋`tokio::spawn` とメッセージ ID 単位の差分取得へ変更
- [ ] **[H-14]** 認証情報ファイルの読み込み・永続化エラーの飲み込み
  - `provider/auth.rs:77-87` が失敗時に空 credentials を返し、set／remove は `()` で persist
    失敗を warn のみ。戻り値を `anyhow::Result` 化し呼び出し側へ明示

### 優先度: 中

- [ ] **[M-01]** 権限チェックの fail-open とデフォルト全許可（要確認）
  - `tool.rs:323,375-381` が ask:None で `Ok(())`、`prompt.rs:1431-1435` が no-op、
    `opencode.jsonc:62-64` が `"*": "allow"`。executor は Ask→Deny 変換（テストあり）で因果は否定、
    影響は executor／callback 未指定経路に限定。fail-closed 化と PermissionBroker 配線
- [ ] **[M-02]** 共有型の重複定義と serde_json 往復による変換
  - session が types に依存せず `SessionMessage` 等を重複定義、`server.rs:569-574` が serde
    往復（フィールド差による欠落は推測）。型を `opencode-types` へ集約し `From` 変換へ置換
- [ ] **[M-03]** 未接続クレートと責務重複（PTY）による責務分離の形骸化
  - pty／util／watcher／local-llm／codemode の 5 クレートが無依存（codemode は ADR-0006
    で既知の先送り）、server が portable-pty を直接依存し同名 PtyManager を再実装。配線または member 外し、
    server を opencode-pty 利用へ
- [ ] **[M-04]** ライブラリ公開 API の `anyhow::Result` とエラー型の原因損失
  - `command/lib.rs:119,173` 等が公開 API で anyhow を返し charter.md:63 と矛盾、
    ToolError／ApiError が String のみで `#[source]` なし。thiserror
    ドメインエラー化とソース保持バリアント追加
- [ ] **[M-05]** Todo 状態の未配線（ServerState / ToolContext / TUI SSE）（要確認）
  - 本番で `TodoManager::update` 呼び出し 0 件（テストからの呼び出しは存在）、`ToolContext::todo_update`
    が None、`todo.updated` を TUI が非処理。`ServerState` 保持＋コールバック配線＋SSE publish
- [ ] **[M-06]** placeholder エンドポイントの未実装隠蔽
  - `/experimental/analyze` と `/generate`（`routes.rs:4382-4384,4440-4454`）が
    `"accepted"` を返すが実処理なし、MAPPING.md に記載なし。実装するか 501 を返し ALIGNMENT／MAPPING に記録
- [ ] **[M-08]** 未知設定キーのサイレント破棄と環境変数展開の未検証
  - `strictPermissionChecks`／`gcBufferMb`／`references.gemini` が Config
    にフィールドなし（docs では起動を止めない意図で明記）、`{env:VAR}` 未定義が空文字化。キー追加／起動時警告と必須項目の非空検証
- [ ] **[M-09]** 認証情報のリテラル設定による環境変数上書き不能
  - `opencode.jsonc:6` のプレースホルダ値が `{env:VAR}` 展開を通らず最初の非空値として採用され
    `ANTHROPIC_API_KEY` が効かない（値自体は秘密ではない）。`{env:ANTHROPIC_API_KEY}` 化または auth
    store へ委譲
- [ ] **[M-10]** ローカル LLM サブプロセスのタイムアウト・kill 欠落
  - `local-llm/main.rs:266-290` が同期 `Command::output()` で ollama を待ち、MCP ループも同期。
    async 化＋`tokio::time::timeout`＋タイムアウト時 kill
- [ ] **[M-11]** 重要パス（認証・PTY・DB・provider）のテストカバレッジ空白
  - mcp/oauth.rs（561 行）・server/oauth.rs・server/pty.rs・storage/database.rs がテスト 0
    件、provider 統合テストに HTTP／ストリーミング／認証フローなし（ADR-0008 の改善対象外だが明示的先送りではない）。モック
    OAuth／callback 分岐／PtyManager／DB in_memory を網羅
- [ ] **[M-12]** 未信頼ツール結果への system-reminder ラップ欠如
  - `prompt.rs:3511` が bash 出力を素のまま `add_tool_result` へ、MCP リソース（:449）や
    AGENTS.md 由来（:612）はラップ済みで不整合（被害はモデル依存の推測）。全ツール結果を `system_reminder` でラップ
- [ ] **[M-13]** 動的実行コマンドの権限抽出漏れ（eval / source / コマンド置換）
  - `bash.rs:338-450` が tree-sitter の command ノードのみ対象、`eval "rm -rf /"`
    の内部コマンドを抽出できず `eval *` が always allow になりうる（無承認実行は推測）。動的実行を検出したら常に承認／deny
- [ ] **[M-14]** サーバ認証の警告文と実装の乖離
  - `main.rs:1895-1896` がパスワード未設定で unsecured と警告するが参照はこの 2 行のみ、実認証はランダム Bearer
    トークン。警告文を実態へ修正するかパスワードを配線
- [ ] **[M-15]** Rust 依存バージョンのピン留め不足と workspace 依存のバイパス
  - 厳密ピンは 8 件のみ、各クレートが serde／axum 等で workspace 定義をバイパスし AGENTS.md §4
    と矛盾（Cargo.lock はコミット済み）。workspace 集約＋厳密ピン＋CI `--locked` 必須化
- [ ] **[M-16]** 依存方向の逆転と ADR／アーキテクチャ方針との矛盾
  - charter の依存方向と逆に permission→plugin、tool→permission/plugin、session→
    provider/tool/mcp/lsp、types が sqlx::Type を derive（循環なし、feature 分割は皆無）。ADR
    改訂またはトレイト注入反転、sqlx derive の storage 側移管
- [ ] **[M-17]** 内部実装モジュールの過剰な pub 露出
  - provider/src/lib.rs が 32 個、tool/src/lib.rs が 24 個の実装モジュールを `pub mod`
    で露出しテストがフルパス参照。`pub(crate) mod` 化しファサード経由で公開
- [ ] **[M-18]** `SessionOps` トレイトの object safety・Future Send 保証欠如
  - `compaction.rs:156-170` が `#[allow(async_fn_in_trait)]` 付き async fn を 3 個、
    dyn 化不可・Future Send 未保証で AGENTS.md §4 と不整合。`#[async_trait]`
    付与または静的ディスパッチの意図を明記
- [ ] **[M-19]** 永続化結果の握りつぶしと成功契約の矛盾
  - `compaction.rs:456,566,574,606,625,770` や `todo.rs:50-158` が
    `let _ = ops.update_message(...)`。失敗を `?` で伝播させるか `warn!` 記録＋契約を doc comment
    に明記
- [ ] **[M-20]** 型付き Event Bus の未配線とイベント系統の分散
  - `Bus::new` はテスト内のみで本番構築 0 件、ServerState は `broadcast<String>`、TUI は
    `mpsc<Event>`、session 側 publish は bus=None で実行されない。単一イベント源として配線するか Bus を削除
- [ ] **[M-21]** ServerState 外のグローバル static 状態
  - `routes.rs:280-283` の SESSION_RUN_STATUS／PROMPT_LOCKS、
    CONFIG_STATE／PROJECT_METADATA／キューが static Lazy でプロセス共有。ServerState フィールドへ移し
    `Arc<ServerState>` 経由で参照
- [ ] **[M-22]** 現況ドキュメントのテスト件数・カバレッジ数値ドリフト
  - ALIGNMENT.md:47,98 が 813 件／34.14%、ADR-0008 が 946 件、CHANGELOG が 946
    件／35.89%（更新漏れは推測）。ALIGNMENT を最終値へ更新し ADR-0008 をリンク
- [ ] **[M-23]** 構造化ログ・スパンの欠如
  - charter.md:64 が構造化ログを求めるがスパン生成コード 0 件、HTTP は TraceLayer のみ、補間文字列ログが多い。
    request／session id を伝播させるスパン導入とフィールド化
- [ ] **[M-24]** 検索処理におけるエラーの飲み込みと不完全結果
  - `grep/search.rs:78` が Result を破棄、`:104` が `filter_map(|e| e.ok())` で WalkDir
    エラーを捨て `Ok` を返し docs:36 と矛盾。warn 記録または Err 返却
- [ ] **[M-25]** TUI 許可プロンプトの送信エラー通知欠如
  - `app.rs:333-360` の全分岐が `let _ = client.reply_permission(...)`、question
    応答はトースト表示があり一貫性なし。`if let Err` でトースト表示し再送可能な状態を保持
- [ ] **[M-26]** wellknown 定数のハードコードと外部ファイルによる環境変数書き換え
  - `wellknown.rs:10-11` が timeout／TTL を const 化、`:46-51` の `~/.local/share`
    が未展開、`:103-105` が auth.json 由来 key を無検証 `set_var`（後段への影響は推測）。Config
    化＋絶対パス＋許可リスト
- [ ] **[M-27]** MCP 操作のタイムアウト未適用
  - `client.rs:734-743` の `read_resource` がタイムアウトなしの `send_request`、receive
    ループは無限で stdio `read_line()` がブロック（docs:40-54 と矛盾）。
    `send_request_with_progress_timeout` へ切替
- [ ] **[M-30]** メッセージ順序のタイブレーク欠如
  - `repository.rs:384-387` が `ORDER BY created_at ASC`
    のみ（ミリ秒精度）でキュー（`:661,:731`）は rowid タイブレークと不整合。同じ順序規則へ統一
- [ ] **[M-31]** ID 生成の順序キー整合性
  - `core/id.rs:35-36` が timestamp と counter を別クリティカルセクションで更新し同一ミリ秒で衝突しうる（推測）、
    counter≥0x1000 で繰り上がり、uuid v4 と形式混在。単一 Mutex＋12bit マスク＋形式統一
- [ ] **[M-32]** README のバイナリ名と実装の乖離
  - `README.md:145` が `binary: opencode` と記載するが実装は `opencoderust`（同一 README
    の他行とも自己矛盾）。`binary: opencoderust` へ修正
- [ ] **[M-33]** OKF フロントマター／文書規約の乖離
  - docs 配下 35 個・crates 22 ディレクトリに index.md が 0 件（docs/README.md が Index で兼ねる）、
    adr-0001 等 4 件の date がオフセットなし、TERMUX_BUILD.md の verified がスカラー。規約に合わせて修正
- [ ] **[M-34]** CI ゲートの抜け（all-features・unsafe lint・依存監査・スクリプト）
  - `ci.yml:21-27` がデフォルト feature のみ、lsp feature の 591→899 差分未検証、unsafe
    lint／cargo outdated／Renovate／`scripts/*.rs` が対象外。`extra_gates` と
    `clippy_args` 追加、`unsafe_code = "deny"`、Renovate 設定
- [ ] **[M-35]** TUI file_index の同期走査と過剰アロケーション
  - `file_index.rs:31-67` が 5 秒毎に WalkDir 同期走査、`:86-100` が全エントリ clone＋sort 後
    truncate（O(n log n) は推測）。Top-K ヒープとバックグラウンド refresh
- [ ] **[M-36]** watch の debounce 仕様と実装の矛盾
  - `docs/opencode-watcher.md:29` が 100ms と仕様化するが `lib.rs:140-147` が到達毎に即 send、
    `debounce_ms` は poll_interval に渡すだけ。パス単位の最終時刻で送出を遅延
- [ ] **[M-37]** async ハンドラ／ツール内の同期ファイル I/O による tokio ワーカー占有（要確認）
  - `routes.rs:3431,3532-3601` と `grep_tool.rs:140-170` が async 内で同期走査（`:2546` は
    tokio::fs と混在、レイテンシ悪化は未計測）。`tokio::fs` と `spawn_blocking` へ移行
- [ ] **[M-38]** ストリーミング応答のタスク寿命・キャンセル安全性（要確認）
  - `routes.rs:1365` が spawn するがハンドル未保持、`:1295` が `let _ = tx.send`
    で切断後も処理継続（切断頻度・課金規模は未計測）。`tx.closed()` を select で監視し AbortHandle を紐付け

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
- [ ] **[L-04]** tracing 初期化の未配線と logLevel 未反映
  - `logging.rs:390` で WorkerGuard が早期ドロップ、rolling 未使用、`init_tracing` 呼び出し 0 件、
    CLI は `fmt::init()` で `config.log_level` 未参照（現状の実行時影響なし）。Guard
    延伸＋rolling＋logLevel 配線
- [ ] **[L-05]** workspace lints の継承漏れ
  - `Cargo.toml:28-34` で deny を定義するが `[lints] workspace = true` は
    opencode-codemode のみ（CI の `-D warnings` で一部カバー）。全 22 クレートへ追加
- [ ] **[L-06]** GET /config による api_key の平文漏洩
  - `routes.rs:2783-2786` が AppConfig をそのまま返し、api_key は `skip_serializing_if`
    のみ（現状値はプレースホルダで実際の秘密ではない）。マスク済み DTO を返す
- [ ] **[L-07]** 依存 feature の過剰指定
  - `Cargo.toml:49,83` が tokio／hyper に `full`、opencode-lsp が重複指定、plugin
    の使用は限定的（ビルド面増大は推測）。 使用中の feature のみに絞る
- [ ] **[L-08]** 公開 API ドキュメント欠落（ADR-0007 既知の先送り）（要確認）
  - 5,604 件を見送る ADR-0007 の一方、types／core が `//!` なし、19 クレートの lib.rs
    先頭もなし（「矛盾」は解釈次第）。段階対象として types／core から `///`／`//!` を付与し ADR 進捗表を更新
- [ ] **[L-09]** LiteRT プロバイダの未実装 FFI の設定面への露出
  - `litert.rs:60-72` が TODO のままエラー、bootstrap が `LITERT_MODEL_PATH` で登録しモデルを広告、
    MAPPING.md に記載なし。feature gate するか MAPPING.md へ追加
- [ ] **[L-10]** POST /log 入力の未検証
  - `routes.rs:4654-4684` が level のみ検証し service／message／extra
    を無検証で出力（改行によるログ汚染は推測）。改行除去・長さ制限・service 許可リスト化
- [ ] **[L-11]** 設定マイグレーションのロールバック手段欠如
  - `loader.rs:391-406` がレガシー TOML を書き込んだ直後に元ファイルを削除、バックアップなし。`.bak`
    退避または削除を任意手順化
- [ ] **[L-13]** codemode の RegExp `lastIndex` 型変換による parity 欠如
  - `interpreter.rs:6120-6126` が負数を 0 にサチュレートして JS と parity を欠く（「usize::MAX
    付近」という主張は技術的に誤りで修正済み）。符号付き型で保持
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
- ベースライン検証（2026-11-17）: `cargo check --all-targets` /
  `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace --all-targets` すべて exit 0。レビューエージェントはコードを変更していない。
