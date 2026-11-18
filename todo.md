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
  - 関連（2026-11-19、combo-sv01/sv06）: `routes.rs:2858-2882` の GET /config マスキングは
    provider 直下の api_key のみで models.*.apiKey / oauth.client_secret が露出しうる
    （likely）。`config/loader.rs:383-408` のレガシー移行は {env:...} 展開済み秘密を
    umask 依存の権限で書き出す（confirmed）。あわせて対処すること
- [ ] **[H-03]** プラグイン subprocess bridge のサプライチェーン／信頼境界
  - `loader.rs:334-349` が未指定 npm spec に `*`、`:268-313` で install 実行、
    plugin-host.ts:403 が検証なし import、`$` ヘルパ／認証ヘッダ渡し fetch／Deno `--allow-all`。
    厳密バージョン＋lockfile＋`--ignore-scripts`、許可リスト化と信頼モデル ADR 化
- [ ] **[H-04]** MCP 実行時依存の完全性検証（2 件は固定済み、残りに絞る）
  - `chrome-devtools-mcp` / `tavily-mcp` は Exact 固定済み。未対応は
    `@modelcontextprotocol/server-devtools` と `server-playwright` が公式 npm で
    404（パッケージ不在）のため固定不能。代替（`@playwright/mcp` 等）の選定、
    lockfile 生成、CI npm スキャン対象化）
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
- [ ] **[M-04]** エラー型の残課題（source 付きバリアント＋ tool/provider 置換済み）
  - command の 3 メソッドと、ToolError/ProviderError の source 付きバリアント追加済み。
    2026-11-19 に opencode-tool の 12 ファイル 33 箇所を `execution_with_source` へ、
    opencode-provider の 20 ファイル 62 箇所を `api_error_with_source` /
    `network_error_with_source` へ置換（メッセージ不変・非対象は据え置き、旧パターン残存 0）。
    残存 `ExecutionError` 17 箇所は下位エラーが無い固定文字列／StatusCode／別バリアント。
    未対応: StreamError(e.to_string()) と InvalidArguments(e.to_string())（serde_json）。
    新バリアントは _enum 非 non_exhaustive_ のため下流列挙には破壊的変更になりうる
- [ ] **[M-05]** Todo 状態の未配線（要確認、配線には他クレート変更が必須）
  - 事実確認済み（2026-11-18、todo-wire-worker）: 本番 update 0 件、todo_update/todo_get
    None。配線には ①session/prompt.rs へコールバック注入 API、②server.rs へ TodoManager
    保持、③routes.rs で broadcast、④TUI の forward_server_event マッピングが必要。
    tool.rs 単独の global static 案は M-21 方針に反するため却下済み
- [ ] **[M-11]** 重要パスのテストカバレッジ空白（多数対応済み）
  - 対応済み: storage/database.rs、server/mcp_oauth.rs・oauth.rs・pty.rs、mcp/oauth.rs・
    protocol.rs、plugin/subprocess/protocol.rs、provider/models.rs・bootstrap.rs、
    watcher、codemode/tools.rs、tui/context/keybind.rs・command.rs、agent/message.rs
    など。未対応は provider 統合テストの HTTP／ストリーミング／認証フロー
    （モック OAuth／callback）
  - 補足: provider/convert.rs は lib.rs から未配線のスタブで、テストを追加しても
    本クレートでは実行されない（2026-11-18 発見、追加は差し戻し済み）
- [ ] **[M-15]** 依存ピン留めの残課題（CI locked と集約 6 件は対応済み）
  - 残り 22 件の workspace バイパス＋9 件の未集約依存は段階的集約が必要。特に serde_json は
    `preserve_order` feature の挙動影響を事前評価。厳密ピン拡充（8/61）は複数バージョン併存
    リスクの精査が必要（Cargo.lock コミット＋CI locked で再現性は担保済み）
  - version/edition/authors/license の workspace 継承は 7 クレートとも完了済み
- [ ] **[M-16]** 依存方向の逆転（sqlx 未使用 derive は削除済み）
  - 未使用の `SessionID` の sqlx::Type derive は削除済み。`SessionStatus` は storage が
    実使用し orphan rule で storage への直接移管は不可（newtype＋From/Into 案は未検証）。
    未対応は charter と逆の permission→plugin、tool→permission/plugin、session→
    provider/tool/mcp/lsp の依存方向（ADR 改訂またはトレイト注入反転）
- [ ] **[M-17]** 内部実装モジュールの pub 露出（7 モジュール縮小済み）
  - 外部フルパス参照ゼロの 7 モジュールを pub(crate) 化済み（ファサード公開維持）。
    残存 14 モジュールは外部参照ありで維持。さらに縮小するには dead_code 解消＋
    ファサード再エクスポート整理（他ファイル変更）が必要
- [ ] **[M-20]** 型付き Event Bus の未配線とイベント系統の分散
  - `Bus::new` はテスト内のみで本番構築 0 件、ServerState は `broadcast<String>`、TUI は
    `mpsc<Event>`、session 側 publish は bus=None で実行されない。単一イベント源として配線するか Bus を削除
  - 補足（2026-11-18、eventbus-tui-worker）: TUI の `EventBus::new()` は受信側を即破棄するが、
    本番で `event_bus.send*` 呼び出しは 0 件。実際の配送は app.rs:102 の別 mpsc チャネルで
    動作しており実害なし。receiver 保持は読む event loop が無くキュー蓄積リスクがあるため
    見送り。真に統合するなら app.rs 改修（EventBus を app のチャネルへ統合、または削除）が必要
  - 調査（2026-11-19、swarm-coding R2-C）: core `Bus` は同期 `Fn` コールバック＋
    broadcast、`publish` は guard 解放後にコールバック実行。本番利用 0 件で実質デッドコード。
    系統は 3 つ（core Bus / TUI `mpsc<Event>` / Server `broadcast<String>`）＋
    PTY `broadcast<Vec<u8>>`。
    Server は発行 10 箇所・購読 2 箇所（SSE）、TUI の実配送は app.rs の別 mpsc。
    方針A（配線）は 5〜7 ファイル・120–300 行、方針B（Bus 削除）は 348 行削除。設計判断待ち。
- [ ] **[M-21]** ServerState 外のグローバル static（ほぼ移行済み）
  - 移行済み: CONFIG_STATE、TUI キュー群、SESSION_RUN_STATUS、PROMPT_LOCKS、
    PROJECT_METADATA、TODO_MANAGER、PTY_MANAGER。
    残存: MODEL_VARIANT_LOOKUP / MCP_OAUTH_MANAGER / PLUGIN_LOADER /
    EXTRA_CORS_WHITELIST（不変データまたは初期化一回の OnceCell／設定関数経由で実害限定的）
- [ ] **[M-34]** CI ゲートの抜け（all-features・unsafe lint・依存監査・スクリプト）
  - `ci.yml:21-27` がデフォルト feature のみ、lsp feature の 591→899 差分未検証、unsafe
    lint／cargo outdated／Renovate／`scripts/*.rs` が対象外。`extra_gates` と
    `clippy_args` 追加、`unsafe_code = "deny"`、Renovate 設定
- [ ] **[M-37]** async ハンドラ内の同期ファイル I/O（tool 側は対応済み）
  - 事実確認済み（2026-11-18、routes-io-worker）: routes.rs の std::fs（3479/3522/
    3623/3644/3688）と git subprocess（3536/3799）、canonicalize 多発（is_within_root）。
    性能改善動機で未計測のため I/O 移行は見送り。推奨: ①find_text/find_files を
    spawn_blocking → ②read_file を tokio::fs → ③list_files → ④git を tokio::process、
    3836 は read guard を落としてから await（要計測）
- [ ] **[M-38]** ストリーミング応答のタスク寿命（切断検知は対応済み）
  - `tx.closed()` の協調 break を実装済み。JoinHandle 保持は再調査（2026-11-18、
    stream-handle-worker）で「tokio の破棄はデタッチ、協調 break で十分、AbortHandle は
    部分永続化の回帰＋循环的参照リスク」のため見送り。残る実ギャップは初期
    `chat_stream()` 確立中の切断検知と provider 側の上流キャンセル（routes.rs 外・未検証）

### 優先度: 低

- [ ] **[L-04]** tracing の残課題（level 反映・CLI 配線は対応済み）
  - `init_tracing` を CLI 起動時に呼び出し `config.logLevel` を実効化済み。
    未対応は `logging.rs` の WorkerGuard 早期ドロップ（API 変更要）と rolling 未使用。
    起動時とハンドラで `load_config` が 2 回走る点は大規模変更のため据え置き
- [ ] **[L-07]** 依存 feature の過剰指定
  - `Cargo.toml:49,83` が tokio／hyper に `full`、opencode-lsp が重複指定、plugin
    の使用は限定的（ビルド面増大は推測）。 使用中の feature のみに絞る
- [ ] **[L-14]** 巨大 JSON 処理とメッセージ履歴の全件アロケ（clone は削除済み）
  - stream_message の冗長な全履歴 clone は削除済み。未対応は list_messages の
    全件 collect（1098-1113、外部契約のためページネーションは変更を伴う）と
    Arc copy-on-write。O(n) の実害は未計測

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

### 優先度: 高（swarm review+discovery 2026-11-19）

- [ ] **[H-19]** MCP OAuth が RFC 8707 resource インジケーターを送信しない
  - `mcp/discovery.rs:38,68` / `oauth.rs:295`。resource をパースするだけで
    認可リクエストへ渡さず confused deputy に無防備（confirmed）
- [ ] **[H-21]** サブセッションが親の disabled_tools を継承しない
  - `agent/executor.rs:439-477,498-500`。tool 引数の agent_name/model で別エージェントを
    選べ、禁止ツールを迂回。深さ上限も無し（confirmed / R1-SV05）
- [ ] **[H-22]** Anthropic SSE の content_block_stop でストリームを終了させる
  - `provider/stream.rs:297-298`。複数ブロックの 2 個目以降が届かない。message_stop のみ
    Done とし、ブロック終了は TextEnd 等へ（confirmed / R1-SV03）
- [ ] **[H-23]** Anthropic ツール入力デルタが常に空 ID で発行される
  - `provider/stream.rs:279-283`。index→id 状態マップを持ち tool_use を対応付ける（confirmed）
- [ ] **[H-25]** LSP の JSON-RPC 読み取りが Content-Length を使わない
  - `lsp/lib.rs:233-259`。lines() で 1 行ずつ読み整形済み JSON や複数メッセージで
    フレーミングが壊れ 30 秒 Timeout。ヘッダー＋read_exact 方式へ（confirmed）

### 優先度: 中（残タスク調査で追加）

- [ ] **[P-03]** 未配線の設定フィールド（MAPPING.md へ明記済み）
  - `Config.keybinds` が TUI の `KeybindRegistry::new()` へ渡されずハードコード固定。
    `snapshot` / `autoupdate` / `enterprise` / `experimental` と
    `ExperimentalConfig` の **6 サブフィールド**
    （現行 schema.rs:1224-1238、旧 todo の「5 件」は古い）も runtime
    参照 0 件。未対応の旨は MAPPING.md の「設定スキーマの追随状況」に明記済み。
    未対応は実際の配線（TUI/CLI 側の改修が必要）
- [ ] **[P-04]** interleaved thinking の runtime 反映（no-op は削除済み）
  - 空本体の `normalize_interleaved_field` と呼び出しを削除済み（挙動不変）。
    未対応は `transform_messages` 経路自体が未呼び出しで、`InterleavedConfig` を渡す
    引数追加＋呼び出し側（bootstrap 等）の改修が必要。model.interleaved → runtime
    capabilities への変換自体は bootstrap.rs で別経路配線済み
- [ ] **[P-05]** MCP protocol Auto の probe（実挙動は明記済み）
  - doc と MAPPING.md を実挙動へ修正済み: Auto は Legacy と同じ `"2025-11-25"`
    を送り、probe/fallback は未実装、新 revision は明示指定が必要。回帰テスト追加済み。
    未対応は probe 実装（opencode-mcp の initialize に retry ロジック追加、外部通信を伴う）
- [ ] **[P-06]** upstream 新規設定キー（timezone 対応、他は明記済み）
  - `timezone` は型が確定（`Option<String>`）し追加済み、回帰テストあり。
    `policies` / `worktree` は構造判明だが runtime 配線が scope 外、`update` / `commands` /
    `plugins` は Rust 側と型不一致（alias すると parse 失敗）のため未対応。
    全キーの型・出典・理由は MAPPING.md に明記済み。未対応はこれら実際の配線
- [ ] **[P-07]** upstream 2026-08〜10 追加グループの parity ギャップ
  - artifacts (G-05)、persistent-pty (G-06)、websearch/rpc/migration/form/media (G-08)、
    v2 core credential/integration/location (G-09)、native ACP (G-11)、JS SDK (G-12)。
    出典は `docs/investigation-final-report.md` と `.cache/investigation/round3-upstream.md`。
    優先度判断は未実施。voice (G-07) は外部送信を伴うため採用には別途合意が必要

### 優先度: 中（swarm review+discovery 2026-11-19）

- [ ] **[M-43]** PTY セッションが Exited/Error に遷移せず child も回収されない
  - `server/pty.rs:124-158,212-223`。EOF 後も status=Running のまま、child.wait() 不在で
    ゾンビ蓄積（confirmed / R1-SV01）
- [ ] **[M-44]** ストレージ復元で summary/metadata が失われ total_cost にトークン数が入る
  - `server.rs:588-627`。`/session/{id}/diff` が空になりコスト表示が壊れる（confirmed）
- [ ] **[M-45]** 未実装ハンドラが 501 ではなく成功レスポンスを返す
  - `routes.rs:2329-2351,2468-2496,3719-3721` 等。execute_shell / lsp status /
    vcs 情報
    等が固定値で success。501 化または実装（confirmed / R1-SV01）
- [ ] **[M-46]** /session/{id}/stream がエージェント・ツールなしの生 chat_stream
  - `routes.rs:1344-1355,1983-2051`。/prompt と応答品質・課金が異なる（confirmed）
- [ ] **[M-47]** PermissionNext::evaluate がパターンを常に "*" で評価
  - `agent/agent.rs:764-800`。個別パス権限が効かない（likely / R1-SV05）
- [ ] **[M-48]** ツール結果から tool_call_id/name/is_error が失われる
  - `agent/message.rs:77-89,134-143`。tool-use 会話の対応が壊れる（confirmed）
- [ ] **[M-49]** retry ヘルパーが未配線で Retry-After も効かない
  - `provider/retry.rs:145-205`。with_retry は定義・re-export のみで呼び出し 0 件、
    delay にヘッダを渡さず固定バックオフ（confirmed / R1-SV03）
- [ ] **[M-50]** 再試行がクレジット枯渇を対象とし retry-after に上限がない
  - `session/retry.rs:40-95`。FreeUsageLimitError が Some を返し無駄な再試行、
    ヘッダ由来遅延がクランプされない（confirmed / R1-SV05）
- [ ] **[M-51]** キーバインド display_thinking / tool_details が未登録
  - `tui/context/keybind.rs:156,213`。参照名と登録名が不一致でトグル到達不能（confirmed）
- [ ] **[M-52]** API bool 応答を unwrap_or(true) で読み失敗を成功と報告
  - `tui/api.rs:404,575,589,600,671`。削除・切断・認証解除の失敗が隠れる（confirmed）
- [ ] **[M-53]** UTF-8 境界を無視したバイトスライスが多バイト文字で panic
  - `tool/read.rs:377-378` / `truncation.rs:50-53`。外部ファイル内容で再現可能な DoS（confirmed）

## 完了

- [x] **[H-16]** NetworkErrorWithSource をリトライ判定へ追加（2026-11-19、swarm-coding R1-R6）
- [x] **[H-17]** env 展開を JSON 文字列として安全に挿入（2026-11-19、swarm-coding R1-R6）
- [x] **[H-18]** MCP initialized を notification 送信へ変更（2026-11-19、swarm-coding R1-R6）
- [x] **[H-20]** FileSystemImpl にパス検証を追加（2026-11-19、swarm-coding R1-R6）
- [x] **[H-15]** multiedit に権限確認（external_directory/edit/file_time_assert/
  with_file_lock）を追加（2026-11-19、swarm-coding R1-R6）
- [x] **[M-54]** apply_patch Move の移送先検証を追加（2026-11-19、swarm-coding R1-R6）
- [x] **[H-24]** WebFetch の SSRF 対策（IP 範囲・DNS 解決・リダイレクト再検証）
  （2026-11-19、swarm-coding R1-R6）
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
- [x] **[M-37]** grep_tool の走査を spawn_blocking へ（2026-11-18、grep-blocking-worker）
- [x] **[L-01]** Event Bus のコールバックをロック解放後に実行（2026-11-18、eventbus-worker）
- [x] **[L-02]** MCP list_changed の非対称を MAPPING.md へ明記
  （2026-11-18、listchanged-worker + 親）
- [x] **[M-15]** 7 クレートの version 等を workspace 継承へ（2026-11-18、workspace-version-worker）
- [x] **[L-04]** init_tracing を CLI へ配線し logLevel を実効化（2026-11-18、tracing-wire-worker）
- [x] **[L-03]** 未配線 dead code 約 400 行と抑制属性を削除（2026-11-18、deadcode-worker）
- [x] **[H-07]** migrate_pool 切り出しで 2 経路を完全統一（2026-11-18、migrate-unify-worker）
- [x] **[M-11 一部]** server/oauth.rs に 7 テスト追加（2026-11-18、oauth2-test-worker）
- [x] **[L-08]** types/core にクレートドキュメントと主要 pub mod の doc（2026-11-18、apidoc-worker）
- [x] **[M-11 一部]** server/pty.rs に 6 テスト追加（2026-11-18、pty-test-worker）
- [x] **[M-11 一部]** mcp/oauth.rs に 8 テスト追加（2026-11-18、mcp-oauth-test-worker）
- [x] **[L-08 第2段]** 残り 12 クレートの lib.rs に //!（2026-11-18、apidoc2-worker）
- [x] **[M-11 一部]** provider models/bootstrap に 11 テスト（2026-11-18、provider-test-worker）
- [x] **[M-04]** 公開 3 メソッドを Result<_, CommandError> 化（2026-11-18、command-api-worker）
- [x] **[L-08 第3段]** missing_docs 276 件を 0 件へ（2026-11-18、apidoc3-worker）
- [x] **[L-09]** 未実装 LiteRT を登録せず警告（2026-11-18、litert-worker）
- [x] **[M-16 一部]** 未使用の SessionID の sqlx derive を削除（2026-11-18、sqlx-derive-worker）
- [x] **[M-11 一部]** watcher に debounce 対照テスト 2 件（2026-11-18、watcher-test-worker）
- [x] **[M-11 一部]** codemode/tools.rs に 4 テスト（2026-11-18、codemode-test-worker）
- [x] **[L-14]** stream_message の冗長な全履歴 clone を削除（2026-11-18、routes-io-worker）
- [x] **[M-11 一部]** mcp/protocol.rs に 17 テスト＋camelCase 修正
  （2026-11-18、mcp-protocol-test-worker + 親）
- [x] **[M-11 一部]** plugin/subprocess/protocol.rs に 8 テスト
  （2026-11-18、plugin-protocol-test-worker）
- [x] **[M-11 一部]** tui/context/keybind.rs に 17 テスト（2026-11-18、keybind-test-worker）
- [x] **[M-11 一部]** tui/command.rs に 10 テスト（2026-11-18、tui-command-test-worker）
- [x] **[M-11 一部]** agent/message.rs に 7 テスト（2026-11-18、agent-message-test-worker）
- [x] **[M-11 一部]** tui/event.rs に 12 テスト（2026-11-18、event-test-worker）
- [x] **[M-17 一部]** 7 モジュールを pub(crate) 化（2026-11-18、pubmod-worker）
- [x] **[M-11 一部]** tui/session_context.rs に 11 テスト（2026-11-18、session-ctx-test-worker）
- [x] **[M-04 一部]** source 付きエラーバリアントを追加（2026-11-18、error-source-worker）
- [x] **[M-21]** PROJECT_METADATA を ServerState へ移行（2026-11-18、project-meta-worker）
- [x] **[M-11 一部]** storage/schema.rs に 7 テスト（2026-11-18、schema-test-worker）
- [x] **[M-11 一部]** mcp/tool.rs に 8 テスト（2026-11-18、mcp-tool-test-worker）
- [x] **[P-03]** 未配線設定フィールドを MAPPING.md へ明記（2026-11-18、p03-worker）
- [x] **[P-04]** interleaved の no-op 関数と呼び出しを削除（2026-11-18、p04-worker）
- [x] **[M-11 一部]** provider/tools.rs に 17 テスト（2026-11-18、provider-tools-test-worker）
- [x] **[P-05]** protocol Auto の実挙動を明記＋回帰テスト（2026-11-18、p05-worker）
- [x] **[P-08]** ADR-0006 の参照と事実関係を修正（2026-11-18、p08-worker）
- [x] **[M-11 一部]** util/logging.rs に build_filter の 3 テスト（2026-11-18、logging-test-worker）
- [x] **[P-06]** timezone 追加と他キーの未対応を明記（2026-11-18、p06-worker）
- [x] **[L-01 一部]** core/bus.rs に 8 テスト（L-01 の中核も検証）（2026-11-18、bus-test-worker）
- [x] **[M-11 一部]** plugin/subprocess/mod.rs に 2 テスト（2026-11-18、plugin-subproc-test-worker）
- [x] **[M-11 一部]** opencode-types 4 ファイルに 16 テスト（2026-11-18、types-data-test-worker）
- [x] **[M-11 一部]** plugin/subprocess/auth.rs に 10 テスト（2026-11-18、plugin-auth-test-worker）
- [x] **[M-11 一部]** tool/edit/mod.rs に 7 テスト（2026-11-18、edit-test-worker）
- [x] **[M-11 一部]** provider/message.rs に 14 テスト（2026-11-18、provider-message-test-worker）
- [x] **[M-11 一部]** plugin/subprocess/runtime.rs に 6 テスト（2026-11-18、runtime-test-worker）
- [x] **[M-21]** TODO_MANAGER / PTY_MANAGER を ServerState へ移行（2026-11-18、managers-state-worker）
- [x] **[M-11 一部]** provider/google.rs に 8 テスト（2026-11-18、google-test-worker）
- [x] **[M-11 一部]** provider/mistral.rs・deepseek.rs に 12 テスト（2026-11-18、mistral-deepseek-test-worker）
- [x] **[M-11 一部]** provider/azure.rs (10)・groq.rs (6)・xai.rs (7) にテスト（2026-11-18）
- [x] **[M-11 一部]** provider/together.rs (6)・perplexity.rs (7)・cerebras.rs (6)（2026-11-18）
- [x] **[M-11 一部]** provider/cohere.rs (8)・deepinfra.rs (8)・openrouter.rs (10)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI router(11)・branding(4)・state(3)・slash(13)・hooks(14)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI router Default 修正(1)・terminal(4)・todo_item/tool_call(24)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI border(8)・markdown parser(9)・status(3)・help(8)・alert(12)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI confirm(13)・session_rename(20)・tag(28)・session_text(9)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI fork(18)・prompt_stash(27)・code_block(13)・command_palette(30)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI session_list(37)・model_select(32)・theme_list(30)・agent_select(21)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI session_export(22)・timeline(19)・skill_list(27)・subagent(22)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI mcp(28)・provider(24)・permission(20)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI dialog(6)・message(12)・home(9)・markdown renderer(12)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-11 一部]** TUI api(13)・context/app_context(14)・clipboard(7)
  （2026-11-18、swarm-coding R1-R6）
- [x] **[M-02 一部]** list_messages の snake_case 契約を双方向で固定
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-04 一部]** tool 12 ファイル 33 箇所を execution_with_source へ置換
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-04 一部]** provider 20 ファイル 62 箇所を api/network_with_source へ置換
  （2026-11-19、swarm-coding R1-R6）

## メモ

- `is_retryable` と `parse_api_call_error` の既存差異（2026-11-19、swarm-coding R4 が発見、
  却下）: openai 系 `ApiErrorWithStatus(404)` は is_retryable 側が非対象
  （429/500/502/503/504 のみ）とし parse 側が retryable=true とするが、
  `test_openai_retryable_404` が意図を固定しており本次差分以前から存在する設計差異のため
  H-16 では対応しない。ポリシー一元化は別タスク候補。
- H-24/H-15/M-54 の残存リスク（2026-11-19、swarm-coding R4/R5）:
  - DNS rebinding（WebFetch の検証時と実際の接続時で DNS 解決が別）は既知残存。
  - パス検証と書込みの間の TOCTOU（symlink 差し替え）は残存。R4 指摘は却下
    （検証時点の canonical パスを使う現設計で、完全対策には書込み直前の再検証が必要）。
  - apply_patch の Add の移送元は `is_external_path` を直接呼ぶため、新規ファイルが
    project_root 内でも external_directory が要求される（M-54 より前からの既存挙動、
    R5 スコープ外）。`is_move_target_external` と同じフォールバックを適用する候補。
- swarm review+discovery（2026-11-19）で採用見送った指摘:
  - `external_directory.rs:61-72` の prefix 比較は誤検知（絶対パスは H-02 で
    `Path::starts_with` のコンポーネント比較へ修正済み）。ただし相対パス `../` の
    無条件許可は残存しており、H-02 のフォローアップ候補。
  - `AgentSelectDialog::set_agents` の選択未リセット、`ThemeListDialog::new()` の
    Some(0) 初期化は現挙動をテストで固定済みで実害なし（各 R2 エージェントの記録）。
  - core `Bus` の未配線・削除判断（方針 A/B）は設計判断待ちのまま M-20 に記録済み。
  - `/api` prefix（P-01）、generate（P-02）、upstream parity（P-07）は既存 todo と重複。
  - performance / scale 系の指摘は測定根拠が無いため todo 化せず保留。
- 単一メッセージ応答のキー不一致（2026-11-18 発見 → 2026-11-19 親がコード追跡で解消）:
  `routes.rs:2264-2269` の `get_message` は `"sessionID"` / `"createdAt"` の手組み
  JSON を返すが、TUI が呼ぶのは `list_messages`（routes.rs:1094、server 側 MessageInfo
  の snake_case を Vec で返す）のみ。TUI の `get_messages` はこれを正しく受け取れる。
  双方向の wire format をテストで固定済み（server 2 件 / TUI 1 件）。実害なし。
- `PermissionType::from_permission_str`（2026-11-18、swarm-coding R2-C が発見、未修正）:
  `ExecuteCommand` / `NetworkRequest` に対応する arm が無く、これら 2 バリアントは
  直接構築でしか到達できない。サーバが `"execute_command"` 等を送る想定なら
  マッピング追加を検討（現挙動はテストで固定済み）。
- `AgentSelectDialog::set_agents`（2026-11-18、swarm-coding R2-C が発見、未修正）:
  選択インデックスをリセットしないため、新リストが現在選択より短いと
  `selected_agent()` が None になる。現挙動はテストで固定済み。ThemeListDialog::filter()
  のように 0 / None へクランプする修正は別タスク。
- `session_text.rs:69-73` の到達不能コード（2026-11-18、swarm-coding R2-C が発見、未修正）:
  `visible_count = if collapsible && collapsed { preview_lines } else { ... }` は同条件が
  48-56 行目で早期 return するため到達不能。動作影響なし。単純化は別タスク。
- `Router::default()` の history 空問題（2026-11-18、swarm-coding R4 発見 → R5 修正済み）:
  `#[derive(Default)]` を `impl Default for Router { Self::new() }` へ変更し、
  `default()` も `new()` と同じ history=[Home] を返すよう解消済み（回帰テスト 1 件追加）。
- serde wire format 不整合（2026-11-18、types-data-test-worker が発見、未修正）:
  `PartType` は `rename_all="camelCase"` がタグ値のみに効き内部フィールド（tool_call_id /
  is_error 等）は snake_case のまま。`SessionStatus` は sqlx rename のみで serde は
  PascalCase（"Active" 等）、`MessageRole` も PascalCase。上流仕様との照合が必要
  （M-02 の共有型整理と関連）。現状はテストで固定済み。
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
