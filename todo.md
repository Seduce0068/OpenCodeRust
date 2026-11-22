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
  - 2026-11-19 一部対応: parse_npm_spec を Result 化し未指定/`*`/dist-tag/range を
    install 前に拒否、npm 名ルール検証、scoped 名保持、`--ignore-scripts` 付与。
    plugin-host.ts に validatePluginImportPath を追加（bare/scoped/pkg@version/file://
    のみ許可、`..` とパーセントエンコード traversal を拒否、import は正規化済み URL）。
    file:// 生成は `Url::from_file_path` へ（Windows/特殊文字対応）。
  - 残存: lockfile/integrity 完全固定、完全な許可リスト、auth.fetch の URL/ヘッダ検証、
    `$` ヘルパ、Deno `--allow-all`、信頼モデル ADR（4 主体の定義と例外手続き）。
    Deno の `deno install --ignore-scripts` 対応は未確認（外部仕様）。
- [ ] **[H-04]** MCP 実行時依存の完全性検証（2 件は固定済み、残りに絞る）
  - `chrome-devtools-mcp` / `tavily-mcp` は Exact 固定済み。未対応は
    `@modelcontextprotocol/server-devtools` と `server-playwright` が公式 npm で
    404（パッケージ不在）のため固定不能。代替（`@playwright/mcp` 等）の選定、
    lockfile 生成、CI npm スキャン対象化）
  - 2026-11-19 調査: 推奨は**外部確認が必要**。代替候補 `@playwright/mcp` は名前のみ
    todo.md に存在し、実在性・バージョン・保守状況の裏付けがローカルに皆無。
    npm lockfile / package.json は存在せず新規設営から。CI は cargo audit のみで
    npm スキャン対象なし（共通ワークフロー委譲のため改修先の調整も必要）。
    chrome-devtools サーバが既存するため devtools は統合/削除の可能性もある（speculative）。
    persist キュー、session_repo.update/create とメッセージ置換の同一トランザクション化、
    古い snapshot の遅延 commit による更新喪失（推測のまま）
    セッション永続化と promoted 更新の同一トランザクション化（repository.rs 変更要）と、
    下流での queue_id による重複レスポンス排除。完全な二重課金防止は未検証
- [ ] **[H-13]** TUI イベントルートのブロッキング HTTP と全件再マッピング
  - 2026-11-19 一部対応: 常時 501 の `refresh_skill_list_dialog` /
    `refresh_lsp_status` を 5 秒 Tick から削除。LSP はステータス表示を開く時のみ、
    スキルは既存のスキル一覧オープン時のみ同期するよう変更（表示内容は不変）。
  - 2026-11-19 調査で訂正: 背景の「Tick 2 秒」は不正確。`TICK_RATE_MS=16` で
    2 秒は session 同期ゲート、5 秒は aux 同期ゲート（app.rs:41,786,792）。
    残存は `get_session` + `get_messages`（2 秒）と `list_sessions` 系 + status + MCP
    （5 秒）の同期 HTTP、全件再取得・再マッピング、30s timeout テールリスク。
  - 推奨は**イベント駆動・差分同期**（server に since/version ベースの増分
    メッセージ API、SSE の message.updated をトリガに全件 get_messages を差分適用へ）。
    spawn_blocking は TUI に Tokio runtime が無く直接用いられない。未計測で体感は speculative。

### 優先度: 中

- [ ] **[M-03]** 未接続クレートと責務重複（PTY）による責務分離の形骸化
  - 2026-11-19 対応済み: 外部参照 0 を再確認し、opencode-pty を workspace member から
    外しクレートごと削除（Cargo.lock のエントリも更新）。server 版 PtyManager は
    現状維持で /pty 契約・M-43 挙動に影響なし。自己テスト 1 件消失で総数 2214→2213。
  - 2026-11-19 調査: 2 つの PtyManager は API 非互換（sync spawn + `mpsc<String>` vs
    async CRUD + `broadcast<Vec<u8>>` + replay）で直接置換不可。配線は約 500 行の
    実移管で wire 契約・M-43 回収の回帰リスクあり（見積りは speculative）。
  - 前提を訂正: util は CLI が実使用（L-04 で配線済み）で「無依存」は stale。
    watcher は core の GitWatcher と config.watcher に機能分散（別 ADR 判断）。
    local-llm は外部 MCP 起動想定（登録有無は未確認）、codemode は ADR-0006 維持。
    2026-11-19 に opencode-tool の 12 ファイル 33 箇所を `execution_with_source` へ、
    opencode-provider の 20 ファイル 62 箇所を `api_error_with_source` /
    `network_error_with_source` へ置換（メッセージ不変・非対象は据え置き、旧パターン残存 0）。
    残存 `ExecutionError` 17 箇所は下位エラーが無い固定文字列／StatusCode／別バリアント。
    未対応: StreamError(e.to_string()) と InvalidArguments(e.to_string())（serde_json）。
    新バリアントは _enum 非 non_exhaustive_ のため下流列挙には破壊的変更になりうる
- [x] **[M-05]** Todo 状態を最小配線（2026-11-19、swarm-coding R1-R6）
  - SessionPrompt に with_todo_manager / with_todo_hooks を追加し ToolContext へ
    with_todo_update / with_todo_get を注入（false success を解消）。server 側で
    state.todo_manager と todo.updated broadcast を配線、TUI はイベント受信と
    /event 再接続時の GET /session/{id}/todo 再同期を追加。
  - 残存: DB 永続化（with_database）、CLI 実コマンド、TodoStatus 3 重定義の統合、
    SessionPrompt::new のデフォルトコールバック None（本番経路は注入済み）。
- [ ] **[M-11 残り]** HTTP テストと OAuth フロー（SSE 行バッファは全adapter対応済み）
  - 2026-11-19 対応済み: Anthropic の SSE 行分割／UTF-8 境界を実 HTTP モックで検証。
    さらに全 16 アダプタを共通 `SseLineBuffer` ベースへ変更（4 エージェント並列）、
    空 TextDelta プレースホルダを全アダプタで廃止、azure/cohere/cerebras/deepinfra は
    try_unfold + 終端 flush 化（R4 の混在/flush 指摘を R5 で修正）。テスト +16 件。
  - 2026-11-19 調査で判明: MCP OAuth callback は実装・配線・テスト済みで旧 TODO は
    stale（server/mcp_oauth.rs:519-564, routes.rs:3228-3240）。state 省略による
    CSRF バイパス（保存 state をエコーして比較が空になる）を修正し、state 必須化
    ＋400 化の回帰テスト 2 件を追加。
  - 2026-11-19 対応済み: registry の OAuth manager を server 名キー HashMap 化し
    `add_http` / `add_sse` へ注入。server `connect()` は Authorized 時に
    `oauth_client_for` の manager を登録（複数 OAuth サーバのトークン誤送信を防止）。
    ログアウト時に manager を削除、read guard を跨ぐ await を解消。
    discovery.rs に loopback HTTP モック（新規依存なし・false green 回避）で
    7 件の実 HTTP テストを追加。テスト +11 件。
  - 2026-11-21 対応済み: openai.rs の per-chunk from_utf8_lossy を廃止し、
    生バイトを改行まで保持してからデコード（UTF-8 境界安全）。
  - 未対応: RFC8707 resource の永続化、`try_unfold` を通す実 HTTP モックハーネス。
    SSE transport は RemoteMcpConfig に選択肢が無く server connect から未到達
    （add_sse 側の注入自体は実装済み）。
  - 残存（R4 記録）: ループバック bind 失敗時の graceful skip は false green、
    2 チャンク到達の明示的検証なし、イベント検証が完全一致でない。
    実行時は `NO_PROXY=127.0.0.1,localhost` 付与が必須（テスト doc に明記済み）。
  - 補足: provider/convert.rs は lib.rs から未配線のスタブで、テストを追加しても
    本クレートでは実行されない（2026-11-18 発見、追加は差し戻し済み）
- [x] **[M-15]** 依存の workspace 集約を完了（2026-11-19、swarm-coding R1-R6）
  - L-07 の 20 件＋残り 3 件（dashmap / jsonc-parser / toml）を集約。クレート側
    feature（jsonc-parser の serde）は維持し実効仕様不変。Cargo.lock 差分なし。
    厳密ピン拡充（8/61）は Cargo.lock コミット＋CI locked で再現性担保済みのため
    複数バージョン併存リスクの精査は不要と判断。
  - version/edition/authors/license の workspace 継承は 7 クレートとも完了済み
    逆依存はすべて DAG 内で循環は無く（AGENTS.md のハードルールは充足）、
    session は実質 composition root として provider/tool/mcp/lsp/plugin を直接
    参照するのは設計上説明可能。大規模なトレイト注入反転（90 参照超、
    plugin グローバル singleton の注入化が前提）はコスト対効果で見送り。
    2026-11-19 実施済み: `opencode_permission::BashArity` を tool 内へ移動し
    tool→permission 依存を解消（R4 の 2 トークン panic 指摘を R5 で修正）。
    残存は `opencode_lsp::detect_language` の移動（任意・低優先）。
  - 未使用の `SessionID` の sqlx::Type derive は削除済み。`SessionStatus` は storage が
    実使用し orphan rule で storage への直接移管は不可（newtype＋From/Into 案は未検証）。
- [x] **[M-17 一部]** transform / registry を pub(crate) 化
  （2026-11-19、swarm-coding R1-R6）
  - 外部フルパス参照ゼロの 7 モジュールに加え transform / registry を縮小。
    残存は bootstrap / skill（ファサードへ未再エクスポートの型あり）と provider
    実装 12 モジュール（意図的な公開 API、意図は speculative）。
- [ ] **[M-34 残り]** 依存更新検知の Renovate 委譲とスクリプトゲート
  - 2026-11-19 対応済み: ci.yml に all-features と unsafe lint ゲートを追加（`extra_gates`
    / `clippy_args`、ローカルで両方 green、共通ワークフロー README で入力仕様確認）。
    2026-11-19 追加済み: `renovate.json`（$schema + config:recommended の最小構成）を
    配置。cargo outdated は時間依存で time-bomb になるため CI には入れない方針で、
    依存更新検知は Renovate へ委譲。**GitHub App / self-hosted の有効化は外部サービス
    操作のためユーザー判断として未実施**（設定ファイルだけでは動作しない）。
    `scripts/opencode_llm_mcp.rs` は workspace member 外で `--locked` 再現性の観点から
    ゲート化を見送り（恒久対策は member 化）。脆弱性監査は `audit: true` で既にゲート済み。
- [ ] **[M-37 残り]** 共通 canonicalize ヘルパの async 化（現状維持・実測トリガ付き保留）
  - 2026-11-19 対応済み: find 系 spawn_blocking、read_file の tokio::fs 化、
    get_file_status / current_project_info の git subprocess を tokio::process 化、
    list_files の read_dir/next_entry/file_type/metadata を非同期化（R4 の
    next_entry Err 打ち切り指摘を R5 で修正）、tool 側 glob/read の WalkDir も
    spawn_blocking 化。
  - 2026-11-19 調査: 推奨は**現状維持**。直接呼び出し 5 + 間接 3 の 8 箇所に波及、
    うち 2 箇所は spawn_blocking 内で `block_on` は採用不可。対象パスはローカル cwd 配下
    で実害未計測。真のホットスポットは list_files がエントリ毎に root を再 canonicalize
    する点（syscall 半減可能で async 化より先に最適化候補）。
  - 再検討トリガ: NFS/FUSE マウント対応、または list_files のワーカー占有計測。
    実装時は「純粋述語 + 同期 I/O 版 + 非同期 I/O 版」の 3 層構成を推奨。
- [x] **[M-38 一部]** /stream の初期切断検知を追加（2026-11-19、swarm-coding R1-R6）
  - chat_stream().await を tx.closed() と biased select! で競合、切断時に future を
    ドロップし永続化してから早期リターン（R4 の永続化回帰指摘を R5 で修正）。
    残存: provider 側の上流キャンセル（CancellationToken 追加は 25 実装以上波及で
    高リスク）、/prompt の fire-and-forget 契約上の切断検知不可（明示 abort のみ）。

### 優先度: 低

- [ ] **Agent part の status が pending のまま更新されない**
  — `session/message.rs:234-244` / AGENT-PART-PENDING / 低
- [ ] **[L-04 残り]** tracing の残課題（observability / 効率のみ）
  - 2026-11-19 対応済み: http_request スパン完成（/api session_id 対応、
    TraceLayer on_response で status/latency/outcome、x-request-id 正規化 128 文字）。
    TUI（Tui / Attach / サブコマンドなし）起動時のみログをファイルへ出力し
    stdout 描画干渉を解消。WorkerGuard は main のローカル束縛で保持し終了時
    フラッシュを保証（R4 の static 保持では終了時フラッシュされない指摘を修正）。
  - 2026-11-19 一部対応: load_config の 11 呼び出しを cwd キーの `OnceLock` キャッシュ
    経由へ集約（同一 cwd の再パースを排除、`--dir`/PROJECT/ACP の cwd 切替は维持、
    エラー伝播と .ok() の握り潰しは不変）。
  - 残存: prompt/stream ハンドラの開始・完了・切断ログ、/log の extra 平坦化
    （いずれもユーザー可視の実害なし）。rolling 未使用はファイルログを本運用する
    段階で検討。二重初期化は .init() が panic するため呼び出しは 1 箇所に限定。
    設定ファイルのプロセス中変更・env 変更は cwd スナップショット化で反映されない
    （意図した意味論変化。--config / 設定書き込み経路は現状存在しない）。
- [ ] **[L-14 残り]** スナップショット全件 clone の実害計測（現状維持）
  - list_messages の後方互換 pagination は対応済み。2026-11-19 調査で「Arc による
    メッセージ COW」という前提は誤りと判明: メッセージは所有 `Vec<SessionMessage>` で、
    実態は Arc<dyn Fn(&Session)> の update_hook 経由で Session 全体を deep clone
    （1 エミットあたり最大 3 回、TextDelta は 50ms スロットル）。
    Arc::make_mut はスナップショット側が Arc を保持する限り deep clone するため
    O(n) を除去できず、不完全木／構造共有は wire・永続化形式の変更リスクが高い。
    推奨は**現状維持＋実害計測を先行**。まずエミット回数と clone 時間を測り、
    問題が確認されればスナップショット全件送りを増分イベント送りへ変更する検討。

### 優先度: 高（残タスク調査で追加）

- [ ] **[P-01 残り]** upstream 126 パスの直接照合と ADR 化
  - /api エイリアスは 2026-11-19 に追加済み（既存ルート維持、TUI/CLI 変更不要）。
    残るは upstream checkout で 126 パスを直接照合し欠落を洗い出すこと、
    `session_id_from_path()` の /api/session 対応（server.rs follow-up）、
    および互換方針の ADR 化。upstream 側のパス情報はローカル間接引用のみで speculative。
- [ ] **[P-02]** `/experimental/generate` の契約が upstream と完全に不一致（現状維持）
  - 2026-11-19 調査（swarm-coding R2）: 推奨は**現状維持（501）＋定義の再設定**。
    Rust 側は既に 501 の not_implemented を返し、旧 placeholder レスポンスは
    wire に出ていない（todo.md の記述は古い）。upstream 想定の
    `/api/experimental/generate/{text,image,decisions}` は未登録で 404 になる。
    upstream 契約の一次情報はローカルに無く（間接引用のみ）、full parity は
    5〜10 ファイル・300〜800 行超の speculative 見積り。
    再定義: 「実装」ではなく「サブパス 404 を契約として明記し 501 方針を維持」。
    follow-up: routes.rs の行番号を 4557-4659 へ更新、MAPPING.md に 404/501 の
    区別を追記。実 handler 実装は upstream checkout 取得後の別タスク。
    text/image/decisions を実装し、一覧・有効/無効 (`routes.rs:4411-4421`) も実処理へ

### 優先度: 高（swarm review+discovery 2026-11-20）

### 優先度: 高（swarm review+discovery 2026-11-19）

### 優先度: 中（残タスク調査で追加）

- [ ] **[P-04]** interleaved thinking の runtime 反映（現状維持・定義を再設定）
  - 2026-11-19 調査: 推奨は**現状維持（明記のみ）**。model.interleaved → runtime
    capabilities の変換は bootstrap.rs:965-971 で完了済み。未配線は transform 側だけ
    ではなく、(a) RuntimeModelInfo が capabilities を落とし config が届かない
    (b) PartType::Reasoning が prompt/server の変換で破棄され transform が no-op
    (c) Anthropic wire が thinking ブロック/signature 非対応、の複合。
    `transform_messages` の一括配線は unsupported_parts/normalize/remap の適用で
    wire 回帰リスクあり。完全対応は 4-6 ファイル・60-150 行超（speculative）で
    別タスク。現状は `apply_caching` のみが本番使用。
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

### 優先度: 中（swarm review+discovery 2026-11-20）

- [ ] **リトライでプラグインフックの外部副作用が二重発火**
  — `session/llm.rs:737-744` / ERR2-06 / 中（speculation）
- [ ] **OpenAI Responses→legacy フォールバックが session リトライと掛け算**
  — `openai.rs:1026-1083` / ERR2-07 / 中（speculation）
- [ ] **POST /prompt に冪等キーが無く再送で二重実行**
  — `routes.rs:1991-1999` / API-05 / 中（speculation）
- [ ] **[残り]** limit=0 の意味論が TUI（未指定扱い）とサーバ（空結果）で不一致
  （引数の露出は完了）
  — `tui/api.rs:681-693` / API-08 / 中
- [ ] **TUI SSE リスナー join が終了まで最大 120 秒ハング**
  — `tui/app.rs:3776-3784` / SHUTDOWN-04 / 中
- [ ] **Snapshot::revert が一部失敗で中途半端な復元状態を残す**
  — `session/snapshot.rs:65-137` / ERRPATH-06 / 中（設計判断）
- [ ] **コマンド timeout 既定が bash 2 分・local-llm 5 分で不一致**
  — `tool/bash.rs:11` / CONSTANTS-05 / 中（設計判断）
- [ ] **reset_processing が DB 全体の processing を書き換え**
  — `server.rs:672-674` / STATE-06
- [ ] **[残り]** custom fetch proxy が opencode-provider の process-global
  （plugin loader / MCP OAuth は per-instance 化済み）
  — `server.rs:1037` / 既知残存

### 優先度: 中（swarm review+discovery 2026-11-19）

## 完了

- [x] **[ログ redact 2 件]** SSE エラー・chat_stream 開始エラーの生出力防止
  （2026-11-21、swarm-coding R1-R6）
- [x] **[設定/状態不変 3 件]** timeout 0・archive 解除・steps 0
  （2026-11-21、swarm-coding R1-R6）
- [x] **[文字列/I/O 5 件]** auth atomic 0600・非UTF8 上書き防止・truncate 境界
  （2026-11-21、swarm-coding R1-R6）
- [x] **[数値/並行 4 件]** days 境界・timeout・登録競合・subsession 直列化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[パス/URL 安全性 7 件]** symlink・共有 URL・blacklist・スキーム等
  （2026-11-21、swarm-coding R1-R6）
- [x] **[serde/後始末 5 件]** old-key alias・round-trip・busy リーク・楽観 msg
  （2026-11-21、swarm-coding R1-R6）
- [x] **[ページネーション引数露出 1 件]** directory/roots/start を API に追加
  （2026-11-21、swarm-coding R1-R6）
- [x] **[run_status 復帰 1 件]** ストリーム異常終了で Idle に戻す
  （2026-11-21、swarm-coding R1-R6）
- [x] **[compaction 状態終端化 1 件]** Compacting を Active に戻す
  （2026-11-21、swarm-coding R1-R6）
- [x] **[unwatch 整合性 1 件]** 失敗時も内部状態をクリーンアップ
  （2026-11-21、swarm-coding R1-R6）
- [x] **[subagent 深さ復元 1 件]** 永続化ラウンドトリップで深さを保持
  （2026-11-21、swarm-coding R1-R6）
- [x] **[optimistic メッセージ残存 1 件]** Home 経路の失敗時 cleanup
  （2026-11-21、swarm-coding R1-R6）
- [x] **[境界 panic 6 件]** 1文字引用符・全角空白・オーバーフロー・巨大年
  （2026-11-21、swarm-coding R1-R6）
- [x] **[client_ip XFF 1 件]** 信頼プロキシ opt-in・fail-closed
  （2026-11-21、swarm-coding R1-R6）
- [x] **[limit クランプ通知 1 件]** x-limit-clamped と CORS expose
  （2026-11-21、swarm-coding R1-R6）
- [x] **[OAuth TTL 1 件]** 放置フローを回収し期限切れを明示エラー化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[plugin loader per-instance 1 件]** process-global を削除し状態を分離
  （2026-11-21、swarm-coding R1-R6）
- [x] **[MCP OAuth per-instance 1 件]** process-global を削除し状態を分離
  （2026-11-21、swarm-coding R1-R6）
- [x] **[part 冪等化 1 件]** 任意 ID 受け付けと再送重複の防止
  （2026-11-21、swarm-coding R1-R6）
- [x] **[provider キー統一 1 件]** /provider に providers エイリアスを追加
  （2026-11-21、swarm-coding R1-R6）
- [x] **[SessionRow status 1 件]** レガシー行がライフサイクル状態を保持
  （2026-11-21、swarm-coding R1-R6）
- [x] **[strict env 1 件]** opt-in で未定義参照を fail-closed 化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[DELETE 冪等性 1 件]** 404 を既削除として扱う
  （2026-11-21、swarm-coding R1-R6）
- [x] **[wellknown/真値 2 件]** ブロックリスト拡張と fail-closed 統一
  （2026-11-21、swarm-coding R1-R6）
- [x] **[inline config 文書 1 件]** 相対ファイル参照の基準を明記
  （2026-11-21、swarm-coding R1-R6）
- [x] **[空モデル ID 1 件]** type 付きエラー化と明示指定も検査
  （2026-11-21、swarm-coding R1-R6）
- [x] **[provider リスト 1 件]** 明示的空配列によるクリア
  （2026-11-21、swarm-coding R1-R6）
- [x] **[error retryable/SSE 2 件]** type フォールバックと失敗伝播
  （2026-11-21、swarm-coding R1-R6）
- [x] **[--project logLevel 1 件]** 起動時設定読み込み先の是正
  （2026-11-21、swarm-coding R1-R6）
- [x] **[wellknown/tools 2 件]** 検証共有と Deny 優先の決定的変換
  （2026-11-21、swarm-coding R1-R6）
- [x] **[usage/走査順 2 件]** デコード失敗の可視化と決定的ソート
  （2026-11-21、swarm-coding R1-R6）
- [x] **[config 移行/文書 2 件]** 明示 mode 保持と設定源の網羅
  （2026-11-21、swarm-coding R1-R6）
- [x] **[MCP/Anthropic error 2 件]** フラグ復元と SSE error 昇格
  （2026-11-21、swarm-coding R1-R6）
- [x] **[LSP/一時ファイル 2 件]** close_document と RAII ガード
  （2026-11-21、swarm-coding R1-R6）
- [x] **[PTY セッション回収 1 件]** reaped フラグと async 側 prune
  （2026-11-21、swarm-coding R1-R6）
- [x] **[リソース累積 2 件]** writer_lock 回収と plugin レジストリ排水
  （2026-11-21、swarm-coding R1-R6）
- [x] **[メッセージ wire 統一 1 件]** get_message を list_messages と同一化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[リソース上限 2 件]** TUI レスポンスキューと MCP ログのリング化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[TUI API 契約 3 件]** compact/revert/fork の不一致を解消
  （2026-11-21、swarm-coding R1-R6）
- [x] **[ビルトイン auth fail-closed 1 件]** load_all の Err 伝播と redact
  （2026-11-21、swarm-coding R1-R6）
- [x] **[rotate_key 永続化 1 件]** persist-before-commit とロールバック
  （2026-11-21、swarm-coding R1-R6）
- [x] **[設定検証コマンド 1 件]** `config --check` 追加
  （2026-11-21、swarm-coding R1-R6）
- [x] **[shutdown キュー終端化 1 件]** プロンプト abort と再スケジュール停止
  （2026-11-21、swarm-coding R1-R6）
- [x] **[APP_VERSION_DATE 1 件]** 2026.11.21 へ同期（固定ラベル方針明記）
  （2026-11-21、swarm-coding R1-R6）
- [x] **[シグナル終了時フラッシュ 1 件]** graceful shutdown で既にカバー
  （2026-11-21、親が実コードで事実確認）
- [x] **[MCP 同期失敗警告 1 件]** ヘッダ伝播とワイヤ互換維持
  （2026-11-21、swarm-coding R1-R6）
- [x] **[高優先: plugin/session 2 件]** init 競合の可視化と sync 削除スコープ
  （2026-11-21、swarm-coding R1-R6）
- [x] **[入れ子キー/不正値警告 2 件]** ユーザー定義名のマスクと warn
  （2026-11-21、swarm-coding R1-R6）
- [x] **[設定エラー/スキーマ明記 2 件]** 値非露出と情報源の明確化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[テスト信頼性 2 件]** overflow 同期と心拍の継続観測
  （2026-11-21、swarm-coding R1-R6）
- [x] **[テスト信頼性 4 件]** env 直列化・時間依存の解消
  （2026-11-21、swarm-coding R1-R6）
- [x] **[共有型 3 件]** AgentColor 正規化・diffs 既定値・PtyStatus 集約
  （2026-11-21、swarm-coding R1-R6）
- [x] **[permission 二重エンコード 1 件]** PATCH 後の CorruptData を解消
  （2026-11-21、swarm-coding R1-R6）
- [x] **[設定ロード失敗警告 1 件]** 値非依存フォールバック通知
  （2026-11-21、swarm-coding R1-R6）
- [x] **[provider 警告/frontmatter/TUI join 3 件]** 値非露出の警告とスレッド回収
  （2026-11-21、swarm-coding R1-R6）
- [x] **[plugin/設定警告 3 件]** kill フォールバックと非推奨キー警告
  （2026-11-21、swarm-coding R1-R6）
- [x] **[graceful shutdown 4 件]** シグナル・flush・設定警告 replay
  （2026-11-21、swarm-coding R1-R6）
- [x] **[移行/マスク/timeout 3 件]** バックアップ順序と形式の明記
  （2026-11-21、swarm-coding R1-R6）
- [x] **[定数整合 6 件]** 単一情報源化と SSE バックオフ文書修正
  （2026-11-21、swarm-coding R1-R6）
- [x] **[永続化/境界 4 件]** 認証整合・ログアウト・models 境界・metadata
  （2026-11-21、swarm-coding R1-R6）
- [x] **[移行 fingerprint 1 件]** 起動時一度きりの設計意図を確認・復元
  （2026-11-21、swarm-coding R1-R6）
- [x] **[完了ログ/variant キャッシュ 2 件]** request_id 明示と無効化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[認証通知/設定反映 3 件]** 起動時固定と README 追随
  （2026-11-21、swarm-coding R1-R6）
- [x] **[readiness/TUI env 2 件]** 失敗 reason と RAII 復元
  （2026-11-21、swarm-coding R1-R6）
- [x] **[loader warn/redact 1 件]** 重複登録の可視化とキー正規化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[observability/dogfood 12 件]** 相関 ID・構造化ログ・手順反映
  （2026-11-21、swarm-coding R1-R6）
- [x] **[共有型集約 1 件]** types 単一情報源化を既存実装として確認
  （2026-11-21、親が実コードで事実確認）
- [x] **[OpenAI UTF-8 境界 1 件]** per-chunk デコードの廃止
  （2026-11-21、swarm-coding R1-R6）
- [x] **[Markdown 表/wrap_spans 2 件]** 幅クランプの本番配線と wide char 境界
  （2026-11-21、swarm-coding R1-R6）
- [x] **[Plugin/Todo 型 2 件]** async_trait 統一・単一情報源化
  （2026-11-21、swarm-coding R1-R6）
- [x] **[MCP progress/OAuth/ratatui 3 件]** hard deadline・並行フロー・feature
  （2026-11-21、swarm-coding R1-R6）
- [x] **[stream/フック 3 件]** 切断即検知・Drop cleanup・個別 timeout
  （2026-11-21、swarm-coding R1-R6）
- [x] **[TUI 表示幅 5 件]** 全角幅計算・centered 境界・toast 省略
  （2026-11-21、swarm-coding R1-R6）
- [x] **[plugin/tool/mcp 4 件]** hook redact・userinfo 除去・npm 排水
  （2026-11-21、swarm-coding R1-R6）
- [x] **[設定階層/retryable 3 件]** .opencode 維持・schema 非公開・retry 保持
  （2026-11-21、swarm-coding R1-R6）
- [x] **[設定キャッシュ/protocol/hook 3 件]** fingerprint・正規化・coalescing
  （2026-11-21、swarm-coding R1-R6）
- [x] **[リソース/ドキュメント 7 件]** 回収・再帰上限・履歴スナップショット
  （2026-11-21、swarm-coding R1-R6）
- [x] **[storage/mcp 5 件]** metadata/cost 往復・v3 移行・権限 fail-closed
  （2026-11-21、swarm-coding R1-R6）
- [x] **[config/wire/glob 7 件]** env 配線・serde 対称化・連鎖置換
  （2026-11-21、swarm-coding R1-R6）
- [x] **[リソース境界/ドキュメント 6 件]** 8 MiB 上限・ログ ring・ADR 追随
  （2026-11-21、swarm-coding R1-R6・制限プロファイル）
- [x] **[config/API/ADR high 10 件]** マージ・再 bootstrap・境界整備
  （2026-11-21、swarm-coding R1-R6・制限プロファイル）
- [x] **[health/ping/logwarn 3 件]** 認証除外・O(1) readiness・失敗可視化
  （2026-11-20、swarm-coding R1-R6）
- [x] **[ログ/health 2 件]** 一意名と世代保持、liveness/readiness 分離
  （2026-11-20、swarm-coding R1-R6）
- [x] **[R4 残存 4 件]** stream redact・serde 集約・OAuth fail-loud・折返し
  （2026-11-20、swarm-coding R1-R6）
- [x] **[wire/秘密/レンダ high 11 件]** 互換・redaction・fail-loud
  （2026-11-20、swarm-coding R1-R6）
- [x] **[テスト品質 8 件]** 恒真・環境依存・env 後始末・start 原子的化
  （2026-11-20、swarm-coding R1-R6）
- [x] **[config/UX/並行 11 件]** コメント判定・トークンマスク・リーク解消
  （2026-11-20、swarm-coding R1-R6）
- [x] **[config/storage 永続化 3 件]** 0600 atomic・jsonc 整合・DB パス
  （2026-11-20、swarm-coding R1-R6）
- [x] **[新 discovery high 10 件]** env/500/stream/権限/plugin
  （2026-11-20、swarm-coding R1-R6）
- [x] **[CORS 分離/Renovate 2 件]** ServerState 単位化と依存グループ化
  （2026-11-20、swarm-coding R1-R6）
- [x] **[permission Always/TLS 2 件]** 動的 allow と rustls 統一
  （2026-11-20、swarm-coding R1-R6）
- [x] **[重複整理 3 件]** Bedrock/CodeMode/local-llm の完了を反映
  （2026-11-20、親が実コードで確認）
- [x] **[rotation/LazyLock/corpus 4 件]** API 提供・標準化・コーパス拡張
  （2026-11-20、swarm-coding R1-R6）
- [x] **[plugin/provider 3 件]** symlink 防御・モデル決定性・EOF 終了
  （2026-11-20、swarm-coding R1-R6）
- [x] **[CLI/session/docs 11 件]** JSON 契約・実配線・セッション保全
  （2026-11-20、swarm-coding R1-R6）
- [x] **[重複整理 18 件]** 過去ラウンドで対応済みの未着手を完了へ
  （2026-11-20、親が実コードで確認）
- [x] **[観測性/整合性 9 件]** ログ・相関 ID・retry・Runbook
  （2026-11-20、swarm-coding R1-R6）
- [x] **[Bash/Web/CI 8 件]** env 検証・上限・audit/TSRV/release ゲート
  （2026-11-20、swarm-coding R1-R6）
- [x] **[Slash クリック・ADR 整合]** ポップアップ選択と 6 文書の追随
  （2026-11-20、swarm-coding R1-R6）
- [x] **[中優先 6 件]** TUI 操作・RPC バージョン・local-llm 上限
  （2026-11-20、swarm-coding R1-R6）
- [x] **[ストリーム構造化エラー]** Retry-After 到達とエンベロープ堅牢化
  （2026-11-20、swarm-coding R1-R6）
- [x] **[provider 残存 3 件]** OAuth 期限配線・ヘッダ保持・共通サニタイズ
  （2026-11-20、swarm-coding R1-R6）
- [x] **[新観点 high 8 件]** 構造化エラー・TUI 入力・ReDoS・Retry-After
  （2026-11-20、swarm-coding R1-R6）
- [x] **[sessions.metadata 列]** スキーマ・ALTER マイグレーション・往復を追加
  （2026-11-20、swarm-coding R1-R6）
- [x] **[SessionRow 可用性]** 起動時一覧のみ破損行スキップ（2026-11-20、swarm-coding R1-R6）
- [x] **[認証・設定の残存 2 件]** 定数時間比較・起動時固定・instructions presence
  （2026-11-20、swarm-coding R1-R6）
- [x] **[中優先 6 件]** CORS/401/auth・worktree 非同期・config 読み取り専用化
  （2026-11-20、swarm-coding R1-R6）
- [x] **[R4 残存 5 件]** ストリーミング上限・summary 永続化・URL redaction
  （2026-11-20、swarm-coding R1-R6）
- [x] **[review+discovery high 11 件]** no-op・fail-open・OOM・ログ基盤を修正
  （2026-11-20、swarm-coding R1-R6）
- [x] **[認証/設定 3 件]** attach パスワード・CLI fail-open・well-known 再注入
  （2026-11-19、swarm-coding R1-R6）
- [x] **[設定/CI/ドキュメント 3 件]** PTY 追随・NO_AUTH fail-open・all-features
  （2026-11-19、swarm-coding R1-R6）
- [x] **[discovery high 3 件]** deny 最優先・OAuth 0600・deps 再検証
  （2026-11-19、swarm-coding R1-R6）
- [x] **[discovery low 3 件]** util symlink・SSE 上限・limit クランプ
  （2026-11-19、swarm-coding R1-R6）
- [x] **[discovery low 4 件]** read/ls/skill symlink・replace_all・metadata・PTY cursor
  （2026-11-19、swarm-coding R1-R6）
- [x] **[discovery medium 3 件]** Broker pending 修正・detached worktree・symlink
  （2026-11-19、swarm-coding R1-R6）
- [x] **[モデルパッチ / OpenRouter]** Update/Move 解釈とヘッダ unwrap 排除
  （2026-11-19、swarm-coding R1-R6）
- [x] **[discovery high 2 件]** UTF-8 境界 panic と apply_patch stale 破壊
  （2026-11-19、swarm-coding R1-R6）
- [x] **[L-04 一部]** load_config を cwd キャッシュへ集約（2026-11-19、swarm-coding R2）
- [x] **[M-03 一部]** orphan クレート opencode-pty を削除（2026-11-19、親が直接実施）
- [x] **[H-03 一部]** npm spec 厳密化と import 検証（2026-11-19、swarm-coding R1-R6）
- [x] **[H-13 一部]** 501 終点のポーリング除去（2026-11-19、swarm-coding R2）
- [x] **[M-55]** Bedrock ストリームの行バッファ化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-11 一部]** OAuth トークン注入と discovery HTTP テスト
  （2026-11-19、swarm-coding R1-R6）
- [x] **[FK 統一 / M-11 一部]** Storage::new の FK cascade と OAuth CSRF 修正
  （2026-11-19、swarm-coding R1-R6）
- [x] **[L-16/L-18/L-19]** キュー終端化・GitWatcher 整理・LLM 切断検知
  （2026-11-19、swarm-coding R1-R6）
- [x] **[L-10〜L-17 一部]** 入力検証・デッドコード・CORS 等を修正
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-61/62/63/65/66]** TodoManager・SQLite・FileWatcher を修正
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-60/M-64/M-67]** worktree・cleanup・edit を修正
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-56〜M-59]** SSE/境界/存在判定/type フィルタを修正
  （2026-11-19、swarm-coding R1-R6）
- [x] **[H-26/H-27]** revert 経路のデータ損失を修正（2026-11-19、swarm-coding R1-R6）
- [x] **[M-11 一部]** 全 16 アダプタの SSE 行バッファ化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-34 一部]** renovate.json を追加（2026-11-19、親が直接実施）
- [x] **[L-04 一部]** TUI ログをファイル出力へ切替（2026-11-19、swarm-coding R1-R6）
- [x] **[M-16 一部]** BashArity を tool 内へ移動し依存を解消
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-11 一部]** Anthropic SSE 行バッファと実 HTTP 回帰テスト
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-37 一部]** tool 側 WalkDir の spawn_blocking 化
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-20]** core Bus と全 publish 配線を削除（2026-11-19、swarm-coding R1-R6）
- [x] **[M-21]** 残存 static 4 件は現状維持（2026-11-19、swarm-coding R1-R6）
- [x] **[M-37 一部]** read_file/git status の非同期化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-34 一部]** CI に all-features / unsafe lint ゲートを追加
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-37 一部]** find 系走査の spawn_blocking 化（2026-11-19、swarm-coding R1-R6）
- [x] **[L-04 一部]** http_request スパンを完成（2026-11-19、swarm-coding R1-R6）
- [x] **[P-01 一部]** /api エイリアスを追加（2026-11-19、swarm-coding R1-R6）
- [x] **[M-46]** /stream の位置づけを MAPPING.md に明記（2026-11-19、swarm-coding R1-R6）
- [x] **[L-07]** 残り 20 件の workspace 依存へ集約（2026-11-19、swarm-coding R1-R6）
- [x] **[L-14 一部]** list_messages に後方互換 pagination を追加（2026-11-19、swarm-coding R1-R6）
- [x] **[L-07]** tokio `full` を9 feature へ削減（2026-11-19、swarm-coding R1-R6）
- [x] **[L-07 一部]** 未使用 workspace 依存 11 件を削除（2026-11-19、swarm-coding R1-R6）
- [x] **[M-47]** permission pattern の明示引数化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-49]** provider の未使用 retry ヘルパーを削除（2026-11-19、swarm-coding R1-R6）
- [x] **[M-50]** FreeUsageLimit の非リトライ化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-48]** tool_result の ID 保持と tool_use の会話伝播
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-45 フォロー]** 残り 5 ハンドラの 501 化と CLI/TUI の整理
  （2026-11-19、swarm-coding R1-R6）
- [x] **[M-44 一部]** 復元時 total_cost を 0.0 に修正（2026-11-19、swarm-coding R1-R6）
- [x] **[M-45]** 未実装 14 ハンドラを 501 化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-50 一部]** retry-after に RETRY_MAX_DELAY を適用（2026-11-19、swarm-coding R1-R6）
- [x] **[M-43]** PTY の status 遷移と child 回収（2026-11-19、swarm-coding R1-R6）
- [x] **[M-51/M-52]** キーバインド登録と bool 応答の厳密化（2026-11-19、swarm-coding R1-R6）
- [x] **[M-53]** UTF-8 境界を尊重するスライス（2026-11-19、swarm-coding R1-R6）
- [x] **[H-19]** MCP OAuth に RFC 8707 resource を追加（2026-11-19、swarm-coding R1-R6）
- [x] **[H-21]** サブセッションへ disabled_tools を継承（2026-11-19、swarm-coding R1-R6）
- [x] **[H-22/H-23]** Anthropic SSE を状態付きパーサへ変更（2026-11-19、swarm-coding R1-R6）
- [x] **[H-25]** LSP を Content-Length フレーミングへ変更（2026-11-19、swarm-coding R1-R6）
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

- 2026-11-22 cross-skill 集約（swarm185、2 担当・8 件、観点: 型/所有権 /
  構造化ログ一貫性）: 親が high 1 件（MCP SSE エラー生ログ）を read で事実確認、
  2 件（SSE 固定文言化、切断 warn→info）を即座修正。R4 が chat_stream 開始
  エラーの生ログを指摘し親が status_code のみ記録へ修正。残存（記録のみ）:
  prompt.rs の session_id 構造化、mcp_name/server・retry_attempt/attempt・
  queue_id/input_id・err/error のフィールド別名、event の snake_case 規約、
  TextComplete フック結果の no-op、正常 SSE 終端の error ログ。
- 2026-11-22 cross-skill 集約（swarm183、2 担当・12 件、観点: 設定値検証 /
  状態遷移不変）: 親が high 1 件（MCP timeout 0）と medium 2 件を read で
  事実確認、3 件（timeout 0 fallback、archive:false unarchive、steps=0 既定化）
  を即座 coding。R4 が unarchive の time.compacting 残存を指摘し親が直接修正。
  残存（記録のみ）: start_compacting→finish の Archived/Completed 格下げ、
  abort_incomplete_tools の時刻上書き、shutdown の Busy 残存、StreamProcessor
  attempt リセット、logLevel 不正値、temperature/top_p 範囲、model 区切り子、
  LSP true、URL 形式検証。
- 2026-11-22 cross-skill 集約（swarm182、2 担当・10 件、観点: 文字列エンコーディング
  / ファイル I/O）: 親が high 1 件（auth.json 非 atomic + 権限露出）を read で
  事実確認、5 件（auth atomic 0600、WriteTool 非 UTF-8、添付 truncate、&amp;
  デコード、provider フィルタ）を即座修正。R4 が set_permissions 失敗時の tmp
  残留を指摘し親が直接修正。残存（記録のみ）: apply_patch 部分適用、models
  キャッシュ親ディレクトリ、find/text と markdown の read 失敗スキップ、
  TUI サジェスト非 ASCII 自己除外、auth エラーのパス出力（既存設計）、非 Unix
  の 0600 保証なし（umask は Unix 固有）。
- 2026-11-22 cross-skill 集約（swarm180、2 担当・6 件、観点: 並行/ロック /
  数値単位）: 親が中 2 件を read で事実確認、4 件（数値2+並行2）即座 coding
  完了。R4 が webfetch ボディのタイムアウト未適用と prompt provider エラーの
  生ログを指摘し親が直接修正。残存: TUI Notify permit 合体（ループ先頭で
  キュー再確認があるため実害低いと保留）、prompt/agent の subsession ロックが
  別レジストリ（ID は uuid 付きで衝突稀）、limit クランプ通知ヘッダが
  list_sessions のみ、不正 days が全期間へフォールバック。
- 2026-11-22 cross-skill 集約（swarm178、2 担当・7 件、観点: パス/URL 安全性
  / コレクション境界）: 親が medium 3 件を read で事実確認、7 件すべて即座
  coding 完了。R4 が symlink 走査の不完全さ（glob literal base / find_up）を
  指摘し親が cwd 起点の全 component 検査へ修正。Low も親が import_session_data
  の完全 URL（トークン query を含む可能性）をユーザー向けエラーへ出す経路を
  safe_import_source_label（host+path / ファイル名のみ）へ修正。
- 2026-11-22 技術判断（現状維持、設計判断のため未変更）: コマンド timeout は
  bash 2 分（opencode-tool の `DEFAULT_TIMEOUT_MS`、呼び出し側で上書き可）と
  local-llm 5 分（`OPENCODE_LOCAL_LLM_TIMEOUT_SECS` で上書き可）で用途が異なり
  （一般シェル vs モデルダウンロードを伴う推論）、不一致は意図的と判断。
  limit=0 はサーバが Some(0)=空結果と明記し既存テストも固定しており、
  TUI は 0 を送らないため実害なし。統一には API 契約変更が必要。
- 2026-11-22 cross-skill 集約（swarm176、2 担当・5 件、観点: serde round-trip /
  エラーパス後始末）: 親が中 2 件を read で事実確認、5 件すべて即座 coding
  完了。R4 指摘なし。子の finding: untagged enum 構造上 protocol=Some の
  手作り Remote JSON は File にマッチするが、コード生成の Remote は
  protocol=None のため実害なし。
- 2026-11-22 R4 指摘（ページネーション引数の修正時）: send() の生エラーが
  reqwest の URL（directory パス）を表示し得るため固定文言へ変換（親が修正）。
  limit=0 の意味論は設計判断（0=全件か空か）のため未対応。
- 2026-11-22 R4 残存（run_status の修正時、記録のみ）: Idle 化が 2〜3 経路で
  重複 broadcast される（状態自体は冪等）、tokio runtime 外の Drop では
  Busy が残る（生成経路は常に runtime 内のため通常は発生しない）。親が
  clear_prompt_task の戻り値で後続 Busy を保護する medium を直接修正。
- 2026-11-22 R4 残存（compaction 状態の修正時、記録のみ）: 回帰テストが
  loop_inner の呼び出しサイトを直接検証しておらず、既存の compaction 完了ログ
  に summary（メッセージ先頭最大500文字）が残る（今回の差分外）。親が
  trigger_compaction が None の失敗経路でも Compacting を終端化するよう修正。
- 2026-11-22 R4 残存（subagent 深さ復元の修正時、記録のみ）: 復元後の
  MAX_SUBAGENT_DEPTH ガード実効化の自動テストが欠落（コード追跡では
  ctx.extra と disabled_tools の両方で効くと評価）。
- 2026-11-22 cross-skill 集約（swarm168、2 担当・12 件、観点: 境界値 panic /
  状態不整合）: 親が high 1 件（compaction 状態固定）を read で事実確認。
  境界 panic 6 件は即座に coding 完了、state 不整合 5 件を高 1/中 3/低 2 で
  triage。R4 は境界 panic 6 件すべて指摘なし。
- 2026-11-22 R4 残存（client_ip XFF の修正時、記録のみ）: ログ非出力テストが
  ソース文字列の静的検査に依存（実 tracing subscriber での検証は未実施）。
  親が複数 XFF ヘッダ行の扱い（get_all の最後の行の最右 hop）を直接修正。
- 2026-11-22 R4 残存（limit クランプ通知の修正時、記録のみ）: 回帰テストが
  ハンドラ直呼びで、axum が (HeaderMap, Json) を合成する実 HTTP レスポンス
  経由の検証は未実施。親が x-limit-clamped / x-mcp-sync-warning を
  cors_layer の expose_headers へ追加する medium を直接修正。
- 2026-11-22 R4 残存（OAuth TTL の修正時、記録のみ）: 回収済み state の
  callback が FlowExpired ではなく NoPendingAuth になる（tombstone が必要）、
  再起動後にディスクのみ残ったフローには TTL が効かない（AuthEntry 側に
  発行時刻が必要）、回収失敗で新規 start_auth が全て失敗し得る。親が
  FLOW_TTL を OPENCODE_MCP_OAUTH_FLOW_TTL_SECS で外部化。
- 2026-11-22 R4 指摘（part 冪等化の修正時）: 親が重複分岐でも persist を再試行
  し、sanitize_part_id で Unicode Cf（RLO/ZWSP/BOM 等）と空白のみも除外。
- 2026-11-22 R4 残存（provider キーの修正時、記録のみ）: all.clone() による
  ペイロード約 2 倍は後方互換のための受容済みトレードオフ。
  /config/providers との組み立てロジック重複、回帰テストが純粋関数のみで
  ハンドラ経由の wire 未検証は今後のリファクタ課題。
- 2026-11-22 R4 指摘（SessionRow の修正時、記録のみ）: to_row/from_row は
  本番永続化経路で未使用（server/repository の status カラムが既に保持）。
  回帰 3 件はテスト専用パスのガード。親がテスト JSON リテラルの i32
  オーバーフロー（i64 サフィックス）を直接修正。
- 2026-11-22 R4 残存（strict env の修正時、記録のみ）: 既定警告は変数名を出す
  （値・パスなし、従来挙動）。連鎖 strict テストは inner 直呼びで実ロード経路
  未検証。16 段超の連鎖で未展開参照が残る場合の fail-closed は不完全（極めて稀）。
  親がコメント内 {env:VAR} の誤検出と README の連鎖明記を直接修正。
- 2026-11-22 R4 残存（DELETE 冪等性の修正時、記録のみ）: 500/403 でサーバが
  session_id / パスを本文に埋める場合、既存 error_detail の tracing ログ側では
  マスクされない（今回の差分外）。親が 404 のボディ読み捨て（Keep-Alive
  コネクションのプール復帰）を直接修正。
- 2026-11-22 R4 指摘（wellknown/真値の修正時）: README の
  OPENCODE_AUTO_SHARE が「終了時共有」と記載していたが実装は attach 時の
  セッション作成直後（ローカル run は非対象）。親が実挙動へ修正。
- 2026-11-22 R4 残存（空モデル ID の修正時、記録のみ）: 回帰テストが
  get_model_id 単体で、? 伝播の統合経路（execute/streaming）は未検証。

  StreamEvent::Error の生文字列が CLI stderr へ出る既存経路も本次差分外。
  親が明示指定の空 model_id も InvalidResponse にする medium を修正。
- 2026-11-22 R4 残存（provider リストの修正時、記録のみ）: ExplicitStringList の
  DerefMut が presence bit を迂回し得るが、現状の呼び出し箇所（as_slice().to_vec()
  / into() / merge）では発生していない。公開 API 契約としての明記は今後の課題。
  親が scope 外の型不一致 3 箇所（server/main/routes）を直接修正。
- 2026-11-22 R4 判断（error retryable/SSE の修正時）: 親が HttpTransport の
  SSE-in-POST 経路でもパース失敗を握りつぶさず ProtocolError を返すよう修正
  （SseTransport と同方針）。既存 code 経路の response_body は従来どおり。
- 2026-11-22 R4 残存（--project logLevel の修正時、記録のみ）: 回帰テストが
  startup_log_level の再計算のみで main の init_tracing への配線を検証していない。
  Commands::Attach { dir } にも同種の問題があるが担当外のため未対応。親が
  相対パス/symlink を canonicalize してキャッシュキーを統一する medium を修正。
- 2026-11-22 R4 残存（wellknown/tools の修正時、記録のみ）: wellknown の型不正
  に対する回帰テストが未追加（未知キーのみ検証）。親がトップレベル未知キーの
  サニタイズ、fetch 失敗の URL/生エラー除去、inject_auth_token の URL 除外を
  直接修正。
- 2026-11-22 R4 残存（usage/走査順の修正時、記録のみ）: ストリーミング経路の
  usage デコード失敗は依然無警告（本次は非ストリーミングのみ）。warn ヘルパーと
  テスト用 Subscriber が openai/responses で重複。親が usage:null の誤警告を
  直接修正（Value::Object のみデコード対象）。
- 2026-11-22 R4 残存（config 移行/文書の修正時、記録のみ）: 回帰テストが
  apply_post_load_transforms 単体のみで多層マージ経路を通っていない。
  親が mode 項目自身が mode を持つ場合の上書きと、README の
  $HOME/.opencode / $OPENCODE_CONFIG_DIR 追記を直接修正。
- 2026-11-22 R4 残存（MCP/Anthropic error の修正時、記録のみ）: 平文
  StreamEvent::Error は session 側の構造化エンベロープと非互換で、
  エンドツーエンドでは固定メッセージへ置換される（design decision:
  provider→session のエンベロープ共有が必要）。親が error ペイロード欠落時の
  空文字列・厳格デコード・SSE バッファ上限を直接修正。
- 2026-11-21 cross-skill 集約（swarm147、2 担当・18 件、観点: 設定/environment /
  エラー伝播と型境界）: 親が high 2 件（tools_changed フラグ消耗、
  Anthropic SSE error イベント消失）を read で事実確認、高 2 / 中 15 / 低 7
  を追記。rustxng MCP は禁止されたためローカルコードレビューのみ実施。
- 2026-11-21 R4 残存（LSP/一時ファイルの修正時、記録のみ）: reader の
  publishDiagnostics が close 後に diagnostics を復活し得る点、unregister
  テストが 1 クライアントのみの点は未対応。親が O_EXCL 予約・AppleScript
  エスケープ・通知成功后のマップ削除を直接修正。
- 2026-11-21 R4 残存（PTY 回収の修正時、記録のみ）: prune 回帰テストは
  resources: None の合成エントリのみで、実 JoinHandle/Sender/writer の強参照
  解放を直接検証していない（検証ギャップ）。親が child mutex poison 時に
  wait() をスキップして reaped が立つ medium を修正。
- 2026-11-21 R4 残存（リソース累積の修正時、記録のみ）: shutdown_all が
  clients/auth_bridges を別クリティカルセクションで排水するため並行 load_all
  の bridge が失われる可能性（本番呼び出しなし）。hook_system の古い hook も
  残存し得る。親が clear の writer_lock 削除が待機者を無視する medium を修正。
- 2026-11-21 R4 判断（リソース上限の修正時）: restart 経路でもログを破棄し、
  remove が close 失敗時にステータス/ログ更新をスキップする問題を修正。
  Disconnected ログは意図的に消去（再接続で stale 履歴を残さない方針）。
- 2026-11-21 cross-skill 集約（swarm140、2 担当・18 件、観点: HTTP API 契約 /
  リソース寿命）: 親が API high 3 件（compact URL / revert レスポンス型 /
  fork 引数 transports）を read で事実確認、高 3 / 中 15 / 低 4 を追記。
  TUI のユーザー操作（compact / undo / fork）が実際に失敗する明確なバグ。
- 2026-11-21 R4 残存（ビルトイン auth fail-closed の修正時、記録のみ）:
  fail-closed の実起動経路（new_with_storage_for_url）を通る統合テストが無く、
  ヘルパーのみの検証。CLI 経路は warn + 継続のまま（担当外・server と不一致）。
  PluginLoader::new() 失敗も warn + 継続。init_global が失敗ローダーを先に
  登録する点は前回の世代管理で可視化済み。親が clippy write_with_newline を修正。
- 2026-11-21 R4 残存（config --check の修正時、記録のみ）: 回帰テストが
  clap 解析と中核関数のみで main のディスパッチ分岐を通っていない。
  親が cwd 取得失敗を「設定不正」と別の固定文へ区別するよう修正。
- 2026-11-21 R4 判断（shutdown キュー終端化の修正時）: abort が
  PromptTaskCleanup::drop 経由で再 spawn する指摘に対し、親がキュー ID を
  abort 前に drain し shutting_down フラグで Drop / recover の再スケジュール
  を二段で停止。回収経路の非 redact ログは到達しなくなった。既知 flaky
  （readiness subscriber 競合）は --test-threads=1 で 2929 件 green を確認。
- 2026-11-21 事実確認（WorkerGuard フラッシュ）: server.rs の
  wait_for_os_shutdown が Ctrl-C / SIGTERM を捕捉し graceful shutdown 後に
  main が正常リターンするため `_log_guard` の Drop が走る。SHUTDOWN-09 は
  graceful shutdown 実装（c1bc2d0）で既に解消済み。
- 2026-11-21 誤検知却下（T-05 update_part）: GET /message が返す parts は
  `Vec<MessagePart>`（`SessionMessage.parts`）で、update_part が `MessagePart`
  を要求するのは GET と対称で正しい。`message_v2::Part` は別経路の型で、
  API 契約違反ではないため todo から削除。
- 2026-11-21 R4 判断（MCP 同期警告の修正時）: ボディへ sync_warning を追加
  すると TUI の HashMap<String, McpStatusInfo> deserialize が壊れるため、
  親が固定文言ヘッダ x-mcp-sync-warning 方式へ変更（キー衝突・部分同期の
  文言不一致も同時解消）。生エラー・パスはログ/wire の双方へ出さない。
- 2026-11-21 R4 残存（plugin/session の修正時、記録のみ）: 明示削除が
  ストレージ失敗時も deleted:true を返し部分残存を放置し得る（API 契約変更を
  伴うため記録のみ）。init_global_replace は本番未使用で先勝ちが仕様。
  親が世代採番をロック取得後へ移動し、Installed 分岐のログも guard drop 後へ修正。
- 2026-11-21 R4 残存（入れ子キー警告の修正時、記録のみ）: mcp 直下の未知
  キーを完全除外するため `mcp.srvrs` のような誤記が server id として扱われ
  下位警告に誤記名が残る。provider スキーマの known 集合（baseURL/apiKey）が
  実際の serde フィールド（base_url/api_key）と不一致で警告されない。
  親がユーザー定義名の `*` マスクと有効な false 値の誤警告を直接修正。
- 2026-11-21 R4 残存（設定エラーの修正時、記録のみ）: キー除去プローブが
  ユーザー定義キー（agent 名等）を field path として出し得る点、キー数に
  比例する探索コストの点は未対応（設計判断・未測定）。親が read / file-reference
  エラーのフルパス露出を固定文言化して解消。
- 2026-11-21 R4 残存（overflow/心拍テストの修正時、記録のみ）: plugin の
  1ms ポーリングと 100ms グレースが極端なスケジューラ遅延で競合し得る。
  bash の心拍停止は子孫終了の代理指標で、metadata 失敗が 0 と丸められる。
  親がチャンク容量 128 を STREAM_CHUNK_BUFFER 定数で単一情報源化。
- 2026-11-21 R4 残存（テスト信頼性の修正時、記録のみ）: config の env ロックが
  2 テストモジュールで分離しているが変数名が衝突しないため実害なし。
  親が unwatch 負の検証で in-flight の遅延イベントを誤検知する問題を修正。
- 2026-11-21 cross-skill 集約（swarm125、2 担当・13 件、観点: 型境界/所有権 /
  テスト信頼性）: 親が T-01 の二重エンコードを read で事実確認（create は
  `Json<T>`、update は `Json<String>` で保存され decode が CorruptData）。
  高 1 / 中 7 / 低 6 を追記。既知の known flaky と同一原因は除外。
- 2026-11-21 R4 残存（設定ロード失敗警告の修正時、記録のみ）: 初期化前バッファ
  経路は本番では到達不能（全呼び出しが TRACING_READY 後）でテスト専用。
  直接 emit と replay でログ field 構成が異なる点、同一 cwd 重複を排除しない
  点も low で記録のみ。
- 2026-11-21 R4 残存（provider 警告/frontmatter の修正時、記録のみ）: frontmatter
  に不正値が複数ある場合、失敗フィールドが `<unknown>` にフォールバックする
  （値・本文は出ず、単一不正時は正しく特定できるため必須修正ではない）。
- 2026-11-21 R4 残存（graceful/警告 replay の修正時、記録のみ）: final flush の
  回帰テストが呼出回数のみで生エラー非露出を実際には検証していない。
  親が TUI の早期 return 時に停止シーケンスが飛ぶ問題と signal feature
  コメントの矛盾を直接修正。
- 2026-11-21 cross-skill 集約（swarm120、2 担当・19 件、観点: シャットダウン /
  設定検証と案内）: 親が 4 件の root を read で事実確認（graceful shutdown 不在、
  TUI abort、final flush 不在、設定警告の初期化前発行）、高 5 / 中 17 / 低 3 を
  追記。既知の PluginLoader::shutdown_all 未呼び出しとは別の指摘に絞った。
- 2026-11-21 R4 残存（永続化/境界の修正時、記録のみ）: auth.json が非アトミック
  書き込みで chmod も事後実行（途中失敗・権限露出の懸念）。ログアウト失敗時に
  静的ヘッダー併用サーバーへ古い OAuth マネージャが注入され得る。
  親が remove_oauth の戻り値をディスク削除成否のみへ修正し、再起動相当の
  回帰テストを追加。
- 2026-11-21 cross-skill 集約（swarm116、2 担当・20 件、観点: 定数整合 /
  エラー経路と回復）: 親が 5 件の根拠を read で事実確認し、高 5 / 中 23 /
  低 5 を追記。特に models.dev の「サイズ超過と警告しつつ利用」は矛盾が
  明確。設計判断が必要な fail-open 系は speculation と明記。
- 2026-11-21 R4 判断（移行 fingerprint の修正時）: loader の JSON 存在ガードに
  より旧 TOML の再移行は設計上行われない。R2 の fingerprint 追跡は無効かつ
  余分な再ロードを誘発したため R5 で OnceLock へ復元。既存 JSON を上書き
  する再移行はユーザー設定破壊のため採用しない。再レビューは自動修正 2 回
  目として省略。
- 2026-11-21 R4 判断（完了ログ/variant の修正時）: 完了ログの path/query が
  TraceLayer 既定スパンから出る指摘は今回の差分外（既存挙動）のため却下。
  実行時 request_id のテスト欠如・源码ガード依存は記録のみ。Mutex の await
  跨ぎは OnceCell generation 分離で修正済み。
- 2026-11-21 R4 指摘（認証通知/設定反映の修正時）: README の表で環境変数行が
  restart_required に現れる誤解を与える旨を指摘され、親が「PATCH 対象外」
  列へ修正済み。
- 2026-11-21 R4 残存（readiness/TUI env の修正時、記録のみ）: ping reason の
  分類が sqlx Display のサブストリング依存で、実 sqlite の BUSY/CORRUPT を
  通す統合テストが無い。RestoreTuiEnv のテストはローカル helper のみで
  run_tui 実経路のガード配置は未検証。
- 2026-11-21 R4 残存（loader warn/redact の修正時、記録のみ）: 複数語の機微値
  （"secret: ab cd ef"）は値の終端判定が曖昧なため 1 語目のみマスク。
  camelCase と OAuth 系キーの suffix 正規化は対応済み。誤マスク（token /
  bearer の非機微文脈）と suffix リストの漏れ（auth / passwd 等）は未対応。
- 2026-11-21 R4 残存（observability/dogfood の修正時、記録のみ）: redact の
  キー一覧が完全一致のみで client_secret / access_token 等を漏らし得る。
  rotation は本番 HTTP 経路へ未配線のまま（NOTE 明記）。restart_required の
  mdnsDomain と README の表が非同期。clap の Models.refresh は残存。
  親が 401 の旧契約 `error: "unauthorized"` を互換分岐で復元。
- 2026-11-21 cross-skill 集約（swarm109、4 担当・32 件、観点: dogfood /
  テストギャップ / observability / 状態ライフサイクル）: 親が high 6 件
  （SCHEMA_VERSION 2 vs 3、ProviderError の相関 ID 欠落、永続化失敗の
  サイレント化、restart_required の log_level 漏れ、PLUGIN_LOADER の
  OnceLock 固定、USER_GUIDE の debug agent 等）を read で事実確認し、
  high 12 / 中 15 / 低 6 を追記。前回観点と重複しないよう既知対応を
  プロンプトに明示した。
- 2026-11-21 R4 残存（Markdown 表/wrap_spans の修正時、記録のみ）: 列幅算出は
  UnicodeWidthStr（書記素考慮）だが切り詰めは UnicodeWidthChar（文字単位）で、
  ZWJ 絵文字等で基準が一致しない懸念（実害未確認・低優先）。
- 2026-11-21 R4 残存（Plugin/Todo 型の修正時、記録のみ）: types 側の
  TodoInfo/TodoItem は実利用がなく session の TodoInfo は未集約。TodoStatus/
  Priority の serde 正式表現が snake_case へ変化（Deserialize alias は旧形式も
  受ける、Serialize 側の旧形式互換は無し）。いずれも実害なし。
- 2026-11-21 R4 残存（MCP progress/OAuth の修正時、記録のみ）: remove_auth が
  prepare slot をロック中に削除でき、logout と並行した再 prepare で二重登録
  され得る（低確率・設計判断）。親が R4 high（transport ID 取り違え）を
  request_lock 直列化で修正、さらに auth store の read-modify-write 競合を
  write_lock で解消、server 側テスト 5 箇所をフローキー対応へ更新。
- 2026-11-21 R4 残存（stream/フックの修正時、記録のみ）: shutdown 回帰テストは
  観測用 task のみで実 trigger の abort は未検証。SSE の [DONE] 後に Inner while を
  抜けても外側 loop は継続し、接続を維持する上流では task が残る（既存挙動）。
- 2026-11-21 R4 残存（plugin/tool/mcp の修正時、記録のみ）: plugin と mcp の
  redactor が分立しておりマスク対象に差がある（共通化は設計判断）。両 redactor
  は key=value 形式のみ対応し key: value / JSON 形式は未カバー（doc で対象を
  明記）。local-llm は library 依存が無いだけで opencode.jsonc の MCP サーバと
  しては有効で、doc を実態へ修正済み。
- 2026-11-21 実装メモ（設定階層/retryable の修正時）: R4 が指摘した
  HOME/.opencode と OPENCODE_CONFIG_DIR の設定階層消失を load_project 側の
  明示読み込みで吸収。storage は意図的に非公開のため Cargo.toml へ
  publish = false を明記（schema 定数は pub(crate)）。
- 2026-11-21 R4 残存（設定キャッシュ/protocol/hook の修正時、記録のみ）:
  config_for の read→load→write に TOCTOU があり、同一 cwd の並行呼び出しで
  load_config が重複実行され得る（結果の整合性は保たれる）。担当外ドキュメント
  （CHANGELOG/MAPPING/investigation-final-report）に McpProtocol::Auto / probe
  未実装の古い記述が残る。
- 2026-11-21 実装メモ（リソース/ドキュメントの修正時）: task 担当が
  executor.rs の子 ToolContext が extra 空で作り直される旨を指摘。親が
  AgentExecutor に subagent_depth を追加し、SubsessionState 経由で子へ +1
  伝播するよう配線（実再帰経路でも上限が効く）。永続化復元側は depth=0。
- 2026-11-21 R4 残存（storage/mcp の修正時、記録のみ）: user_version=3 なのに
  messages.metadata/usage 列が欠ける不整合 DB は自己修復されない（エッジケース）。
  起動時 tolerant 一覧のコメント（session 破損はスキップ）と message 側 fail-loud
  の二層方針が矛盾。互換ラッパ read_all_or_default / get は fail-open のまま
  （doc で明記、deprecated）。
- 2026-11-21 R4 残存（config/wire/glob の修正時、記録のみ）: SSE の
  todo.updated ペイロードは旧値のまま（TUI は GET /session/{id}/todo 経由のため
  今回の退化は解消）。AgentColor::Custom("primary") 等組み込み名と衝突する
  文字列は新 wire では Primary 等へ復元される（常時文字列化に内在する制約）。
- 2026-11-21 検証メモ（リソース境界/ドキュメントの修正時）: workspace 全体の
  cargo test が初回のみ `secure_auth_store_behaviour` で failed（MCP auth file の
  env 競合）となったが、単体再実行と workspace 再実行で pass（flaky）。
  investigation-final-report の status を Superseded へ変更（OKF status enum 外、
  バリデータ導入時に要確認）。remotelimit 担当が変更を返さなかったため親が直接実装。
- 2026-11-21 R4 残存（config/API/ADR の修正時、記録のみ）: 正規化ロジックが
  opencode-config と server 側でミラー状態（長期的には pub 関数を共有）、
  PATCH の restart_required は plugin/watcher 等を未列挙。
- 2026-11-20 cross-skill 集約（swarm93、4 担当・38 件、観点: リソース上限 /
  API 境界 / ADR 整合 / 設定の実効性）: high 10 / 中 16 / 低 12 を追記。親が
  high 4 件（timezone の merge 欠落・AgentError 2 定義・SessionOps の
  async_fn_in_trait・ADR-0002 の always 記述）を read で事実確認。前回観点
  （wire / レンダリング / エラー伝播 / 依存）とは重複しないよう既知対応を
  プロンプトに明示した。
- 2026-11-20 R4 残存（ログ/health の修正時）: 3 件とも次ラウンドで対応済み
  （liveness の認証除外・InputRepository::ping() による O(1) readiness・
  削除失敗の WARN と失敗数返却）。
- 2026-11-20 R4 残存（health/ping/logwarn の修正時、記録のみ）: /api 配下の
  liveness エイリアスは認証除外されない旨を README へ明記（ルート 3 パスのみ
  無認証）。無認証 liveness が CARGO_PKG_VERSION を公開すること、
  CLI 起動待ちテストが 1 回の TCP read に完全なリクエスト行を期待すること
  （仮説・再現根拠なし）は未対応。
- 2026-11-20 R4 残存（high 11 件の修正時）: SSE 経路の生エラー漏洩は
  safe_prompt_failure_client_info 経由で修正済み。decode_stream_error の
  message 非 redact、MCP OAuth 本線の fail-open、question のクリック判定、
  wrapped_row_count の単語境界、session 側 serde 属性は次ラウンドで対応済み。
  残存は messages テーブルに metadata/usage 列が無く DB 往復で失われる問題
  （v3 マイグレーション要）のみ。
- 2026-11-20 R4 残存（stream/OAuth/レンダ 4 件の修正時、記録のみ）: OAuth
  callback の保存確認が has_stored_tokens の互換ラッパのまま、response_body
  preview と encode 側 Other が非 redact、引用符付き credential が session 側
  redact 正規表現で伏せられない、相関 ID がボディ読取失敗時に 'unavailable'
  へ落ちる。いずれも low・多層防御の問題で実害は限定的。
- 2026-11-20 cross-skill 集約（swarm88、4 担当・36 件、観点: wire 形式 /
  レンダリング / エラー伝播 / 依存 feature）: high 11 / 中 25 を追記。親が
  high 4 件（audit.toml の stale ignore・LSP feature 未配線・Google の key=・
  SessionStatus の PascalCase）を read で事実確認。前回観点（並行 / テスト品質 /
  UX / 設定運用）とは重複しないよう既知対応をプロンプトに明示した。
- 2026-11-20 R4 残存（テスト品質 8 件の修正時、記録のみ）: constant_time_eq の
  戻り値検証では定数時間性（短絡の有無）を検出できないため、テスト名とコメントを
  検証内容に合わせて改名し、実装・レビューで担保する方針へ変更した。
  TUI の STATE_DIR_LOCK は home.rs / prompt.rs のヘルパのみが参加し、
  app.rs / app_context.rs / session.rs の AppContext::new() テストは並行時に
  一時 OPENCODE_STATE_DIR を observe し得る（Drop ガード化も未対応）。
- 2026-11-20 R4 残存（config/UX/並行の修正時、記録のみ）: 更新 API は
  `.opencode/` と継承祖先設定を書き込まず、実効 `.opencode/opencode.json` が
  あるとパッチが次回ロードでシャドーされる旨を docs へ明記した。
  SSE Drop abort の回帰テストは tokio AbortHandle に is_aborted() が無いため
  二重 abort が安全なことのみを検証（実装の Drop abort は維持）。
  R4 の「{file:...} 側が不完全」という指摘は実コードと逆で、不完全だったのは
  {env:...} 側だった（子が実コードを確認して {file:...} を修正）。
- 2026-11-20 R4 残存（config/storage の修正時、記録のみ）: atomic write が
  親ディレクトリを fsync しないため、クラッシュ時にディレクトリエントリが
  失われる可能性（プロセス中断に対しては atomic、電源断永続性は未保証）。
  jsonc 再シリアライズでコメント/trailing comma が失われるトレードオフは
  doc コメントへ明記済み。server.rs の auth_data_dir は OPENCODE_DATA_DIR の
  サブディレクトリ規約が DB と異なる（scope 外・既存挙動）。
- 2026-11-20 R4 残存（high 10 件の修正時、記録のみ）: stream_message の
  AbortHandle 登録が spawn 後で、極短命タスクでは終了済みハンドルが残り得る
  （AbortHandle::new_pair で先に登録する設計判断が必要）。
- 2026-11-20 cross-skill 集約（swarm83、4 担当・38 件、観点: 並行性 /
  テスト品質 / UX / 設定運用）: high 10 / 中 26 / 低 2 を追記。親が high 4 件
  （DISABLE_* の is_ok、stream の prompt_locks 未取得、plugin trigger の
  RwLock 保持、InternalError の生文字列）を read で事実確認。前回観点
  （TUI 内部 / provider retry・rate-limit / codemode・local-llm・plugin host /
  ADR 整合）とは重複しないよう既知対応をプロンプトに明示した。
- 2026-11-20 R4 指摘（CORS/Renovate の修正時、記録のみ）: LEGACY_CORS_SEED は
  process-global static のまま残存（起動時シード専用、ServerState 生成後に
  pub fn set_cors_whitelist を再呼び出しても既存インスタンスには反映されない旨を
  doc に明記済み）。CLI 側の直接呼び出し移行は scope 外。
  plugin init_global / fetch-proxy も process-global のまま。
- 2026-11-20 R4 指摘（permission Always/TLS の修正時、記録のみ）:
  PermissionRequest.always（再利用候補パターン）が動的 allow の記録に使われず
  patterns（具体値）のみが記録される。UI が always をどう提示するかはローカルに
  契約が無く未確認。安全側の機能劣化であり、設計契約の確定が必要。
- 2026-11-20 R4 指摘（rotation/LazyLock/corpus の修正時）: 2 件とも対応済み。
  rotate_provider_key の doc comment に「HTTP 経路へ未配線」を明記し、
  AGENTS.md のクレート数も 21 に追随。once_cell / lazy_static は syntax.rs と
  クレート Cargo.toml が scope 外で残存（workspace.dependencies 削除は未実施）。
- 2026-11-20 R4 残存（auth/llm/codemode の修正時、記録のみ）: キーローテーション
  は API のみ提供で server.rs の 401/429 検知・反映は未配線、再起動でプール消滅。
  CodeMode の同期 ToolHost では壁時計の完全強制不可（async 化が必要）。
  local-llm の同期 stdout flush が select ループをブロックし得る。
  CodeMode の timeout チェックが単一式中では行われない。
- 2026-11-20 R4 残存（plugin/provider の修正時、記録のみ）: verify_script が
  load_all 開始時の一度のみで spawn 直前の TOCTOU を覆えていない。deno が
  ローカルに未インストールのため plugin-host.ts の deno check / smoke test 未実施。
  load_all が検証失敗時に spawn しない統合テストが不足（単体テストのみ）。
- 2026-11-20 R4 指摘（CLI/session/docs の修正時）: 4 件すべて対応済み。
  docs/opencode-config.md の 55-57 行目（timeout マッピング済み）と 72-73 行目
  （timeout 未マッピング）の内部矛盾は「timeout 側は触らない」指示で未修正。
- 2026-11-20 R4 残存（TUI/retry/plugin の修正時、記録のみ）: App::drop の
  JoinHandle::join が SSE 読み出しタイムアウト（120s）までブロックしうる
  （blocking reqwest のため設計判断）。TS 側で auth.callback が失敗すると
  pending フローが TTL 10 分まで残存（再試行意図なら正常）。terminal::restore が
  run() と Drop で二重呼び出し（crossterm は冪等と推定）。Retry-After のキー比較が
  provider 側は大小文字無視・session 側は小文字完全一致（本番は小文字化され実害なし）。
- 2026-11-20 R4 残存（観測性/整合性 9 件の修正時、記録のみ）: Snapshot::revert は
  1 ファイル失敗でも他を復元する部分適用状態で Err を返す（all-or-nothing 化は
  設計判断）。ToolCallEnd.input が未参照のため Delta を出さないプロバイダでは
  不正 JSON を取りこぼす可能性（confidence 0.4、Delta 専用が前提）。
  README の -D warnings 主張は共通ワークフロー v1 の既定値に依存し未検証。
  docs/architecture.md:110-119 の検証コマンドも CI と不整合（scope 外）。
- 2026-11-20 R4 残存（Bash/Web/CI 8 件の修正時、記録のみ）: process group kill が
  外部 `kill` バイナリ依存で、最小コンテナ等で失敗すると無音フォールバックする
  （nix 等の syscall ラッパ導入は設計判断）。schedule 実行の失敗通知運用は
  docs/security-audit.md が scope 外で未対応。MSRV 1.97 は依存マニフェストの
  要求値から確定したが、cargo +1.97 check の実検証は未実施（現行 1.100 で green）。
- 2026-11-20 R4 残存（中優先 6 件の修正時、記録のみ）: SlashCommandPopup の
  クリック対応は次イテレーションで対応済み。書記素境界は std のみの実用近似で
  UAX #29 の完全規則（GB9c 等）に非対応。
  plugin protocolVersion は欠落時既定値 1 を許容（互換維持）。local-llm の
  ollama CLI が cobra の `--` を解釈する前提は外部確認していない。
  local-llm の直列リクエスト処理・CM-HOST-TIMEOUT は未対応。
- 2026-11-20 R4 残存（ストリーム構造化エラーの修正時、記録のみ）:
  encode_stream_error の Other バリアントが ApiError(String) へ平坦化され、
  status_code とバリアント種別が失われる（現状は非リトライで結果一致だが、
  将来 source 由来を retryable へ拡張するとストリーム経路だけ乖離する）。
  型定義が opencode-provider crate にあるため enum 拡張ではなく JSON
  エンベロープ方式を採用（下位互換維持）。
- 2026-11-20 R4 残存（provider 残存 3 件の修正時、記録のみ）: ストリーミング経路の
  Retry-After 未到達は次イテレーションで対応済み。get_oauth_token は本番コール元が
  無く実質デッドコード（起動時の bootstrap 検知は機能）。refresh 自動更新は未実装。
- 2026-11-20 R4 残存（新観点 high 8 件の修正時、記録のみ）: ADR1-TOKEN は
  トークン配布経路の再定義が必要で仕様変更のため未実施。OAuth 期限検知ヘルパが
  本番リクエストパスへ未配線（expires 単位は epoch ms と仮定で未確認、
  refresh 自動更新も未配線）。本番 HTTP ヘッダを api_error_with_headers 経由で
  渡すアダプタ置換は未実施（受け口と session 側配線のみ完成）。
  models.rs のフォールバック取得だけ共通 Client に未接続。
  TUI の q/Tab/Paste は純粋関数テストのみで handle_event 統合テストが無い。
- 2026-11-20 cross-skill 集約（swarm68、4 担当・38 件、観点: TUI 内部 /
  provider 共通基盤 / codemode・local-llm・plugin host / ADR 整合）:
  high 9 / medium 18 / low 11 を `## 未着手` へ追記。親が high 8 件の根拠を
  read で確認済み。P-01 は「リトライが一切働かない」は誇張で、
  ApiErrorWithStatus バリアントは 429/5xx を既にリトライ可と判定しているが、
  OpenAI 等が ApiError(String) へ平坦化している点は事実。
  ADR1-TOKEN は設計判断（トークン配布経路の再定義）を含むため要検討。
  重複: 旧 discovery の TUI HTTP/cost 系・plugin 信頼境界・SSE バッファ・
  SSE テスト・Litert/M-55 は既往項目として統合した。
- 2026-11-20 R4 残存（中優先 6 件の修正時、記録のみ）: 401 の error フィールドが
  文字列→オブジェクトへ型変更（旧クライアントが error を文字列比較していた場合は
  error.type への移行が必要）、CORS 許可ヘッダの明示列挙（リスト外カスタムヘッダは
  プリフライト拒否）、GET /worktree の 200+[] → 400/502 への破壊的変更。
  auth_decision の定数時間比較・is_auth_enabled の起動時固定化・
  instructions の presence 永続化は次イテレーションで対応済み。
- 2026-11-20 R4 残存（high 11 件の修正時）: うち 5 件は次イテレーションで対応済み
  （WebFetch 5MB 逐次強制、MCP HTTP/SSE 上限＋bounded、bash 行長制限、
  CLI log_dir None、server の summary 復元）。残存は以下のみ:
  - SessionRow の二層方針・sessions.metadata 列はともに対応済み。
    R4 残存: message_repo.list_for_session は依然 fail-loud（既存設計意図で scope 外）、
    LIMIT 適用後に破損行をスキップするため極端な件数では正常行が欠落しうる（実用上は limit=100_000 で影響小）、
    user_version=1 かつ metadata 列ありの中間状態 DB は未 commit のため非該当だが
    PRAGMA ガードは未追加、破損 metadata の list_children は未検証、
    wellknown の MutexGuard await 跨ぎは current_thread ランタイムで安全。
- 2026-11-19 R6 cross-skill 集約（swarm61、7 担当・68 件）: high 11 / medium 34 /
  low 9 を `## 未着手` へ追記。重複・却下は以下:
  - 却下: docs/README.md:49 の PTY リンク（R1-TS05-04）は前回意図的に
    opencode-server.md へ変更済みで opencode-pty.md は stub 化している。
  - 既知: ADR-0008 の opencode-pty カバレッジ行（履歴スナップショット）、
    {file:...} 重複時のコメント判定（R1-AR01-05、メモ済み）、worktree list の
    unwrap_or_default（R1-AR07 は低として重複登録）、Litert スタブ（既知 L-09）、
    Bedrock 完全 eventstream デコーダ（既知 M-55 残存）、TUI /lsp /formatter の
    501 到達不能分岐（既知 501 終点）、@v1 タグピン（AGENTS.md 方針）、
    MCP discovery の URL 未検証（swarm57 で既知）、20ms flaky テストと
    SSE bind 失敗 pass（既知）。
  - 事実確認: 親が `/model` no-op、logging init 無音化、SessionRow fail-open、
    AgentRegistry 握り潰し、transport Content-Length、DNS rebinding、ログエスケープ、
    README:142 の 22-crate を read で確認済み。
  - プロファイル: 無制限（hy4-flash-free）、並列度 7、深度 1、全 7 件が正常完了。
- 2026-11-19 R4 残存（認証/設定 3 件、記録のみ）: `parse_no_auth` が CLI と server に
  複製（共有化は公開 API 追加の判断が必要）、wellknown の `is_safe_env_key` が
  BASH_ENV / NODE_OPTIONS / HTTP_PROXY 等を許容（許可リスト方式の検討課題）、
  `redact_token` が先頭 4 文字と長さを info ログへ出力（生成トークンは 43 文字で
  実害限定的）。TUI の実起動を伴う end-to-end 警告テストも未追加。
- 2026-11-19 R4 残存（設定/CI/ドキュメント 3 件、記録のみ）: CLI main.rs:1939 が
  `is_ok()` 判定で fail-open（server と挙動矛盾。認証強制自体は server 側が行うため
  バイパスは起きず誤通知のみ）、起動時警告が CLI と server で二重、
  警告文が `OPENCODE_NO_AUTH=1` ハードコード（true/yes も受け付けるため不一致）、
  README:158-163 の検証コマンドが CI ゲートと不整合、
  共通ワークフローの extra_gates 複数行実行は CI 初回ログで確認必要。
  履歴文書（cross-review-report-2026-11-17.md:22 の「22 crate」、
  adr-0008:118 の opencode-pty カバレッジ行）は履歴スナップショットとして保持。
- 2026-11-19 discovery（swarm57、観点: セキュリティ/CI/テスト）27 件。対応済み 3 件:
  PermissionEngine の deny 優先化、MCP OAuth ストアの 0600 atomic 化、
  plugin キャッシュ deps の再検証。残存（要 triage、優先度順）:
  - **high/medium**: plugin auth bridge の provider 名キー後勝ち上書き、
    provider/plugin OAuth callback の CSRF state 欠如、MCP discovery の URL 未検証
    （SSRF・平文 token_url）、OPENCODE_NO_AUTH の fail-open 判定（値を見ない）、
    plugin spawn 失敗の握りつぶし、McpToolRegistry full_name の名前空間衝突、
    Bearer 比較の定数時間化。
  - **medium（設定/CI/ドキュメント）**: opencode-pty 削除後のドキュメント未追随
    （22→21 crate、arch/README/MAPPING）、all-features ゲートが check のみ、
    attach -p/--password と run --thinking が no-op、CI master 固定と AGENTS.md の
    main drift、{file:...} コメント判定の誤判定、well-known キャッシュのトークン
    再注入漏れ、experimental.todo のサイレント破棄、MCP timeout ドキュメント stale。
  - **medium（テストギャップ）**: MCP transport のエラー/境界分岐、custom_fetch
    レジストリの poison/並行、TUI terminal init の部分初期化リーク、
    InputRepository::claim_pending の並行 CAS、Storage::new 接続失敗経路。
  - 既知残存: MCP OAuth ストアの read-modify-write 競合（Mutex 化）、
    汚染済み node_modules の purge、approved パスは hook を通さない（既存仕様）。
- 2026-11-19 discovery 新規候補（swarm51）: high 2 件（session の UTF-8 境界 panic、
  apply_patch の stale 破壊）は対応済み。残存（要 triage）:
  - **medium**: openrouter.rs の unwrap、glob/grep の symlink 権限すり抜け、
    worktree.rs の detached HEAD 除外、PermissionBroker/QuestionBroker の pending
    残留と false success は対応済み。残存は multiedit の逐次書き込み部分適用のみ。
  - **low**: すべて対応済み（PTY cursor、replace_all、metadata エラー、
    util の follow_links、SseLineBuffer の 16 MiB 上限、limit クランプ 5000、
    `parse_bool_arg` / `parse_usize_arg` の `crate::tool` 共通化）。
    R4 残存: 16 MiB 超の単一チャンクは extend_from_slice 前に一時確保される、
    上限ドロップ境界でマルチバイトが分断され lossy になる（いずれも病理的ケースで
    実害未計測）。`SessionFilter.limit` は list_filtered へ適用済み。
  - 2026-11-19 対応済み: `*** Update File:` と `@@` hunk のパース、`*** Move To:` の
    全文置換（Add-only hunk を Move に転用しない）を実装。宣言行数との突き合わせで
    カウント検証を強化（R4 の指摘を R5 で修正）。
  - 残存: truncate_bytes が prompt.rs/summary.rs に重複（DRY）、
    末尾改行が Update で失われる（既存仕様の可能性）、同一 path の複数 FilePatch
    が先の変更を上書き、new_start 整合性は debug! のみ。
    hunk 本体の接頭辞なし行がノイズとして無視される（正当行の取りこぼし可能性）、
    裸 `@@` 連続時に old_start を直前 hunk の末尾と仮定（隙間のある正しいパッチが
    拒否されうる）、`@@ -1 +1 @@` の行数省略形は本文から算出する寛容性を採用。
  - 2026-11-19 R4 残存: glob/grep を `follow_links(false)` にしたため正当 symlink
    配下のファイルが検索結果から消えた（権限モデルを変えず最小修正を優先）。
    同種の follow_links(true) が read.rs:327 / ls.rs:158 / skill.rs:161 /
    util/filesystem.rs:112 に残存（別タスク）。Broker の pending map は std Mutex
    化で Drop ガードを実装。API クライアントから見た配信失敗時の 404 は意図どおり。
- M-55 R4 の残存（2026-11-19、記録のみ）: 完全な eventstream フレームデコーダ
  （preamble length / prelude CRC / message CRC）は未実装で、既知 7 キーの JSON テキスト
  検索ヒューリスティックを維持。未知イベント（modelStreamErrorException 等）が
  サイレント破棄されエラー伝播しない、キー直前の `{` 起点がテキスト内 JSON で
  境界ずれしうる、既知キー無しストリームでバッファが無上限に伸びる可能性。
  try_unfold 状態遷移の直接テストも未追加。実フレーム検証は external 送信不可のため未実施。
- H-13 調査（2026-11-19、swarm-coding R2）: 推奨は**イベント駆動・差分同期**
  （server に since/version ベースの増分メッセージ API、SSE の message.updated を
  トリガに 2 秒の全件 get_messages を差分適用へ）。背景の「Tick 2 秒」は不正確で
  TICK_RATE_MS=16、2 秒は session 同期ゲート、5 秒は aux 同期ゲート。常時 501 の
  list_skills/lsp/formatter を 5 秒ごとにポーリングする無駄は副作用なしで除去可能。
  spawn_blocking は TUI に Tokio runtime が無く直接用いられない。未計測で体感は speculative。
- M-04 調査（2026-11-19、swarm-coding R2）: 推奨は**現状維持**。StreamError(e.to_string())
  20 箇所は reqwest::Error の source を失うが、既存バリアント流用は retry/表示意味論が
  変わり回帰。新規 StreamErrorWithSource を追加すると non_exhaustive でない enum の
  下流網羅 match を破壊しうる。InvalidArguments(serde_json) 14 箇所は source 利得が低い。
- M-11 R4 の残存（2026-11-19、記録のみ）: `add_sse` の OAuth 注入は実装済みだが
  `RemoteMcpConfig` に transport 選択肢が無く server `connect()` は `add_http` のみ。
  SSE リモートをサポートするなら transport 指定を追加して `add_sse` を選択する。
- M-11/P-04 R2 の残存（2026-11-19、記録のみ）:
  - MCP OAuth のストアは env グローバル free function ＋ static OnceLock マネージャで
    DI ポイントが無い。テストは env 書換＋static Mutex で直列化しており並列不安全。
    AuthStore trait 注入 or manager 単位パス保持は ServerState 移行とセットで判断。
  - discovery.rs は http_client() が private 固定で注入点なし、dev-dep 未整備。
    server 側 mcp_oauth.rs:1128-1241 に axum 実 HTTP モックがあるため DRY に注意。
  - provider_integration.rs の std::net モックは 1 接続 1 ショットで discovery の
    複数リクエスト/path 分岐にそのまま使えない。bind 失敗 SKIPPED は false green。
  - P-04 は「bootstrap 変換済み／transform 未配線／reasoning 破棄／provider 非対応」
    と定義を再設定。一括 transform 置換は wire 回帰リスクで見送り。
- L-16/18/19 R4 の残存（2026-11-19、記録のみ）:
  - リトライ上限到達時にキュー項目が pending/processing のまま残り、再起動時の
    reset_processing/resume まで同一セッションの後続キューがブロックされうる
    （retry_queued_prompts は session_id しか持たず queue_id を mark できない）。
  - `chat_stream(request).await` の確立中は abort/closed と競合していない（転送ループ
    の select! はアイドル中をカバー）。上流 HTTP の実停止効果は provider の Drop に
    依存し speculative。
  - attempt 伝播チェーンの統合テストが無い（算術テストのみ）。InputRepository を
    差し替えて連続失敗させ、上限で打ち切り・成功でリセットを検証する別タスク。
  - GitEvent::IndexChanged の doc を「`.git` 配下の任意の変更」へ修正済み。
- L-10〜17 R4 の残存（2026-11-19、記録のみ）:
  - `truncation.rs:16` に同種の `floor_char_boundary` が既に存在。grep_tool.rs と
    read.rs にもローカル実装を置いたため 3 重複。共通化は別タスク候補。
  - スキーマを `type:number` → `integer` に変更したため、浮動小数表記（2000.0）を
    送るクライアントは影響を受ける（整数リテラルは従来どおり）。
  - delete_message の 404 化、CORS の localhost port 省略許可はいずれも他ホストを
    緩めていない。DNS rebinding 対策（Host ヘッダ検証）は別途検討課題。
- M-61〜66 R4 の残存（2026-11-19、記録のみ）:
  - `Storage::new`（lib.rs:31）は `after_connect` を適用せず FK cascade が無効なまま
    （Database::new/in_memory は有効）。セッション単位 writer 直列化は実装済み。
  - FK 有効化により `InputRepository::enqueue` が未永続化セッションで失敗しうる
    （routes.rs:2491-2522 はメモリ上の存在しか確認していない）。
  - `remove()` は replace_for_session で全行再挿入するため生存行の created_at が更新される。
  - `MessageRepository::replace_for_session` の破損行チェックはトランザクション外で TOCTOU。
  - watcher は unwatch/stop が event_last_seen をクリアせず再 watch 直後イベントが抑制される。
    debounce キーが PathBuf のみで種別を区別せず Remove→Add が失われうる。
  - writer_locks レジストリはセッション終了後に削除せずセッション数分だけ増える。
- M-60/M-64/M-67 R4 の残存（2026-11-19、記録のみ）:
  - 既存ブランチが元リポジトリ/別 worktree でチェックアウト済みの場合、
    `git worktree add <path> <branch>` は git 側で拒否される（修正前も別エラーで
    失敗しており回帰ではない）。`WorktreeError` の区別化は別タスク。
  - edit の存在確認エラーを `map_err` + `?` で伝播したが、write.rs:104 は
    まだ `try_exists().unwrap_or(false)`（M-58 の同一弱点）。別タスク候補。
  - edit の Err 伝播テストは NUL パスに依存（Linux では決定的、他 OS で不安定なら
    注入可能な存在確認リファクタへ差し替え）。
- M-56〜59 R4 の low/info 指摘（2026-11-19、記録のみ）:
  - responses.rs の reqwest 経路で `flush()` 未呼び出し（改行なし末尾行は旧実装も
    実質未処理のため機能回帰ではない）、CRLF が LF へ正規化（フレーム解析は等価）。
  - find_files の broken symlink が list_files と分類不一致、不正 type は後方互換のため
    未指定扱い（`limit` の serde エラーとは方針差）。
  - write.rs の `try_exists` が Err を `unwrap_or(false)` で丸め、権限エラー時に
    既存ファイルを新規扱いする経路が残存。
  - custom fetch 経路（`CustomFetchChunkStream<Item=Result<String,_>>`）は生バイトを
    保持できず UTF-8 分断問題が残存（型変更が必要な別タスク）。
- H-26 R4 の low 指摘（2026-11-19、記録のみ）: `checkout_failure_is_missing_path`
  の分類関数単体テストのみで、`revert_with_ls_tree` を通した「他理由失敗時に
  ls-tree/削除を行わない」フローは未固定。checkout 実行を注入する fixture で
  ファイル残存を検証するテストが別タスク候補。
- M-11 R4 の low 指摘（2026-11-19、記録のみ）: 既存 reassemble 系テストは
  SseLineBuffer と各 `parse_*` を直接呼ぶだけで try_unfold の pending/exhausted
  キューイングを通さない。vertex/together/vercel/xai には UTF-8 途中分割/CRLF の
  再組立テストが無い（他 12 ファイルにはある）。共通 `bytes_stream` モック
  ハーネスの整備を推奨。
- M-05 の R4 誤検知（2026-11-19、親が grep で confirmed）: R4 は
  「TodoWriteTool の session_id 引数が他セッションの TODO を上書きできる」と
  指摘したが、tool/todo.rs:178 の `ctx.do_todo_update(new_todos)` は session_id を
  取らず ToolContext.session_id（正しいセッション）がコールバックへ渡る。
  input.session_id は ask_permission のみに使用。却下。
- M-05 調査結果（2026-11-19、swarm-coding R2）: 推奨は**最小配線を入れる**
  （現状維持は不可）。todowrite が成功を返すのに ToolContext の
  with_todo_update/with_todo_get が本番未注入で状態が消え、todoread は常に空、
  GET /session/{id}/todo も常に空配列（ServerState の TodoManager 保持は既に
  済みだが update() の本番呼び出し 0 件）。TUI の session_ctx.todos にも
  populate 経路が無く、server→TUI の todo イベントも未定義。
  最小配線は session/prompt.rs（コールバック注入）＋server/routes.rs（broadcast）
  ＋tui/app.rs・session_context.rs（イベント受信）の 4 ファイル・80〜150 行
  （見積りは speculative）。DB 永続化・CLI 実コマンド・TodoStatus 3 重定義は
  後回し。tool.rs 単独の global static は M-21 方針に反し却下。
- M-11 調査結果（2026-11-19、swarm-coding R2）: provider の統合テスト 8 件は
  registry / 純粋ビルダーのみで chat/chat_stream を一度も呼ばない。HTTP モックは
  新規依存不要で std::net::TcpListener + std::thread の 1 ショットが最小。
  推奨次の 1 件はAnthropic chat_stream の SSE 分割フレームテスト＋バッファ修正
  （base_url 注入が既存で本番変更なし、既知残存 H-19 を実 HTTP 境界で潰せる）。
  同一の per-chunk lines() パターンは 15 アダプタに重複。OAuth callback の E2E は
  ストア注入方針が未確定で後回し（discovery.rs の HTTP テストは別件として残す）。
- M-17 調査結果（2026-11-19、swarm-coding R2）: 推奨は「ファサード先行・段階的
  pub(crate) 化」。transform（session/prompt.rs の import 1 行）と registry
  （cli/main.rs の import 1 行）は即時縮小可能。bootstrap / skill は
  ファサードへ未再エクスポートの型があるため追加が必要。provider 実装 12 モジュール
  は意図的な公開 API として pub 維持（意図は speculative）。他ファイル変更を伴うため
  実装は別タスク。
- M-38 調査結果（2026-11-19、swarm-coding R2）: 推奨は「/stream の chat_stream().await
  を tx.closed() と tokio::select! で競わせる」（routes.rs 1 ファイル・5〜10 行・低
  リスク、既存 biased select と同パターン）。provider 側に上流キャンセルの既存 API は
  無く、CancellationToken 追加は 25 実装以上波及で高リスクのため非推奨。/prompt は
  fire-and-forget 契約上クライアント切断検知手段が無く対象外（明示 abort は既存）。
  reqwest future drop による上流停止効果は speculative。
- 子エージェントの誤記（2026-11-19、swarm-coding R4 が発見、親が grep で confirmed）:
  M-46 調査担当は「/prompt は StreamProcessor を駆動する」と報告したが、
  `StreamProcessor::new()` の参照は llm.rs:1873 のテストモジュールのみで本番配線 0 件。
  実際は `prompt_with_update_hook` → `prompt.rs::loop_inner` → `provider.chat_stream()`。
  MAPPING.md の誤記は R5 で修正済み。子のレポートは仮説として事実確認する好例。
- M-46 調査結果（2026-11-19、swarm-coding R2）: 推奨は「現状維持＋ドキュメント
  明記」。/stream と /prompt はレスポンス契約（SSE vs JSON＋broadcast）、
  system prompt、tools/権限、StreamProcessor、キャンセル、永続化、キューがすべて
  異なり、経路統合は破壊的変更（5〜7 ファイル、見積りは speculative）。
  M-38 で AbortHandle 保持が既に見送り済みで、H-10/H-11 未完領域への変更もリスク。
  最小 follow-up は MAPPING.md と stream_message の doc comment に
  「agent/tool なし・生 chat_stream・/prompt と品質/課金が異なる」と明記すること。
- P-01 調査結果（2026-11-19、swarm-coding R2）: 「サーバ↔クライアントの不一致」は
  存在せず（両者とも /api なし）、不一致は upstream の /api/... 契約との間にある
  （upstream 側はローカル間接引用のみで speculative）。推奨は routes.rs の router を
  api_routes() へ切り出し既存ルートを維持したまま `.nest("/api", ...)` を追加する
  A 案。クライアント変更は不要。follow-up: session_id_from_path() の /api 対応、
  "/" の web_index を /api 下に置くかの方針、upstream 126 パスの直接照合、
  todo.md:156-161 と docs/investigation-final-report.md の根拠行更新。
- M-47 の残存（2026-11-19、swarm-coding R4 が指摘、未修正）:
  - 2026-11-19 調査（swarm-coding R2）: 推奨方針は `opencode_permission::evaluate`
    に read かつ dotenv 保護対象（`.env` / `.env.*` / `*.env`、`.example` は除外）なら
    Ask を返す不変チェックを追加（ruleset.rs:79-91、約15-30行）。contains を使わない
    精密 matcher で過剰一致も同時解消し、explore の `read * = Allow` とユーザー
    wildcard allow による上書きも順序に依存せず防ぐ。**ただしユーザーが明示許可した
    .env を読めなくなる政策変更を伴うため、実施には判断が必要**。
    既存テスト agent.rs:1216-1218（secrets.env.local = Allow）の期待値更新が必要。
    target が `*` にフォールバックする経路（../.env 等）は残存。
  - ビルトイン `*.env.*` は contains 一致のため `src/app.env.mock` 等も Ask に該当し
    executor では PermissionDenied になる（過剰一致）。
  - `.env` 保護は default_ruleset のみで、explore エージェント（`read * = Allow`）や
    ユーザー設定の wildcard allow には上書きされる（全エージェントで保証されない）。
  - `wildcard_match` は `src/*.rs` のような中間 `*` をリテラル扱いし、絶対パスも
    `src/*` に一致しない。設定ガイドとテストで未カバー。
  - symlink による脱出は構文的な `..` 解決のみで未対応（新規ファイル作成時に
    canonicalize が失敗するため、祖先 canonicalize＋tail 結合のハイブリッド案）。
  - 文字列判定の FreeUsage 検出は誤検知の余地があり、旧「add credits」案内も弱化。
    構造化フィールド照合と専用エラー型導入が望ましい。
  - `docs/opencode-provider.md:37` が削除済み with_retry API を記載（低優先）。
- M-44/M-45/M-50 の残存（2026-11-19、swarm-coding R4 が指摘）:
  - 2026-11-19 対応済み: CLI 統計の placeholder を撤去し Cost unavailable 表示へ、
    固定値ハンドラ 5 件（list_commands / get_global_config / list_tool_ids /
    list_tools / get_doc）も 501 化済み。TUI 旧レスポンス型も削除。
  - 残存: 実コスト表示には TokenUsage への total_cost 追加と storage/session の
    書き込み経路変更が必要（スキーマ変更、別タスク）。
  - 新規 501 テストはハンドラ直接呼び出しで、router() を通す HTTP 層の検証は未実施。
  - summary/metadata の永続化はスキーマ変更を伴うため別タスク。
- M-43 の残存（2026-11-19、swarm-coding R4 が指摘、却下/未修正）:
  - Linux の PTY master は EOF が EIO（Err）として表面化することがあり、
    `Ok(0)=Exited / Err(_)=Error` の二分だと正常終了が error と報告され得る
    （confidence medium）。実 PTY での確認が必要なため推測に基づく分類変更は避けた。
  - `spawn_blocking` の `reader_handle.abort()` は実行中スレッドを停止できず、
    kill() が read() を解除するまで削除後も broadcast が続く（コメントは実態どおり修正済み）。
    完全停止には shutdown フラグ＋JoinHandle await が必要。
- H-19/H-21/H-22/H-23/H-25 の残存（2026-11-19、swarm-coding R2/R5 が申告、未修正）:
  - `anthropic.rs` は bytes chunk 単位で `lines()` を切るため SSE フレームが chunk を
    またぐと取りこぼす。今回は現状維持（従来と同等の空 TextDelta プレースホルダ）。
  - サブセッションの深さ上限は未実装。`SubsectionState` への depth 追加と
    ポリシー決定が必要なため提案のみ（禁止ツールの伝播は union で維持）。
  - OAuth `refresh_token()` のトークンリクエストにも RFC 8707 の resource を付与すべき
    （今回は start_auth / finish_auth のみ対応）。resource の永続化も未対応。
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
