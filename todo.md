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
  - 未対応: MCP OAuth callback（ストア注入方針が未確定）、discovery.rs の HTTP テスト、
    `try_unfold` を通す実 HTTP モックテスト（`bytes_stream` モック共通ハーネス）。
    `openai.rs:954` 等にも per-chunk from_utf8_lossy の残存あり（横断タスク候補）。
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
- [ ] **[M-16]** 依存方向の逆転（sqlx 未使用 derive は削除済み）
  - 2026-11-19 調査（swarm-coding R2）: 推奨は **charter/ADR 改訂主軸**。
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
- [ ] **[M-37 残り]** 共通 canonicalize ヘルパの async 化
  - 2026-11-19 対応済み: find 系 spawn_blocking、read_file の tokio::fs 化、
    get_file_status / current_project_info の git subprocess を tokio::process 化、
    list_files の read_dir/next_entry/file_type/metadata を非同期化（R4 の
    next_entry Err 打ち切り指摘を R5 で修正）、tool 側 glob/read の WalkDir も
    spawn_blocking 化。
  - 残存: is_within_root / canonicalize_within_root の async 化（呼び出し 5 箇所に波及）。
    性能改善の実測は未実施。
- [x] **[M-38 一部]** /stream の初期切断検知を追加（2026-11-19、swarm-coding R1-R6）
  - chat_stream().await を tx.closed() と biased select! で競合、切断時に future を
    ドロップし永続化してから早期リターン（R4 の永続化回帰指摘を R5 で修正）。
    残存: provider 側の上流キャンセル（CancellationToken 追加は 25 実装以上波及で
    高リスク）、/prompt の fire-and-forget 契約上の切断検知不可（明示 abort のみ）。

### 優先度: 低

- [ ] **[L-04 残り]** tracing の残課題（observability / 効率のみ）
  - 2026-11-19 対応済み: http_request スパン完成（/api session_id 対応、
    TraceLayer on_response で status/latency/outcome、x-request-id 正規化 128 文字）。
    TUI（Tui / Attach / サブコマンドなし）起動時のみログをファイルへ出力し
    stdout 描画干渉を解消。WorkerGuard は main のローカル束縛で保持し終了時
    フラッシュを保証（R4 の static 保持では終了時フラッシュされない指摘を修正）。
  - 残存: prompt/stream ハンドラの開始・完了・切断ログ、/log の extra 平坦化、
    load_config 二重化（いずれもユーザー可視の実害なし）。rolling 未使用は
    ファイルログを本運用する段階で検討。二重初期化は .init() が panic する
    ため呼び出しは 1 箇所に限定（doc 明記済み）。
- [ ] **[L-14 残り]** スナップショット全件 clone の実害計測（現状維持）
  - list_messages の後方互換 pagination は対応済み。2026-11-19 調査で「Arc による
    メッセージ COW」という前提は誤りと判明: メッセージは所有 `Vec<SessionMessage>` で、
    実態は Arc<dyn Fn(&Session)> の update_hook 経由で Session 全体を deep clone
    （1 エミットあたり最大 3 回、TextDelta は 50ms スロットル）。
    Arc::make_mut はスナップショット側が Arc を保持する限り deep clone するため
    O(n) を除去できず、不完全木／構造共有は wire・永続化形式の変更リスクが高い。
    推奨は**現状維持＋実害計測を先行**。まずエミット回数と clone 時間を測り、
    問題が確認されればスナップショット全件送りを増分イベント送りへ変更する検討。
- [ ] **[L-10]** grep ツールの不正 glob が黙って無視され検索範囲が広がる
  - `tool/grep_tool.rs:132-134`。`glob::Pattern::new(g).ok()` が parse 失敗を None
    （フィルタなし）へ変換。glob_tool は InvalidArguments を返しており非対称。
- [ ] **[L-11]** provider/retry.rs の sleep/delay/定数が本番配線ゼロの重複実装
  - `provider/retry.rs:8-11,28-43,58-103`。IsRetryable のみ再エクスポートされ、
    sleep/delay/RETRY_* は参照 0。session/retry.rs を単一情報源へ縮小候補
    （pub API のため外部利用確認が必要、M-49 の別ヘルパとは別）。
- [ ] **[L-12]** parse_anthropic_sse が本番未使用のレガシーラッパー
  - `stream.rs:385-393`。呼び出しはテストのみで anthropic.rs は AnthropicSseParser を
    直接使用。tool_use id が空文字列になる誤挙動を含むラッパーが pub API として残存。
    削除 or pub(crate) 化 or #[deprecated]。
- [ ] **[L-13]** ls ツールが serde 解析失敗を既定値にフォールバック
  - `tool/ls.rs:97-102`。`unwrap_or(LsInput{path:None,...})` のため型違反でも path="." で
    実行され意図しない permission プロンプト。他ツールと入力検証方針が不整合。
- [ ] **[L-14b]** read/bash/grep の数値・真偽引数が型違反/負値を黙って既定値へ丸める
  - `tool/read.rs:100-105`, `bash.rs:91`, `grep_tool.rs:94-96`。
    as_u64().unwrap_or /
    as_bool().unwrap_or のため offset=-1 が 1 に丸まる等。InvalidArguments を返すか
    型付き Input 構造体へ寄せる。
- [ ] **[L-15]** delete_message が存在しない message_id でも 200 deleted:true
  - `server/routes.rs:1151-1163,1273-1277`。remove_message の戻り値を破棄。delete_part は
    NotFound を返しており同一 API 内で契約が不一致。
- [ ] **[L-16]** retry_queued_prompts に上限・バックオフがなく永続失敗時に無限リトライ
  - `server/routes.rs:2243-2247,2284-2289`。DB 破損等で 1 秒間隔が終了条件なしに回り
    エラーログとタスク生成が際限なく増える。H-10/H-11 とは別の失敗時終端化欠落。
- [ ] **[L-17]** CORS localhost 許可が明示的ポートを要求
  - `server/server.rs:940-943,1047-1062`。is_localhost が port.is_some() を要求するため
    ブラウザがポート省略する既定ポート（`http://localhost`）の Origin が拒否されうる。
- [ ] **[L-18]** GitWatcher の BranchChanged / CommitMade が一切発行されない
  - `core/git_watcher.rs:6-15,31-36`。受け取った Event を _event として無視し常に
    IndexChanged を send。バリアント削除 or HEAD/refs 変更の実検出。
- [ ] **[L-19]** llm::stream がキャンセル/受信側切断を次のイベント到達まで検知できない
  - `session/llm.rs:962-995`。abort と tx.send 失敗を stream.next() 後のループ先頭でしか
    見ないためアイドル中は上流 HTTP ストリームと spawn タスクが生存。select! で
    abort.cancelled() / rx.closed() を競合させる（上流停止効果は speculative）。

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

### 優先度: 高（swarm review+discovery 2026-11-19）

### 優先度: 中（残タスク調査で追加）

- [ ] **[P-03]** 未配線の設定フィールド（MAPPING.md へ明記済み）
  - 2026-11-19 調査（swarm-coding R2）: 推奨は**現状維持（明記のみ）**。
    11 フィールドはいずれも serde parse・known_config_keys・DeepMerge に参加し
    起動失敗やデータ損失は無い。唯一の配線候補は `Config.keybinds`（GET /config
    が値を返すため API 追加不要、TUI 側マージのみ）だが TUI 直接検証が scope 外
    で別タスク。他 10 件は意味論未確定／型不一致／既存経路との競合。
    `Config.enterprise` は loader の managed config（OS 固定ディレクトリ）と
    無関係、`experimental.mcp_timeout` は実効の `McpConfig::timeout` と別経路。
    2026-11-19 対応済み: MAPPING.md:168 の loader.rs 参照を 538-548 → 629-637 へ更新。
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

- [ ] **[M-55]** Bedrock ストリームが AWS eventstream バイナリを per-chunk lossy デコード
  - `provider/bedrock.rs:404-412,513-533`。各 chunk を個別に from_utf8_lossy してから
    parse_bedrock_stream へ渡すため、UTF-8/JSON トークンが境界で切れると U+FFFD に
    固定化される。eventstream は長さ付きフレームで 1 チャンク収まり保証が無い。
    空 TextDelta プレースホルダも残存（confirmed）。
  - 対応: フレーム長/CRC を読むバイト単位 stateful デコーダ、完全フレーム単位で JSON パース。

## 完了

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
  SseLineBuffer と各 parse_* を直接呼ぶだけで try_unfold の pending/exhausted
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
