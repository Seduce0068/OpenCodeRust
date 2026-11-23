use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Path, Query, State,
    },
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::sse::{Event, Sse},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, Mutex, OnceCell, RwLock};
use tokio_stream::{
    wrappers::{BroadcastStream, ReceiverStream},
    StreamExt,
};

use crate::mcp_oauth::{
    LocalMcpConfig, McpOAuthError, McpOAuthManager, McpRuntimeConfig,
    McpServerInfo as McpServerInfoStruct, McpServerLogEntry, RemoteMcpConfig, RemoteOAuthSettings,
};
use crate::oauth::{ProviderAuth, ProviderAuthError};
use crate::pty::{PtySession as PtySessionStruct, PtySubscription};
use crate::worktree::{self, WorktreeError, WorktreeInfo as WorktreeInfoStruct};
use crate::{ApiError, Result, ServerState, MAX_TUI_RESPONSE_QUEUE};
use opencode_agent::{AgentInfo as AgentConfigInfo, AgentMode, AgentRegistry, BuiltinAgent};
use opencode_config::{
    apply_environment_overrides, apply_post_load_transforms, load_config, Config as AppConfig,
    McpServerConfig as LoadedMcpServerConfig, McpTimeoutConfig,
};
use opencode_mcp::client::McpTimeouts;
use opencode_plugin::subprocess::{PluginAuthBridge, PluginLoader, PluginSubprocessError};
use opencode_provider::{
    temperature_for_model, top_p_for_model, AuthInfo, AuthMethodType, ModelsData, ModelsDevInfo,
    ModelsRegistry,
};
use opencode_session::system::{EnvironmentContext, SystemPrompt};
use opencode_tool::create_default_registry;

/// prompt 失敗の内部詳細を error.rs の共通 redact 経路へ渡し、
/// クライアント向けの汎用文言と相関 ID を取り出す。
///
/// [`ApiError::InternalError`] の `IntoResponse` が `redact_sensitive_detail` を
/// 通して詳細を tracing ログへ残し、ボディには汎用文言と相関 ID だけを載せる。
/// ここではそのボディを読み取るだけで、生の `error.to_string()` は一切
/// metadata / add_text / 独自ログへ書き込まない。
async fn safe_prompt_failure_client_info(detail: &str) -> (String, String) {
    let response = ApiError::InternalError(detail.to_string()).into_response();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let correlation_id = body
        .get("error")
        .and_then(|error| error.get("correlation_id"))
        .and_then(|value| value.as_str())
        .unwrap_or("unavailable")
        .to_owned();
    let message = body
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|value| value.as_str())
        .unwrap_or("Internal server error")
        .to_owned();
    (correlation_id, message)
}

#[cfg(test)]
mod prompt_failure_redaction_tests {
    use super::*;

    /// 回帰ガード: prompt 失敗の生エラー文言・トークン・パスが
    /// クライアント向けの値へ流出しないこと。
    #[tokio::test]
    async fn prompt_failure_client_info_is_generic_and_correlated() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let detail = format!(
            "read /home/user/.secrets/auth.json with token={secret} while calling provider"
        );
        let (correlation_id, message) = safe_prompt_failure_client_info(&detail).await;

        assert_eq!(message, "Internal server error");
        assert!(
            uuid::Uuid::parse_str(&correlation_id).is_ok(),
            "correlation id must be a UUID: {correlation_id}"
        );
        for leaked in [secret, "/home/user", "auth.json", "provider"] {
            assert!(!message.contains(leaked), "client message leaked: {leaked}");
            assert!(
                !correlation_id.contains(leaked),
                "correlation id leaked: {leaked}"
            );
        }
    }

    /// 回帰ガード: 失敗ごとに相関 ID が変わり、文言は一定であること。
    #[tokio::test]
    async fn prompt_failure_client_info_uses_fresh_correlation_id() {
        let (id_a, message_a) = safe_prompt_failure_client_info("failure A").await;
        let (id_b, message_b) = safe_prompt_failure_client_info("failure B").await;

        assert_eq!(message_a, "Internal server error");
        assert_eq!(message_b, message_a);
        assert_ne!(id_a, id_b, "each failure needs its own correlation id");
    }
}

#[cfg(test)]
mod compaction_kill_switch_tests {
    /// 回帰ガード (swarm263-r4): load_config 失敗時の既定 Config へ環境
    /// オーバーライドを適用してから compaction を組み立てること。
    /// 設定値・パスの内容は検査せず、呼び出し順だけを確認する。
    #[test]
    fn task_route_applies_environment_overrides_before_compaction() {
        let source = include_str!("routes.rs");
        let anchor = source
            .find("let config = match load_config(&cwd) {")
            .expect("task route must fall back to a default config");
        let tail = &source[anchor..];
        let apply_at = tail
            .find("apply_environment_overrides(&mut config);")
            .expect("environment overrides must run on the default config");
        let compaction_at = tail
            .find("let compaction_config = config")
            .expect("compaction config must be built afterwards");
        assert!(
            apply_at < compaction_at,
            "kill switches must be applied before compaction is derived"
        );
    }
}

/// アプリケーション全体のルータ。
///
/// 既存の全ルート（prefix なし）を維持したまま、同じハンドラを `/api` 配下にも
/// 公開する。upstream の `/api/...` 契約に合わせたクライアント向けの互換口であり、
/// 既存ルート・wire format・ハンドラは一切変更しない。
pub fn router() -> Router<Arc<ServerState>> {
    // `Router<Arc<ServerState>>` は Clone なので api_routes() を 2 回呼んで
    // 独立したルータを生成し、片方を `/api` に nest してマージする。
    api_routes().merge(Router::new().nest("/api", api_routes()))
}

/// prefix なしの API ルート定義（`router()` の本体）。
///
/// `router()` から 2 回呼ばれ、1 回目は既存パス、2 回目は `/api` 配下の
/// エイリアスとして使用される。
fn api_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(web_index))
        // `/health` と `/health/live` は liveness（既存の静的 ok を維持）。
        // `/health/ready` は DB を実際に問い合わせる readiness。
        .route("/health", get(health))
        .route("/health/live", get(health))
        .route("/health/ready", get(readiness))
        .route("/event", get(event_stream))
        .route("/path", get(get_paths))
        .route("/vcs", get(get_vcs_info))
        .route("/command", get(list_commands))
        .route("/agent", get(list_agents))
        .route("/skill", get(list_skills))
        .route("/lsp", get(get_lsp_status))
        .route("/formatter", get(get_formatter_status))
        .route("/auth/{id}", put(set_auth).delete(delete_auth))
        .route("/doc", get(get_doc))
        .route("/log", post(write_log))
        .nest(
            "/session",
            session_routes().layer(middleware::from_fn(inject_opencode_directory)),
        )
        .nest("/provider", provider_routes())
        .nest("/config", config_routes())
        .nest("/mcp", mcp_routes())
        .nest("/file", file_routes())
        .nest("/find", find_routes())
        .nest("/permission", permission_routes())
        .nest("/project", project_routes())
        .nest("/pty", pty_routes())
        .nest("/question", question_routes())
        .nest("/tui", tui_routes())
        .nest("/global", global_routes())
        .nest("/experimental", experimental_routes())
        .nest("/plugin", plugin_auth_routes())
}

/// Injected by middleware from x-opencode-directory header for session create.
#[derive(Clone)]
struct OpenCodeDirectory(pub Option<String>);

async fn inject_opencode_directory(mut request: Request<axum::body::Body>, next: Next) -> Response {
    let dir = request
        .headers()
        .get("x-opencode-directory")
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);
    request.extensions_mut().insert(OpenCodeDirectory(dir));
    next.run(request).await
}

fn session_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_sessions).post(create_session))
        .route("/status", get(session_status))
        .route(
            "/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/{id}/children", get(get_session_children))
        .route("/{id}/todo", get(get_session_todos))
        .route("/{id}/fork", post(fork_session))
        .route("/{id}/abort", post(abort_session))
        .route("/{id}/share", post(share_session).delete(unshare_session))
        .route("/{id}/archive", post(archive_session))
        .route("/{id}/title", patch(set_session_title))
        .route("/{id}/permission", patch(set_session_permission))
        .route(
            "/{id}/summary",
            get(get_session_summary).patch(set_session_summary),
        )
        .route(
            "/{id}/revert",
            post(session_revert).delete(clear_session_revert),
        )
        .route("/{id}/unrevert", post(session_unrevert))
        .route("/{id}/compaction", post(start_compaction))
        .route("/{id}/summarize", post(summarize_session))
        .route("/{id}/init", post(init_session))
        .route("/{id}/command", post(execute_command))
        .route("/{id}/shell", post(execute_shell))
        .route("/{id}/message", post(send_message).get(list_messages))
        .route(
            "/{id}/message/{msgID}",
            get(get_message).delete(delete_message),
        )
        .route("/{id}/message/{msgID}/part", post(add_message_part))
        .route(
            "/{id}/message/{msgID}/part/{partID}",
            delete(delete_part).patch(update_part),
        )
        .route("/{id}/stream", post(stream_message))
        .route("/{id}/prompt", post(session_prompt))
        .route("/{id}/prompt/abort", post(abort_prompt))
        .route("/{id}/prompt_async", post(prompt_async))
        .route("/{id}/diff", get(get_session_diff))
}

/// クライアントが指定できる `limit` クエリのサーバ側上限。
///
/// 極端な値（例: `?limit=18446744073709551615`）が巨大リポジトリの全走査や
/// 全メッセージのシリアライズを誘発するのを防ぐ安全上限。超過時はこの値へ
/// クランプする。JSON ボディの wire format は変更せず、クランプの通知は
/// [`LIMIT_CLAMPED_HEADER`] ヘッダで行う。
const MAX_LIMIT: usize = 5000;

/// クエリの `limit` を [`MAX_LIMIT`] 以下へクランプする純粋ヘルパ。
///
/// - `None` はそのまま `None`（未指定時の既存意味論を維持）
/// - `Some(v)` は `v.min(MAX_LIMIT)`（上限ちょうどは不変、上限超のみクランプ）
/// - `Some(0)` はそのまま `Some(0)`（`limit=0` の既存意味論を維持）
fn clamp_limit(limit: Option<usize>) -> Option<usize> {
    limit.map(|v| v.min(MAX_LIMIT))
}

/// クライアントが要求した `limit` が [`MAX_LIMIT`] を超えてクランプされたことを
/// 通知するレスポンスヘッダ名。既存の [`MCP_SYNC_WARNING_HEADER`] と同じ固定
/// ヘッダ方式を採用し、JSON ボディ形状は変更しない。
pub const LIMIT_CLAMPED_HEADER: &str = "x-limit-clamped";

/// クランプ通知ヘッダの固定値。要求値・パス・トークンは一切含めない。
const LIMIT_CLAMPED_VALUE: &str = "true";

/// 元の `limit` が `Some(v)` かつ `v > MAX_LIMIT` の場合にのみ、
/// 固定値 [`LIMIT_CLAMPED_VALUE`] の通知ヘッダを付けた [`HeaderMap`] を返す。
///
/// 未指定 (`None`)・上限以下・上限ちょうどではヘッダを付けない。
/// ヘッダ値は固定文字列のみで、機微情報は載せない。
fn build_limit_clamp_headers(original_limit: Option<usize>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if matches!(original_limit, Some(v) if v > MAX_LIMIT) {
        if let Ok(value) = LIMIT_CLAMPED_VALUE.parse() {
            headers.insert(LIMIT_CLAMPED_HEADER, value);
        }
    }
    headers
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    pub directory: Option<String>,
    pub roots: Option<bool>,
    pub start: Option<i64>,
    pub search: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /session/{id}/message` の後方互換な pagination クエリ。
///
/// 両フィールドとも任意で、未指定の場合は全件を返す（旧 wire format を維持）。
#[derive(Debug, Deserialize)]
pub struct ListMessagesQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub slug: String,
    pub project_id: String,
    pub directory: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub version: String,
    pub time: SessionTimeInfo,
    pub summary: Option<SessionSummaryInfo>,
    pub share: Option<SessionShareInfo>,
    pub revert: Option<SessionRevertInfo>,
    pub permission: Option<PermissionRulesetInfo>,
}

#[derive(Debug, Serialize)]
pub struct SessionTimeInfo {
    pub created: i64,
    pub updated: i64,
    pub compacting: Option<i64>,
    pub archived: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SessionSummaryInfo {
    pub additions: u64,
    pub deletions: u64,
    pub files: u64,
}

#[derive(Debug, Serialize)]
pub struct SessionShareInfo {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct SessionRevertInfo {
    pub message_id: String,
    pub part_id: Option<String>,
    pub snapshot: Option<String>,
    pub diff: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PermissionRulesetInfo {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub mode: Option<String>,
}

fn session_to_info(session: &opencode_session::Session) -> SessionInfo {
    SessionInfo {
        id: session.id.clone(),
        slug: session.slug.clone(),
        project_id: session.project_id.clone(),
        directory: session.directory.clone(),
        parent_id: session.parent_id.clone(),
        title: session.title.clone(),
        version: session.version.clone(),
        time: SessionTimeInfo {
            created: session.time.created,
            updated: session.time.updated,
            compacting: session.time.compacting,
            archived: session.time.archived,
        },
        summary: session.summary.as_ref().map(|s| SessionSummaryInfo {
            additions: s.additions,
            deletions: s.deletions,
            files: s.files,
        }),
        share: session
            .share
            .as_ref()
            .map(|s| SessionShareInfo { url: s.url.clone() }),
        revert: session.revert.as_ref().map(|r| SessionRevertInfo {
            message_id: r.message_id.clone(),
            part_id: r.part_id.clone(),
            snapshot: r.snapshot.clone(),
            diff: r.diff.clone(),
        }),
        permission: session.permission.as_ref().map(|p| PermissionRulesetInfo {
            allow: p.allow.clone(),
            deny: p.deny.clone(),
            mode: p.mode.clone(),
        }),
    }
}

async fn persist_sessions_if_enabled(state: &Arc<ServerState>) {
    if let Err(err) = state.sync_sessions_to_storage().await {
        // 生の anyhow::Error を `{}` で出すと DB パスやトークン値がログへ漏れる。
        // 共通ヘルパで相関 ID を払い出し、redact 済みの構造化フィールドだけを残す。
        // クライアント伝播は API 契約を壊さないため行わない（方針は report 参照）。
        let _correlation_id = crate::error::log_error_with_correlation_id(
            "session sync to storage",
            "session_sync_failure",
            &crate::error::redact_sensitive_detail(&err.to_string()),
        );
    }
}

async fn list_sessions(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ListSessionsQuery>,
) -> Result<(HeaderMap, Json<Vec<SessionInfo>>)> {
    // クランプ判定は元の query.limit に対して行う（クランプ後の値では検出できない）。
    let headers = build_limit_clamp_headers(query.limit);
    let limit = clamp_limit(query.limit);
    let filter = opencode_session::SessionFilter {
        directory: query.directory,
        roots: query.roots.unwrap_or(false),
        start: query.start,
        search: query.search,
        limit,
    };
    let manager = state.sessions.lock().await;
    let sessions = manager.list_filtered(filter);
    let infos: Vec<SessionInfo> = sessions.into_iter().map(session_to_info).collect();
    Ok((headers, Json(infos)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[derive(Default)]
pub(crate) enum SessionRunStatus {
    #[default]
    Idle,
    Busy,
    Retry {
        attempt: u32,
        message: String,
        next: i64,
    },
}

async fn set_session_run_status(
    state: &Arc<ServerState>,
    session_id: &str,
    status: SessionRunStatus,
) {
    {
        let mut statuses = state.session_run_status.write().await;
        match &status {
            SessionRunStatus::Idle => {
                statuses.remove(session_id);
            }
            _ => {
                statuses.insert(session_id.to_string(), status.clone());
            }
        }
    }

    state.broadcast(
        &serde_json::json!({
            "type": "session.status",
            "sessionID": session_id,
            "status": status,
        })
        .to_string(),
    );
}

async fn session_status(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<HashMap<String, SessionStatusInfo>>> {
    let run_status = state.session_run_status.read().await.clone();
    let manager = state.sessions.lock().await;
    let sessions = manager.list();
    let status: HashMap<String, SessionStatusInfo> = sessions
        .into_iter()
        .map(|s| {
            // session 側の serde 属性を単一情報源として wire 名を導出する。
            let lifecycle_status = serde_enum_name(&s.status);
            let run = run_status.get(&s.id).cloned().unwrap_or_default();
            let (status, idle, busy, attempt, message, next) = match run {
                SessionRunStatus::Idle => (lifecycle_status, true, false, None, None, None),
                SessionRunStatus::Busy => ("busy".to_string(), false, true, None, None, None),
                SessionRunStatus::Retry {
                    attempt,
                    message,
                    next,
                } => (
                    "retry".to_string(),
                    false,
                    true,
                    Some(attempt),
                    Some(message),
                    Some(next),
                ),
            };
            (
                s.id.clone(),
                SessionStatusInfo {
                    status,
                    idle,
                    busy,
                    attempt,
                    message,
                    next,
                },
            )
        })
        .collect();
    Ok(Json(status))
}

#[derive(Debug, Serialize)]
pub struct SessionStatusInfo {
    pub status: String,
    pub idle: bool,
    pub busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub parent_id: Option<String>,
    pub title: Option<String>,
    pub permission: Option<PermissionRulesetInput>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionQuery {
    pub directory: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PermissionRulesetInput {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub mode: Option<String>,
}

async fn create_session(
    State(state): State<Arc<ServerState>>,
    Extension(open_code_dir): Extension<OpenCodeDirectory>,
    Query(query): Query<CreateSessionQuery>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionInfo>> {
    let directory = query
        .directory
        .filter(|d| !d.is_empty())
        .or_else(|| open_code_dir.0.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });
    let session = if let Some(parent_id) = &req.parent_id {
        state
            .sessions
            .lock()
            .await
            .create_child(parent_id)
            .ok_or_else(|| ApiError::SessionNotFound(parent_id.clone()))?
    } else {
        state.sessions.lock().await.create("default", directory)
    };
    persist_sessions_if_enabled(&state).await;
    Ok(Json(session_to_info(&session)))
}

async fn get_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionInfo>> {
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&id).ok_or(ApiError::SessionNotFound(id))?;
    Ok(Json(session_to_info(session)))
}

/// 削除されるセッションのキュー項目を終端化する（冪等）。
///
/// 実行中項目の登録を取り出して failed にし、未着手項目もすべて failed にする。
/// 登録を取り出すことで、後続の `PromptTaskCleanup::drop` が回収（＝再生成）を
/// 行わないようにもなる。DB エラー詳細はログへ出さない（パス等を含み得るため）。
async fn terminalize_queue_for_deleted_session(state: &ServerState, session_id: &str) {
    let Some(repo) = state.input_repo.as_ref() else {
        return;
    };
    if let Some(queue_id) = state.take_prompt_queue_id(session_id) {
        if repo.mark_as_failed(&queue_id).await.is_err() {
            tracing::warn!(
                input_id = %queue_id,
                "failed terminalizing running queued prompt on session deletion"
            );
        }
    }
    match repo.list_pending(session_id).await {
        Ok(pending) => {
            for input in pending {
                if repo.mark_as_failed(&input.id).await.is_err() {
                    tracing::warn!(
                        input_id = %input.id,
                        "failed terminalizing pending queued prompt on session deletion"
                    );
                }
            }
        }
        Err(_) => {
            tracing::warn!(
                session_id = %session_id,
                "failed listing queued prompts on session deletion"
            );
        }
    }
}

async fn delete_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    state
        .sessions
        .lock()
        .await
        .delete(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    // 削除済みセッションの生成を止める。abort とキュー終端化は冪等。
    state.abort_prompt_task(&id);
    terminalize_queue_for_deleted_session(&state, &id).await;
    state.session_run_status.write().await.remove(&id);
    state.prompt_locks.write().await.remove(&id);
    // sync は「追加・更新のみ」へ変更したため、削除は明示 API で反映する。
    // 生エラー（id / パスを含み得る）は出さず、固定文言の構造化 warn のみ。
    if let Err(_error) = state.delete_session_from_storage(&id).await {
        tracing::warn!(
            event = "session_storage_delete_failed",
            "failed to delete session from storage after explicit deletion"
        );
    }
    persist_sessions_if_enabled(&state).await;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn get_session_children(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<SessionInfo>>> {
    let manager = state.sessions.lock().await;
    let children = manager.children(&id);
    Ok(Json(children.into_iter().map(session_to_info).collect()))
}

#[derive(Debug, Serialize)]
pub struct TodoInfo {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
}

/// Todo の wire 値を canonical な表現へ正規化する。
///
/// 旧 PascalCase (`InProgress`, `High`) やハイフン区切り (`in-progress`) が
/// そのままクライアントへ渡ると、snake_case / 小文字しか解釈しない TUI 側で
/// `Pending` / `Medium` へ退化する。`opencode_session` の `parse_status` /
/// `parse_priority` (ともに pub) を単一情報源として `as_str()` に落とす。
fn normalize_todo_wire_values(status: &str, priority: &str) -> (String, String) {
    (
        opencode_session::parse_status(status).as_str().to_string(),
        opencode_session::parse_priority(priority)
            .as_str()
            .to_string(),
    )
}

async fn get_session_todos(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TodoInfo>>> {
    let sessions = state.sessions.lock().await;
    if sessions.get(&id).is_none() {
        return Err(ApiError::SessionNotFound(id));
    }
    drop(sessions);

    let todos = state.todo_manager.get_with_ids(&id).await;
    let items = todos
        .into_iter()
        .map(|entry| {
            let (status, priority) =
                normalize_todo_wire_values(&entry.todo.status, &entry.todo.priority);
            TodoInfo {
                id: entry.id,
                content: entry.todo.content,
                status,
                priority,
            }
        })
        .collect();
    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
pub struct ForkSessionRequest {
    pub message_id: Option<String>,
}

async fn fork_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ForkSessionRequest>,
) -> Result<Json<SessionInfo>> {
    let forked = state
        .sessions
        .lock()
        .await
        .fork(&id, req.message_id.as_deref())
        .ok_or(ApiError::SessionNotFound(id))?;
    persist_sessions_if_enabled(&state).await;
    Ok(Json(session_to_info(&forked)))
}

async fn share_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionShareInfo>> {
    let mut sessions = state.sessions.lock().await;
    let share_url = format!("https://share.opencode.ai/{}", id);
    sessions
        .share(&id, share_url.clone())
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(SessionShareInfo { url: share_url }))
}

async fn unshare_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let mut sessions = state.sessions.lock().await;
    sessions
        .unshare(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(serde_json::json!({ "unshared": true })))
}

#[derive(Debug, Deserialize)]
pub struct ArchiveSessionRequest {
    pub archive: Option<bool>,
}

async fn archive_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ArchiveSessionRequest>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let info = if req.archive.unwrap_or(true) {
        let updated = sessions
            .set_archived(&id, None)
            .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
        session_to_info(&updated)
    } else {
        let updated = sessions
            .unarchive(&id)
            .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
        session_to_info(&updated)
    };
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

#[cfg(test)]
mod archive_session_tests {
    use super::*;
    use opencode_session::SessionStatus;

    async fn state_with_session() -> (Arc<ServerState>, String) {
        let state = Arc::new(ServerState::new());
        let id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id
        };
        (state, id)
    }

    /// archive:false が no-op だった回帰ガード。
    /// 解除後は status=Active / time.archived=None になること。
    #[tokio::test]
    async fn archive_false_reactivates_archived_session() {
        let (state, id) = state_with_session().await;

        let archived = archive_session(
            State(state.clone()),
            Path(id.clone()),
            Json(ArchiveSessionRequest {
                archive: Some(true),
            }),
        )
        .await
        .expect("archive should succeed");
        assert!(archived.time.archived.is_some());
        {
            let sessions = state.sessions.lock().await;
            assert_eq!(
                sessions.get(&id).expect("session should exist").status,
                SessionStatus::Archived
            );
        }

        let restored = archive_session(
            State(state.clone()),
            Path(id.clone()),
            Json(ArchiveSessionRequest {
                archive: Some(false),
            }),
        )
        .await
        .expect("unarchive should succeed");
        assert_eq!(restored.time.archived, None);

        let sessions = state.sessions.lock().await;
        let session = sessions.get(&id).expect("session should exist");
        assert_eq!(session.status, SessionStatus::Active);
        assert_eq!(session.time.archived, None);
    }

    /// archive=true と未指定（既定の archive）の wire 挙動は従来どおり。
    #[tokio::test]
    async fn archive_true_and_default_keep_legacy_behaviour() {
        let (state, id) = state_with_session().await;

        let archived = archive_session(
            State(state.clone()),
            Path(id.clone()),
            Json(ArchiveSessionRequest {
                archive: Some(true),
            }),
        )
        .await
        .expect("archive should succeed");
        assert!(archived.time.archived.is_some());
        let original_archived_at = archived.time.archived;

        let again = archive_session(
            State(state.clone()),
            Path(id.clone()),
            Json(ArchiveSessionRequest { archive: None }),
        )
        .await
        .expect("default archive should succeed");
        assert!(again.time.archived.is_some());
        assert!(again.time.archived >= original_archived_at);
        let sessions = state.sessions.lock().await;
        assert_eq!(
            sessions.get(&id).expect("session should exist").status,
            SessionStatus::Archived
        );
    }
}

#[derive(Debug, Deserialize)]
pub struct SetTitleRequest {
    pub title: String,
}

async fn set_session_title(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<SetTitleRequest>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    session.set_title(&req.title);
    let info = session_to_info(session);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

async fn set_session_permission(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<PermissionRulesetInput>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let updated = sessions
        .set_permission(
            &id,
            opencode_session::PermissionRuleset {
                allow: req.allow.unwrap_or_default(),
                deny: req.deny.unwrap_or_default(),
                mode: req.mode,
            },
        )
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    let info = session_to_info(&updated);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

async fn get_session_summary(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<Option<SessionSummaryInfo>>> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    Ok(Json(session.summary.as_ref().map(|s| SessionSummaryInfo {
        additions: s.additions,
        deletions: s.deletions,
        files: s.files,
    })))
}

#[derive(Debug, Deserialize)]
pub struct SetSummaryRequest {
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub files: Option<u64>,
}

async fn set_session_summary(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<SetSummaryRequest>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let updated = sessions
        .set_summary(
            &id,
            opencode_session::SessionSummary {
                additions: req.additions.unwrap_or(0),
                deletions: req.deletions.unwrap_or(0),
                files: req.files.unwrap_or(0),
                // R4(high): diffs=None は直列化時に省略され、永続化側の
                // opencode_types::SessionSummary.diffs (Vec, serde(default) なし) が
                // "missing field `diffs`" で復元失敗 → 要約が失われる。空配列で正規化する。
                diffs: Some(Vec::new()),
            },
        )
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    let info = session_to_info(&updated);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

#[derive(Debug, Deserialize)]
pub struct RevertRequest {
    pub message_id: String,
    pub part_id: Option<String>,
    pub snapshot: Option<String>,
    pub diff: Option<String>,
}

async fn session_revert(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<RevertRequest>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let updated = sessions
        .set_revert(
            &id,
            opencode_session::SessionRevert {
                message_id: req.message_id,
                part_id: req.part_id,
                snapshot: req.snapshot,
                diff: req.diff,
            },
        )
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    let info = session_to_info(&updated);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

async fn clear_session_revert(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let updated = sessions
        .clear_revert(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    let info = session_to_info(&updated);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

async fn start_compaction(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    session.start_compacting();
    let info = session_to_info(session);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub stream: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MessageInfo {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub parts: Vec<PartInfo>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub finish: Option<String>,
    pub error: Option<String>,
    pub cost: f64,
    pub tokens: MessageTokensInfo,
}

#[derive(Debug, Serialize, Default)]
pub struct MessageTokensInfo {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Serialize)]
pub struct PartInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub part_type: String,
    pub text: Option<String>,
    pub file: Option<MessageFileInfo>,
    pub tool_call: Option<ToolCallInfo>,
    pub tool_result: Option<ToolResultInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MessageFileInfo {
    pub url: String,
    pub filename: String,
    pub mime: String,
}

#[derive(Debug, Serialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ToolResultInfo {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
    /// provider 添付を session 側 `FilePart` に正規化したもの。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<opencode_session::FilePart>>,
}

/// enum の wire 名を serde の `rename_all` から導出する単一の変換経路。
///
/// `opencode_session::SessionStatus` / `MessageRole` に付与した
/// `#[serde(rename_all = "lowercase")]` を共有し、routes 側の手書き
/// lowercase match を排除する。unit enum の直列化は失敗しないため、
/// フォールバックは防御用の空文字列にとどめる（既存の値は変化しない）。
fn serde_enum_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

fn message_role_name(role: &opencode_session::MessageRole) -> String {
    serde_enum_name(role)
}

fn part_type_name(part_type: &opencode_session::PartType) -> &'static str {
    match part_type {
        opencode_session::PartType::Text { .. } => "text",
        opencode_session::PartType::ToolCall { .. } => "tool_call",
        opencode_session::PartType::ToolResult { .. } => "tool_result",
        opencode_session::PartType::Reasoning { .. } => "reasoning",
        opencode_session::PartType::File { .. } => "file",
        opencode_session::PartType::StepStart { .. } => "step_start",
        opencode_session::PartType::StepFinish { .. } => "step_finish",
        opencode_session::PartType::Snapshot { .. } => "snapshot",
        opencode_session::PartType::Patch { .. } => "patch",
        opencode_session::PartType::Agent { .. } => "agent",
        opencode_session::PartType::Subtask { .. } => "subtask",
        opencode_session::PartType::Retry { .. } => "retry",
        opencode_session::PartType::Compaction { .. } => "compaction",
    }
}

fn part_to_info(part: &opencode_session::MessagePart) -> PartInfo {
    let (synthetic, ignored) = match &part.part_type {
        opencode_session::PartType::Text {
            synthetic, ignored, ..
        } => (*synthetic, *ignored),
        _ => (None, None),
    };
    PartInfo {
        id: part.id.clone(),
        part_type: part_type_name(&part.part_type).to_string(),
        text: match &part.part_type {
            opencode_session::PartType::Text { text, .. } => Some(text.clone()),
            opencode_session::PartType::Reasoning { text } => Some(text.clone()),
            _ => None,
        },
        file: if let opencode_session::PartType::File {
            url,
            filename,
            mime,
        } = &part.part_type
        {
            Some(MessageFileInfo {
                url: url.clone(),
                filename: filename.clone(),
                mime: mime.clone(),
            })
        } else {
            None
        },
        tool_call: if let opencode_session::PartType::ToolCall { id, name, input } = &part.part_type
        {
            Some(ToolCallInfo {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            })
        } else {
            None
        },
        tool_result: if let opencode_session::PartType::ToolResult {
            tool_call_id,
            content,
            is_error,
            attachments,
        } = &part.part_type
        {
            Some(ToolResultInfo {
                tool_call_id: tool_call_id.clone(),
                content: content.clone(),
                is_error: *is_error,
                attachments: attachments.clone(),
            })
        } else {
            None
        },
        synthetic,
        ignored,
    }
}

fn message_to_info(session_id: &str, message: &opencode_session::SessionMessage) -> MessageInfo {
    let usage = message.usage.clone().unwrap_or_default();
    let model_id = message
        .metadata
        .get("model_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let model_provider = message
        .metadata
        .get("model_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let model = match (model_provider.as_deref(), model_id.as_deref()) {
        (Some(provider), Some(model)) => Some(format!("{}/{}", provider, model)),
        (None, Some(model)) => Some(model.to_string()),
        _ => None,
    };
    let cost = if usage.total_cost > 0.0 {
        usage.total_cost
    } else {
        message
            .metadata
            .get("cost")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    };

    MessageInfo {
        id: message.id.clone(),
        session_id: session_id.to_string(),
        role: message_role_name(&message.role),
        parts: message.parts.iter().map(part_to_info).collect(),
        created_at: message.created_at.timestamp_millis(),
        completed_at: message
            .metadata
            .get("completed_at")
            .and_then(|v| v.as_i64()),
        agent: message
            .metadata
            .get("agent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        model,
        mode: message
            .metadata
            .get("mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        finish: message
            .metadata
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        error: message
            .metadata
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        cost,
        tokens: MessageTokensInfo {
            input: usage.input_tokens,
            output: usage.output_tokens,
            reasoning: usage.reasoning_tokens,
            cache_read: usage.cache_read_tokens,
            cache_write: usage.cache_write_tokens,
        },
    }
}

fn session_parts_to_provider_content(
    parts: &[opencode_session::MessagePart],
) -> opencode_provider::Content {
    let has_non_text = parts
        .iter()
        .any(|part| !matches!(part.part_type, opencode_session::PartType::Text { .. }));

    if !has_non_text {
        let text = parts
            .iter()
            .filter_map(|part| match &part.part_type {
                opencode_session::PartType::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return opencode_provider::Content::Text(text);
    }

    let content_parts = parts
        .iter()
        .filter_map(|part| match &part.part_type {
            opencode_session::PartType::Text { text, .. } => Some(opencode_provider::ContentPart {
                content_type: "text".to_string(),
                text: Some(text.clone()),
                image_url: None,
                tool_use: None,
                tool_result: None,
                cache_control: None,
                filename: None,
                media_type: None,
                provider_options: None,
            }),
            opencode_session::PartType::ToolCall { id, name, input } => {
                Some(opencode_provider::ContentPart {
                    content_type: "tool_use".to_string(),
                    text: None,
                    image_url: None,
                    tool_use: Some(opencode_provider::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }),
                    tool_result: None,
                    cache_control: None,
                    filename: None,
                    media_type: None,
                    provider_options: None,
                })
            }
            opencode_session::PartType::ToolResult {
                tool_call_id,
                content,
                is_error,
                ..
            } => Some(opencode_provider::ContentPart {
                content_type: "tool_result".to_string(),
                text: None,
                image_url: None,
                tool_use: None,
                tool_result: Some(opencode_provider::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
                    is_error: Some(*is_error),
                }),
                cache_control: None,
                filename: None,
                media_type: None,
                provider_options: None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    opencode_provider::Content::Parts(content_parts)
}

fn session_messages_to_provider_messages(
    session_messages: &[opencode_session::SessionMessage],
) -> Vec<opencode_provider::Message> {
    session_messages
        .iter()
        .map(|message| {
            let role = match message.role {
                opencode_session::MessageRole::User => opencode_provider::Role::User,
                opencode_session::MessageRole::Assistant => opencode_provider::Role::Assistant,
                opencode_session::MessageRole::System => opencode_provider::Role::System,
                opencode_session::MessageRole::Tool => opencode_provider::Role::Tool,
            };
            opencode_provider::Message {
                role,
                content: session_parts_to_provider_content(&message.parts),
                cache_control: None,
                provider_options: None,
            }
        })
        .collect()
}

fn resolve_provider_and_model(
    state: &ServerState,
    request_model: Option<&str>,
    config_model: Option<&str>,
    config_provider: Option<&str>,
) -> Result<(Arc<dyn opencode_provider::Provider>, String, String)> {
    let resolve_from_model = |model: &str| -> Result<(String, String)> {
        state
            .providers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .parse_model_string(model)
            .ok_or_else(|| ApiError::BadRequest(format!("Model not found: {}", model)))
    };

    let (provider_id, model_id) = if let Some(model) = request_model {
        resolve_from_model(model)?
    } else if let Some(model) = config_model {
        if let Some(provider_id) = config_provider {
            (provider_id.to_string(), model.to_string())
        } else {
            resolve_from_model(model)?
        }
    } else {
        let first = state
            .providers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .list_models()
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::BadRequest("No providers configured".to_string()))?;
        (first.provider, first.id)
    };

    let provider = state
        .providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_provider(&provider_id)
        .map_err(|e| ApiError::ProviderError {
            provider_id: provider_id.clone(),
            status: e.status_code(),
            detail: e.to_string(),
        })?;
    if provider.get_model(&model_id).is_none() {
        return Err(ApiError::BadRequest(format!(
            "Model `{}` not found for provider `{}`",
            model_id, provider_id
        )));
    }

    Ok((provider, provider_id, model_id))
}

async fn send_message(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<MessageInfo>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;
    session.add_user_message(&req.content);
    if let Some(variant) = req.variant.as_deref() {
        session
            .metadata
            .insert("model_variant".to_string(), serde_json::json!(variant));
    }
    let assistant_msg = session.add_assistant_message();
    let info = message_to_info(&session_id, assistant_msg);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

async fn list_messages(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
) -> Result<Json<Vec<MessageInfo>>> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;

    // offset/limit が指定されたときだけ skip/take を適用する。
    // 未指定時は従来どおり全件を返し、wire format を維持する。
    // limit=0 は take(0)、範囲外 offset は skip(len 超) により空結果になる。
    // limit は MAX_LIMIT 以下へクランプする（未指定・上限以下は既存挙動を維持）。
    let messages = session.messages.iter();
    let infos: Vec<MessageInfo> = match (query.offset, clamp_limit(query.limit)) {
        (Some(offset), Some(limit)) => messages
            .skip(offset)
            .take(limit)
            .map(|m| message_to_info(&session_id, m))
            .collect(),
        (Some(offset), None) => messages
            .skip(offset)
            .map(|m| message_to_info(&session_id, m))
            .collect(),
        (None, Some(limit)) => messages
            .take(limit)
            .map(|m| message_to_info(&session_id, m))
            .collect(),
        (None, None) => messages.map(|m| message_to_info(&session_id, m)).collect(),
    };
    Ok(Json(infos))
}

async fn delete_message(
    State(state): State<Arc<ServerState>>,
    Path((session_id, msg_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;
    // delete_part と契約を統一する: 存在しない ID は 200 deleted:true ではなく 404 を返す。
    if session.remove_message(&msg_id).is_none() {
        return Err(ApiError::NotFound(format!("Message not found: {}", msg_id)));
    }
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
pub struct AddPartRequest {
    #[serde(rename = "type")]
    pub part_type: String,
    /// R2/API-06: クライアントが再送時に同じ part を識別するための任意 id。
    /// 未指定（または空）の場合は従来どおり UUID を採番する。
    #[serde(default)]
    pub id: Option<String>,
    pub text: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub content: Option<String>,
    pub is_error: Option<bool>,
}

/// 従来どおりの part id (`prt_` + UUID v4 simple) を採番する。
fn new_part_id() -> String {
    format!("prt_{}", uuid::Uuid::new_v4().simple())
}

/// クライアント指定の part id をサニタイズする。
///
/// - 制御文字（Cc）と書式制御文字（Cf）を除去する
/// - 最大 128 文字に切り詰める
/// - 結果が空文字列なら UUID 採番へフォールバックする
fn sanitize_part_id(raw: &str) -> String {
    // Cc（is_control）に加え、Cf（書式制御: RLO / ZWSP / BOM 等）も除外する（R4）。
    // これらは UI 表示やログで視覚的なりすましに使われ得る。
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !is_format_control(*c))
        .take(128)
        .collect();
    if cleaned.trim().is_empty() {
        new_part_id()
    } else {
        cleaned
    }
}

/// Unicode 一般カテゴリ Cf（書式制御文字）かどうか。
fn is_format_control(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}'
            | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}' | '\u{110BD}' | '\u{110CD}'
    )
}

fn build_message_part(req: AddPartRequest, msg_id: &str) -> Result<opencode_session::MessagePart> {
    // R2/API-06: 指定 id があればサニタイズして採用し、未指定なら従来の UUID 採番を維持。
    let id = match req.id.as_deref() {
        Some(raw) => sanitize_part_id(raw),
        None => new_part_id(),
    };
    let part_type = match req.part_type.as_str() {
        "text" => opencode_session::PartType::Text {
            text: req.text.ok_or_else(|| {
                ApiError::BadRequest("Field `text` is required for text parts".to_string())
            })?,
            synthetic: None,
            ignored: None,
        },
        "reasoning" => opencode_session::PartType::Reasoning {
            text: req.text.ok_or_else(|| {
                ApiError::BadRequest("Field `text` is required for reasoning parts".to_string())
            })?,
        },
        "tool_call" => opencode_session::PartType::ToolCall {
            id: req.tool_call_id.ok_or_else(|| {
                ApiError::BadRequest(
                    "Field `tool_call_id` is required for tool_call parts".to_string(),
                )
            })?,
            name: req.tool_name.ok_or_else(|| {
                ApiError::BadRequest(
                    "Field `tool_name` is required for tool_call parts".to_string(),
                )
            })?,
            input: req.tool_input.unwrap_or_else(|| serde_json::json!({})),
        },
        "tool_result" => opencode_session::PartType::ToolResult {
            tool_call_id: req.tool_call_id.ok_or_else(|| {
                ApiError::BadRequest(
                    "Field `tool_call_id` is required for tool_result parts".to_string(),
                )
            })?,
            content: req.content.ok_or_else(|| {
                ApiError::BadRequest(
                    "Field `content` is required for tool_result parts".to_string(),
                )
            })?,
            is_error: req.is_error.unwrap_or(false),
            attachments: None,
        },
        unsupported => {
            return Err(ApiError::BadRequest(format!(
                "Unsupported part type: {}",
                unsupported
            )));
        }
    };

    Ok(opencode_session::MessagePart {
        id,
        part_type,
        created_at: chrono::Utc::now(),
        message_id: Some(msg_id.to_string()),
    })
}

async fn add_message_part(
    State(state): State<Arc<ServerState>>,
    Path((session_id, msg_id)): Path<(String, String)>,
    Json(req): Json<AddPartRequest>,
) -> Result<Json<serde_json::Value>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;
    let message = session
        .get_message_mut(&msg_id)
        .ok_or_else(|| ApiError::NotFound(format!("Message not found: {}", msg_id)))?;

    let part = build_message_part(req, &msg_id)?;

    // R2/API-06: 再送などで同一 id の part が既に存在する場合は二重永続化を避け、
    // `added:false` と既存の part_id を返す。未指定時は毎回新しい UUID が採番されるため
    // この分岐に入らず、従来の挙動（常に追加・`added:true`）は不変。
    let duplicate_id = message
        .parts
        .iter()
        .find(|p| p.id == part.id)
        .map(|p| p.id.clone());
    if let Some(existing_id) = duplicate_id {
        drop(sessions);
        // 初回永続化が失敗していた場合でも、再送時に回復する機会を残す（R4）。
        persist_sessions_if_enabled(&state).await;
        return Ok(Json(serde_json::json!({
            "added": false,
            "session_id": session_id,
            "message_id": msg_id,
            "part_id": existing_id,
        })));
    }

    let part_id = part.id.clone();
    message.parts.push(part);
    session.touch();
    drop(sessions);
    persist_sessions_if_enabled(&state).await;

    Ok(Json(serde_json::json!({
        "added": true,
        "session_id": session_id,
        "message_id": msg_id,
        "part_id": part_id,
    })))
}

#[cfg(test)]
mod add_part_idempotency_tests {
    use super::*;
    use serde_json::json;

    fn text_part_request(id: Option<&str>, text: &str) -> AddPartRequest {
        AddPartRequest {
            part_type: "text".to_string(),
            id: id.map(|raw| raw.to_string()),
            text: Some(text.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            content: None,
            is_error: None,
        }
    }

    async fn state_with_one_message() -> (Arc<ServerState>, String, String) {
        let state = Arc::new(ServerState::new());
        let (session_id, msg_id) = {
            let mut sessions = state.sessions.lock().await;
            let session_id = sessions.create("default", ".").id;
            let session = sessions
                .get_mut(&session_id)
                .expect("created session should be available for mutation");
            let msg_id = session.add_user_message("hello").id.clone();
            (session_id, msg_id)
        };
        (state, session_id, msg_id)
    }

    /// R2/API-06: 同じ id を 2 回送った場合、part は 1 つだけ永続化され、
    /// 2 回目は `added:false` と既存 part_id を返す。
    #[tokio::test]
    async fn explicit_id_retry_adds_only_one_part() {
        let (state, session_id, msg_id) = state_with_one_message().await;

        let first = add_message_part(
            State(state.clone()),
            Path((session_id.clone(), msg_id.clone())),
            Json(text_part_request(Some("prt_fixed_1"), "retry")),
        )
        .await
        .expect("first add should succeed");
        assert_eq!(first.0.get("added"), Some(&json!(true)));
        assert_eq!(first.0.get("part_id"), Some(&json!("prt_fixed_1")));

        let second = add_message_part(
            State(state.clone()),
            Path((session_id.clone(), msg_id.clone())),
            Json(text_part_request(Some("prt_fixed_1"), "retry")),
        )
        .await
        .expect("retry should be handled idempotently");
        assert_eq!(second.0.get("added"), Some(&json!(false)));
        assert_eq!(second.0.get("part_id"), Some(&json!("prt_fixed_1")));
        // レスポンスの他のキーは従来どおり維持される。
        assert_eq!(
            second.0.get("session_id").and_then(|v| v.as_str()),
            Some(session_id.as_str())
        );
        assert_eq!(
            second.0.get("message_id").and_then(|v| v.as_str()),
            Some(msg_id.as_str())
        );

        let sessions = state.sessions.lock().await;
        let session = sessions.get(&session_id).expect("session should exist");
        let message = session.get_message(&msg_id).expect("message should exist");
        let matches: Vec<_> = message
            .parts
            .iter()
            .filter(|part| part.id == "prt_fixed_1")
            .collect();
        assert_eq!(matches.len(), 1, "retry must not duplicate the part");
        assert_eq!(message.parts.len(), 2, "hello + one retry part");
    }

    /// R2/API-06: id 未指定は従来どおり毎回 UUID を採番し、2 件とも追加される。
    #[tokio::test]
    async fn unspecified_id_keeps_uuid_behavior_and_adds_both() {
        let (state, session_id, msg_id) = state_with_one_message().await;

        let first = add_message_part(
            State(state.clone()),
            Path((session_id.clone(), msg_id.clone())),
            Json(text_part_request(None, "first")),
        )
        .await
        .expect("first add should succeed");
        let second = add_message_part(
            State(state.clone()),
            Path((session_id.clone(), msg_id.clone())),
            Json(text_part_request(None, "second")),
        )
        .await
        .expect("second add should succeed");

        assert_eq!(first.0.get("added"), Some(&json!(true)));
        assert_eq!(second.0.get("added"), Some(&json!(true)));
        let first_id = first
            .0
            .get("part_id")
            .and_then(|v| v.as_str())
            .expect("part_id")
            .to_string();
        let second_id = second
            .0
            .get("part_id")
            .and_then(|v| v.as_str())
            .expect("part_id")
            .to_string();
        assert_ne!(first_id, second_id, "omitted id must get a fresh UUID");
        for id in [&first_id, &second_id] {
            let raw = id.strip_prefix("prt_").expect("legacy prt_ prefix");
            assert!(
                uuid::Uuid::parse_str(raw).is_ok(),
                "omitted id must stay prt_UUID: {id}"
            );
        }

        let sessions = state.sessions.lock().await;
        let session = sessions.get(&session_id).expect("session should exist");
        let message = session.get_message(&msg_id).expect("message should exist");
        assert_eq!(message.parts.len(), 3, "hello + both parts");
    }

    /// R2/API-06: 不正な id はサニタイズされるか、UUID へフォールバックする。
    #[test]
    fn invalid_explicit_id_is_sanitized_or_falls_back() {
        assert_eq!(sanitize_part_id("prt_ab\u{1}\ncd"), "prt_abcd");
        let long = "a".repeat(200);
        assert_eq!(sanitize_part_id(&long).chars().count(), 128);
        for raw in ["", "\u{0}\n\t"] {
            let sanitized = sanitize_part_id(raw);
            let raw_uuid = sanitized.strip_prefix("prt_").expect("fallback prefix");
            assert!(
                uuid::Uuid::parse_str(raw_uuid).is_ok(),
                "empty id must fall back to prt_UUID"
            );
        }
    }

    #[test]
    fn build_message_part_honors_explicit_id_and_defaults_to_uuid() {
        let explicit = build_message_part(text_part_request(Some("prt_client_1"), "x"), "msg_1")
            .expect("build explicit part");
        assert_eq!(explicit.id, "prt_client_1");
        let implicit =
            build_message_part(text_part_request(None, "x"), "msg_1").expect("build default part");
        assert!(implicit.id.starts_with("prt_"));
        assert_ne!(explicit.id, implicit.id);
    }

    /// 後方互換: `id` を送らない旧クライアントは問題なくデシリアライズできる。
    #[test]
    fn omitted_id_field_deserializes_to_none() {
        let req: AddPartRequest = serde_json::from_value(json!({"type": "text", "text": "hi"}))
            .expect("legacy request without id should deserialize");
        assert!(req.id.is_none());
    }
}

async fn delete_part(
    State(state): State<Arc<ServerState>>,
    Path((session_id, msg_id, part_id)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;
    let message = session
        .get_message_mut(&msg_id)
        .ok_or_else(|| ApiError::NotFound(format!("Message not found: {}", msg_id)))?;

    let before = message.parts.len();
    message.parts.retain(|part| part.id != part_id);
    if message.parts.len() == before {
        return Err(ApiError::NotFound(format!("Part not found: {}", part_id)));
    }
    session.touch();
    drop(sessions);
    persist_sessions_if_enabled(&state).await;

    Ok(Json(serde_json::json!({
        "deleted": true,
        "session_id": session_id,
        "message_id": msg_id,
        "part_id": part_id,
    })))
}

#[derive(Debug, Serialize, Clone)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    name: Option<String>,
    input: String,
}

async fn send_sse_event(
    tx: &mpsc::Sender<std::result::Result<Event, Infallible>>,
    name: &str,
    payload: StreamEvent,
) {
    if let Ok(event) = Event::default().event(name).json_data(payload) {
        let _ = tx.send(Ok(event)).await;
    }
}

/// ストリームの共通終端処理。
///
/// 正常終了・クライアント切断の両分岐から呼び、受け取り済みの
/// `final_text` / `pending_tool_calls` を漏れなくセッションへ永続化する。
/// 実行状態の Idle 復帰は呼び出し側が行う（既存の源码ガード契約を維持）。
async fn finalize_stream_message(
    state: &Arc<ServerState>,
    session_id: &str,
    msg_id: &str,
    final_text: String,
    pending_tool_calls: HashMap<String, PendingToolCall>,
    stream_failed: Option<String>,
) {
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(message) = session.get_message_mut(msg_id) {
                if !final_text.is_empty() {
                    message.add_text(final_text);
                }
                for (id, pending) in pending_tool_calls {
                    let name = pending.name.unwrap_or_else(|| "unknown_tool".to_string());
                    let parsed_input = serde_json::from_str::<serde_json::Value>(&pending.input)
                        .unwrap_or_else(|_| serde_json::json!({ "raw": pending.input }));
                    let call_id = if id.is_empty() {
                        format!("call_{}", uuid::Uuid::new_v4().simple())
                    } else {
                        id
                    };
                    message.add_tool_call(call_id, name, parsed_input);
                }
                if let Some(error) = stream_failed {
                    if message.parts.is_empty() {
                        // 生のプロバイダエラーを会話へ永続化しない。
                        let (_, safe_message) = safe_prompt_failure_client_info(&error).await;
                        message.add_text(safe_message);
                    }
                }
            }
            session.touch();
        }
    }
    persist_sessions_if_enabled(state).await;
}

async fn stream_message(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> std::result::Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>, ApiError>
{
    let config = state.config.read().await;
    let (provider, provider_id, model_id) =
        resolve_provider_and_model(&state, req.model.as_deref(), config.model.as_deref(), None)?;
    drop(config);

    // `/prompt`（spawn_prompt_execution）と同じセッション単位ロックを、
    // セッション更新の前からストリーム完了まで保持する。OwnedMutexGuard を使う
    // のは、ガードを spawn 先へ 'static で移し、ハンドラがすぐ Sse を返せるようにするため。
    // これにより並行経路での更新が失われない。
    let run_lock = {
        let mut locks = state.prompt_locks.write().await;
        locks
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let run_guard = run_lock.lock_owned().await;

    let (provider_messages, msg_id, selected_variant) = {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;

        session.add_user_message(&req.content);
        // 全履歴を clone せず、ロック保持中に provider 用メッセージへ直接変換する。
        // 変換結果は所有値なのでロック外へ持ち出せる。assistant プレースホルダー
        // 追加前に変換するため、provider へ渡る履歴は従来と同一。
        let provider_messages = session_messages_to_provider_messages(&session.messages);
        let selected_variant = req.variant.clone().or_else(|| {
            session
                .metadata
                .get("model_variant")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        });
        if let Some(variant) = req.variant.as_deref() {
            session
                .metadata
                .insert("model_variant".to_string(), serde_json::json!(variant));
        }
        session.metadata.insert(
            "model_provider".to_string(),
            serde_json::json!(provider_id.clone()),
        );
        session
            .metadata
            .insert("model_id".to_string(), serde_json::json!(model_id.clone()));

        let assistant_msg = session.add_assistant_message();
        (
            provider_messages,
            assistant_msg.id.clone(),
            selected_variant,
        )
    };

    // 実行中ステータスも /prompt と同じ経路で更新する（abort が状態を戻せるようにする）。
    set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;

    let variant = selected_variant.clone();
    let temperature = temperature_for_model(&model_id).or(Some(0.7));
    let top_p = top_p_for_model(&model_id);
    let max_tokens = max_tokens_for_variant(4096, variant.as_deref());
    let request = opencode_provider::ChatRequest {
        model: model_id.clone(),
        messages: provider_messages,
        max_tokens: Some(max_tokens),
        temperature,
        top_p,
        system: None,
        tools: None,
        stream: Some(true),
        variant,
        provider_options: None,
    };

    let (tx, rx) = mpsc::channel::<std::result::Result<Event, Infallible>>(128);
    let stream_state = state.clone();
    let stream_session_id = session_id.clone();
    let stream_msg_id = msg_id.clone();
    // /prompt と同じく、親の AbortHandle 登録をタスク本体より先に確定させる。
    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();

    let stream_task = tokio::spawn(async move {
        // /prompt と同じ後始末（AbortHandle 登録済みタスクの解除）を行う。
        // キュー項目は持たないため、キュー回収は行わない。
        let _cleanup = PromptTaskCleanup {
            state: stream_state.clone(),
            session_id: stream_session_id.clone(),
            task_id: tokio::task::id(),
            queue_id: None,
            claimed: Arc::new(AtomicBool::new(false)),
            queue_finalized: Arc::new(AtomicBool::new(true)),
        };
        // 親が AbortHandle を登録し終えるまで待ち、早期 return 時の
        // cleanup が残骸を残さない順序を保証する。
        let _ = start_rx.await;
        // ハンドラで取得したセッション単位ロックを完了まで保持する。
        let _run_guard = run_guard;
        send_sse_event(
            &tx,
            "message_start",
            StreamEvent {
                event_type: "message_start".to_string(),
                content: None,
                message_id: Some(stream_msg_id.clone()),
                done: None,
                tool_call_id: None,
                tool_name: None,
                input: None,
                prompt_tokens: None,
                completion_tokens: None,
                error: None,
            },
        )
        .await;

        let mut final_text = String::new();
        let mut pending_tool_calls: HashMap<String, PendingToolCall> = HashMap::new();
        let mut stream_failed: Option<String> = None;

        // 初期ストリーム確立前の切断を検知する。`tx.closed()` を優先して競わせ、
        // クライアントが既に切断していれば `chat_stream` の future をドロップして
        // 早期リターンする（不要な生成の課金を防ぐ）。通常経路では SSE の
        // イベント内容・順序・wire format は不変。
        match tokio::select! {
            biased;
            _ = tx.closed() => None,
            result = provider.chat_stream(request) => Some(result),
        } {
            Some(Ok(mut provider_stream)) => {
                'provider_stream: loop {
                    // クライアント切断時は Sse/ReceiverStream がドロップされ `rx` が閉じる。
                    // `tx.closed()` も同時に待つことで、切断後もプロバイダーの
                    // ストリームを消費し続ける（＝不要な生成の課金が継続する）
                    // ことを防ぐ。SSE のイベント内容・順序は不変。
                    let next = tokio::select! {
                        biased;
                        _ = tx.closed() => None,
                        next = provider_stream.next() => next,
                    };
                    let item = match next {
                        Some(item) => item,
                        None => break 'provider_stream,
                    };
                    match item {
                        Ok(opencode_provider::StreamEvent::TextDelta(delta)) => {
                            if delta.is_empty() {
                                continue;
                            }
                            final_text.push_str(&delta);
                            send_sse_event(
                                &tx,
                                "message_delta",
                                StreamEvent {
                                    event_type: "message_delta".to_string(),
                                    content: Some(delta),
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: None,
                                    tool_name: None,
                                    input: None,
                                    prompt_tokens: None,
                                    completion_tokens: None,
                                    error: None,
                                },
                            )
                            .await;
                        }
                        Ok(opencode_provider::StreamEvent::ToolCallStart { id, name }) => {
                            pending_tool_calls.entry(id.clone()).or_default().name =
                                Some(name.clone());
                            send_sse_event(
                                &tx,
                                "tool_call_start",
                                StreamEvent {
                                    event_type: "tool_call_start".to_string(),
                                    content: None,
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: Some(id),
                                    tool_name: Some(name),
                                    input: None,
                                    prompt_tokens: None,
                                    completion_tokens: None,
                                    error: None,
                                },
                            )
                            .await;
                        }
                        Ok(opencode_provider::StreamEvent::ToolCallDelta { id, input }) => {
                            pending_tool_calls
                                .entry(id.clone())
                                .or_default()
                                .input
                                .push_str(&input);
                            send_sse_event(
                                &tx,
                                "tool_call_delta",
                                StreamEvent {
                                    event_type: "tool_call_delta".to_string(),
                                    content: None,
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: Some(id),
                                    tool_name: None,
                                    input: Some(input),
                                    prompt_tokens: None,
                                    completion_tokens: None,
                                    error: None,
                                },
                            )
                            .await;
                        }
                        Ok(opencode_provider::StreamEvent::ToolCallEnd { id, name, input }) => {
                            let call = pending_tool_calls.entry(id.clone()).or_default();
                            call.name = Some(name.clone());
                            call.input = input.to_string();
                            send_sse_event(
                                &tx,
                                "tool_call_end",
                                StreamEvent {
                                    event_type: "tool_call_end".to_string(),
                                    content: None,
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: Some(id),
                                    tool_name: Some(name),
                                    input: Some(input.to_string()),
                                    prompt_tokens: None,
                                    completion_tokens: None,
                                    error: None,
                                },
                            )
                            .await;
                        }
                        Ok(opencode_provider::StreamEvent::Usage {
                            prompt_tokens,
                            completion_tokens,
                        }) => {
                            send_sse_event(
                                &tx,
                                "usage",
                                StreamEvent {
                                    event_type: "usage".to_string(),
                                    content: None,
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: None,
                                    tool_name: None,
                                    input: None,
                                    prompt_tokens: Some(prompt_tokens),
                                    completion_tokens: Some(completion_tokens),
                                    error: None,
                                },
                            )
                            .await;
                        }
                        Ok(opencode_provider::StreamEvent::Done) => break,
                        Ok(opencode_provider::StreamEvent::Error(err)) => {
                            stream_failed = Some(err.clone());
                            send_sse_event(
                                &tx,
                                "error",
                                StreamEvent {
                                    event_type: "error".to_string(),
                                    content: None,
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: None,
                                    tool_name: None,
                                    input: None,
                                    prompt_tokens: None,
                                    completion_tokens: None,
                                    error: Some(err),
                                },
                            )
                            .await;
                            break;
                        }
                        Err(err) => {
                            let error_message = err.to_string();
                            stream_failed = Some(error_message.clone());
                            let (_, safe_error) =
                                safe_prompt_failure_client_info(&error_message).await;
                            send_sse_event(
                                &tx,
                                "error",
                                StreamEvent {
                                    event_type: "error".to_string(),
                                    content: None,
                                    message_id: Some(stream_msg_id.clone()),
                                    done: None,
                                    tool_call_id: None,
                                    tool_name: None,
                                    input: None,
                                    prompt_tokens: None,
                                    completion_tokens: None,
                                    error: Some(safe_error),
                                },
                            )
                            .await;
                            break;
                        }
                        Ok(_) => {}
                    }
                }
            }
            Some(Err(err)) => {
                let error_message = err.to_string();
                stream_failed = Some(error_message.clone());
                let (_, safe_error) = safe_prompt_failure_client_info(&error_message).await;
                send_sse_event(
                    &tx,
                    "error",
                    StreamEvent {
                        event_type: "error".to_string(),
                        content: None,
                        message_id: Some(stream_msg_id.clone()),
                        done: None,
                        tool_call_id: None,
                        tool_name: None,
                        input: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        error: Some(safe_error),
                    },
                )
                .await;
            }
            None => {
                // クライアント切断を検知。`chat_stream` の future は select! により
                // ドロップ済み。ここまでに受け取った final_text / pending_tool_calls を
                // 正常経路と同じ finalize ヘルパで永続化してからリターンする。
                finalize_stream_message(
                    &stream_state,
                    &stream_session_id,
                    &stream_msg_id,
                    final_text,
                    pending_tool_calls,
                    stream_failed,
                )
                .await;
                set_session_run_status(&stream_state, &stream_session_id, SessionRunStatus::Idle)
                    .await;
                return;
            }
        }

        finalize_stream_message(
            &stream_state,
            &stream_session_id,
            &stream_msg_id,
            final_text,
            pending_tool_calls,
            stream_failed,
        )
        .await;
        set_session_run_status(&stream_state, &stream_session_id, SessionRunStatus::Idle).await;

        send_sse_event(
            &tx,
            "message_end",
            StreamEvent {
                event_type: "message_end".to_string(),
                content: None,
                message_id: Some(stream_msg_id),
                done: Some(true),
                tool_call_id: None,
                tool_name: None,
                input: None,
                prompt_tokens: None,
                completion_tokens: None,
                error: None,
            },
        )
        .await;
    });

    // /prompt と同じくセッション単位で AbortHandle を登録し、abort からも止められるようにする。
    state.register_prompt_task(session_id, stream_task.abort_handle());
    let _ = start_tx.send(());

    Ok(Sse::new(ReceiverStream::new(rx)))
}

#[derive(Debug, Deserialize)]
pub struct SessionPromptRequest {
    pub message: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub agent: Option<String>,
    pub command: Option<String>,
    pub arguments: Option<String>,
}

async fn session_prompt(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<SessionPromptRequest>,
) -> Result<Json<serde_json::Value>> {
    let prompt_text = if let Some(message) = req.message.as_deref() {
        message.to_string()
    } else if let Some(command) = req.command.as_deref() {
        req.arguments
            .as_deref()
            .map(|args| format!("/{command} {args}"))
            .unwrap_or_else(|| format!("/{command}"))
    } else {
        return Err(ApiError::BadRequest(
            "Either `message` or `command` must be provided".to_string(),
        ));
    };

    {
        let sessions = state.sessions.lock().await;
        if sessions.get(&id).is_none() {
            return Err(ApiError::SessionNotFound(id));
        }
    }

    let config = state.config.read().await;
    let (_provider, provider_id, model_id) =
        resolve_provider_and_model(&state, req.model.as_deref(), config.model.as_deref(), None)?;
    drop(config);

    let queue_request = QueuedPromptRequest {
        model: ProviderModel {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
        },
        agent: req.agent.clone(),
        variant: req.variant.clone(),
    };
    spawn_prompt_execution(
        state.clone(),
        id.clone(),
        prompt_text,
        queue_request,
        None,
        None,
        0,
    );

    Ok(Json(serde_json::json!({
        "status": "started",
        "model": format!("{}/{}", provider_id, model_id),
        "variant": req.variant,
    })))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProviderModel {
    provider_id: String,
    model_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct QueuedPromptRequest {
    model: ProviderModel,
    agent: Option<String>,
    variant: Option<String>,
}

/// プロンプトタスクの終了時に `ServerState` のキャンセルハンドルを後始末する。
/// 正常終了・早期 return・abort のいずれの経路でも Drop が走る。
///
/// キュー由来のタスクが中断された場合は、キュー項目を failed へ終端化し、
/// 次のキュー投入を再スケジュールする（abort 経路と同じ回収処理）。
struct PromptTaskCleanup {
    state: Arc<ServerState>,
    session_id: String,
    task_id: tokio::task::Id,
    queue_id: Option<String>,
    /// `claim_pending` に成功したか。未 claim ならキュー項目は pending のままなので触らない。
    claimed: Arc<AtomicBool>,
    /// 通常の終端処理（promote / failed + 次キュー起動）が済んだか。
    queue_finalized: Arc<AtomicBool>,
}

impl Drop for PromptTaskCleanup {
    fn drop(&mut self) {
        let cleared_own_task = self.state.clear_prompt_task(&self.session_id, self.task_id);

        // ストリームタスクが panic / 早期ドロップした場合でも Busy が残らないよう
        // run_status を Idle に戻す。ただし後続タスクが既に登録済みで自分の
        // ハンドルを消せなかった場合は、後続の Busy を誤って消さないようスキップ
        // する（R4）。Drop は同期なので tokio runtime が有る場合だけ async に
        // 引き継ぎ、無い場合は固定文言のみログに出す（機微値は出さない）。
        if cleared_own_task {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let state = self.state.clone();
                let session_id = self.session_id.clone();
                handle.spawn(async move {
                    set_session_run_status(&state, &session_id, SessionRunStatus::Idle).await;
                });
            } else {
                tracing::warn!("no tokio runtime for prompt run-status cleanup");
            }
        }

        // graceful shutdown 中は再スケジュールしない（二重実行防止）。
        if self.state.shutting_down.load(Ordering::Acquire) {
            if let Some(queue_id) = &self.queue_id {
                self.state.clear_prompt_queue_id(&self.session_id, queue_id);
            }
            return;
        }
        let Some(queue_id) = self.queue_id.clone() else {
            return;
        };
        // キュー回収は「running_prompt_queue_ids から ID を取り出した側だけ」が行う
        // 単発フラグ方式（abort と Drop の二重回収を防ぐ）。
        if !self.claimed.load(Ordering::Acquire) || self.queue_finalized.load(Ordering::Acquire) {
            self.state
                .clear_prompt_queue_id(&self.session_id, &queue_id);
            return;
        }
        match self.state.take_prompt_queue_id(&self.session_id) {
            Some(id) if id == queue_id => {
                // 自分の ID を取得した＝回収担当。Drop は同期なのでランタイムへ引き継ぐ。
                let Ok(handle) = tokio::runtime::Handle::try_current() else {
                    tracing::warn!(input_id = %queue_id, "no tokio runtime for queued prompt cleanup");
                    return;
                };
                let state = self.state.clone();
                let session_id = self.session_id.clone();
                handle.spawn(async move {
                    recover_interrupted_queued_prompt(state, session_id, Some(queue_id)).await;
                });
            }
            Some(other) => {
                // 後続タスクの登録を誤って取り出したので元に戻す。
                self.state
                    .register_prompt_queue_id(self.session_id.clone(), other);
            }
            None => {
                // abort / delete_session 側が先に取り出して回収担当となっている。
            }
        }
    }
}

/// キュー投入プロンプトの実行で新規生成された assistant メッセージに、
/// 起点となったキュー項目の `queue_id` を相関 ID として付与する。
///
/// 再実行時に重複した assistant レスポンスを検出・識別するための付加情報であり、
/// キュー状態そのものは変更しない（冪等性そのものはキュー側の終端状態管理が担う）。
fn correlate_assistant_messages_with_queue(
    session: &mut opencode_session::Session,
    first_message_index: usize,
    queue_id: &str,
) {
    for message in session.messages.iter_mut().skip(first_message_index) {
        if matches!(message.role, opencode_session::MessageRole::Assistant) {
            message
                .metadata
                .insert("queue_id".to_string(), serde_json::json!(queue_id));
        }
    }
}

/// HTTP 経路の permission コールバック本体。
///
/// `Always` 応答時にセッションスコープの動的 allow を記録し、以降は同一の
/// `(permission, patterns)` リクエストをプロンプトなしでスキップする。
/// 静的な `AgentInfo.permission` ルール（deny/allow）は変更しない。
async fn handle_permission_request(
    state: &Arc<ServerState>,
    broker: &crate::server::PermissionBroker,
    session_id: &str,
    req: opencode_tool::PermissionRequest,
) -> std::result::Result<(), opencode_tool::ToolError> {
    let permission = req.permission.clone();
    let patterns = req.patterns.clone();

    // 動的 allow の事前判定。broker 待機の前にロックを必ず解放する。
    {
        let sessions = state.sessions.lock().await;
        if sessions.is_permission_approved(session_id, &permission, &patterns) {
            return Ok(());
        }
    }

    let metadata = req.metadata.clone();
    let always = req.always.clone();
    let info = crate::server::PermissionRequestInfo {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        tool: permission.clone(),
        input: serde_json::json!({
            "patterns": patterns,
            "metadata": metadata,
            "always": always,
        }),
        message: format!("Tool '{}' is requesting permission", permission),
        permission_type: permission.clone(),
        patterns: patterns.clone(),
        metadata,
        always,
    };

    match broker.ask(info).await {
        crate::server::PermissionResponse::Once => Ok(()),
        crate::server::PermissionResponse::Always => {
            // 承認時に要求された pattern をそのまま記録する（ワイルドカード拡張なし）。
            // broker.ask() 待機中にセッションが削除されていた場合は記録せず、
            // 孤立した動的 allow が残らないようにする。
            let mut sessions = state.sessions.lock().await;
            let recorded = sessions.record_permission_allow(session_id, &permission, patterns);
            if !recorded {
                // パターンはパスを含み得るためログに出さず、session_id のみを残す。
                tracing::debug!(
                    session_id = %session_id,
                    "dynamic permission allow discarded: session no longer exists"
                );
            }
            Ok(())
        }
        crate::server::PermissionResponse::Reject { message } => {
            Err(opencode_tool::ToolError::PermissionDenied(
                message.unwrap_or_else(|| "Permission denied".to_string()),
            ))
        }
    }
}

/// セッション更新通知のキュー上限。
///
/// `Session` は全メッセージ履歴を含むため deep clone が高価であり、50ms 間隔の
/// スナップショットを無制限に積むと消費者が遅れた際にメモリが際限なく増える。
/// 最新状態のみが必要なので、通知はこの件数で coalesce する。
const SESSION_UPDATE_QUEUE_CAPACITY: usize = 16;

type LatestSessionSlot = Arc<std::sync::Mutex<Option<opencode_session::Session>>>;

/// 最新スナップショット 1 件と、バウンドされた通知キューを保持する producer。
///
/// スロットを常に最新で上書きし、通知は `try_send` で送る。キューが満杯の場合は
/// 通知を破棄するが、スロットの上書きによって古いスナップショットは coalesce
/// され、消費者には常に最新状態が渡る。
struct CoalescedSessionUpdates {
    notify_tx: mpsc::Sender<()>,
    latest: LatestSessionSlot,
}

impl CoalescedSessionUpdates {
    fn new(capacity: usize) -> (Self, SessionUpdateReceiver) {
        let (notify_tx, notify_rx) = mpsc::channel(capacity);
        let latest: LatestSessionSlot = Arc::new(std::sync::Mutex::new(None));
        (
            Self {
                notify_tx,
                latest: Arc::clone(&latest),
            },
            SessionUpdateReceiver { notify_rx, latest },
        )
    }

    /// 最新スナップショットを記録して消費者を起こす。
    ///
    /// 送信失敗時の挙動:
    /// - `TrySendError::Full`: 未処理の通知が既にあるため新たな通知は送らず、
    ///   スロットの上書きだけで最新状態を反映する（古いスナップショットを破棄）。
    /// - `TrySendError::Closed` / 事前の `is_closed()`: 消費者が終了済みなので
    ///   更新全体を破棄する。
    ///
    /// スロット・ログ・エラーにはトークン値やパスを書き込まない。
    fn publish(&self, snapshot: &opencode_session::Session) {
        if self.notify_tx.is_closed() {
            return;
        }
        {
            let mut latest = self
                .latest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *latest = Some(snapshot.clone());
        }
        match self.notify_tx.try_send(()) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(())) => {
                tracing::trace!("session update coalesced: notification queue full");
            }
            Err(mpsc::error::TrySendError::Closed(())) => {
                tracing::trace!("session update discarded: consumer closed");
            }
        }
    }
}

/// [`CoalescedSessionUpdates`] の消費者側ハンドル。
struct SessionUpdateReceiver {
    notify_rx: mpsc::Receiver<()>,
    latest: LatestSessionSlot,
}

impl SessionUpdateReceiver {
    /// 次の通知を待ち、溜まった古い通知をまとめて読み捨ててから
    /// 最新スナップショットを 1 件だけ返す。
    async fn recv_latest(&mut self) -> Option<opencode_session::Session> {
        loop {
            self.notify_rx.recv().await?;
            while self.notify_rx.try_recv().is_ok() {}
            if let Some(snapshot) = self
                .latest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                return Some(snapshot);
            }
            // スロットが空なら次の通知を待つ（通常は起こらない）。
        }
    }
}

#[cfg(test)]
mod session_update_coalescing_tests {
    use super::*;

    fn marked_session(seq: u64) -> opencode_session::Session {
        let mut session = opencode_session::Session::new("proj", ".");
        session
            .metadata
            .insert("seq".to_string(), serde_json::json!(seq));
        session
    }

    fn session_seq(session: &opencode_session::Session) -> u64 {
        session
            .metadata
            .get("seq")
            .and_then(|value| value.as_u64())
            .unwrap_or_default()
    }

    /// 回帰ガード: 消费者が遅れても通知キューは上限を超えず、
    /// 保留中スナップショットは最新 1 件だけになる。
    #[tokio::test]
    async fn notification_queue_is_bounded_and_holds_only_latest() {
        const CAPACITY: usize = 4;
        let (updates, receiver) = CoalescedSessionUpdates::new(CAPACITY);

        for seq in 0..100 {
            updates.publish(&marked_session(seq));
        }

        assert!(
            receiver.notify_rx.len() <= CAPACITY,
            "notification queue exceeded capacity"
        );
        assert_eq!(
            receiver.notify_rx.len(),
            CAPACITY,
            "queue should saturate at its capacity"
        );
        let latest = receiver
            .latest
            .lock()
            .expect("slot lock should not poison")
            .as_ref()
            .map(session_seq);
        assert_eq!(latest, Some(99), "stale snapshots must be coalesced away");
    }

    /// 回帰ガード: 遅延後に消费者が取り出すのは最新スナップショットであり、
    /// 古い通知はすべて読み捨てられる。
    #[tokio::test]
    async fn receiver_applies_only_latest_snapshot_after_lag() {
        let (updates, mut receiver) = CoalescedSessionUpdates::new(2);

        for seq in 0..8 {
            updates.publish(&marked_session(seq));
        }

        let snapshot = receiver
            .recv_latest()
            .await
            .expect("pending update should be delivered");
        assert_eq!(session_seq(&snapshot), 7, "latest snapshot must win");
        assert!(
            receiver.notify_rx.try_recv().is_err(),
            "older notifications must be drained"
        );
        assert!(
            receiver
                .latest
                .lock()
                .expect("slot lock should not poison")
                .is_none(),
            "slot must be empty after being consumed"
        );

        updates.publish(&marked_session(8));
        let next = receiver
            .recv_latest()
            .await
            .expect("new update should be delivered");
        assert_eq!(session_seq(&next), 8);
    }

    /// 消費者終了後の publish は破棄され、パニックしない。
    #[test]
    fn publish_after_consumer_exit_is_discarded() {
        let (updates, receiver) = CoalescedSessionUpdates::new(2);
        drop(receiver);
        updates.publish(&marked_session(0));
    }
}

fn spawn_prompt_execution(
    task_state: Arc<ServerState>,
    session_id: String,
    prompt_text: String,
    request: QueuedPromptRequest,
    queue_id: Option<String>,
    message_id: Option<String>,
    retry_attempt: u32,
) {
    let task_variant = request.variant.clone();
    let task_agent = request.agent.clone();
    let task_model = request.model.model_id.clone();
    let task_provider = request.model.provider_id.clone();
    let provider = match task_state
        .providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_provider(&task_provider)
    {
        Ok(provider) => provider,
        Err(error) => {
            // 生エラー（トークン値・パスを含み得る）はログへ出さない（R4）。
            let _ = &error;
            tracing::error!(session_id = %session_id, "prompt provider unavailable");
            if let (Some(queue_id), Some(repo)) =
                (queue_id.as_deref(), task_state.input_repo.as_ref())
            {
                let repo = repo.clone();
                let id = queue_id.to_owned();
                let next_state = task_state.clone();
                let next_session = session_id.clone();
                tokio::spawn(async move {
                    if let Err(mark_error) = repo.mark_as_failed(&id).await {
                        tracing::error!(%id, %mark_error, "failed marking queued prompt failed");
                    }
                    spawn_next_queued_prompt(next_state, next_session);
                });
            }
            return;
        }
    };
    let register_state = task_state.clone();
    let task_session_id = session_id.clone();
    let registered_queue_id = queue_id.clone();
    // Drop 経路のキュー回収用フラグ。claim 成功後にのみ回収対象となり、
    // 通常の終端処理が済んだら finalized を立てて二重処理を防ぐ。
    let queue_claimed = Arc::new(AtomicBool::new(false));
    let queue_finalized = Arc::new(AtomicBool::new(false));
    // 親の登録がタスク本体（早期 return とその Drop を含む）より必ず先に
    // 完了するようバリアを張る。spawn 直後に親がハンドルとキュー ID を登録
    // してから解放するため、cleanup の clear が対象を見つけられずに
    // running_prompt_tasks / running_prompt_queue_ids へ残骸が残る競合を排除する。
    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _cleanup = PromptTaskCleanup {
            state: task_state.clone(),
            session_id: session_id.clone(),
            task_id: tokio::task::id(),
            queue_id: queue_id.clone(),
            claimed: queue_claimed.clone(),
            queue_finalized: queue_finalized.clone(),
        };
        // 親が AbortHandle / キュー ID の登録を終えるまでここで待つ。
        let _ = start_rx.await;
        let run_lock = {
            let mut locks = task_state.prompt_locks.write().await;
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _run_guard = run_lock.lock().await;
        if let Some(queue_id) = queue_id.as_deref() {
            let Some(input_repo) = task_state.input_repo.as_ref() else {
                tracing::error!(%queue_id, "queued prompt storage unavailable");
                return;
            };
            let pending = match input_repo.list_pending(&session_id).await {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::error!(%session_id, retry_attempt, %error, "failed reading queued prompts");
                    retry_queued_prompts(
                        task_state.clone(),
                        session_id.clone(),
                        retry_attempt + 1,
                        Some(queue_id.to_string()),
                    );
                    return;
                }
            };
            if pending.first().is_none_or(|input| input.id != queue_id) {
                return;
            }
            match input_repo.claim_pending(queue_id).await {
                // claim 成功でプロンプトは処理へ進む。以降の次項目起動は
                // リセット用 wrapper を通るため retry_attempt は引き継がない。
                Ok(true) => {
                    queue_claimed.store(true, Ordering::Release);
                }
                Ok(false) => return,
                Err(error) => {
                    tracing::error!(%queue_id, retry_attempt, %error, "failed claiming queued prompt");
                    retry_queued_prompts(
                        task_state.clone(),
                        session_id.clone(),
                        retry_attempt + 1,
                        Some(queue_id.to_string()),
                    );
                    return;
                }
            }
        }
        let session = {
            let sessions = task_state.sessions.lock().await;
            sessions.get(&session_id).cloned()
        };
        let Some(mut session) = session else {
            if let (Some(queue_id), Some(repo)) =
                (queue_id.as_deref(), task_state.input_repo.as_ref())
            {
                if let Err(error) = repo.mark_as_failed(queue_id).await {
                    tracing::error!(%queue_id, %error, "failed marking orphaned queued prompt");
                }
            }
            if queue_id.is_some() {
                queue_finalized.store(true, Ordering::Release);
                spawn_next_queued_prompt(task_state.clone(), session_id.clone());
            }
            return;
        };
        let message_id = if let Some(queue_id) = queue_id.as_deref() {
            if let Some(existing) = session
                .messages
                .iter()
                .find(|message| message.id == queue_id)
            {
                Some(existing.id.clone())
            } else {
                let message = session.add_user_message(&prompt_text);
                message.id = queue_id.to_owned();
                Some(queue_id.to_owned())
            }
        } else {
            message_id
        };
        persist_sessions_if_enabled(&task_state).await;
        set_session_run_status(&task_state, &session_id, SessionRunStatus::Busy).await;

        if let Some(variant) = task_variant.as_deref() {
            session
                .metadata
                .insert("model_variant".to_string(), serde_json::json!(variant));
        } else {
            session.metadata.remove("model_variant");
        }
        session.metadata.insert(
            "model_provider".to_string(),
            serde_json::json!(&task_provider),
        );
        session
            .metadata
            .insert("model_id".to_string(), serde_json::json!(&task_model));
        if let Some(agent) = task_agent.as_deref() {
            session
                .metadata
                .insert("agent".to_string(), serde_json::json!(agent));
        }

        let (updates, mut update_rx) = CoalescedSessionUpdates::new(SESSION_UPDATE_QUEUE_CAPACITY);
        let update_state = task_state.clone();
        let update_task = tokio::spawn(async move {
            while let Some(snapshot) = update_rx.recv_latest().await {
                let snapshot_id = snapshot.id.clone();
                {
                    let mut sessions = update_state.sessions.lock().await;
                    sessions.update(snapshot);
                }
                update_state.broadcast(
                    &serde_json::json!({
                        "type": "session.updated",
                        "sessionID": snapshot_id,
                        "source": "prompt.stream",
                    })
                    .to_string(),
                );
            }
        });
        let update_hook: opencode_session::SessionUpdateHook = Arc::new(move |snapshot| {
            updates.publish(snapshot);
        });

        // Create permission callback that uses the server's PermissionBroker.
        // `Always` 応答はセッションスコープの動的 allow として記録され、
        // 同一ツールの次回呼び出しではプロンプトをスキップする。
        let permission_broker = task_state.permission_broker.clone();
        let permission_state = task_state.clone();
        let session_id_for_cb = session_id.clone();
        let permission_callback: opencode_session::prompt::PermissionCallback =
            Some(Arc::new(move |req: opencode_tool::PermissionRequest| {
                let broker = permission_broker.clone();
                let state = permission_state.clone();
                let session_id = session_id_for_cb.clone();
                Box::pin(async move {
                    handle_permission_request(&state, &broker, &session_id, req).await
                })
            }));

        // Create question callback that uses the server's QuestionBroker
        let question_broker = task_state.question_broker.clone();
        let session_id_for_question_cb = session_id.clone();
        let question_callback: opencode_session::prompt::QuestionCallback = Some(Arc::new(
            move |questions: Vec<opencode_tool::QuestionDef>| {
                let broker = question_broker.clone();
                let session_id = session_id_for_question_cb.clone();

                Box::pin(async move {
                    let info = crate::server::QuestionRequestInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        session_id,
                        questions,
                    };
                    match broker.ask(info).await {
                        crate::server::QuestionResponse::Answered(answers) => Ok(answers),
                        crate::server::QuestionResponse::Rejected => {
                            Err(opencode_tool::ToolError::QuestionRejected(
                                "User rejected the question".to_string(),
                            ))
                        }
                    }
                })
            },
        ));

        // TODO 状態を ServerState の TodoManager へ直接配線する。
        // ServerState は Arc で共有されているため、所有権を移さずにコールバックへ保持でき、
        // `GET /session/{id}/todo` と同じインスタンスを参照できる。
        let todo_update_state = task_state.clone();
        let todo_update: opencode_tool::TodoUpdateCallback = Arc::new(move |session_id, todos| {
            let state = todo_update_state.clone();
            Box::pin(async move {
                let items: Vec<opencode_session::TodoInfo> = todos
                    .iter()
                    .map(|todo| opencode_session::TodoInfo {
                        content: todo.content.clone(),
                        status: todo.status.clone(),
                        priority: todo.priority.clone(),
                    })
                    .collect();
                state.todo_manager.update(&session_id, items).await;
                state.broadcast(
                    &serde_json::json!({
                        "type": "todo.updated",
                        "sessionID": session_id,
                        "todos": todos.iter().map(|todo| serde_json::json!({
                            "content": todo.content,
                            "status": todo.status,
                            "priority": todo.priority,
                        })).collect::<Vec<_>>(),
                    })
                    .to_string(),
                );
                Ok(())
            })
        });
        let todo_get_state = task_state.clone();
        let todo_get: opencode_tool::TodoGetCallback = Arc::new(move |session_id| {
            let state = todo_get_state.clone();
            Box::pin(async move {
                Ok(state
                    .todo_manager
                    .get(&session_id)
                    .await
                    .into_iter()
                    .map(|todo| opencode_tool::TodoItemData {
                        content: todo.content,
                        status: todo.status,
                        priority: todo.priority,
                    })
                    .collect())
            })
        });

        // Build AgentRegistry from config
        let cwd = std::env::current_dir().unwrap_or_default();
        // load_config が失敗して既定 Config へフォールバックした場合のみ env
        // override を適用し、compaction の kill switch (OPENCODE_DISABLE_*) を
        // 失わないようにする。成功時に再適用すると managed config の優先順位が
        // 壊れるため、失敗経路だけに限定する（R4）。
        let config = match load_config(&cwd) {
            Ok(config) => config,
            Err(_) => {
                let mut config = AppConfig::default();
                apply_environment_overrides(&mut config);
                config
            }
        };
        let agent_registry = AgentRegistry::from_config(&config);

        // Determine agent name (default to "build")
        let agent_name = task_agent
            .as_deref()
            .unwrap_or(BuiltinAgent::Build.as_str());

        let agent_info = agent_registry
            .get(agent_name)
            .cloned()
            .unwrap_or_else(|| AgentConfigInfo::from_builtin(BuiltinAgent::Build));

        // Build system prompt: provider-specific + agent system prompt + environment
        let provider_specific_prompt = SystemPrompt::for_model(&task_model);
        let agent_system_prompt = agent_info.system_prompt.unwrap_or_default();
        let env_ctx =
            EnvironmentContext::from_current(&task_model, &task_provider, &session.directory);
        let environment_prompt = SystemPrompt::environment(&env_ctx);

        let system_prompt = if agent_system_prompt.is_empty() {
            format!("{}\n\n{}", provider_specific_prompt, environment_prompt)
        } else {
            format!(
                "{}\n\n{}\n\n{}",
                provider_specific_prompt, agent_system_prompt, environment_prompt
            )
        };

        // Get tool definitions from default registry
        let tool_registry = create_default_registry().await;
        let tool_definitions = tool_registry.list_schemas().await;
        let tools: Vec<opencode_provider::ToolDefinition> = tool_definitions
            .into_iter()
            .map(|t| opencode_provider::ToolDefinition {
                name: t.name,
                description: Some(t.description),
                parameters: t.parameters,
            })
            .collect();

        // Build AgentParams from agent config
        let agent_params = opencode_session::AgentParams {
            max_tokens: agent_info.max_tokens,
            temperature: agent_info.temperature,
            top_p: agent_info.top_p,
        };

        // CFGAPPLY-01: config.compaction を実行系へ渡す。未設定（None）の
        // フィールドは session 側 default（auto=true, prune=true）を維持する。
        // 設定値やパスはログ・エラーに出さない。
        let compaction_config = config
            .compaction
            .as_ref()
            .map_or_else(opencode_session::CompactionConfig::default, |c| {
                opencode_session::CompactionConfig::from_options(c.auto, c.prune, c.reserved)
            });
        let prompt_runner = opencode_session::SessionPrompt::new(Arc::new(RwLock::new(
            opencode_session::SessionStateManager::new(),
        )))
        .with_todo_hooks(todo_update, todo_get)
        .with_compaction_config(compaction_config);
        let input = opencode_session::PromptInput {
            session_id: session_id.clone(),
            message_id,
            model: Some(opencode_session::prompt::ModelRef {
                provider_id: task_provider.clone(),
                model_id: task_model.clone(),
            }),
            agent: task_agent.clone(),
            no_reply: false,
            system: Some(system_prompt.clone()),
            variant: task_variant.clone(),
            parts: vec![opencode_session::PartInput::Text { text: prompt_text }],
            tools: Some(tools.iter().map(|t| (t.name.clone(), true)).collect()),
        };

        // この実行で追加された assistant メッセージだけに queue_id を付与するため、
        // プロンプト実行前のメッセージ数を記録する。
        let first_message_index = session.messages.len();
        let result = prompt_runner
            .prompt_with_update_hook(
                input,
                &mut session,
                provider,
                Some(system_prompt),
                tools,
                agent_params,
                Some(update_hook),
                permission_callback,
                question_callback,
            )
            .await;
        if let Err(error) = &result {
            // 内部詳細は error.rs の共通 redact を通してログへ残し、
            // クライアントには汎用文言と相関 ID だけを渡す（生文字列は出さない）。
            let (correlation_id, client_message) =
                safe_prompt_failure_client_info(&error.to_string()).await;
            tracing::error!(
                session_id = %session_id,
                provider_id = %task_provider,
                model_id = %task_model,
                correlation_id = %correlation_id,
                "session prompt failed"
            );
            let assistant = session.add_assistant_message();
            assistant
                .metadata
                .insert("error".to_string(), serde_json::json!(&client_message));
            assistant.metadata.insert(
                "correlation_id".to_string(),
                serde_json::json!(&correlation_id),
            );
            assistant
                .metadata
                .insert("finish_reason".to_string(), serde_json::json!("error"));
            assistant.metadata.insert(
                "model_provider".to_string(),
                serde_json::json!(&task_provider),
            );
            assistant
                .metadata
                .insert("model_id".to_string(), serde_json::json!(&task_model));
            if let Some(agent) = task_agent.as_deref() {
                assistant
                    .metadata
                    .insert("agent".to_string(), serde_json::json!(agent));
            }
            assistant.add_text(format!("{client_message} (reference: {correlation_id})"));
            if let (Some(queue_id), Some(input_repo)) =
                (queue_id.as_deref(), task_state.input_repo.as_ref())
            {
                if let Err(queue_error) = input_repo.mark_as_failed(queue_id).await {
                    tracing::error!(%queue_id, %queue_error, "failed marking queued prompt failed");
                }
            }
        }
        if let Some(queue_id) = queue_id.as_deref() {
            correlate_assistant_messages_with_queue(&mut session, first_message_index, queue_id);
        }
        let _ = update_task.await;

        {
            let mut sessions = task_state.sessions.lock().await;
            sessions.update(session);
        }
        task_state.broadcast(
            &serde_json::json!({
                "type": "session.updated",
                "sessionID": session_id,
                "source": "prompt.final",
            })
            .to_string(),
        );
        set_session_run_status(&task_state, &session_id, SessionRunStatus::Idle).await;
        persist_sessions_if_enabled(&task_state).await;
        if let Some(queue_id) = queue_id.as_deref() {
            if let Some(input_repo) = task_state.input_repo.as_ref() {
                if result.is_ok() {
                    if let Err(error) = input_repo.mark_as_promoted(queue_id).await {
                        tracing::error!(%queue_id, %error, "failed marking queued prompt complete");
                        // assistant レスポンスは既にセッションへ永続化済みであり、この
                        // キュー項目を processing のまま残すのは安全ではない。再起動時の
                        // reset_processing() で pending に戻り再実行されるのを避けるため、
                        // 終端状態である failed への切り替えをフォールバックで試みる。
                        if let Err(fail_error) = input_repo.mark_as_failed(queue_id).await {
                            tracing::error!(%queue_id, %fail_error, "failed marking queued prompt failed after promote failure");
                        }
                    }
                } else if let Err(error) = input_repo.mark_as_failed(queue_id).await {
                    tracing::error!(%queue_id, %error, "failed marking queued prompt failed");
                }
            }
            // Drop 経路の回収をここで無効化してから次キューを起動する。
            queue_finalized.store(true, Ordering::Release);
            spawn_next_queued_prompt(task_state.clone(), session_id.clone());
        }
    });
    if let Some(queue_id) = registered_queue_id {
        register_state.register_prompt_queue_id(task_session_id.clone(), queue_id);
    }
    register_state.register_prompt_task(task_session_id, task.abort_handle());
    // 登録完了後にタスク本体を解放する。早期 return 経路でも cleanup の
    // clear が必ず自分のハンドルを見つけられる順序が保証される。
    let _ = start_tx.send(());
}

/// キュー投入プロンプトの連続失敗に対する再試行上限。
///
/// DB 破損等で `list_pending` / `claim_pending` が固定失敗すると、旧実装は
/// 1 秒ごとに再スケジュールし続け終了条件がなかった。attempt がこの値を
/// 超えた時点で再スケジュールを停止する（terminal 相当の終端化）。
const QUEUED_PROMPT_MAX_RETRIES: u32 = 5;

/// 初回再試行の待機時間。以降は attempt ごとに 2 倍し、指数バックオフとする。
const QUEUED_PROMPT_INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);

/// 連続失敗 `attempt`（1 始まり）に対する再試行待機時間を返す。
///
/// `attempt == 0` は成功後にカウンタがリセットされた状態を表し、再試行は
/// 行わない。上限超過も `None` を返し、これが再スケジュールの終端条件になる。
fn queued_retry_delay(attempt: u32) -> Option<Duration> {
    if attempt == 0 || attempt > QUEUED_PROMPT_MAX_RETRIES {
        return None;
    }
    // attempt=1 -> 1s, 2 -> 2s, 3 -> 4s, ... と 2 倍ずつ増加する。
    let factor = 1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX);
    Some(QUEUED_PROMPT_INITIAL_RETRY_DELAY.saturating_mul(factor))
}

/// キュー投入プロンプトの次を起動する。
///
/// 通常の進行・終端処理（promote / failed 登録 / 再起動時復旧 / 新規 enqueue）
/// はすべてこの wrapper を通るため、成功時に再試行カウンタが 0 にリセット
/// される。失敗した経路のみ `spawn_next_queued_prompt_with_attempt` が
/// attempt を引き継ぎ、指数バックオフ付きで再試行する。
fn spawn_next_queued_prompt(state: Arc<ServerState>, session_id: String) {
    spawn_next_queued_prompt_with_attempt(state, session_id, 0);
}

fn spawn_next_queued_prompt_with_attempt(
    state: Arc<ServerState>,
    session_id: String,
    attempt: u32,
) {
    tokio::spawn(async move {
        let Some(input_repo) = state.input_repo.as_ref() else {
            return;
        };
        let pending = match input_repo.list_pending(&session_id).await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::error!(%session_id, attempt, %error, "failed reading queued prompts");
                retry_queued_prompts(state.clone(), session_id.clone(), attempt + 1, None);
                return;
            }
        };
        let Some(input) = pending.into_iter().next() else {
            return;
        };
        let Some(metadata) = input.metadata else {
            tracing::error!(input_id = %input.id, "queued prompt has no request metadata");
            if let Err(error) = input_repo.mark_as_failed(&input.id).await {
                tracing::error!(input_id = %input.id, %error, "failed marking malformed prompt");
                return;
            }
            spawn_next_queued_prompt(state.clone(), session_id.clone());
            return;
        };
        let request: QueuedPromptRequest = match serde_json::from_value(metadata) {
            Ok(request) => request,
            Err(error) => {
                tracing::error!(input_id = %input.id, %error, "invalid queued prompt metadata");
                if let Err(mark_error) = input_repo.mark_as_failed(&input.id).await {
                    tracing::error!(input_id = %input.id, %mark_error, "failed marking malformed prompt");
                    return;
                }
                spawn_next_queued_prompt(state.clone(), session_id.clone());
                return;
            }
        };
        spawn_prompt_execution(
            state,
            session_id,
            input.prompt,
            request,
            Some(input.id),
            None,
            attempt,
        );
    });
}

/// 失敗したキュー処理を指数バックオフ付きで再スケジュールする。
///
/// 上限を超えた場合は task を起動せず、既知のキュー項目を `mark_as_failed`
/// で terminal 化して無限リトライを防ぐ。再起動時の
/// `resume_queued_prompts` は pending のみを再読込するため、failed 化した行は
/// 再投入されず attempt がリセットされることもない。
/// `queue_id` が不明な経路（DB 読取自体の失敗）では terminal 化を行わず、
/// 既存の error ログのみで終端する。
fn retry_queued_prompts(
    state: Arc<ServerState>,
    session_id: String,
    attempt: u32,
    queue_id: Option<String>,
) {
    let Some(delay) = queued_retry_delay(attempt) else {
        tracing::error!(%session_id, attempt, max = QUEUED_PROMPT_MAX_RETRIES,
            "queued prompt retries exhausted; stopping reschedule");
        if let Some(queue_id) = queue_id {
            tokio::spawn(async move {
                if let Some(input_repo) = state.input_repo.as_ref() {
                    if input_repo.mark_as_failed(&queue_id).await.is_err() {
                        tracing::warn!(%queue_id,
                            "failed terminalizing exhausted queued prompt");
                    }
                }
            });
        }
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        spawn_next_queued_prompt_with_attempt(state, session_id, attempt);
    });
}

/// 中断されたキュー項目を failed へ終端化し、次のキュー投入を再スケジュールする。
///
/// abort 経路と `PromptTaskCleanup::drop` 経路の両方から呼ばれるが、実際に
/// 回収を行うのは `running_prompt_queue_ids` から ID を取り出した側 1 つだけ
/// （単発フラグ）。冪等性は `mark_as_failed` の
/// `status IN ('pending','processing')` 条件と、`spawn_prompt_execution` の
/// `claim_pending` の原子性により担保される。
async fn recover_interrupted_queued_prompt(
    state: Arc<ServerState>,
    session_id: String,
    queue_id: Option<String>,
) {
    if let (Some(queue_id), Some(input_repo)) = (queue_id.as_deref(), state.input_repo.as_ref()) {
        if let Err(error) = input_repo.mark_as_failed(queue_id).await {
            tracing::error!(input_id = %queue_id, %error, "failed terminalizing interrupted queued prompt");
        }
    }
    // 削除済みセッションでは次キューを起動しない（生成の再開を防ぐ）。
    let session_exists = state.sessions.lock().await.get(&session_id).is_some();
    if session_exists {
        spawn_next_queued_prompt(state, session_id);
    }
}

/// セッションの実行中プロンプトをキャンセルし、実行状態を Idle に戻す。
/// レスポンスの形は従来どおり `aborted: true` を維持する。
async fn abort_running_prompt(
    state: &Arc<ServerState>,
    session_id: &str,
) -> Result<Json<serde_json::Value>> {
    {
        let sessions = state.sessions.lock().await;
        if sessions.get(session_id).is_none() {
            return Err(ApiError::SessionNotFound(session_id.to_string()));
        }
    }
    // キュー項目 ID の取り出しを単発フラグとして使う。取得できた側だけが回収し、
    // タスク側の Drop は取り出し失敗を以て回収済みと判断してスキップする。
    // ID が無い（キュー由来でない／既に回収済み）場合は何も再スケジュールしない。
    let queue_id = state.take_prompt_queue_id(session_id);
    let found = state.abort_prompt_task(session_id);
    set_session_run_status(state, session_id, SessionRunStatus::Idle).await;
    tracing::info!(session_id = %session_id, task_found = found, "session abort requested");
    if let Some(queue_id) = queue_id {
        recover_interrupted_queued_prompt(state.clone(), session_id.to_string(), Some(queue_id))
            .await;
    }
    Ok(Json(serde_json::json!({ "aborted": true })))
}

async fn abort_prompt(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    abort_running_prompt(&state, &id).await
}

async fn abort_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    abort_running_prompt(&state, &id).await
}

#[derive(Debug, Deserialize)]
pub struct UpdateSessionTimeRequest {
    pub archived: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSessionRequest {
    pub title: Option<String>,
    pub time: Option<UpdateSessionTimeRequest>,
}

async fn update_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionRequest>,
) -> Result<Json<SessionInfo>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;

    if let Some(title) = req.title {
        session.set_title(title);
    }
    if let Some(time) = req.time {
        if let Some(archived) = time.archived {
            session.set_archived(Some(archived));
        }
    }
    let info = session_to_info(session);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;
    Ok(Json(info))
}

async fn get_message(
    State(state): State<Arc<ServerState>>,
    Path((session_id, msg_id)): Path<(String, String)>,
) -> Result<Json<MessageInfo>> {
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;
    let message = session
        .get_message(&msg_id)
        .ok_or_else(|| ApiError::NotFound(format!("Message not found: {}", msg_id)))?;

    // list_messages と同じ message_to_info()/part_to_info() を経由させ、
    // wire スキーマ（snake_case キー + 正規化済み parts）を統一する。
    // 旧来の手組み `{"info": {"sessionID", "createdAt", ...}, "parts": raw}` は
    // TUI の MessageInfo（sessionID/createdAt に alias なし）で受け取れなかった。
    // ログやエラーにトークン値・パスを出力する処理は追加しない。
    Ok(Json(message_to_info(&session_id, message)))
}

#[derive(Debug, Deserialize)]
pub struct UpdatePartRequest {
    pub part: serde_json::Value,
}

async fn update_part(
    State(state): State<Arc<ServerState>>,
    Path((session_id, msg_id, part_id)): Path<(String, String, String)>,
    Json(req): Json<UpdatePartRequest>,
) -> Result<Json<serde_json::Value>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::SessionNotFound(session_id.clone()))?;
    let message = session
        .get_message_mut(&msg_id)
        .ok_or_else(|| ApiError::NotFound(format!("Message not found: {}", msg_id)))?;

    let mut part: opencode_session::MessagePart = serde_json::from_value(req.part)
        .map_err(|e| ApiError::BadRequest(format!("Invalid part payload: {}", e)))?;
    if part.id != part_id {
        return Err(ApiError::BadRequest(format!(
            "Part id mismatch: body has {}, path has {}",
            part.id, part_id
        )));
    }
    part.message_id = Some(msg_id.clone());

    let updated_part = {
        let target = message
            .parts
            .iter_mut()
            .find(|existing| existing.id == part_id)
            .ok_or_else(|| ApiError::NotFound(format!("Part not found: {}", part_id)))?;
        *target = part.clone();
        target.clone()
    };
    session.touch();
    drop(sessions);
    persist_sessions_if_enabled(&state).await;

    Ok(Json(serde_json::json!({
        "updated": true,
        "part": updated_part,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ExecuteShellRequest {
    pub command: String,
    pub workdir: Option<String>,
}

async fn execute_shell(
    State(_state): State<Arc<ServerState>>,
    Path(_id): Path<String>,
    Json(_req): Json<ExecuteShellRequest>,
) -> Response {
    not_implemented("shell")
}

#[derive(Debug, Deserialize)]
pub struct TextPartInput {
    #[serde(rename = "type")]
    pub part_type: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PromptAsyncRequest {
    pub message: Option<String>,
    pub parts: Option<Vec<TextPartInput>>,
    pub model: Option<serde_json::Value>,
}

fn text_from_parts(parts: &[TextPartInput]) -> String {
    parts
        .iter()
        .filter_map(|p| {
            if p.part_type.as_deref() == Some("text") {
                p.text.as_deref()
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn prompt_async(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<PromptAsyncRequest>,
) -> Result<Json<serde_json::Value>> {
    let message_text: String = req
        .message
        .clone()
        .or_else(|| {
            req.parts.as_ref().and_then(|p| {
                let s = text_from_parts(p);
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            })
        })
        .unwrap_or_default();

    {
        let sessions = state.sessions.lock().await;
        if sessions.get(&id).is_none() {
            return Err(ApiError::SessionNotFound(id));
        }
    }

    let input_repo = state
        .input_repo
        .as_ref()
        .ok_or_else(|| ApiError::InternalError("durable queue storage unavailable".into()))?;
    let (provider_id, model_id) = {
        let request_model = req.model.as_ref().and_then(serde_json::Value::as_str);
        let config = state.config.read().await;
        let (_, provider_id, model_id) =
            resolve_provider_and_model(&state, request_model, config.model.as_deref(), None)?;
        (provider_id, model_id)
    };
    let request = QueuedPromptRequest {
        model: ProviderModel {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
        },
        agent: None,
        variant: None,
    };
    let metadata = serde_json::to_value(&request)
        .map_err(|error| ApiError::InternalError(error.to_string()))?;
    let input = input_repo
        .enqueue(&id, &message_text, Some("queue"), Some(&metadata))
        .await
        .map_err(|error| ApiError::InternalError(error.to_string()))?;
    spawn_prompt_execution(
        state.clone(),
        id,
        message_text,
        request,
        Some(input.id.clone()),
        None,
        0,
    );

    Ok(Json(serde_json::json!({
        "status": "queued",
        "message_id": input.id,
        "model": req.model,
    })))
}

pub(crate) async fn resume_queued_prompts(state: Arc<ServerState>) -> anyhow::Result<()> {
    let Some(input_repo) = &state.input_repo else {
        return Ok(());
    };
    let mut session_ids = std::collections::HashSet::new();
    for input in input_repo.list_pending_all().await? {
        session_ids.insert(input.session_id);
    }
    for session_id in session_ids {
        spawn_next_queued_prompt(state.clone(), session_id);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct InitSessionRequest {
    pub force: Option<bool>,
}

async fn init_session(Path(_id): Path<String>, Json(_req): Json<InitSessionRequest>) -> Response {
    not_implemented("init")
}

#[derive(Debug, Deserialize)]
pub struct SummarizeSessionRequest {
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

async fn summarize_session(
    Path(_id): Path<String>,
    Json(_req): Json<SummarizeSessionRequest>,
) -> Response {
    not_implemented("summarize")
}

async fn session_unrevert(Path(_id): Path<String>) -> Response {
    not_implemented("unrevert")
}

#[derive(Debug, Deserialize)]
pub struct ExecuteCommandRequest {
    pub command: String,
    pub arguments: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
}

async fn execute_command(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ExecuteCommandRequest>,
) -> Result<Json<serde_json::Value>> {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| ApiError::SessionNotFound(id.clone()))?;
    let text = req
        .arguments
        .as_deref()
        .map(|args| format!("/{cmd} {args}", cmd = req.command))
        .unwrap_or_else(|| format!("/{}", req.command));
    session.add_user_message(text);
    let assistant = session.add_assistant_message();
    assistant.add_text(format!("Command queued: {}", req.command));
    let assistant_id = assistant.id.clone();
    let arguments = req
        .arguments
        .as_deref()
        .map(|value| {
            value
                .split_whitespace()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    sessions.publish_command_executed(&req.command, &id, arguments, &assistant_id);
    drop(sessions);
    persist_sessions_if_enabled(&state).await;

    Ok(Json(serde_json::json!({
        "executed": true,
        "command": req.command,
        "arguments": req.arguments,
        "model": req.model,
        "agent": req.agent,
        "message_id": assistant_id,
    })))
}

async fn get_session_diff(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<FileDiffInfo>>> {
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&id).ok_or(ApiError::SessionNotFound(id))?;
    let diffs = session
        .summary
        .as_ref()
        .and_then(|summary| summary.diffs.as_ref())
        .map(|items| {
            items
                .iter()
                .map(|diff| FileDiffInfo {
                    path: diff.path.clone(),
                    additions: diff.additions,
                    deletions: diff.deletions,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Json(diffs))
}

#[derive(Debug, Serialize)]
pub struct FileDiffInfo {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

fn provider_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_providers))
        .route("/auth", get(get_provider_auth))
        .route("/{id}/oauth/authorize", post(oauth_authorize))
        .route("/{id}/oauth/callback", post(oauth_callback))
}

#[derive(Debug, Serialize)]
pub struct ProviderListResponse {
    pub all: Vec<ProviderInfo>,
    /// `/config/providers` と同じトップレベルキー。`all` のエイリアス。
    /// 既存クライアント互換のため `all` は維持したまま、新規クライアントは
    /// この `providers` キーを統一的に利用できる。
    pub providers: Vec<ProviderInfo>,
    #[serde(rename = "default")]
    pub default_model: HashMap<String, String>,
    pub connected: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
}

type ModelVariantLookup = HashMap<String, HashMap<String, Vec<String>>>;

// 外側の Mutex は「現在の generation を表す OnceCell の差し替え」だけを守り、
// 決して await を跨いで保持しない（patch 経路の invalidate を 2 秒待たせない）。
//
// generation ごとに別の `Arc<OnceCell<_>>` を使う理由:
//   * `OnceCell::get_or_init` が初期化を 1 回だけ実行し、並行 init でも load は 1 回。
//   * invalidate は cell を差し替えるだけなので、古い load が完了しても
//     切り離された旧 cell に書き込まれるだけで、新しいキャッシュを上書きしない。
// 値は models.json 由来の provider_id / model_id / variant 名のみで、
// トークン値やパスは含めず、ログやエラーにも出さない。
type VariantLookupCell = Arc<OnceCell<Arc<ModelVariantLookup>>>;

static MODEL_VARIANT_LOOKUP: Mutex<Option<VariantLookupCell>> = Mutex::const_new(None);

#[cfg(test)]
static VARIANT_LOOKUP_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn load_models_dev_data() -> ModelsData {
    let cache_path = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("opencode")
        .join("models.json");

    if let Ok(content) = tokio::fs::read_to_string(&cache_path).await {
        if let Ok(parsed) = serde_json::from_str::<ModelsData>(&content) {
            return parsed;
        }
    }

    let registry = ModelsRegistry::default();
    match tokio::time::timeout(Duration::from_secs(2), registry.get()).await {
        Ok(data) => data,
        Err(_) => HashMap::new(),
    }
}

fn build_model_variant_lookup(data: ModelsData) -> HashMap<String, HashMap<String, Vec<String>>> {
    data.into_iter()
        .map(|(provider_id, provider)| {
            let model_map = provider
                .models
                .into_iter()
                .map(|(model_id, model)| {
                    let mut variants = model
                        .variants
                        .as_ref()
                        .map(|items| items.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default();
                    if variants.is_empty() {
                        variants = synthetic_variant_names(&provider_id, &model);
                    }
                    variants.sort();
                    (model_id, variants)
                })
                .collect::<HashMap<_, _>>();
            (provider_id, model_map)
        })
        .collect()
}

fn synthetic_variant_names(provider_id: &str, model: &ModelsDevInfo) -> Vec<String> {
    if !model.reasoning {
        return Vec::new();
    }

    let provider = provider_id.to_ascii_lowercase();
    let model_id = model.id.to_ascii_lowercase();
    let is_anthropic = provider.contains("anthropic") || model_id.contains("claude");
    if is_anthropic {
        return vec!["high".to_string(), "max".to_string()];
    }

    let is_google =
        provider.contains("google") || provider.contains("vertex") || model_id.contains("gemini");
    if is_google {
        return vec!["high".to_string(), "max".to_string()];
    }

    vec!["low".to_string(), "medium".to_string(), "high".to_string()]
}

fn max_tokens_for_variant(default_max_tokens: u64, variant: Option<&str>) -> u64 {
    match variant.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "none" || v == "minimal" => 1024,
        Some(v) if v == "low" => 2048,
        Some(v) if v == "medium" => 4096,
        Some(v) if v == "high" => 8192,
        Some(v) if v == "max" || v == "xhigh" => 16384,
        _ => default_max_tokens,
    }
}

async fn get_model_variant_lookup() -> Arc<ModelVariantLookup> {
    get_model_variant_lookup_with(load_models_dev_data).await
}

/// `loader` を差し替え可能な取得本体。実処理はロックを一切跨がない。
///
/// 1. キャッシュ命中確認は Mutex を短時間だけ取り、ガードを落としてから返す。
/// 2. 未命中時は cell を 1 つだけ選び、`OnceCell::get_or_init` 内で
///    `loader().await` を実行する。他スレッドが初期化中ならその完了を待ち、
///    既に格納されていればその値を返す（load は 1 回だけ）。
/// 3. invalidate が途中で走ると cell 自体が差し替わるため、古い load の結果は
///    切り離された旧 cell に留まり、新しいキャッシュへ復活しない。
async fn get_model_variant_lookup_with<F, Fut>(loader: F) -> Arc<ModelVariantLookup>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ModelsData>,
{
    // このブロック内では await しない。ガードはここで必ずドロップされる。
    let cell = {
        let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
        if let Some(cached) = guard.as_ref() {
            if let Some(lookup) = cached.get() {
                return Arc::clone(lookup);
            }
            Arc::clone(cached)
        } else {
            let created = Arc::new(OnceCell::new());
            *guard = Some(Arc::clone(&created));
            created
        }
    };

    let lookup = cell
        .get_or_init(move || async move {
            let data = loader().await;
            Arc::new(build_model_variant_lookup(data))
        })
        .await;
    Arc::clone(lookup)
}

/// models.json 由来の variant lookup キャッシュを破棄する。
/// 破棄するのは派生データのみで、トークン値・パスをログやエラーへ出さない。
async fn invalidate_model_variant_lookup() {
    let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
    let _ = guard.take();
}

fn variants_for_model(
    lookup: &HashMap<String, HashMap<String, Vec<String>>>,
    provider_id: &str,
    model_id: &str,
) -> Vec<String> {
    lookup
        .get(provider_id)
        .and_then(|models| models.get(model_id))
        .cloned()
        .unwrap_or_default()
}

async fn list_providers(State(state): State<Arc<ServerState>>) -> Json<ProviderListResponse> {
    let variant_lookup = get_model_variant_lookup().await;
    let models = state
        .providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .list_models();
    Json(build_provider_list_response(models, &variant_lookup))
}

/// `/provider` レスポンスを組み立てる純粋関数。
///
/// `all` は不変のまま維持し、`providers` には同一の `Vec<ProviderInfo>` を
/// クローンして設定する（後方互換のため `all` キーは削除しない）。
/// 入力は provider ID / model ID / variant 名のみで、トークン値やパスは
/// 含めないため、ログやエラーへ流出することはない。
fn build_provider_list_response(
    models: Vec<opencode_provider::ModelInfo>,
    variant_lookup: &ModelVariantLookup,
) -> ProviderListResponse {
    let mut provider_map: HashMap<String, Vec<ModelInfo>> = HashMap::new();
    for m in models {
        let provider_id = m.provider.clone();
        let model_id = m.id.clone();
        let variants = variants_for_model(variant_lookup, &provider_id, &model_id);
        provider_map
            .entry(provider_id.clone())
            .or_default()
            .push(ModelInfo {
                id: model_id,
                name: m.name,
                provider: provider_id,
                variants,
            });
    }
    let all: Vec<ProviderInfo> = provider_map
        .into_iter()
        .map(|(id, models)| ProviderInfo {
            id: id.clone(),
            name: id,
            models,
        })
        .collect();
    let providers = all.clone();
    let connected: Vec<String> = all.iter().map(|p| p.id.clone()).collect();
    let default_model: HashMap<String, String> = all
        .iter()
        .filter_map(|p| p.models.first().map(|m| (p.id.clone(), m.id.clone())))
        .collect();
    ProviderListResponse {
        all,
        providers,
        default_model,
        connected,
    }
}

#[cfg(test)]
mod provider_list_response_tests {
    use super::*;

    /// テスト専用のモデル情報。トークン値やパスは含めず、ログ・エラーへも出さない。
    fn sample_model(id: &str, provider: &str) -> opencode_provider::ModelInfo {
        opencode_provider::ModelInfo {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            context_window: 128_000,
            max_output_tokens: 8_192,
            supports_vision: false,
            supports_tools: true,
            cost_per_million_input: 0.0,
            cost_per_million_output: 0.0,
        }
    }

    /// API-07 回帰ガード: `/provider` レスポンスには `all` と同一内容の
    /// `providers` キーが存在し、既存の `default` / `connected` の形状が
    /// 不変であること。
    #[test]
    fn provider_list_response_has_providers_alias_equal_to_all() {
        let lookup = ModelVariantLookup::new();
        let models = vec![
            sample_model("model-a", "alpha"),
            sample_model("model-b", "alpha"),
            sample_model("model-c", "beta"),
        ];

        let response = build_provider_list_response(models, &lookup);
        let value = serde_json::to_value(&response).expect("response must serialize");

        let all = value.get("all").expect("`all` key must exist");
        let providers = value
            .get("providers")
            .expect("`providers` alias key must exist");
        assert_eq!(all, providers, "`providers` must mirror `all` exactly");

        let default = value
            .get("default")
            .and_then(|entry| entry.as_object())
            .expect("`default` must remain an object");
        assert_eq!(
            default.get("alpha").and_then(|entry| entry.as_str()),
            Some("model-a")
        );
        assert_eq!(
            default.get("beta").and_then(|entry| entry.as_str()),
            Some("model-c")
        );

        let connected = value
            .get("connected")
            .and_then(|entry| entry.as_array())
            .expect("`connected` must remain an array");
        assert_eq!(connected.len(), 2);
        for provider_id in ["alpha", "beta"] {
            assert!(
                connected
                    .iter()
                    .any(|entry| entry.as_str() == Some(provider_id)),
                "`connected` must still list provider ids: {provider_id}"
            );
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuthMethodInfo {
    pub name: String,
    pub description: String,
}

async fn get_provider_auth(
    State(state): State<Arc<ServerState>>,
) -> Json<HashMap<String, Vec<AuthMethodInfo>>> {
    let Some(loader) = state.plugin_loader() else {
        return Json(HashMap::new());
    };
    let methods = ProviderAuth::methods(&loader).await;
    let result = methods
        .into_iter()
        .map(|(provider, values)| {
            let mapped = values
                .into_iter()
                .map(|method| AuthMethodInfo {
                    name: method.label,
                    description: method.method_type,
                })
                .collect::<Vec<_>>();
            (provider, mapped)
        })
        .collect::<HashMap<_, _>>();
    Json(result)
}

#[derive(Debug, Deserialize)]
pub struct OAuthAuthorizeRequest {
    pub method: usize,
}

#[derive(Debug, Serialize)]
pub struct OAuthAuthorizeResponse {
    pub url: String,
    #[serde(rename = "method")]
    pub method_type: String,
    pub instructions: String,
}

async fn oauth_authorize(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<OAuthAuthorizeRequest>,
) -> Result<Json<OAuthAuthorizeResponse>> {
    let loader = state
        .plugin_loader()
        .ok_or_else(|| ApiError::NotFound("no plugin loader initialized".to_string()))?;
    let authorization = ProviderAuth::authorize(&loader, &id, req.method, None)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(OAuthAuthorizeResponse {
        url: authorization.url,
        method_type: match authorization.method {
            AuthMethodType::Auto => "auto".to_string(),
            AuthMethodType::Code => "code".to_string(),
        },
        instructions: authorization.instructions,
    }))
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackRequest {
    pub method: usize,
    pub code: Option<String>,
}

async fn oauth_callback(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<OAuthCallbackRequest>,
) -> Result<Json<bool>> {
    let loader = state
        .plugin_loader()
        .ok_or_else(|| ApiError::NotFound("no plugin loader initialized".to_string()))?;
    ProviderAuth::new(state.auth_manager.clone())
        .callback(&loader, &id, req.code.as_deref())
        .await
        .map_err(|error| match error {
            ProviderAuthError::Auth(error) => ApiError::BadRequest(error.to_string()),
            ProviderAuthError::Persist(error) => ApiError::InternalError(error.to_string()),
        })?;

    // Refresh auth loader state after callback and apply custom-fetch proxy changes immediately.
    if let Some(bridge) = loader.auth_bridge(&id).await {
        match bridge.load().await {
            Ok(load_result) => {
                crate::server::sync_custom_fetch_proxy(&id, bridge, load_result.has_custom_fetch);
            }
            Err(error) => {
                crate::server::sync_custom_fetch_proxy(&id, bridge, false);
                tracing::warn!(
                    provider = %id,
                    %error,
                    "failed to refresh plugin auth loader after oauth callback"
                );
            }
        }
    }

    Ok(Json(true))
}

fn config_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(get_config).patch(patch_config))
        .route("/providers", get(get_config_providers))
}

/// Whether a JSON object key names a credential that must never be returned.
///
/// Matching is case-insensitive and ignores `-`/`_` separators so `apiKey`,
/// `api_key`, `API-KEY` and `Authorization` are all covered by one check.
fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "apikey"
            | "authorization"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "secret"
            | "clientsecret"
            | "password"
            | "passwd"
            | "credential"
            | "credentials"
            | "privatekey"
    )
}

/// Recursively replace non-empty values under known secret keys with `***`.
///
/// Objects and arrays are walked so credentials nested in `settings`,
/// `options` or `headers` are covered as well, while the response shape stays
/// unchanged. Only non-empty string values are masked, and original values
/// never reach logs or errors.
fn mask_nested_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_secret_key(key.as_str()) && child.as_str().is_some_and(|s| !s.is_empty()) {
                    *child = serde_json::json!("***");
                } else {
                    mask_nested_secrets(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                mask_nested_secrets(item);
            }
        }
        _ => {}
    }
}

/// Serialize a config for API responses with provider credentials masked at
/// every nesting level (top level, `settings`, `options` and `headers`).
fn config_for_response(config: &AppConfig) -> serde_json::Value {
    let mut value = match serde_json::to_value(config) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };
    if let Some(obj) = value.as_object_mut() {
        for key in ["provider", "providers"] {
            if let Some(providers) = obj.get_mut(key).and_then(|p| p.as_object_mut()) {
                for provider in providers.values_mut() {
                    mask_nested_secrets(provider);
                }
            }
        }
    }
    value
}

async fn get_config(State(state): State<Arc<ServerState>>) -> Result<Json<serde_json::Value>> {
    let config = state.config.read().await;
    Ok(Json(config_for_response(&config)))
}

/// PATCH のうち再起動が必要なキーを列挙する。設定値（特に秘密情報）は含めない。
fn restart_required_keys(patch: &AppConfig) -> Vec<String> {
    let mut keys = Vec::new();
    // log_level は起動時に解決され、PATCH 後も再読されないため再起動が必要。
    if patch.log_level.is_some() {
        keys.push("log_level".to_string());
    }
    let Some(server) = patch.server.as_ref() else {
        return keys;
    };
    if server.port.is_some() {
        keys.push("server.port".to_string());
    }
    if server.hostname.is_some() {
        keys.push("server.hostname".to_string());
    }
    if server.mdns.is_some() {
        keys.push("server.mdns".to_string());
    }
    if server.mdns_domain.is_some() {
        keys.push("server.mdnsDomain".to_string());
    }
    if server.cors.is_some() {
        keys.push("server.cors".to_string());
    }
    keys
}

/// 現在の config/auth から `ProviderRegistry` を作り直して差し替える。
/// 値ではなく provider_id のみを扱い、トークン値をログへ出さない。
async fn rebuild_provider_registry_after_patch(state: &Arc<ServerState>) {
    let registry = state.build_provider_registry_from_auth().await;
    state.replace_provider_registry(registry);
    // /providers が起動時の variants を返し続けないよう、models.json 由来の
    // キャッシュも破棄して次回リクエストで再計算させる（STATE-04）。
    invalidate_model_variant_lookup().await;
}

async fn patch_config(
    State(state): State<Arc<ServerState>>,
    Json(patch): Json<AppConfig>,
) -> Result<Json<serde_json::Value>> {
    let mut config = state.config.write().await;
    config.merge(patch.clone());
    // PATCH 後のインメモリ設定にも起動時と同じ post-load 変換を適用し、
    // mode/tools 移行や steps=0 の既定化が PATCH でも欠落しないようにする。
    // 環境由来の username 注入は永続化時と同じく行わない（親が R2 blocker を修正）。
    apply_post_load_transforms(&mut config, false);
    let updated = config.clone();
    drop(config);

    // Persist the merged config to the project directory so changes survive
    // a server restart. Failures are logged but do not abort the request.
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Err(error) = opencode_config::update_config(&cwd, &patch) {
        tracing::warn!(%error, "failed to persist patched config");
    }

    // 永続化後に再 bootstrap し、後続のモデル解決が更新済み config/auth を
    // 参照するようにする（再起動不要な項目は即時反映）。
    rebuild_provider_registry_after_patch(&state).await;
    let restart_required = restart_required_keys(&patch);

    let mut response = config_for_response(&updated);
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "restart_required".to_string(),
            serde_json::json!(restart_required),
        );
    }

    state.broadcast(
        &serde_json::json!({
            "type": "config.updated",
        })
        .to_string(),
    );
    Ok(Json(response))
}

#[derive(Debug, Serialize)]
pub struct ConfigProvidersResponse {
    pub providers: Vec<ProviderInfo>,
    #[serde(rename = "default")]
    pub default_model: HashMap<String, String>,
}

async fn get_config_providers(
    State(state): State<Arc<ServerState>>,
) -> Json<ConfigProvidersResponse> {
    let variant_lookup = get_model_variant_lookup().await;
    let models = state
        .providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .list_models();
    let mut provider_map: HashMap<String, Vec<ModelInfo>> = HashMap::new();
    for m in models {
        let provider_id = m.provider.clone();
        let model_id = m.id.clone();
        let variants = variants_for_model(&variant_lookup, &provider_id, &model_id);
        provider_map
            .entry(provider_id.clone())
            .or_default()
            .push(ModelInfo {
                id: model_id,
                name: m.name,
                provider: provider_id,
                variants,
            });
    }
    let providers: Vec<ProviderInfo> = provider_map
        .into_iter()
        .map(|(id, models)| ProviderInfo {
            id: id.clone(),
            name: id,
            models,
        })
        .collect();
    let default_model: HashMap<String, String> = providers
        .iter()
        .filter_map(|p| p.models.first().map(|m| (p.id.clone(), m.id.clone())))
        .collect();
    Json(ConfigProvidersResponse {
        providers,
        default_model,
    })
}

fn mcp_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(get_mcp_status).post(add_mcp_server))
        .route("/{name}/auth", post(start_mcp_auth).delete(remove_mcp_auth))
        .route("/{name}/auth/callback", post(mcp_auth_callback))
        .route("/{name}/auth/authenticate", post(mcp_authenticate))
        .route("/{name}/connect", post(connect_mcp))
        .route("/{name}/disconnect", post(disconnect_mcp))
        .route("/{name}/logs", get(get_mcp_logs))
        .route("/{name}/restart", post(restart_mcp))
}

#[derive(Debug, Serialize)]
pub struct McpStatusInfo {
    pub name: String,
    pub status: String,
    pub tools: usize,
    pub resources: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Fixed event name for the structured warn emitted when the on-disk MCP
/// configuration cannot be synchronized. It carries no error value, path, URL,
/// or token: the raw `ApiError` from `sync_mcp_from_disk` can embed absolute
/// config paths or the current working directory in its Display text.
const MCP_SYNC_WARN_EVENT: &str = "mcp_sync_from_disk_failed";

/// Fixed, non-sensitive warning text returned to clients when synchronization
/// from disk fails. The underlying error is intentionally omitted so no path,
/// URL, or token leaks onto the wire.
const MCP_SYNC_WARNING: &str =
    "MCP configuration could not be synchronized from disk; returning cached in-memory state.";

/// Header used to expose a disk-sync degradation without changing the
/// `GET /mcp` JSON body shape. The value is a fixed, non-sensitive string.
pub const MCP_SYNC_WARNING_HEADER: &str = "x-mcp-sync-warning";

/// MCP OAuth manager を保持する `ServerState` から取得する。
///
/// 旧実装は `OnceLock` の process-global static を参照していたため、同一プロセス内の
/// 別 `ServerState`（TUI 埋め込み再起動等）が OAuth フロー状態を共有していた。
/// 常にリクエストの `ServerState` 経由で取得する。
fn get_mcp_oauth_manager(state: &ServerState) -> &McpOAuthManager {
    &state.mcp_oauth_manager
}

impl From<McpServerInfoStruct> for McpStatusInfo {
    fn from(info: McpServerInfoStruct) -> Self {
        Self {
            name: info.name,
            status: info.status,
            tools: info.tools,
            resources: info.resources,
            error: info.error,
        }
    }
}

/// Builds the MCP status response from a disk-sync result and the currently
/// registered servers.
///
/// The raw `Err` is intentionally discarded (`Err(_)`): `sync_mcp_from_disk`
/// wraps config/cwd errors whose Display text can contain absolute paths. Both
/// the client-facing `sync_warning` and the tracing event use fixed text only,
/// so no path, URL, or token reaches the client or the logs.
fn build_mcp_status_response(
    sync_result: Result<()>,
    servers: Vec<McpServerInfoStruct>,
) -> (HeaderMap, HashMap<String, McpStatusInfo>) {
    let mut headers = HeaderMap::new();
    if sync_result.is_err() {
        tracing::warn!(
            event = MCP_SYNC_WARN_EVENT,
            "MCP configuration could not be synchronized from disk"
        );
        // 固定値のみをヘッダへ載せる（パス・URL・トークンは出さない）。
        if let Ok(value) = MCP_SYNC_WARNING.parse() {
            headers.insert(MCP_SYNC_WARNING_HEADER, value);
        }
    }
    let mut result = HashMap::new();
    for server in servers {
        result.insert(server.name.clone(), McpStatusInfo::from(server));
    }
    (headers, result)
}

async fn get_mcp_status(
    State(state): State<Arc<ServerState>>,
) -> (HeaderMap, Json<HashMap<String, McpStatusInfo>>) {
    let manager = get_mcp_oauth_manager(&state);
    let sync_result = sync_mcp_from_disk(manager).await;
    let servers = manager.list_servers().await;
    let (headers, body) = build_mcp_status_response(sync_result, servers);
    (headers, Json(body))
}

#[derive(Debug, Deserialize)]
pub struct AddMcpRequest {
    pub name: String,
    pub config: McpConfigInput,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum McpCommandInput {
    String(String),
    Array(Vec<String>),
}

#[derive(Debug, Deserialize)]
pub struct McpConfigInput {
    #[serde(rename = "type")]
    pub server_type: Option<String>,
    pub command: Option<McpCommandInput>,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
    pub environment: Option<HashMap<String, String>>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    pub enabled: Option<bool>,
    pub timeout: Option<McpTimeoutConfig>,
    pub protocol: Option<String>,
    pub oauth: Option<serde_json::Value>,
    pub client_id: Option<String>,
    pub authorization_url: Option<String>,
}

async fn add_mcp_server(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<AddMcpRequest>,
) -> Result<Json<HashMap<String, McpStatusInfo>>> {
    let manager = get_mcp_oauth_manager(&state);
    let (runtime, enabled) = parse_runtime_from_request(req.config)?;
    manager.add_server(req.name.clone(), runtime, enabled).await;
    if enabled {
        manager
            .connect(&req.name)
            .await
            .map_err(mcp_error_to_api_error)?;
    }

    let servers = manager.list_servers().await;
    let mut result = HashMap::new();
    for server in servers {
        result.insert(server.name.clone(), McpStatusInfo::from(server));
    }
    Ok(Json(result))
}

async fn start_mcp_auth(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    let oauth_state = manager
        .start_oauth(&name)
        .await
        .map_err(mcp_error_to_api_error)?;

    Ok(Json(serde_json::json!({
        "authorization_url": oauth_state.authorization_url,
        "client_id": oauth_state.client_id,
        "status": oauth_state.status
    })))
}

#[derive(Debug, Deserialize)]
pub struct McpAuthCallbackRequest {
    pub code: String,
    /// CSRF state returned by the authorization server. Required: a missing
    /// value is rejected with 400 rather than falling back to the stored state,
    /// which would make the state comparison vacuous.
    #[serde(default)]
    pub state: Option<String>,
}

async fn mcp_auth_callback(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
    Json(req): Json<McpAuthCallbackRequest>,
) -> Result<Json<McpStatusInfo>> {
    // The caller must echo back the state it received. Without this check,
    // handle_callback would compare the stored state against itself and a
    // code-only callback could complete the authorization flow.
    let oauth_state = req.state.as_deref().ok_or_else(|| {
        ApiError::BadRequest("mcp oauth callback requires the state parameter".to_string())
    })?;
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    let server_info = manager
        .handle_callback(&name, &req.code, Some(oauth_state))
        .await
        .map_err(mcp_error_to_api_error)?;

    Ok(Json(McpStatusInfo::from(server_info)))
}

async fn mcp_authenticate(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<McpStatusInfo>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    let server_info = manager
        .authenticate(&name)
        .await
        .map_err(mcp_error_to_api_error)?;

    Ok(Json(McpStatusInfo::from(server_info)))
}

async fn remove_mcp_auth(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    // ディスク上の認証情報削除に失敗した場合は success を返さない。
    // 古いトークンが残ったまま「ログアウト済み」と誤認されるのを防ぐ。
    if !manager.remove_oauth(&name).await {
        return Err(ApiError::InternalError(
            "failed to delete stored MCP OAuth credentials".to_string(),
        ));
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn connect_mcp(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<bool>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    manager
        .connect(&name)
        .await
        .map_err(mcp_error_to_api_error)?;
    Ok(Json(true))
}

async fn disconnect_mcp(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<bool>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    manager
        .disconnect(&name)
        .await
        .map_err(mcp_error_to_api_error)?;
    Ok(Json(true))
}

#[derive(Debug, Serialize)]
pub struct McpLogsResponse {
    pub name: String,
    pub logs: Vec<McpServerLogEntry>,
}

async fn get_mcp_logs(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<McpLogsResponse>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    let logs = manager
        .get_logs(&name)
        .await
        .map_err(mcp_error_to_api_error)?;

    Ok(Json(McpLogsResponse { name, logs }))
}

#[derive(Debug, Serialize)]
pub struct McpRestartResponse {
    pub success: bool,
    pub server: McpStatusInfo,
}

async fn restart_mcp(
    State(state): State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<McpRestartResponse>> {
    let manager = get_mcp_oauth_manager(&state);
    ensure_mcp_server_registered(manager, &name).await?;
    let server = manager
        .restart(&name)
        .await
        .map_err(mcp_error_to_api_error)?;

    Ok(Json(McpRestartResponse {
        success: true,
        server: server.into(),
    }))
}

fn mcp_error_to_api_error(error: McpOAuthError) -> ApiError {
    match error {
        McpOAuthError::ServerNotFound(name) => {
            ApiError::NotFound(format!("MCP server not found: {}", name))
        }
        McpOAuthError::OAuthNotSupported(name) => {
            ApiError::BadRequest(format!("MCP server does not support OAuth: {}", name))
        }
        McpOAuthError::OAuthInProgress => ApiError::BadRequest("OAuth already in progress".into()),
        McpOAuthError::OAuthFailed(message) => ApiError::BadRequest(message),
        McpOAuthError::RuntimeError(message) => ApiError::BadRequest(message),
    }
}

/// Converts the config `oauth` object into the runtime settings.
fn remote_oauth_settings(
    oauth: Option<&opencode_config::McpOAuthConfig>,
) -> Option<RemoteOAuthSettings> {
    match oauth {
        Some(opencode_config::McpOAuthConfig::Config(config)) => Some(RemoteOAuthSettings {
            client_secret: config.client_secret.clone(),
            scope: config.scope.clone(),
            callback_port: config.callback_port,
            redirect_uri: config.redirect_uri.clone(),
            auth_server_metadata_url: config.auth_server_metadata_url.clone(),
        }),
        _ => None,
    }
}

/// Resolves effective MCP timeouts from the global `mcp.timeout` defaults and
/// a server's own `timeout` override (see the OpenCode MCP schema).
fn mcp_timeouts(
    global: Option<&McpTimeoutConfig>,
    server: Option<&McpTimeoutConfig>,
) -> McpTimeouts {
    let (startup, catalog, execution) = McpTimeoutConfig::resolve(global, server);
    McpTimeouts {
        startup_ms: startup,
        catalog_ms: catalog,
        execution_ms: execution,
    }
}

fn parse_runtime_from_request(config: McpConfigInput) -> Result<(McpRuntimeConfig, bool)> {
    let enabled = config.enabled.unwrap_or(true);
    let timeouts = mcp_timeouts(None, config.timeout.as_ref());
    let is_remote = matches!(config.server_type.as_deref(), Some("remote")) || config.url.is_some();

    if is_remote {
        let url = config
            .url
            .ok_or_else(|| ApiError::BadRequest("MCP remote config requires `url`".to_string()))?;
        let oauth_enabled = !matches!(config.oauth, Some(serde_json::Value::Bool(false)));
        return Ok((
            McpRuntimeConfig::Remote(RemoteMcpConfig {
                url,
                oauth_enabled,
                client_id: config.client_id,
                authorization_url: config.authorization_url,
                headers: None,
                timeouts,
                oauth: None,
                protocol: config.protocol.clone(),
            }),
            enabled,
        ));
    }

    let (command, args) = parse_command_and_args(config.command, config.args.unwrap_or_default())?;
    Ok((
        McpRuntimeConfig::Local(LocalMcpConfig {
            command,
            args,
            env: config.env.or(config.environment),
            cwd: config.cwd,
            timeouts,
            protocol: config.protocol.clone(),
        }),
        enabled,
    ))
}

fn parse_command_and_args(
    command: Option<McpCommandInput>,
    extra_args: Vec<String>,
) -> Result<(String, Vec<String>)> {
    match command {
        Some(McpCommandInput::String(cmd)) => {
            if cmd.trim().is_empty() {
                return Err(ApiError::BadRequest(
                    "MCP local config `command` cannot be empty".to_string(),
                ));
            }
            Ok((cmd, extra_args))
        }
        Some(McpCommandInput::Array(parts)) => {
            let mut iter = parts.into_iter();
            let cmd = iter
                .next()
                .ok_or_else(|| {
                    ApiError::BadRequest(
                        "MCP local config `command` array cannot be empty".to_string(),
                    )
                })?
                .trim()
                .to_string();
            if cmd.is_empty() {
                return Err(ApiError::BadRequest(
                    "MCP local config `command` cannot be empty".to_string(),
                ));
            }
            let mut args: Vec<String> = iter.collect();
            args.extend(extra_args);
            Ok((cmd, args))
        }
        None => Err(ApiError::BadRequest(
            "MCP local config requires `command`".to_string(),
        )),
    }
}

fn parse_runtime_from_loaded_config(
    config: LoadedMcpServerConfig,
    global_timeout: Option<&McpTimeoutConfig>,
) -> Result<Option<(McpRuntimeConfig, bool)>> {
    match config {
        LoadedMcpServerConfig::Enabled { .. } => Ok(None),
        LoadedMcpServerConfig::Full(server) => {
            let enabled = server.enabled.unwrap_or(true);
            let timeouts = mcp_timeouts(global_timeout, server.timeout.as_ref());

            if let Some(url) = server.url {
                let oauth_enabled = !matches!(
                    server.oauth.as_ref(),
                    Some(opencode_config::McpOAuthConfig::Disabled(false))
                );
                return Ok(Some((
                    McpRuntimeConfig::Remote(RemoteMcpConfig {
                        url,
                        oauth_enabled,
                        client_id: server.client_id,
                        authorization_url: server.authorization_url,
                        headers: server.headers,
                        timeouts,
                        oauth: remote_oauth_settings(server.oauth.as_ref()),
                        protocol: server.protocol.map(|p| p.revision().to_string()),
                    }),
                    enabled,
                )));
            }

            if !server.command.is_empty() {
                let mut cmd_iter = server.command.into_iter();
                let command = cmd_iter.next().unwrap();
                let mut args: Vec<String> = cmd_iter.collect();
                args.extend(server.args);
                return Ok(Some((
                    McpRuntimeConfig::Local(LocalMcpConfig {
                        command,
                        args,
                        env: server.env.or(server.environment),
                        cwd: server.cwd,
                        timeouts,
                        protocol: server.protocol.map(|p| p.revision().to_string()),
                    }),
                    enabled,
                )));
            }

            Ok(None)
        }
    }
}

async fn sync_mcp_from_disk(manager: &McpOAuthManager) -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve current directory: {}", e)))?;
    let config = load_config(&cwd)
        .map_err(|e| ApiError::BadRequest(format!("Failed to load config: {}", e)))?;

    // An empty (or absent) MCP section still means "no servers on disk", so
    // previously disk-loaded entries must be evicted rather than cached
    // forever.
    let (mcp_map, global_timeout) = match config.mcp {
        Some(section) => (section.servers.unwrap_or_default(), section.timeout.clone()),
        None => (std::collections::HashMap::new(), None),
    };

    // Reconciliation is limited to entries that were originally loaded from
    // disk. CRUD-created servers are intentionally left alone so API-added
    // servers keep surviving later GET /mcp calls (existing behaviour).
    let disk_names: std::collections::HashSet<String> =
        manager.disk_server_names().await.into_iter().collect();
    for name in &disk_names {
        if !mcp_map.contains_key(name) {
            manager.remove_server(name).await;
        }
    }

    for (name, server_config) in mcp_map {
        let enabled = match &server_config {
            LoadedMcpServerConfig::Enabled { enabled } => Some(*enabled),
            LoadedMcpServerConfig::Full(server) => server.enabled,
        };

        if manager.has_server(&name).await {
            // Only disk-sourced entries follow the config file's enabled flag.
            // Existing runtime/config values are preserved for compatibility.
            if disk_names.contains(&name) {
                if let Some(enabled) = enabled {
                    manager.set_server_enabled(&name, enabled).await;
                }
            }
            continue;
        }
        if let Some((runtime, enabled)) =
            parse_runtime_from_loaded_config(server_config, global_timeout.as_ref())?
        {
            manager.add_disk_server(name, runtime, enabled).await;
        }
    }

    Ok(())
}

async fn ensure_mcp_server_registered(manager: &McpOAuthManager, name: &str) -> Result<()> {
    if manager.has_server(name).await {
        return Ok(());
    }

    sync_mcp_from_disk(manager).await?;
    if manager.has_server(name).await {
        return Ok(());
    }

    Err(ApiError::NotFound(format!(
        "MCP server not found: {}",
        name
    )))
}

fn file_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_files))
        .route("/content", get(read_file))
        .route("/status", get(get_file_status))
}

#[derive(Debug, Deserialize)]
pub struct ListFilesQuery {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub file_type: String,
    pub size: Option<u64>,
    pub modified: Option<i64>,
}

fn project_root() -> Result<PathBuf> {
    std::env::current_dir()
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve current directory: {}", e)))
}

fn canonicalize_within_root(path: &FsPath, root: &FsPath) -> Result<PathBuf> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve project root: {}", e)))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve path: {}", e)))?;

    if !canonical_path.starts_with(&canonical_root) {
        return Err(ApiError::BadRequest(
            "Access denied: path escapes project directory".to_string(),
        ));
    }

    Ok(canonical_path)
}

fn resolve_input_path(input: &str, root: &FsPath) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    let resolved = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if !resolved.exists() {
        return Err(ApiError::NotFound("File not found".to_string()));
    }
    canonicalize_within_root(&resolved, root)
}

fn is_within_root(path: &FsPath, root: &FsPath) -> bool {
    canonicalize_within_root(path, root).is_ok()
}

async fn list_files(Query(query): Query<ListFilesQuery>) -> Result<Json<Vec<FileInfo>>> {
    let root = project_root()?;
    let path = resolve_input_path(&query.path, &root)?;
    let mut files = Vec::new();

    if path.is_dir() {
        if let Ok(mut entries) = tokio::fs::read_dir(&path).await {
            // 旧実装の entries.flatten() と同じく、個別エントリの Err はスキップして継続する。
            // ただし連続エラーが無限に続くとハングするため上限を設ける。
            let mut consecutive_errors = 0usize;
            loop {
                let entry = match entries.next_entry().await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => break,
                    Err(_) => {
                        consecutive_errors += 1;
                        if consecutive_errors >= 100 {
                            break;
                        }
                        continue;
                    }
                };
                consecutive_errors = 0;
                let path_buf = entry.path();
                if !is_within_root(&path_buf, &root) {
                    continue;
                }
                let metadata = tokio::fs::metadata(&path_buf).await.ok();
                let file_type = match entry.file_type().await {
                    Ok(t) if t.is_dir() => "directory",
                    Ok(t) if t.is_file() => "file",
                    // シンボリックリンク等は元の Path::is_dir()/is_file() と同じく
                    // metadata を辿って判定し、wire format を維持する
                    _ => {
                        if metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
                            "directory"
                        } else {
                            "file"
                        }
                    }
                };
                let size = if metadata.as_ref().map(|m| m.is_file()).unwrap_or(false) {
                    metadata.as_ref().map(|m| m.len())
                } else {
                    None
                };
                let modified = metadata.and_then(|m| {
                    m.modified().ok().map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as i64
                    })
                });

                files.push(FileInfo {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: path_buf.to_string_lossy().to_string(),
                    file_type: file_type.to_string(),
                    size,
                    modified,
                });
            }
        }
    }

    Ok(Json(files))
}

async fn read_file(Query(query): Query<ListFilesQuery>) -> Result<Json<serde_json::Value>> {
    let root = project_root()?;
    let path = resolve_input_path(&query.path, &root)?;

    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(Json(
                serde_json::json!({ "content": content, "path": query.path }),
            )),
            Err(e) => Err(ApiError::BadRequest(format!("Failed to read file: {}", e))),
        }
    } else {
        Err(ApiError::BadRequest("Path is not a file".to_string()))
    }
}

async fn get_file_status() -> Result<Json<Vec<FileStatusInfo>>> {
    let cwd = std::env::current_dir()
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve current directory: {}", e)))?;
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .arg("status")
        .arg("--porcelain")
        .output()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Failed to run git status: {}", e)))?;

    if !output.status.success() {
        return Ok(Json(Vec::new()));
    }

    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.len() < 4 {
            continue;
        }
        let status_code = &line[..2];
        let mut path = line[3..].trim().to_string();
        if let Some((_, renamed_to)) = path.rsplit_once(" -> ") {
            path = renamed_to.to_string();
        }

        let staged = status_code.chars().next().unwrap_or(' ') != ' ';
        let status_char = if staged {
            status_code.chars().next().unwrap_or(' ')
        } else {
            status_code.chars().nth(1).unwrap_or(' ')
        };
        let status = match status_char {
            'M' => "modified",
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            'C' => "copied",
            'U' => "unmerged",
            '?' => "untracked",
            _ => "unknown",
        };

        files.push(FileStatusInfo {
            path,
            status: status.to_string(),
            staged,
        });
    }

    Ok(Json(files))
}

#[derive(Debug, Serialize)]
pub struct FileStatusInfo {
    pub path: String,
    pub status: String,
    pub staged: bool,
}

fn find_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/text", get(find_text))
        .route("/file", get(find_files))
        .route("/symbol", get(find_symbols))
}

#[derive(Debug, Deserialize)]
pub struct FindTextQuery {
    pub pattern: String,
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub match_text: String,
}

async fn find_text(Query(query): Query<FindTextQuery>) -> Result<Json<Vec<SearchResult>>> {
    let root = project_root()?;
    let base_input = query
        .path
        .unwrap_or_else(|| root.to_string_lossy().to_string());
    let base_path = resolve_input_path(&base_input, &root)?;
    let pattern = query.pattern;

    // 再帰走査とファイル読み取りは同期 I/O なので、所有値を move したクロージャを
    // spawn_blocking に逃がし、tokio ワーカーを占有しないようにする
    // （grep_tool.rs の採用前例に準拠）。ApiError / SearchResult は Send + 'static。
    let results = tokio::task::spawn_blocking(move || {
        fn search_in_file(path: &std::path::Path, pattern: &str, results: &mut Vec<SearchResult>) {
            if let Ok(content) = std::fs::read_to_string(path) {
                for (line_num, line) in content.lines().enumerate() {
                    if let Some(col) = line.find(pattern) {
                        results.push(SearchResult {
                            path: path.to_string_lossy().to_string(),
                            line: line_num + 1,
                            column: col + 1,
                            match_text: line.to_string(),
                        });
                    }
                }
            }
        }

        fn search_recursive(
            path: &FsPath,
            root: &FsPath,
            pattern: &str,
            results: &mut Vec<SearchResult>,
        ) {
            if path.is_dir() {
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        let path_buf = entry.path();
                        if !is_within_root(&path_buf, root) {
                            continue;
                        }
                        if path_buf.is_dir() {
                            search_recursive(&path_buf, root, pattern, results);
                        } else if path_buf.is_file() {
                            search_in_file(&path_buf, pattern, results);
                        }
                    }
                }
            }
        }

        let mut results = Vec::new();
        search_recursive(&base_path, &root, &pattern, &mut results);
        results
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("find_text scan task failed: {}", e)))?;

    Ok(Json(results))
}

#[derive(Debug, Deserialize)]
pub struct FindFilesQuery {
    pub query: String,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub limit: Option<usize>,
}

/// `/find/file` の `type` クエリ値。
/// 未指定・不正値は `None`（= ファイルとディレクトリの両方を返す）として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindFileTypeFilter {
    File,
    Directory,
}

fn parse_find_file_type(file_type: Option<&str>) -> Option<FindFileTypeFilter> {
    match file_type {
        Some("file") => Some(FindFileTypeFilter::File),
        Some("directory") => Some(FindFileTypeFilter::Directory),
        // 不正値はこのファイルの他クエリと同様にエラーにせず、未指定として扱う。
        _ => None,
    }
}

/// エントリが type フィルタに一致するか判定する。
///
/// `DirEntry::file_type()` はシンボリックリンクを辿らないため、リンク等は
/// `Path::is_dir()` / `Path::is_file()`（metadata を辿る）で補完する。
/// type 判定の方針は `list_files` と同じ。
fn find_entry_matches_type(
    entry: &std::fs::DirEntry,
    path: &FsPath,
    file_type: Option<FindFileTypeFilter>,
) -> bool {
    let file_type = match file_type {
        Some(file_type) => file_type,
        None => return true,
    };
    let (is_dir, is_file) = match entry.file_type() {
        Ok(kind) if kind.is_dir() => (true, false),
        Ok(kind) if kind.is_file() => (false, true),
        _ => (path.is_dir(), path.is_file()),
    };
    match file_type {
        FindFileTypeFilter::File => is_file,
        FindFileTypeFilter::Directory => is_dir,
    }
}

/// `find_files` の同期走査本体。
/// spawn_blocking 内とテストから共通で呼べるようモジュールスコープに置く。
/// 打ち切り条件・出力順は従来どおり維持する。
fn collect_find_files(
    base_path: PathBuf,
    query: String,
    file_type: Option<FindFileTypeFilter>,
    limit: usize,
) -> Vec<String> {
    fn find_recursive(
        path: &FsPath,
        root: &FsPath,
        query: &str,
        file_type: Option<FindFileTypeFilter>,
        results: &mut Vec<String>,
        limit: usize,
    ) {
        if results.len() >= limit {
            return;
        }
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let path_buf = entry.path();
                    if !is_within_root(&path_buf, root) {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.contains(query) && find_entry_matches_type(&entry, &path_buf, file_type)
                    {
                        results.push(path_buf.to_string_lossy().to_string());
                    }
                    if path_buf.is_dir() && results.len() < limit {
                        find_recursive(&path_buf, root, query, file_type, results, limit);
                    }
                }
            }
        }
    }

    let mut results = Vec::new();
    find_recursive(
        &base_path,
        &base_path,
        &query,
        file_type,
        &mut results,
        limit,
    );
    results
}

async fn find_files(Query(query): Query<FindFilesQuery>) -> Result<Json<Vec<String>>> {
    let base_path = project_root()?;
    let limit = clamp_limit(query.limit).unwrap_or(100);
    let name_query = query.query;
    let file_type = parse_find_file_type(query.file_type.as_deref());

    // 再帰走査は同期 I/O なので spawn_blocking に逃がす。打ち切り条件
    // （results.len() >= limit / results.len() < limit）と出力順はそのまま維持。
    let results = tokio::task::spawn_blocking(move || {
        collect_find_files(base_path, name_query, file_type, limit)
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("find_files scan task failed: {}", e)))?;

    Ok(Json(results))
}

#[derive(Debug, Deserialize)]
pub struct FindSymbolsQuery {
    pub query: String,
}

#[derive(Debug, Serialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: usize,
}

async fn find_symbols(Query(_query): Query<FindSymbolsQuery>) -> Response {
    not_implemented("find_symbols")
}

fn permission_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_permissions))
        .route("/{id}/reply", post(reply_permission))
}

async fn list_permissions(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Vec<crate::server::PermissionRequestInfo>>> {
    let permissions = state.permission_broker.list().await;
    Ok(Json(permissions))
}

#[derive(Debug, Deserialize)]
pub struct ReplyPermissionRequest {
    pub reply: String,
    pub message: Option<String>,
}

async fn reply_permission(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ReplyPermissionRequest>,
) -> Result<Json<bool>> {
    let response = match req.reply.as_str() {
        "once" => crate::server::PermissionResponse::Once,
        "always" => crate::server::PermissionResponse::Always,
        "reject" => crate::server::PermissionResponse::Reject {
            message: req.message,
        },
        _ => {
            return Err(ApiError::BadRequest(
                "Invalid reply; expected `once`, `always`, or `reject`".to_string(),
            ));
        }
    };

    let ok = state.permission_broker.reply(&id, response).await;
    if !ok {
        return Err(ApiError::NotFound(format!(
            "Permission request not found: {}",
            id
        )));
    }
    Ok(Json(true))
}

fn project_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_projects))
        .route("/current", get(get_current_project))
        .route("/{id}", patch(update_project))
}

#[derive(Debug, Serialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub vcs: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectMetadata {
    name: Option<String>,
    icon: Option<String>,
}

async fn is_git_repository(path: &FsPath) -> bool {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "true",
        _ => false,
    }
}

async fn current_project_info(state: &Arc<ServerState>) -> Result<ProjectInfo> {
    let cwd = std::env::current_dir()
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve current directory: {}", e)))?;
    let canonical = tokio::fs::canonicalize(&cwd)
        .await
        .map_err(|e| ApiError::BadRequest(format!("Failed to resolve project path: {}", e)))?;
    let project_id = canonical.to_string_lossy().to_string();
    let project_path = canonical.to_string_lossy().to_string();
    let default_name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".to_string());

    let metadata = state.project_metadata.read().await;
    let name = metadata
        .get(&project_id)
        .and_then(|m| m.name.clone())
        .unwrap_or(default_name);
    let _icon = metadata.get(&project_id).and_then(|m| m.icon.clone());

    Ok(ProjectInfo {
        id: project_id,
        name,
        path: project_path,
        vcs: is_git_repository(&canonical).await,
    })
}

async fn list_projects(State(state): State<Arc<ServerState>>) -> Result<Json<Vec<ProjectInfo>>> {
    Ok(Json(vec![current_project_info(&state).await?]))
}

async fn get_current_project(State(state): State<Arc<ServerState>>) -> Result<Json<ProjectInfo>> {
    Ok(Json(current_project_info(&state).await?))
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub icon: Option<String>,
}

async fn update_project(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateProjectRequest>,
) -> Result<Json<ProjectInfo>> {
    let current = current_project_info(&state).await?;
    if id != current.id {
        return Err(ApiError::NotFound(format!("Project not found: {}", id)));
    }

    let mut metadata = state.project_metadata.write().await;
    let entry = metadata.entry(id).or_default();
    if let Some(name) = req.name {
        entry.name = Some(name);
    }
    if let Some(icon) = req.icon {
        entry.icon = Some(icon);
    }
    drop(metadata);

    Ok(Json(current_project_info(&state).await?))
}

fn pty_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_pty).post(create_pty))
        .route("/{id}", get(get_pty).put(update_pty).delete(delete_pty))
        .route("/{id}/connect", get(pty_connect))
}

#[derive(Debug, Serialize)]
pub struct PtyInfo {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
}

impl From<PtySessionStruct> for PtyInfo {
    fn from(session: PtySessionStruct) -> Self {
        Self {
            id: session.id,
            command: session.command,
            cwd: session.cwd,
            // 共有型 `opencode_types::PtyStatus` と同じ lowercase wire 名へ変換する。
            // `Error` を含む全バリアントを網羅し、wire の "error" を落とさない。
            status: match session.status {
                crate::pty::PtyStatus::Running => "running".to_string(),
                crate::pty::PtyStatus::Exited => "exited".to_string(),
                crate::pty::PtyStatus::Error => "error".to_string(),
            },
        }
    }
}

async fn list_pty(State(state): State<Arc<ServerState>>) -> Json<Vec<PtyInfo>> {
    let sessions = state.pty_manager.list_sessions().await;
    Json(sessions.into_iter().map(PtyInfo::from).collect())
}

#[derive(Debug, Deserialize)]
pub struct CreatePtyRequest {
    pub command: String,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

async fn create_pty(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<CreatePtyRequest>,
) -> Result<Json<PtyInfo>> {
    let session = state
        .pty_manager
        .create_session(&req.command, req.cwd.as_deref(), req.env)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(Json(PtyInfo::from(session)))
}

async fn get_pty(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<PtyInfo>> {
    let session = state
        .pty_manager
        .get_session(&id)
        .await
        .ok_or_else(|| ApiError::NotFound("PTY session not found".to_string()))?;
    Ok(Json(PtyInfo::from(session)))
}

#[derive(Debug, Deserialize)]
pub struct UpdatePtyRequest {
    pub command: Option<String>,
    pub cwd: Option<String>,
}

async fn update_pty(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdatePtyRequest>,
) -> Result<Json<PtyInfo>> {
    let session = state
        .pty_manager
        .update_session(&id, req.command.as_deref(), req.cwd.as_deref())
        .await
        .map_err(|e| ApiError::NotFound(e.to_string()))?;
    Ok(Json(PtyInfo::from(session)))
}

async fn delete_pty(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<bool>> {
    let deleted = state.pty_manager.delete_session(&id).await;
    Ok(Json(deleted))
}

#[cfg(test)]
mod pty_status_wire_tests {
    use super::*;

    /// 回帰ガード: reader の Err 経路が publish する `Error` が
    /// `"status":"error"` として wire へ出力され、共有型
    /// `opencode_types::PtyStatus` へ復元されること（T-02）。
    /// 値はすべてダミーで、トークン値や実パスは含めない。
    #[test]
    fn pty_error_status_survives_wire_conversion_to_shared_type() {
        let session = PtySessionStruct {
            id: "pty_dummy_error".to_string(),
            command: "dummy-command".to_string(),
            cwd: "/dummy/cwd".to_string(),
            status: crate::pty::PtyStatus::Error,
            env: HashMap::new(),
            created_at: 1_700_000_000,
        };

        let info = PtyInfo::from(session);
        assert_eq!(info.status, "error");

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"status\":\"error\""));

        let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
        let status_value = wire
            .get("status")
            .and_then(|value| value.as_str())
            .expect("status must be a string on the wire")
            .to_string();
        let shared: opencode_types::PtyStatus =
            serde_json::from_value(serde_json::Value::String(status_value)).unwrap();
        assert_eq!(shared, opencode_types::PtyStatus::Error);
    }
}

#[derive(Debug, Deserialize)]
pub struct PtyConnectQuery {
    /// Byte cursor from which to replay buffered output.
    /// Any negative value (including `-1`) means skip all buffered output and
    /// only receive live data. Omitted or `0` means replay from the beginning
    /// of the retained buffer.
    pub cursor: Option<i64>,
}

/// WebSocket endpoint that bridges a client to a PTY session, matching the TS
/// `Pty.connect` protocol:
///   1. On connect: replay buffered output from the requested cursor, then send
///      a binary metadata frame (`0x00` + JSON `{"cursor":<n>}`) so the client
///      knows the current position.
///   2. Forward live PTY output to the client as binary frames.
///   3. Forward text/binary messages from the client into the PTY as input.
async fn pty_connect(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Query(query): Query<PtyConnectQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Validate the session exists before upgrading.
    let subscription = match state.pty_manager.subscribe(&id).await {
        Ok(sub) => sub,
        Err(_) => {
            return axum::response::Response::builder()
                .status(404)
                .body(axum::body::Body::from("PTY session not found"))
                .unwrap()
                .into_response();
        }
    };

    let cursor_param = query.cursor.unwrap_or(0);

    ws.on_upgrade(move |socket| handle_pty_websocket(socket, subscription, cursor_param))
        .into_response()
}

/// Map a client-supplied PTY cursor to the absolute byte offset from which to
/// replay buffered output.
///
/// - Any negative value (`-1`, `-2`, ...) means "skip all buffered output" and
///   maps to `cursor` (the end of the retained buffer). Treating *all* negative
///   values this way — rather than only exactly `-1` — prevents a malformed
///   negative cursor from falling through to `0` and replaying the entire
///   retained buffer (up to 2 MiB) in 64 KiB chunks.
/// - `0` means replay from the beginning of the retained buffer.
/// - A positive value is used directly as an absolute byte offset.
fn pty_replay_from(cursor_param: i64, cursor: usize) -> usize {
    if cursor_param < 0 {
        cursor
    } else if cursor_param > 0 {
        cursor_param as usize
    } else {
        0
    }
}

/// Resolve the byte offset into the retained buffer at which replay starts.
///
/// Returns `None` when no buffered output should be replayed, i.e. when the
/// resolved `from` offset is at or past the current cursor (client is already
/// up to date) or when the retained buffer is empty. Offsets below
/// `buffer_start` are clamped to the start of the retained buffer.
fn compute_pty_replay_offset(
    cursor_param: i64,
    buffer_start: usize,
    cursor: usize,
    buffer_len: usize,
) -> Option<usize> {
    let from = pty_replay_from(cursor_param, cursor);
    if from >= cursor || buffer_len == 0 {
        return None;
    }
    let offset = from.saturating_sub(buffer_start);
    if offset < buffer_len {
        Some(offset)
    } else {
        None
    }
}

async fn handle_pty_websocket(mut socket: WebSocket, sub: PtySubscription, cursor_param: i64) {
    // --- Phase 1: Replay buffered output ---
    // Resolve the replay slice up front via a pure helper so cursor boundary
    // handling is testable without driving the full async WebSocket handler.
    if let Some(offset) =
        compute_pty_replay_offset(cursor_param, sub.buffer_start, sub.cursor, sub.buffer.len())
    {
        let replay = &sub.buffer[offset..];
        // Send in 64 KiB chunks to avoid oversized frames (matching TS).
        for chunk in replay.chunks(64 * 1024) {
            let bytes = axum::body::Bytes::copy_from_slice(chunk);
            if socket.send(Message::Binary(bytes)).await.is_err() {
                return;
            }
        }
    }

    // Send metadata frame: 0x00 byte prefix + JSON `{"cursor":<n>}`.
    {
        let meta_json = format!("{{\"cursor\":{}}}", sub.cursor);
        let json_bytes = meta_json.as_bytes();
        let mut frame = Vec::with_capacity(1 + json_bytes.len());
        frame.push(0x00);
        frame.extend_from_slice(json_bytes);
        let bytes = axum::body::Bytes::from(frame);
        if socket.send(Message::Binary(bytes)).await.is_err() {
            return;
        }
    }

    // --- Phase 2: Bridge live I/O ---
    // Use a channel to decouple the broadcast receiver from the socket send
    // loop, since WebSocket requires &mut self for both send and recv.
    let (ws_tx, mut ws_rx) = mpsc::channel::<Vec<u8>>(256);
    let mut rx = sub.rx;
    let writer = sub.writer;

    // Task: forward broadcast PTY output into the mpsc channel.
    let forward_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(data) => {
                    if ws_tx.send(data).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Main loop: multiplex between sending PTY output and receiving WS input.
    loop {
        tokio::select! {
            // Live PTY output ready to send to the client.
            Some(data) = ws_rx.recv() => {
                let bytes = axum::body::Bytes::from(data);
                if socket.send(Message::Binary(bytes)).await.is_err() {
                    break;
                }
            }
            // Client sent a message (input for the PTY).
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        let data = t.as_bytes().to_vec();
                        if data.is_empty() { continue; }
                        let w = writer.clone();
                        let res = tokio::task::spawn_blocking(move || {
                            let mut guard = w.lock().unwrap();
                            guard.write_all(&data)?;
                            guard.flush()
                        }).await;
                        if res.is_err() || res.unwrap().is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        let data = b.to_vec();
                        if data.is_empty() { continue; }
                        let w = writer.clone();
                        let res = tokio::task::spawn_blocking(move || {
                            let mut guard = w.lock().unwrap();
                            guard.write_all(&data)?;
                            guard.flush()
                        }).await;
                        if res.is_err() || res.unwrap().is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => continue,
                }
            }
        }
    }

    forward_task.abort();
}

#[cfg(test)]
mod pty_cursor_tests {
    use super::*;

    /// Representative snapshot: the retained buffer covers bytes [100, 200).
    const BUFFER_START: usize = 100;
    const CURSOR: usize = 200;
    const BUFFER_LEN: usize = 100;

    #[test]
    fn negative_cursor_maps_to_end_of_buffer() {
        // The documented `-1` and every other negative value must skip all
        // buffered output. A negative value must never fall through to `0`,
        // otherwise the whole retained buffer would be replayed.
        assert_eq!(pty_replay_from(-1, CURSOR), CURSOR);
        assert_eq!(pty_replay_from(-2, CURSOR), CURSOR);
        assert_eq!(pty_replay_from(-100, CURSOR), CURSOR);
        assert_eq!(pty_replay_from(i64::MIN, CURSOR), CURSOR);
    }

    #[test]
    fn zero_and_positive_cursor_map_to_absolute_offsets() {
        assert_eq!(pty_replay_from(0, CURSOR), 0);
        assert_eq!(pty_replay_from(1, CURSOR), 1);
        assert_eq!(pty_replay_from(150, CURSOR), 150);
    }

    #[test]
    fn replay_offset_boundaries() {
        // -1 / -2 -> skip all buffered output (no replay). This is the
        // regression: -2 used to resolve to `0` and replay the whole buffer.
        assert_eq!(
            compute_pty_replay_offset(-1, BUFFER_START, CURSOR, BUFFER_LEN),
            None
        );
        assert_eq!(
            compute_pty_replay_offset(-2, BUFFER_START, CURSOR, BUFFER_LEN),
            None
        );

        // 0 -> replay from the beginning of the retained buffer.
        assert_eq!(
            compute_pty_replay_offset(0, BUFFER_START, CURSOR, BUFFER_LEN),
            Some(0)
        );

        // 1 (below buffer_start) -> saturating_sub clamps to the beginning.
        assert_eq!(
            compute_pty_replay_offset(1, BUFFER_START, CURSOR, BUFFER_LEN),
            Some(0)
        );
        assert_eq!(
            compute_pty_replay_offset(50, BUFFER_START, CURSOR, BUFFER_LEN),
            Some(0)
        );

        // Inside the buffer -> offset relative to the retained buffer.
        assert_eq!(
            compute_pty_replay_offset(150, BUFFER_START, CURSOR, BUFFER_LEN),
            Some(50)
        );

        // At / above cursor -> nothing to replay (client is already current).
        assert_eq!(
            compute_pty_replay_offset(200, BUFFER_START, CURSOR, BUFFER_LEN),
            None
        );
        assert_eq!(
            compute_pty_replay_offset(250, BUFFER_START, CURSOR, BUFFER_LEN),
            None
        );
        assert_eq!(
            compute_pty_replay_offset(i64::MAX, BUFFER_START, CURSOR, BUFFER_LEN),
            None
        );
    }

    #[test]
    fn empty_buffer_never_replays() {
        for cursor_param in [-1_i64, -2, 0, 1, 50] {
            assert_eq!(
                compute_pty_replay_offset(cursor_param, 0, 0, 0),
                None,
                "cursor_param={cursor_param}"
            );
        }
    }

    #[test]
    fn fresh_session_buffer_starts_at_zero() {
        // buffer_start == 0: 0 replays everything, 1 starts at byte 1.
        assert_eq!(compute_pty_replay_offset(0, 0, 50, 50), Some(0));
        assert_eq!(compute_pty_replay_offset(1, 0, 50, 50), Some(1));
        assert_eq!(compute_pty_replay_offset(25, 0, 50, 50), Some(25));
        assert_eq!(compute_pty_replay_offset(-2, 0, 50, 50), None);
    }
}

fn question_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_questions))
        .route("/{id}/reply", post(reply_question))
        .route("/{id}/reject", post(reject_question))
}

async fn list_questions(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Vec<crate::server::QuestionRequestInfo>>> {
    let questions = state.question_broker.list().await;
    Ok(Json(questions))
}

#[derive(Debug, Deserialize)]
pub struct ReplyQuestionRequest {
    pub answers: Vec<Vec<String>>,
}

async fn reply_question(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ReplyQuestionRequest>,
) -> Result<Json<bool>> {
    let ok = state.question_broker.reply(&id, req.answers).await;
    if !ok {
        return Err(ApiError::NotFound(format!(
            "Question request not found: {}",
            id
        )));
    }
    Ok(Json(true))
}

async fn reject_question(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<bool>> {
    let ok = state.question_broker.reject(&id).await;
    if !ok {
        return Err(ApiError::NotFound(format!(
            "Question request not found: {}",
            id
        )));
    }
    Ok(Json(true))
}

/// TUI communication routes.
///
/// Architecture note: In the TypeScript version the TUI (Ink/React) runs as a
/// separate process and communicates with the backend exclusively over HTTP.
/// The Rust TUI (`opencode-tui`) uses ratatui/crossterm and runs in its own
/// binary, but still talks to the server through an HTTP `ApiClient`.  These
/// endpoints therefore remain necessary -- they bridge external TUI requests
/// into an internal queue that the TUI polls via `/control/next` and answers
/// via `/control/response`.
///
/// The `/set-prompt` endpoint is a Rust-only addition (not present in the TS
/// codebase) that allows overwriting the prompt text rather than appending.
fn tui_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/append-prompt", post(append_prompt))
        .route("/set-prompt", post(set_prompt))
        .route("/submit-prompt", post(submit_prompt))
        .route("/clear-prompt", post(clear_prompt))
        .route("/open-help", post(open_help))
        .route("/open-sessions", post(open_sessions))
        .route("/open-themes", post(open_themes))
        .route("/open-models", post(open_models))
        .route("/execute-command", post(execute_tui_command))
        .route("/show-toast", post(show_toast))
        .route("/publish", post(publish_tui_event))
        .route("/select-session", post(select_session))
        .route("/control/next", get(get_next_tui_request))
        .route("/control/response", post(submit_tui_response))
}

#[derive(Debug, Deserialize)]
pub struct PromptRequest {
    pub text: String,
}

async fn enqueue_tui_request(state: &Arc<ServerState>, path: &str, body: serde_json::Value) {
    let mut queue = state.tui_request_queue.lock().await;
    queue.push_back(TuiRequest {
        path: path.to_string(),
        body: body.clone(),
    });
    drop(queue);
    state.tui_request_notify.notify_one();

    state.broadcast(
        &serde_json::json!({
            "type": "tui.request",
            "path": path,
            "body": body,
        })
        .to_string(),
    );
}

async fn append_prompt(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<PromptRequest>,
) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/append-prompt",
        serde_json::json!({ "text": req.text }),
    )
    .await;
    Ok(Json(true))
}

async fn set_prompt(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<PromptRequest>,
) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/set-prompt",
        serde_json::json!({ "text": req.text }),
    )
    .await;
    Ok(Json(true))
}

async fn submit_prompt(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<PromptRequest>,
) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/submit-prompt",
        serde_json::json!({ "text": req.text }),
    )
    .await;
    Ok(Json(true))
}

async fn clear_prompt(State(state): State<Arc<ServerState>>) -> Result<Json<bool>> {
    enqueue_tui_request(&state, "/tui/clear-prompt", serde_json::json!({})).await;
    Ok(Json(true))
}

async fn open_help(State(state): State<Arc<ServerState>>) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/open-help",
        serde_json::json!({ "command": "help.show" }),
    )
    .await;
    Ok(Json(true))
}

async fn open_sessions(State(state): State<Arc<ServerState>>) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/open-sessions",
        serde_json::json!({ "command": "session.list" }),
    )
    .await;
    Ok(Json(true))
}

async fn open_themes(State(state): State<Arc<ServerState>>) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/open-themes",
        serde_json::json!({ "command": "theme.list" }),
    )
    .await;
    Ok(Json(true))
}

async fn open_models(State(state): State<Arc<ServerState>>) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/open-models",
        serde_json::json!({ "command": "model.list" }),
    )
    .await;
    Ok(Json(true))
}

#[derive(Debug, Deserialize)]
pub struct TuiCommandRequest {
    pub command: String,
    pub arguments: Option<serde_json::Value>,
}

fn map_tui_command(command: &str) -> &str {
    match command {
        "session_new" => "session.new",
        "session_share" => "session.share",
        "session_interrupt" => "session.interrupt",
        "session_compact" => "session.compact",
        "messages_page_up" => "session.page.up",
        "messages_page_down" => "session.page.down",
        "messages_line_up" => "session.line.up",
        "messages_line_down" => "session.line.down",
        "messages_half_page_up" => "session.half.page.up",
        "messages_half_page_down" => "session.half.page.down",
        "messages_first" => "session.first",
        "messages_last" => "session.last",
        "agent_cycle" => "agent.cycle",
        other => other,
    }
}

async fn execute_tui_command(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<TuiCommandRequest>,
) -> Result<Json<bool>> {
    let mapped = map_tui_command(&req.command);
    enqueue_tui_request(
        &state,
        "/tui/execute-command",
        serde_json::json!({
            "command": mapped,
            "arguments": req.arguments,
        }),
    )
    .await;
    Ok(Json(true))
}

#[derive(Debug, Deserialize)]
pub struct ToastRequest {
    pub message: String,
    pub level: Option<String>,
}

async fn show_toast(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<ToastRequest>,
) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/show-toast",
        serde_json::json!({
            "message": req.message,
            "level": req.level,
        }),
    )
    .await;
    Ok(Json(true))
}

#[derive(Debug, Deserialize)]
pub struct PublishEventRequest {
    pub event: String,
    pub data: Option<serde_json::Value>,
}

async fn publish_tui_event(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<PublishEventRequest>,
) -> Result<Json<bool>> {
    enqueue_tui_request(
        &state,
        "/tui/publish",
        serde_json::json!({
            "event": req.event,
            "data": req.data,
        }),
    )
    .await;
    Ok(Json(true))
}

#[derive(Debug, Deserialize)]
pub struct SelectSessionRequest {
    pub session_id: String,
}

async fn select_session(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<SelectSessionRequest>,
) -> Result<Json<bool>> {
    let sessions = state.sessions.lock().await;
    if sessions.get(&req.session_id).is_none() {
        return Err(ApiError::SessionNotFound(req.session_id));
    }
    drop(sessions);

    enqueue_tui_request(
        &state,
        "/tui/select-session",
        serde_json::json!({ "sessionID": req.session_id }),
    )
    .await;
    Ok(Json(true))
}

#[derive(Debug, Serialize)]
pub struct TuiRequest {
    pub path: String,
    pub body: serde_json::Value,
}

async fn get_next_tui_request(State(state): State<Arc<ServerState>>) -> Json<Option<TuiRequest>> {
    loop {
        let mut queue = state.tui_request_queue.lock().await;
        if let Some(next) = queue.pop_front() {
            return Json(Some(next));
        }
        drop(queue);
        state.tui_request_notify.notified().await;
    }
}

// NOTE: `/control/response` is a registered HTTP route and part of the
// documented TUI bridge (see `tui_routes`). This codebase currently has no
// consumer that pops from the response queue, but the endpoint is kept for
// external API compatibility; its storage now lives on `ServerState` instead
// of a process-global static (see `ServerState::tui_response_queue`).
async fn submit_tui_response(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<bool> {
    let mut queue = state.tui_response_queue.lock().await;
    // RES-02: cap the externally writable inbox. Drop the oldest entry so
    // repeated POSTs cannot grow the queue without bound.
    if queue.len() >= MAX_TUI_RESPONSE_QUEUE {
        queue.pop_front();
    }
    queue.push_back(body);
    drop(queue);
    state.tui_response_notify.notify_one();
    Json(true)
}

fn global_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/health", get(global_health))
        .route("/ready", get(readiness))
        .route("/event", get(global_event_stream))
        .route("/config", get(get_global_config))
        .route("/dispose", post(dispose_all))
}

/// `/global/health` は liveness のまま既存の `healthy` フィールドを維持し、
/// レスポンススキーマは `HealthResponse` に統一した（詳細は README 参照）。
async fn global_health() -> Json<HealthResponse> {
    Json(health_response("ok"))
}

async fn global_event_stream(
    State(state): State<Arc<ServerState>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = state.event_bus.subscribe();
    Sse::new(BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => Some(Ok(Event::default().data(event))),
        Err(_) => None,
    }))
}

async fn get_global_config(State(_state): State<Arc<ServerState>>) -> Response {
    not_implemented("global config")
}

async fn dispose_all() -> Response {
    not_implemented("dispose")
}

fn experimental_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list_experimental))
        .route("/analyze", post(experimental_analyze))
        .route("/generate", post(experimental_generate))
        .route("/refactor", post(experimental_refactor))
        .route("/test", post(experimental_test))
        .route(
            "/{feature}",
            post(enable_experimental).delete(disable_experimental),
        )
        .route("/tool/ids", get(list_tool_ids))
        .route("/tool", get(list_tools))
        .route(
            "/worktree",
            get(list_worktrees)
                .post(create_worktree)
                .delete(remove_worktree),
        )
        .route("/worktree/reset", post(reset_worktree))
        .route("/resource", get(list_resources))
}

#[derive(Debug, Serialize)]
pub struct ExperimentalFeature {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

async fn list_experimental() -> Response {
    not_implemented("experimental list")
}

async fn enable_experimental(Path(_feature): Path<String>) -> Response {
    not_implemented("experimental enable")
}

async fn disable_experimental(Path(_feature): Path<String>) -> Response {
    not_implemented("experimental disable")
}

#[derive(Debug, Deserialize)]
pub struct ExperimentalTaskRequest {
    pub prompt: Option<String>,
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ExperimentalTaskResponse {
    pub operation: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

/// 未実装の一般エンドポイントであることを明示する 501 レスポンスを生成する。
fn not_implemented(operation: &str) -> Response {
    let body = Json(serde_json::json!({
        "error": {
            "message": format!(
                "Endpoint `{}` is not implemented in the Rust server.",
                operation
            ),
            "type": "not_implemented"
        }
    }));
    (StatusCode::NOT_IMPLEMENTED, body).into_response()
}

/// 未実装の experimental エンドポイントであることを明示する 501 レスポンスを生成する。
fn experimental_not_implemented(operation: &str) -> Response {
    let body = Json(serde_json::json!({
        "error": {
            "message": format!(
                "Experimental endpoint `{}` is not implemented in the Rust server.",
                operation
            ),
            "type": "not_implemented"
        }
    }));
    (StatusCode::NOT_IMPLEMENTED, body).into_response()
}

async fn experimental_analyze(Json(_payload): Json<ExperimentalTaskRequest>) -> Response {
    experimental_not_implemented("analyze")
}

async fn experimental_generate(Json(_payload): Json<ExperimentalTaskRequest>) -> Response {
    experimental_not_implemented("generate")
}

async fn experimental_refactor(Json(_payload): Json<ExperimentalTaskRequest>) -> Response {
    experimental_not_implemented("refactor")
}

async fn experimental_test(Json(_payload): Json<ExperimentalTaskRequest>) -> Response {
    experimental_not_implemented("test")
}

#[derive(Debug, Serialize)]
pub struct ToolInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

async fn list_tool_ids() -> Response {
    not_implemented("tool ids")
}

async fn list_tools(Query(_params): Query<HashMap<String, String>>) -> Response {
    not_implemented("tools")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub head: String,
}

impl From<WorktreeInfoStruct> for WorktreeInfo {
    fn from(info: WorktreeInfoStruct) -> Self {
        Self {
            path: info.path,
            branch: info.branch,
            head: info.head,
        }
    }
}

/// worktree 層のエラーを API エラーへ変換する。
///
/// git の stderr や `io::Error` には絶対パス・利用者固有の情報が含まれ得るため、
/// レスポンス本文にはバリアントに応じた固定文言のみを載せる（情報漏洩防止）。
fn map_worktree_error(err: WorktreeError) -> ApiError {
    match err {
        WorktreeError::NotGitRepo => ApiError::BadRequest("not a git repository".to_string()),
        WorktreeError::GitError(_) => ApiError::ProviderError {
            provider_id: "worktree".to_string(),
            status: None,
            detail: "git command failed".to_string(),
        },
        WorktreeError::NotFound(_) => ApiError::NotFound("worktree not found".to_string()),
        WorktreeError::IoError(_) => ApiError::InternalError("io error".to_string()),
    }
}

/// 作業ディレクトリ取得失敗も、パスを含めない固定文言で返す。
fn current_dir_for_worktree() -> Result<PathBuf> {
    std::env::current_dir()
        .map_err(|_| ApiError::BadRequest("cannot determine current directory".to_string()))
}

async fn list_worktrees() -> Result<Json<Vec<WorktreeInfo>>> {
    let cwd = current_dir_for_worktree()?;
    let worktrees = worktree::list_worktrees_async(&cwd)
        .await
        .map_err(map_worktree_error)?;
    Ok(Json(worktrees.into_iter().map(|w| w.into()).collect()))
}

#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: Option<String>,
    pub path: Option<String>,
}

async fn create_worktree(Json(req): Json<CreateWorktreeRequest>) -> Result<Json<WorktreeInfo>> {
    let cwd = current_dir_for_worktree()?;

    let info = worktree::create_worktree_async(&cwd, req.branch.as_deref(), req.path.as_deref())
        .await
        .map_err(map_worktree_error)?;

    Ok(Json(info.into()))
}

#[derive(Debug, Deserialize)]
pub struct RemoveWorktreeRequest {
    pub path: String,
    pub force: Option<bool>,
}

async fn remove_worktree(Json(req): Json<RemoveWorktreeRequest>) -> Result<Json<bool>> {
    let cwd = current_dir_for_worktree()?;

    worktree::remove_worktree_async(&cwd, &req.path, req.force.unwrap_or(false))
        .await
        .map_err(map_worktree_error)?;

    Ok(Json(true))
}

async fn reset_worktree() -> Result<Json<bool>> {
    let cwd = current_dir_for_worktree()?;

    worktree::prune_worktrees_async(&cwd)
        .await
        .map_err(map_worktree_error)?;

    Ok(Json(true))
}

#[derive(Debug, Serialize)]
pub struct ResourceInfo {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
}

async fn list_resources() -> Response {
    not_implemented("resource")
}

/// `/health` と `/global/health` が共有するレスポンススキーマ。
///
/// 既存クライアント（CLI の起動待ち）を壊さないよう `status` / `version` は
/// 維持したまま、global 側にあった `healthy` を追加して両者を統一する。
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// `ok`（利用可能）または `error`（readiness 失敗）。
    pub status: String,
    /// サーババージョン。
    pub version: String,
    /// `/global/health` 互換の健全性フラグ。`status == "ok"` と同期する。
    pub healthy: bool,
    /// readiness 失敗時にのみ設定する原因種別。
    /// `busy` / `corrupt` / `closed` / `uninitialized` / `other` の固定語のみ。
    /// 成功時（liveness を含む）は `skip_serializing_if` で省略され、
    /// 既存の `{status, version, healthy}` wire format を維持する。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 旧公開型名を維持するための別名。統合後も同一 JSON を返す。
pub type GlobalHealthResponse = HealthResponse;

/// `status` から統一スキーマのレスポンスを組み立てる（reason なし）。
fn health_response(status: &str) -> HealthResponse {
    health_response_with_reason(status, None)
}

/// readiness 失敗時に reason コードを添えたレスポンスを組み立てる。
///
/// `reason` が `None` のときは既存の `{status, version, healthy}` と同じ
/// JSON になる（新フィールドは `skip_serializing_if` で省略）。
fn health_response_with_reason(status: &str, reason: Option<&str>) -> HealthResponse {
    HealthResponse {
        status: status.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        healthy: status == "ok",
        reason: reason.map(str::to_string),
    }
}

/// Liveness probe。プロセスが HTTP に応答できることだけを確認し、DB は見ない。
///
/// CLI の起動待ちと既存クライアント互換のため、従来どおり常に 200 の
/// `{"status":"ok","version":...}` を返す（`healthy` は追加フィールド）。
async fn health() -> Json<HealthResponse> {
    Json(health_response("ok"))
}

/// `ping()` の失敗から原因種別コード（`busy` / `corrupt` / `closed` / `other`）を導出する。
///
/// `InputRepository::ping()` は sqlx エラーを `DatabaseError::QueryError(String)` へ
/// 畳み込むため、variant だけでは SQLITE_BUSY / corrupt / closed pool を切り分けられない。
/// ここで文字列を「分類のためだけに」読み、固定コード以外を一切外へ出さない。
/// エラー本文・DB パス・トークン値はログにもレスポンスにも渡さない。
///
/// 根拠（sqlx 0.9 実コード）:
/// - `SqliteError` の Display は `(code: N) message`（code 5=BUSY, 6=LOCKED,
///   11=CORRUPT, 26=NOTADB）。
/// - `sqlx::Error::PoolClosed` の Display は
///   "attempted to acquire a connection on a closed pool"。
fn readiness_failure_reason(err: &opencode_storage::DatabaseError) -> &'static str {
    use opencode_storage::DatabaseError;

    match err {
        // ping() は現在この variant を返さないが、将来の永続化側変更に備える。
        DatabaseError::CorruptData(_) => "corrupt",
        _ => classify_ping_error_text(&err.to_string()),
    }
}

/// 分類専用の内部ヘルパ。戻り値は固定の reason コードだけ。
fn classify_ping_error_text(text: &str) -> &'static str {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("closed pool") || lowered.contains("pool closed") {
        "closed"
    } else if lowered.contains("(code: 5)")
        || lowered.contains("(code: 6)")
        || lowered.contains("database is locked")
        || lowered.contains("database table is locked")
    {
        "busy"
    } else if lowered.contains("(code: 11)")
        || lowered.contains("(code: 26)")
        || lowered.contains("malformed")
        || lowered.contains("not a database")
    {
        "corrupt"
    } else {
        "other"
    }
}

/// Readiness probe。DB が利用可能かを実際のクエリで確認する。
///
/// `ServerState` は DB プールを直接保持しないため、`pub(crate)` な
/// `input_repo` 経由で軽量な `ping()`（`SELECT 1`）を発行する。
/// キュー内容をフェッチしないため、保留中プロンプトが溜まっていても
/// 応答は遅延せず、接続不可・プールクローズ・ロック等でのみ失敗する。
/// 利用可能なら 200、それ以外は 503 を返す。失敗時は構造化 `reason` を
/// warn フィールドと 503 ボディの両方に載せる（生エラー文言は載せない）。
async fn readiness(State(state): State<Arc<ServerState>>) -> Response {
    let failure_reason = match &state.input_repo {
        Some(repo) => match repo.ping().await {
            Ok(_) => None,
            Err(err) => {
                let reason = readiness_failure_reason(&err);
                // エラー文字列は `{}` / `?` で一切フォーマットしない。
                // 記録するのは固定コードの `reason` と汎用メッセージだけ。
                tracing::warn!(reason = reason, "readiness probe: database query failed");
                Some(reason)
            }
        },
        None => {
            tracing::warn!(
                reason = "uninitialized",
                "readiness probe: database repository is not initialized"
            );
            Some("uninitialized")
        }
    };

    match failure_reason {
        None => (StatusCode::OK, Json(health_response("ok"))).into_response(),
        Some(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(health_response_with_reason("error", Some(reason))),
        )
            .into_response(),
    }
}

async fn web_index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

// --- /doc endpoint: 未実装 (501) ---

async fn get_doc() -> Response {
    not_implemented("doc")
}

// --- /log endpoint: accepts a log entry and writes it via tracing ---

#[derive(Debug, Deserialize)]
struct WriteLogRequest {
    service: String,
    level: String,
    message: String,
    #[serde(default)]
    extra: Option<HashMap<String, serde_json::Value>>,
}

/// Sanitize an untrusted log field: strip control characters (newlines in
/// particular) and Unicode format controls (Cf: RLO / ZWSP / BOM etc., same
/// rule as `sanitize_part_id`) and cap its length to avoid log-forging and
/// oversized entries.
fn sanitize_log_field(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() && !is_format_control(*c))
        .take(max_chars)
        .collect()
}

async fn write_log(Json(req): Json<WriteLogRequest>) -> Result<Json<bool>> {
    let service = sanitize_log_field(&req.service, 64);
    let message = sanitize_log_field(&req.message, 4096);
    if service.is_empty() || message.is_empty() {
        return Err(ApiError::BadRequest(
            "service and message must not be empty".to_string(),
        ));
    }
    let extra_str = req
        .extra
        .as_ref()
        .map(|e| serde_json::to_string(e).unwrap_or_default())
        .unwrap_or_default();
    let extra_str = sanitize_log_field(&extra_str, 4096);

    match req.level.as_str() {
        "debug" => tracing::debug!(service = %service, extra = %extra_str, "{}", message),
        "info" => tracing::info!(service = %service, extra = %extra_str, "{}", message),
        "warn" => tracing::warn!(service = %service, extra = %extra_str, "{}", message),
        "error" => tracing::error!(service = %service, extra = %extra_str, "{}", message),
        other => {
            return Err(ApiError::BadRequest(format!(
                "invalid log level: '{}', expected one of: debug, info, warn, error",
                other
            )));
        }
    }

    Ok(Json(true))
}

async fn event_stream(
    State(state): State<Arc<ServerState>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = state.event_bus.subscribe();
    Sse::new(BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => Some(Ok(Event::default().data(event))),
        Err(_) => None,
    }))
}

#[derive(Debug, Serialize)]
struct PathsResponse {
    home: String,
    state: String,
    config: String,
    worktree: String,
    directory: String,
}

async fn get_paths(Query(params): Query<PathQuery>) -> Result<Json<PathsResponse>> {
    let home = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let config = dirs::config_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let state = dirs::data_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let worktree = params.directory.as_deref().unwrap_or(&cwd).to_string();
    let directory = worktree.clone();
    Ok(Json(PathsResponse {
        home,
        state,
        config,
        worktree,
        directory,
    }))
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    directory: Option<String>,
}

async fn get_vcs_info() -> Response {
    not_implemented("vcs")
}

async fn list_commands() -> Response {
    not_implemented("commands")
}

#[derive(Debug, Serialize)]
struct AgentInfo {
    id: String,
    name: String,
    description: Option<String>,
}

async fn list_agents(State(state): State<Arc<ServerState>>) -> Result<Json<Vec<AgentInfo>>> {
    let mut config = std::env::current_dir()
        .ok()
        .and_then(|cwd| load_config(&cwd).ok());

    if let (Some(loader), Some(config)) = (state.plugin_loader(), config.as_mut()) {
        apply_plugin_config_hooks(&loader, config).await;
    }

    let registry = AgentRegistry::from_optional_config(config.as_ref());
    let agents = registry
        .list()
        .into_iter()
        .filter(|agent| !matches!(agent.mode, AgentMode::Subagent))
        .map(|agent| AgentInfo {
            id: agent.name.clone(),
            name: agent.name.clone(),
            description: agent.description.clone(),
        })
        .collect();
    Ok(Json(agents))
}

async fn apply_plugin_config_hooks(loader: &Arc<PluginLoader>, config: &mut AppConfig) {
    let mut config_value = match serde_json::to_value(config.clone()) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize config for plugin config hook");
            return;
        }
    };

    for client in loader.clients().await {
        match client
            .invoke_hook("config", config_value.clone(), config_value.clone())
            .await
        {
            Ok(next_config) => {
                if next_config.is_object() {
                    config_value = next_config;
                } else {
                    tracing::warn!(
                        plugin = client.name(),
                        "plugin config hook returned non-object config payload"
                    );
                }
            }
            Err(PluginSubprocessError::Rpc { code, .. }) if code == -32601 => {
                // Plugin does not implement config hook.
            }
            Err(error) => {
                tracing::warn!(
                    plugin = client.name(),
                    %error,
                    "plugin config hook invocation failed"
                );
            }
        }
    }

    match serde_json::from_value::<AppConfig>(config_value) {
        Ok(next) => *config = next,
        Err(error) => {
            tracing::warn!(%error, "failed to deserialize config after plugin hooks");
        }
    }
}

async fn list_skills() -> Response {
    not_implemented("skill")
}

async fn get_lsp_status() -> Response {
    not_implemented("lsp status")
}

async fn get_formatter_status() -> Response {
    not_implemented("formatter status")
}

#[derive(Debug, Deserialize)]
struct SetAuthRequest {
    #[serde(flatten)]
    body: serde_json::Value,
}

async fn set_auth(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<SetAuthRequest>,
) -> Result<Json<serde_json::Value>> {
    let auth_info = parse_auth_info_payload(req.body)
        .ok_or_else(|| ApiError::BadRequest("Invalid auth payload".to_string()))?;
    state
        .auth_manager
        .try_set(&id, auth_info)
        .await
        .map_err(|error| ApiError::InternalError(error.to_string()))?;
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn delete_auth(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    state
        .auth_manager
        .try_remove(&id)
        .await
        .map_err(|error| ApiError::InternalError(error.to_string()))?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

fn parse_auth_info_payload(payload: serde_json::Value) -> Option<AuthInfo> {
    if let Ok(auth) = serde_json::from_value::<AuthInfo>(payload.clone()) {
        return Some(auth);
    }

    let key = payload
        .get("api_key")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("apiKey").and_then(|v| v.as_str()))
        .or_else(|| payload.get("token").and_then(|v| v.as_str()))
        .or_else(|| payload.get("key").and_then(|v| v.as_str()))
        .map(str::to_string)?;

    Some(AuthInfo::Api { key })
}

// ===========================================================================
// Plugin auth routes
// ===========================================================================

fn plugin_auth_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/auth", get(list_plugin_auth))
        .route("/{name}/auth/authorize", post(plugin_auth_authorize))
        .route("/{name}/auth/callback", post(plugin_auth_callback))
        .route("/{name}/auth/load", post(plugin_auth_load))
        .route("/{name}/auth/fetch", post(plugin_auth_fetch))
}

#[derive(Debug, Serialize)]
struct PluginAuthInfo {
    provider: String,
    methods: Vec<PluginAuthMethodInfo>,
}

#[derive(Debug, Serialize)]
struct PluginAuthMethodInfo {
    #[serde(rename = "type")]
    method_type: String,
    label: String,
}

async fn list_plugin_auth(state: State<Arc<ServerState>>) -> Result<Json<Vec<PluginAuthInfo>>> {
    let Some(loader) = state.plugin_loader() else {
        return Ok(Json(Vec::new()));
    };

    let bridges = loader.auth_bridges().await;
    let result: Vec<PluginAuthInfo> = bridges
        .values()
        .map(|bridge| PluginAuthInfo {
            provider: bridge.provider().to_string(),
            methods: bridge
                .methods()
                .iter()
                .map(|m| PluginAuthMethodInfo {
                    method_type: m.method_type.clone(),
                    label: m.label.clone(),
                })
                .collect(),
        })
        .collect();

    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct PluginAuthAuthorizeRequest {
    method: usize,
    #[serde(default)]
    inputs: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct PluginAuthAuthorizeResponse {
    url: Option<String>,
    instructions: Option<String>,
    method: Option<String>,
    /// Opaque id from the plugin host; lets the client correlate a later
    /// callback with this specific authorize flow.
    #[serde(rename = "flowId", skip_serializing_if = "Option::is_none")]
    flow_id: Option<String>,
}

async fn plugin_auth_authorize(
    state: State<Arc<ServerState>>,
    Path(name): Path<String>,
    Json(req): Json<PluginAuthAuthorizeRequest>,
) -> Result<Json<PluginAuthAuthorizeResponse>> {
    let bridge = get_auth_bridge(&state, &name).await?;

    let result = bridge
        .authorize(req.method, req.inputs)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(PluginAuthAuthorizeResponse {
        url: result.url,
        instructions: result.instructions,
        method: result.method,
        flow_id: result.flow_id,
    }))
}

#[derive(Debug, Deserialize)]
struct PluginAuthCallbackRequest {
    code: Option<String>,
    /// Opaque id returned by `auth.authorize`. Absent for legacy clients.
    #[serde(rename = "flowId", default)]
    flow_id: Option<String>,
}

/// Extract the arguments forwarded to the bridge. Pure so the flowId wiring
/// can be regression-tested without a running plugin subprocess.
fn plugin_callback_args(req: &PluginAuthCallbackRequest) -> (Option<&str>, Option<&str>) {
    (req.code.as_deref(), req.flow_id.as_deref())
}

async fn plugin_auth_callback(
    state: State<Arc<ServerState>>,
    Path(name): Path<String>,
    Json(req): Json<PluginAuthCallbackRequest>,
) -> Result<Json<serde_json::Value>> {
    let bridge = get_auth_bridge(&state, &name).await?;

    let (code, flow_id) = plugin_callback_args(&req);
    let result = bridge
        .callback_with_flow(code, flow_id)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(result))
}

#[derive(Debug, Serialize)]
struct PluginAuthLoadResponse {
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    #[serde(rename = "hasCustomFetch")]
    has_custom_fetch: bool,
}

async fn plugin_auth_load(
    state: State<Arc<ServerState>>,
    Path(name): Path<String>,
) -> Result<Json<PluginAuthLoadResponse>> {
    let bridge = get_auth_bridge(&state, &name).await?;

    let result = bridge
        .load()
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(PluginAuthLoadResponse {
        api_key: result.api_key,
        has_custom_fetch: result.has_custom_fetch,
    }))
}

#[derive(Debug, Deserialize)]
struct PluginAuthFetchRequest {
    url: String,
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    body: Option<String>,
}

#[derive(Debug, Serialize)]
struct PluginAuthFetchResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

async fn plugin_auth_fetch(
    state: State<Arc<ServerState>>,
    Path(name): Path<String>,
    Json(req): Json<PluginAuthFetchRequest>,
) -> Result<Json<PluginAuthFetchResponse>> {
    let bridge = get_auth_bridge(&state, &name).await?;

    let fetch_req = opencode_plugin::subprocess::PluginFetchRequest {
        url: req.url,
        method: req.method,
        headers: req.headers,
        body: req.body,
    };

    let result = bridge
        .fetch_proxy(fetch_req)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    Ok(Json(PluginAuthFetchResponse {
        status: result.status,
        headers: result.headers,
        body: result.body,
    }))
}

/// Helper: look up the auth bridge for a provider name.
async fn get_auth_bridge(state: &ServerState, provider: &str) -> Result<Arc<PluginAuthBridge>> {
    let loader = state
        .plugin_loader()
        .ok_or_else(|| ApiError::NotFound("no plugin loader initialized".into()))?;

    loader
        .auth_bridge(provider)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("no auth plugin for provider: {}", provider)))
}

#[cfg(test)]
mod plugin_auth_flow_id_tests {
    use super::*;

    #[test]
    fn authorize_response_includes_flow_id() {
        let response = PluginAuthAuthorizeResponse {
            url: Some("https://example.invalid/authorize".to_string()),
            instructions: None,
            method: Some("oauth".to_string()),
            flow_id: Some("flow-abc123".to_string()),
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value.get("flowId").and_then(serde_json::Value::as_str),
            Some("flow-abc123")
        );

        // Older clients see the same response shape when no flow id is present.
        let legacy = PluginAuthAuthorizeResponse {
            flow_id: None,
            ..response
        };
        let legacy_value = serde_json::to_value(&legacy).unwrap();
        assert!(legacy_value.get("flowId").is_none());
    }

    #[test]
    fn callback_request_deserializes_flow_id_and_defaults_when_absent() {
        let parsed: PluginAuthCallbackRequest = serde_json::from_value(serde_json::json!({
            "code": "dummy-code",
            "flowId": "flow-abc123",
        }))
        .unwrap();
        assert_eq!(parsed.code.as_deref(), Some("dummy-code"));
        assert_eq!(parsed.flow_id.as_deref(), Some("flow-abc123"));

        let legacy: PluginAuthCallbackRequest =
            serde_json::from_value(serde_json::json!({ "code": "dummy-code" })).unwrap();
        assert_eq!(legacy.code.as_deref(), Some("dummy-code"));
        assert_eq!(legacy.flow_id, None);
    }

    #[test]
    fn callback_forwards_code_and_flow_id_to_bridge() {
        let req: PluginAuthCallbackRequest = serde_json::from_value(serde_json::json!({
            "code": "dummy-code",
            "flowId": "flow-abc123",
        }))
        .unwrap();
        let (code, flow_id) = plugin_callback_args(&req);
        assert_eq!(code, Some("dummy-code"));
        assert_eq!(flow_id, Some("flow-abc123"));

        // Legacy requests must forward None so the bridge keeps its legacy path.
        let legacy: PluginAuthCallbackRequest =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let (code, flow_id) = plugin_callback_args(&legacy);
        assert_eq!(code, None);
        assert_eq!(flow_id, None);
    }
}

#[cfg(test)]
mod mcp_loaded_config_tests {
    use super::*;
    use opencode_config::{
        McpOAuthConfig, McpPerOperationTimeout, McpServer, McpServerConfig, McpTimeoutConfig,
    };

    fn full(server: McpServer) -> McpServerConfig {
        McpServerConfig::Full(Box::new(server))
    }

    #[test]
    fn loaded_local_config_uses_environment_field() {
        let config = full(McpServer {
            command: vec!["node".to_string(), "server.js".to_string()],
            environment: Some(HashMap::from([("KEY".to_string(), "value".to_string())])),
            cwd: Some("/tmp/project".to_string()),
            timeout: Some(McpTimeoutConfig::PerOperation(McpPerOperationTimeout {
                startup: Some(120),
                catalog: Some(340),
                execution: Some(560),
            })),
            ..Default::default()
        });

        let (runtime, enabled) = parse_runtime_from_loaded_config(config, None)
            .unwrap()
            .unwrap();
        assert!(enabled);
        match runtime {
            McpRuntimeConfig::Local(local) => {
                assert_eq!(local.command, "node");
                assert_eq!(local.args, vec!["server.js"]);
                assert_eq!(
                    local.env.and_then(|env| env.get("KEY").cloned()),
                    Some("value".to_string())
                );
                assert_eq!(local.cwd.as_deref(), Some("/tmp/project"));
                assert_eq!(local.timeouts.startup_ms, 120);
                assert_eq!(local.timeouts.catalog_ms, 340);
                assert_eq!(local.timeouts.execution_ms, 560);
            }
            McpRuntimeConfig::Remote(_) => panic!("expected local config"),
        }
    }

    #[test]
    fn loaded_remote_config_honors_disabled_oauth() {
        let config = full(McpServer {
            url: Some("https://example.com/mcp".to_string()),
            oauth: Some(McpOAuthConfig::Disabled(false)),
            ..Default::default()
        });

        let (runtime, _) = parse_runtime_from_loaded_config(config, None)
            .unwrap()
            .unwrap();
        match runtime {
            McpRuntimeConfig::Remote(remote) => {
                assert!(!remote.oauth_enabled);
                assert_eq!(remote.url, "https://example.com/mcp");
            }
            McpRuntimeConfig::Local(_) => panic!("expected remote config"),
        }
    }
}

#[cfg(test)]
mod mcp_callback_tests {
    use super::*;

    /// state を省略した callback は 400 を返す。保存済み state をフォールバックに
    /// すると比較が自明になり CSRF 検証が無効化されるため、ハンドラで必ず拒否する。
    #[tokio::test]
    async fn callback_without_state_returns_bad_request() {
        let result = mcp_auth_callback(
            State(Arc::new(ServerState::new())),
            Path("remote".to_string()),
            Json(McpAuthCallbackRequest {
                code: "dummy-code".to_string(),
                state: None,
            }),
        )
        .await;

        let error = result.expect_err("missing state must be rejected before any exchange");
        assert!(
            matches!(&error, ApiError::BadRequest(message) if message.contains("state")),
            "unexpected error: {error}"
        );
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }
}

#[cfg(test)]
// テスト専用のプロセスグローバル env 直列化ロックは non-Send ガードを非同期処理
// 跨ぎで保持するが、#[tokio::test] の single-thread runtime 内だけで実害はない。
#[allow(clippy::await_holding_lock)]
mod mcp_disk_reconcile_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// `OPENCODE_CONFIG` is process-global, so tests that change it must not
    /// run concurrently (including with tests in other modules of this binary
    /// that happen to read config). A fresh temp file per test keeps the
    /// fixture self-contained.
    static CONFIG_ENV_LOCK: StdMutex<()> = StdMutex::new(());

    struct ConfigEnvGuard {
        previous_config: Option<std::ffi::OsString>,
        previous_content: Option<std::ffi::OsString>,
        path: std::path::PathBuf,
    }

    impl ConfigEnvGuard {
        fn install(servers_json: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "opencode-mcp-sync-{}-{}.json",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::write(&path, format!("{{\"mcp\":{{\"servers\":{servers_json}}}}}"))
                .expect("write temp config");
            let previous_config = std::env::var_os("OPENCODE_CONFIG");
            let previous_content = std::env::var_os("OPENCODE_CONFIG_CONTENT");
            std::env::set_var("OPENCODE_CONFIG", &path);
            // Inline content has higher precedence than the explicit file.
            std::env::remove_var("OPENCODE_CONFIG_CONTENT");
            Self {
                previous_config,
                previous_content,
                path,
            }
        }

        fn rewrite(&self, servers_json: &str) {
            std::fs::write(
                &self.path,
                format!("{{\"mcp\":{{\"servers\":{servers_json}}}}}"),
            )
            .expect("rewrite temp config");
        }
    }

    impl Drop for ConfigEnvGuard {
        fn drop(&mut self) {
            match &self.previous_config {
                Some(value) => std::env::set_var("OPENCODE_CONFIG", value),
                None => std::env::remove_var("OPENCODE_CONFIG"),
            }
            match &self.previous_content {
                Some(value) => std::env::set_var("OPENCODE_CONFIG_CONTENT", value),
                None => std::env::remove_var("OPENCODE_CONFIG_CONTENT"),
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn local_server_json(enabled: bool) -> String {
        format!("{{\"command\":[\"true\"],\"enabled\":{enabled}}}")
    }

    #[tokio::test]
    async fn disk_removed_server_disappears_from_status() {
        let _lock = CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config = ConfigEnvGuard::install(&format!(
            "{{\"sync-removed-a\":{}, \"sync-removed-b\":{}}}",
            local_server_json(true),
            local_server_json(true)
        ));
        let manager = McpOAuthManager::new();

        sync_mcp_from_disk(&manager).await.expect("initial sync");
        assert!(manager.has_server("sync-removed-a").await);
        assert!(manager.has_server("sync-removed-b").await);

        config.rewrite(&format!(
            "{{\"sync-removed-a\":{}}}",
            local_server_json(true)
        ));
        sync_mcp_from_disk(&manager).await.expect("second sync");

        assert!(manager.has_server("sync-removed-a").await);
        assert!(
            !manager.has_server("sync-removed-b").await,
            "server removed from disk must disappear from in-memory status"
        );
        assert!(
            manager.get_server("sync-removed-b").await.is_none(),
            "GET /mcp must no longer report the removed server"
        );
    }

    #[tokio::test]
    async fn enabled_false_from_disk_is_applied_to_existing_server() {
        let _lock = CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let enabled_json = "{\"url\":\"http://127.0.0.1:1/mcp\",\"enabled\":true}";
        let disabled_json = "{\"url\":\"http://127.0.0.1:1/mcp\",\"enabled\":false}";
        let config = ConfigEnvGuard::install(&format!("{{\"sync-enabled\":{enabled_json}}}"));
        let manager = McpOAuthManager::new();

        sync_mcp_from_disk(&manager).await.expect("initial sync");
        // A failed connection gives a non-disabled status, so the transition
        // to `disabled` below proves the disk flag was actually applied.
        let connected = manager
            .connect("sync-enabled")
            .await
            .expect("connect returns runtime status");
        assert_ne!(connected.status, "disabled");

        config.rewrite(&format!("{{\"sync-enabled\":{disabled_json}}}"));
        sync_mcp_from_disk(&manager)
            .await
            .expect("sync after disable");

        let status = manager
            .get_server("sync-enabled")
            .await
            .expect("server still exists");
        assert_eq!(
            status.status, "disabled",
            "enabled=false from disk must be reflected in GET /mcp status"
        );

        config.rewrite(&format!("{{\"sync-enabled\":{enabled_json}}}"));
        sync_mcp_from_disk(&manager)
            .await
            .expect("sync after enable");
        assert!(
            manager.has_server("sync-enabled").await,
            "re-enabling must keep the server registered"
        );
    }

    #[tokio::test]
    async fn crud_added_server_survives_disk_sync() {
        let _lock = CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config = ConfigEnvGuard::install(&format!(
            "{{\"sync-crud-disk\":{}}}",
            local_server_json(true)
        ));
        let manager = McpOAuthManager::new();
        sync_mcp_from_disk(&manager).await.expect("initial sync");

        manager
            .add_server(
                "sync-crud-only".to_string(),
                McpRuntimeConfig::Local(LocalMcpConfig {
                    command: "true".to_string(),
                    args: Vec::new(),
                    env: None,
                    cwd: None,
                    timeouts: McpTimeouts::default(),
                    protocol: None,
                }),
                true,
            )
            .await;

        config.rewrite("{}");
        sync_mcp_from_disk(&manager)
            .await
            .expect("sync with empty disk config");

        assert!(
            manager.has_server("sync-crud-only").await,
            "CRUD-added servers must not be removed by disk reconciliation"
        );
        assert!(
            !manager.has_server("sync-crud-disk").await,
            "disk-sourced server must still be removed"
        );
    }
}

#[cfg(test)]
mod mcp_sync_status_tests {
    use super::*;
    use std::fmt::Write as _;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// `tracing-subscriber` を依存追加せずに warn 本文を捕捉する最小バッファ。
    #[derive(Clone, Default)]
    struct LogBuffer(std::sync::Arc<std::sync::Mutex<String>>);

    impl LogBuffer {
        fn text(&self) -> String {
            self.0.lock().expect("log buffer lock").clone()
        }
    }

    struct CaptureVisitor(LogBuffer);

    impl Visit for CaptureVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let mut guard = self.0 .0.lock().expect("log buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let mut guard = self.0 .0.lock().expect("log buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={} ", field.name(), value);
        }
    }

    struct CaptureSubscriber {
        buffer: LogBuffer,
    }

    impl Subscriber for CaptureSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            event.record(&mut CaptureVisitor(self.buffer.clone()));
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    fn dummy_server(name: &str) -> McpServerInfoStruct {
        McpServerInfoStruct {
            name: name.to_string(),
            status: "connected".to_string(),
            tools: 3,
            resources: 1,
            error: None,
            oauth_required: false,
            oauth_status: None,
        }
    }

    /// 回帰ガード: 同期失敗時に固定警告がヘッダへ露出し、ボディは従来の
    /// `{server: McpStatusInfo}` 形状を維持すること。生エラー（パス・トークン）は出ない。
    #[test]
    fn sync_failure_exposes_fixed_warning_without_leaking_raw_error() {
        let secret_path = "/home/user/.config/opencode/secrets/auth.json";
        let secret_token = "sk-supersecret-token-value";
        let sync_result: Result<()> = Err(ApiError::BadRequest(format!(
            "Failed to load config from {secret_path} with token {secret_token}"
        )));

        let (headers, body) = build_mcp_status_response(sync_result, vec![dummy_server("cached")]);
        let json = serde_json::to_string(&body).unwrap();

        assert_eq!(
            headers
                .get(MCP_SYNC_WARNING_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some(MCP_SYNC_WARNING)
        );
        assert!(body.contains_key("cached"));
        assert!(!json.contains("sync_warning"), "{json}");
        assert!(!json.contains(secret_path), "{json}");
        assert!(!json.contains(secret_token), "{json}");
        assert!(!json.contains("Failed to load config"), "{json}");
    }

    /// 回帰ガード: 同期成功時は従来のフラットな `{server: McpStatusInfo}` 形状を
    /// 維持し、警告ヘッダも現れないこと（既存クライアント非破壊）。
    #[test]
    fn sync_success_keeps_backward_compatible_wire_shape() {
        let (headers, body) = build_mcp_status_response(Ok(()), vec![dummy_server("ok-server")]);
        assert!(headers.get(MCP_SYNC_WARNING_HEADER).is_none());

        let value: serde_json::Value = serde_json::to_value(&body).unwrap();
        assert!(value.get("sync_warning").is_none(), "{value}");
        assert_eq!(value["ok-server"]["name"], "ok-server");
        assert_eq!(value["ok-server"]["status"], "connected");
    }

    /// 回帰ガード: 同期失敗の warn は固定 event 名のみで、生エラー値・パス・
    /// トークンがログへ出ないこと。
    #[test]
    fn sync_failure_warn_is_fixed_without_error_value_or_path() {
        let secret_path = "/home/user/.config/opencode/config.json";
        let secret_token = "sk-supersecret-token-value";
        let sync_result: Result<()> = Err(ApiError::BadRequest(format!(
            "Failed to load config from {secret_path} with token {secret_token}"
        )));

        let buffer = LogBuffer::default();
        let subscriber = CaptureSubscriber {
            buffer: buffer.clone(),
        };
        tracing::subscriber::with_default(subscriber, || {
            let _ = build_mcp_status_response(sync_result, vec![]);
        });

        let text = buffer.text();
        assert!(text.contains(MCP_SYNC_WARN_EVENT), "{text}");
        for forbidden in [
            secret_path,
            secret_token,
            "error=",
            "Failed to load config",
            "/home",
            "sk-",
        ] {
            assert!(!text.contains(forbidden), "warn leaked {forbidden}: {text}");
        }
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    #[test]
    fn role_names() {
        use opencode_session::MessageRole;
        assert_eq!(message_role_name(&MessageRole::User), "user");
        assert_eq!(message_role_name(&MessageRole::Assistant), "assistant");
        assert_eq!(message_role_name(&MessageRole::System), "system");
        assert_eq!(message_role_name(&MessageRole::Tool), "tool");
    }

    /// R4: 旧 PascalCase / ハイフン区切りの Todo wire 値が TUI で Pending に
    /// 退化しないよう、canonical な snake_case / 小文字へ正規化されること。
    #[test]
    fn todo_wire_values_are_normalized() {
        let (status, priority) = normalize_todo_wire_values("InProgress", "High");
        assert_eq!(status, "in_progress");
        assert_eq!(priority, "high");

        let (status, priority) = normalize_todo_wire_values("in-progress", "MEDIUM");
        assert_eq!(status, "in_progress");
        assert_eq!(priority, "medium");

        // 既存の canonical 値は不変。
        let (status, priority) = normalize_todo_wire_values("completed", "low");
        assert_eq!(status, "completed");
        assert_eq!(priority, "low");
    }

    /// serde ベースへ置換しても `/session/status` の status 値は従来と同一。
    #[test]
    fn session_status_wire_names_match_legacy_values() {
        use opencode_session::SessionStatus;
        assert_eq!(serde_enum_name(&SessionStatus::Active), "active");
        assert_eq!(serde_enum_name(&SessionStatus::Completed), "completed");
        assert_eq!(serde_enum_name(&SessionStatus::Archived), "archived");
        assert_eq!(serde_enum_name(&SessionStatus::Compacting), "compacting");
    }

    #[test]
    fn correlate_assistant_messages_tags_only_new_assistants() {
        use opencode_session::{MessageRole, Session};

        let mut session = Session::new("proj", "/tmp");
        // 実行前の既存メッセージ（相関 ID は付与されない）
        session.add_user_message("old");
        session.add_assistant_message();
        let first_new_index = session.messages.len();

        // 今回の実行で追加されたメッセージ
        session.add_user_message("new");
        session.add_assistant_message();
        let last = session.add_assistant_message();
        last.add_text("response");

        correlate_assistant_messages_with_queue(&mut session, first_new_index, "in_123");

        assert!(session.messages[..first_new_index]
            .iter()
            .all(|m| !m.metadata.contains_key("queue_id")));
        for message in &session.messages[first_new_index..] {
            if matches!(message.role, MessageRole::Assistant) {
                assert_eq!(
                    message.metadata.get("queue_id"),
                    Some(&serde_json::json!("in_123"))
                );
            } else {
                assert!(!message.metadata.contains_key("queue_id"));
            }
        }
    }

    #[test]
    fn max_tokens_by_variant() {
        assert_eq!(max_tokens_for_variant(9999, None), 9999);
        assert_eq!(max_tokens_for_variant(9999, Some("low")), 2048);
        assert_eq!(max_tokens_for_variant(9999, Some("HIGH")), 8192);
        assert_eq!(max_tokens_for_variant(9999, Some("xhigh")), 16384);
        assert_eq!(max_tokens_for_variant(9999, Some("unknown")), 9999);
    }

    #[test]
    fn tui_command_mapping() {
        assert_eq!(map_tui_command("session_new"), "session.new");
        assert_eq!(map_tui_command("messages_page_up"), "session.page.up");
        assert_eq!(map_tui_command("unknown"), "unknown");
    }

    #[test]
    fn text_from_parts_joins_text() {
        use crate::routes::TextPartInput;
        let parts = vec![
            TextPartInput {
                part_type: Some("text".to_owned()),
                text: Some("hello ".to_owned()),
            },
            TextPartInput {
                part_type: Some("text".to_owned()),
                text: Some("world".to_owned()),
            },
        ];
        assert_eq!(text_from_parts(&parts), "hello  world");
        assert_eq!(text_from_parts(&[]), "");
    }

    #[test]
    fn sanitize_log_field_strips_control_chars_and_caps_length() {
        assert_eq!(sanitize_log_field("hello\nworld", 100), "helloworld");
        assert_eq!(sanitize_log_field("a\r\nb\tc", 100), "abc");
        assert_eq!(sanitize_log_field("abcdef", 3), "abc");
        assert_eq!(sanitize_log_field("", 10), "");
    }

    /// ENC-03: Cf は `is_control()` に該当しないため、既存の
    /// `is_format_control` で明示的に除外する（sanitize_part_id と同基準）。
    #[test]
    fn sanitize_log_field_strips_unicode_format_controls() {
        assert_eq!(
            sanitize_log_field("a\u{202E}b\u{200B}c\u{FEFF}d", 100),
            "abcd"
        );
        assert_eq!(sanitize_log_field("\u{FEFF}", 100), "");
        // フィルタ後に Cc 除去と max_chars 制限が従来どおり適用されること。
        assert_eq!(sanitize_log_field("a\n\u{202E}bcdef", 3), "abc");
    }

    #[test]
    fn config_for_response_masks_provider_api_keys() {
        let mut config = AppConfig::default();
        let provider = opencode_config::schema::ProviderConfig {
            api_key: Some("super-secret-key".to_string()),
            ..Default::default()
        };
        config.provider = Some(HashMap::from([("openai".to_string(), provider)]));

        let value = config_for_response(&config);
        assert_eq!(value["provider"]["openai"]["api_key"], "***");
        assert!(!value.to_string().contains("super-secret-key"));
    }

    #[test]
    fn config_for_response_masks_nested_provider_secrets() {
        // テスト専用のダミー値。ログ・エラーへは出さない。
        let provider = opencode_config::schema::ProviderConfig {
            api_key: Some("dummy-top-level-key".to_string()),
            settings: Some(HashMap::from([(
                "apiKey".to_string(),
                serde_json::json!("dummy-settings-key"),
            )])),
            headers: Some(HashMap::from([(
                "Authorization".to_string(),
                serde_json::json!("Bearer dummy-header-token"),
            )])),
            options: Some(HashMap::from([(
                "nested".to_string(),
                serde_json::json!({"token": "dummy-nested-token", "keep": "visible"}),
            )])),
            ..Default::default()
        };
        let config = AppConfig {
            provider: Some(HashMap::from([("openai".to_string(), provider.clone())])),
            providers: Some(HashMap::from([("plural".to_string(), provider)])),
            ..Default::default()
        };

        let value = config_for_response(&config);
        for (section, id) in [("provider", "openai"), ("providers", "plural")] {
            assert_eq!(value[section][id]["api_key"], "***");
            assert_eq!(value[section][id]["settings"]["apiKey"], "***");
            assert_eq!(value[section][id]["headers"]["Authorization"], "***");
            assert_eq!(value[section][id]["options"]["nested"]["token"], "***");
            assert_eq!(value[section][id]["options"]["nested"]["keep"], "visible");
        }
        let rendered = value.to_string();
        for secret in [
            "dummy-top-level-key",
            "dummy-settings-key",
            "Bearer dummy-header-token",
            "dummy-nested-token",
        ] {
            assert!(
                !rendered.contains(secret),
                "a provider secret leaked into the response"
            );
        }

        // 空文字列はマスクしない（既存仕様を維持）。
        let empty_config = AppConfig {
            provider: Some(HashMap::from([(
                "openai".to_string(),
                opencode_config::schema::ProviderConfig {
                    api_key: Some(String::new()),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };
        let empty_value = config_for_response(&empty_config);
        assert_eq!(empty_value["provider"]["openai"]["api_key"], "");
    }
}

#[cfg(test)]
mod stream_message_serialization_tests {
    fn source_fn_body<'a>(source: &'a str, marker: &str) -> &'a str {
        let start = source.find(marker).expect("function must exist in source");
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("function closing brace");
        &rest[..end]
    }

    #[test]
    fn stream_message_serializes_with_prompt_and_registers_abort_handle() {
        let source = include_str!("routes.rs");
        let body = source_fn_body(source, "async fn stream_message(");
        assert!(
            body.contains("state.prompt_locks"),
            "stream must share the per-session lock used by /prompt: {body}"
        );
        assert!(
            body.contains("lock_owned()"),
            "guard must own the lock so it can move into the stream task: {body}"
        );
        assert!(
            body.contains("register_prompt_task("),
            "abort handle must be registered: {body}"
        );
        assert!(body.contains("SessionRunStatus::Busy"), "{body}");
        assert!(body.contains("SessionRunStatus::Idle"), "{body}");
        assert!(
            body.contains(".get_mut(&session_id)"),
            "session update must use the get_mut path: {body}"
        );
        assert!(
            !body.contains("sessions.update("),
            "clone+update path must not be used: {body}"
        );
    }

    #[test]
    fn stream_task_holds_run_guard_and_cleans_up() {
        let source = include_str!("routes.rs");
        let body = source_fn_body(source, "async fn stream_message(");
        assert!(body.contains("let _run_guard = run_guard;"), "{body}");
        assert!(body.contains("PromptTaskCleanup"), "{body}");
    }

    #[test]
    fn disconnect_and_normal_paths_share_finalize_helper() {
        let source = include_str!("routes.rs");
        let body = source_fn_body(source, "async fn stream_message(");
        assert_eq!(
            body.matches("finalize_stream_message(").count(),
            2,
            "both the disconnect branch and the normal path must call the shared helper: {body}"
        );
        let helper = source_fn_body(source, "async fn finalize_stream_message(");
        assert!(helper.contains("add_text(final_text)"), "{helper}");
        assert!(helper.contains("add_tool_call"), "{helper}");
        assert!(helper.contains("persist_sessions_if_enabled"), "{helper}");
    }
}

#[cfg(test)]
mod abort_tests {
    use super::*;

    async fn state_with_running_prompt() -> (Arc<ServerState>, String, tokio::task::JoinHandle<()>)
    {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id.clone()
        };
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;
        let task = tokio::spawn(std::future::pending::<()>());
        state.register_prompt_task(session_id.clone(), task.abort_handle());
        (state, session_id, task)
    }

    #[tokio::test]
    async fn abort_prompt_cancels_task_and_sets_idle() {
        let (state, session_id, task) = state_with_running_prompt().await;

        let response = abort_prompt(State(state.clone()), Path(session_id.clone()))
            .await
            .expect("abort should succeed");
        assert_eq!(response.0["aborted"], true);

        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("task should stop after abort");
        assert!(outcome.unwrap_err().is_cancelled());
        assert!(state
            .session_run_status
            .read()
            .await
            .get(&session_id)
            .is_none());
        assert!(state
            .running_prompt_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
    }

    #[tokio::test]
    async fn abort_session_cancels_task_and_sets_idle() {
        let (state, session_id, task) = state_with_running_prompt().await;

        let response = abort_session(State(state.clone()), Path(session_id.clone()))
            .await
            .expect("abort should succeed");
        assert_eq!(response.0["aborted"], true);

        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("task should stop after abort");
        assert!(outcome.unwrap_err().is_cancelled());
        assert!(state
            .session_run_status
            .read()
            .await
            .get(&session_id)
            .is_none());
    }

    #[tokio::test]
    async fn abort_prompt_rejects_unknown_session() {
        let state = Arc::new(ServerState::new());
        let error = abort_prompt(State(state), Path("missing-session".to_string()))
            .await
            .expect_err("unknown session must be rejected");
        assert!(matches!(error, ApiError::SessionNotFound(_)));
    }

    async fn state_with_queued_input() -> (
        Arc<ServerState>,
        String,
        opencode_storage::InputRepository,
        opencode_storage::Database,
    ) {
        let mut state = ServerState::new();
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            let session = sessions.create("default", ".");
            let stored: opencode_types::Session =
                serde_json::from_value(serde_json::to_value(&session).expect("session serializes"))
                    .expect("session converts to stored form");
            let session_repo = opencode_storage::SessionRepository::new(db.pool().clone());
            session_repo
                .create(&stored)
                .await
                .expect("stored session must exist for the foreign key");
            session.id.clone()
        };
        let repo = opencode_storage::InputRepository::new(db.pool().clone());
        state.input_repo = Some(repo.clone());
        (Arc::new(state), session_id, repo, db)
    }

    #[tokio::test]
    async fn abort_finalizes_queued_item_and_reschedules_idempotently() {
        let (state, session_id, repo, _db) = state_with_queued_input().await;
        let input = repo
            .enqueue(&session_id, "queued prompt", Some("queue"), None)
            .await
            .expect("enqueue should succeed");
        assert!(repo.claim_pending(&input.id).await.expect("claim succeeds"));
        state.register_prompt_queue_id(session_id.clone(), input.id.clone());
        let task = tokio::spawn(std::future::pending::<()>());
        state.register_prompt_task(session_id.clone(), task.abort_handle());
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;

        let response = abort_prompt(State(state.clone()), Path(session_id.clone()))
            .await
            .expect("abort should succeed");
        assert_eq!(response.0["aborted"], true);
        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("task should stop after abort");
        assert!(outcome.unwrap_err().is_cancelled());

        // 同じ回収を再実行しても終端状態は変わらない（冪等）。
        recover_interrupted_queued_prompt(
            state.clone(),
            session_id.clone(),
            Some(input.id.clone()),
        )
        .await;
        assert!(
            !repo
                .claim_pending(&input.id)
                .await
                .expect("claim after abort"),
            "aborted queued item must be terminalized"
        );
        assert!(state.take_prompt_queue_id(&session_id).is_none());
        assert!(state
            .session_run_status
            .read()
            .await
            .get(&session_id)
            .is_none());
    }

    #[tokio::test]
    async fn prompt_cleanup_finalizes_claimed_queue_item_when_interrupted() {
        let (state, session_id, repo, _db) = state_with_queued_input().await;
        let input = repo
            .enqueue(&session_id, "queued prompt", Some("queue"), None)
            .await
            .expect("enqueue should succeed");
        assert!(repo.claim_pending(&input.id).await.expect("claim succeeds"));
        // 実際の実行タスクと同じくキュー ID を登録しておく。abort 側が先に
        // 取り出さない限り、Drop が単発フラグを取得して回収する。
        state.register_prompt_queue_id(session_id.clone(), input.id.clone());

        let task_id = tokio::spawn(async { tokio::task::id() })
            .await
            .expect("task id");
        let cleanup = PromptTaskCleanup {
            state: state.clone(),
            session_id: session_id.clone(),
            task_id,
            queue_id: Some(input.id.clone()),
            claimed: Arc::new(AtomicBool::new(true)),
            queue_finalized: Arc::new(AtomicBool::new(false)),
        };
        drop(cleanup);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !repo
                .claim_pending(&input.id)
                .await
                .expect("claim after cleanup"),
            "interrupted queued item must be terminalized by cleanup"
        );
    }

    #[tokio::test]
    async fn prompt_cleanup_leaves_unclaimed_queue_item_pending() {
        let (state, session_id, repo, _db) = state_with_queued_input().await;
        let input = repo
            .enqueue(&session_id, "queued prompt", Some("queue"), None)
            .await
            .expect("enqueue should succeed");

        let task_id = tokio::spawn(async { tokio::task::id() })
            .await
            .expect("task id");
        let cleanup = PromptTaskCleanup {
            state: state.clone(),
            session_id: session_id.clone(),
            task_id,
            queue_id: Some(input.id.clone()),
            claimed: Arc::new(AtomicBool::new(false)),
            queue_finalized: Arc::new(AtomicBool::new(false)),
        };
        drop(cleanup);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            repo.claim_pending(&input.id).await.expect("still pending"),
            "unclaimed queued item must stay pending (not terminalized)"
        );
    }

    /// R4 回帰: 回収はキュー ID を取り出した側だけが行う単発フラグ方式であること。
    #[test]
    fn abort_recovery_is_single_shot() {
        let source = include_str!("routes.rs");
        let start = source
            .find("async fn abort_running_prompt(")
            .expect("abort function must exist");
        let abort_body = &source[start..];
        let abort_body = &abort_body[..abort_body
            .find("\n}\n")
            .expect("abort function closing brace")];
        assert!(
            abort_body.contains("if let Some(queue_id) = queue_id"),
            "abort must recover only when it won the queue-id take: {abort_body}"
        );

        let start = source
            .find("impl Drop for PromptTaskCleanup")
            .expect("cleanup impl must exist");
        let drop_body = &source[start..];
        let drop_body = &drop_body[..drop_body.find("\n}\n").expect("cleanup impl closing brace")];
        assert!(
            drop_body.contains("take_prompt_queue_id"),
            "cleanup must skip recovery when abort already took the id: {drop_body}"
        );
    }

    /// R4 回帰: 最初の abort の回収後に登録された実タスクを、
    /// 後続の abort が確実にキャンセルできること。
    #[tokio::test]
    async fn subsequent_abort_cancels_the_real_task() {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id.clone()
        };
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;

        let first = tokio::spawn(std::future::pending::<()>());
        state.register_prompt_task(session_id.clone(), first.abort_handle());
        let _ = abort_prompt(State(state.clone()), Path(session_id.clone()))
            .await
            .expect("first abort should succeed");
        let outcome = tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .expect("first task should stop");
        assert!(outcome.unwrap_err().is_cancelled());

        // 回収後に新しい実タスクが登録された状況を模擬する。
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;
        let second = tokio::spawn(std::future::pending::<()>());
        state.register_prompt_task(session_id.clone(), second.abort_handle());

        let _ = abort_prompt(State(state.clone()), Path(session_id.clone()))
            .await
            .expect("second abort should succeed");
        let outcome = tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("second task should stop");
        assert!(
            outcome.unwrap_err().is_cancelled(),
            "subsequent abort must cancel the real task"
        );
        assert!(state
            .running_prompt_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
    }

    /// R4 回帰: セッション削除が実行中タスクを止め、キューを終端化すること（冪等）。
    #[tokio::test]
    async fn delete_session_aborts_task_and_terminalizes_queue() {
        let (state, session_id, repo, _db) = state_with_queued_input().await;
        let running = repo
            .enqueue(&session_id, "running", Some("queue"), None)
            .await
            .expect("enqueue running prompt");
        repo.claim_pending(&running.id)
            .await
            .expect("claim running prompt");
        state.register_prompt_queue_id(session_id.clone(), running.id.clone());
        let task = tokio::spawn(std::future::pending::<()>());
        state.register_prompt_task(session_id.clone(), task.abort_handle());
        let pending = repo
            .enqueue(&session_id, "pending", Some("queue"), None)
            .await
            .expect("enqueue pending prompt");
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;

        let response = delete_session(State(state.clone()), Path(session_id.clone()))
            .await
            .expect("delete should succeed");
        assert_eq!(response.0["deleted"], true);
        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("running task should stop on deletion");
        assert!(outcome.unwrap_err().is_cancelled());
        assert!(
            !repo
                .claim_pending(&running.id)
                .await
                .expect("claim after deletion"),
            "running queued item must be terminalized"
        );
        assert!(
            !repo
                .claim_pending(&pending.id)
                .await
                .expect("claim after deletion"),
            "pending queued items must be terminalized too"
        );
        assert!(state.take_prompt_queue_id(&session_id).is_none());
        // 冪等: 二重削除は SessionNotFound で安全に失敗する。
        let error = delete_session(State(state.clone()), Path(session_id.clone()))
            .await
            .expect_err("second delete must fail safely");
        assert!(matches!(error, ApiError::SessionNotFound(_)));
    }

    #[test]
    fn abort_and_cleanup_both_recover_interrupted_queue_items() {
        let source = include_str!("routes.rs");
        let start = source
            .find("async fn abort_running_prompt(")
            .expect("abort function must exist");
        let abort_body = &source[start..];
        let abort_body = &abort_body[..abort_body
            .find("\n}\n")
            .expect("abort function closing brace")];
        assert!(
            abort_body.contains("recover_interrupted_queued_prompt"),
            "abort must finalize queued item and reschedule: {abort_body}"
        );
        assert!(
            abort_body.contains("take_prompt_queue_id"),
            "abort must take the running queue id: {abort_body}"
        );

        let start = source
            .find("impl Drop for PromptTaskCleanup")
            .expect("cleanup impl must exist");
        let drop_body = &source[start..];
        let drop_body = &drop_body[..drop_body.find("\n}\n").expect("cleanup impl closing brace")];
        assert!(
            drop_body.contains("recover_interrupted_queued_prompt"),
            "cleanup must finalize queued item and reschedule: {drop_body}"
        );
        assert!(
            drop_body.contains("queue_finalized"),
            "cleanup must skip already finalized items: {drop_body}"
        );
    }

    /// STREAM-RUN-STATUS-ON-PANIC 回帰の待機ヘルパ。
    /// Drop が spawn した Idle 化が反映されるまでポーリングする。
    async fn assert_run_status_idle(state: &Arc<ServerState>, session_id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let status = state
                .session_run_status
                .read()
                .await
                .get(session_id)
                .cloned();
            if status.is_none() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "cleanup drop must reset run status to idle: {status:?}"
            );
            tokio::task::yield_now().await;
        }
    }

    /// キュー ID が無いタスクでも、Drop が Busy を Idle（エントリ削除）へ戻す。
    #[tokio::test]
    async fn prompt_cleanup_drop_resets_busy_status_without_queue_id() {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id.clone()
        };
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;
        let spawned = tokio::spawn(async {});
        let task_id = spawned.id();
        // 自分のハンドルを登録し、clear_prompt_task が true を返すようにする（R4）。
        state.register_prompt_task(session_id.clone(), spawned.abort_handle());
        let _ = spawned.await;
        let cleanup = PromptTaskCleanup {
            state: state.clone(),
            session_id: session_id.clone(),
            task_id,
            queue_id: None,
            claimed: Arc::new(AtomicBool::new(false)),
            queue_finalized: Arc::new(AtomicBool::new(false)),
        };
        drop(cleanup);
        assert_run_status_idle(&state, &session_id).await;
    }

    /// キュー ID あり・shutting_down 中でも Idle 化し、通常完了 / abort 経路の
    /// Idle 設定と二重になっても壊れない（冪等）こと。
    #[tokio::test]
    async fn prompt_cleanup_drop_resets_busy_status_with_queue_id_idempotently() {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id.clone()
        };
        state.shutting_down.store(true, Ordering::Release);
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;
        let spawned = tokio::spawn(async {});
        let task_id = spawned.id();
        state.register_prompt_task(session_id.clone(), spawned.abort_handle());
        let _ = spawned.await;
        let cleanup = PromptTaskCleanup {
            state: state.clone(),
            session_id: session_id.clone(),
            task_id,
            queue_id: Some("cleanup-test-queue".to_string()),
            claimed: Arc::new(AtomicBool::new(true)),
            queue_finalized: Arc::new(AtomicBool::new(true)),
        };
        drop(cleanup);
        assert_run_status_idle(&state, &session_id).await;

        // 既に Idle の状態での再実行（正常完了 / abort + 複数 Drop）も安全。
        set_session_run_status(&state, &session_id, SessionRunStatus::Idle).await;
        let cleanup_again = PromptTaskCleanup {
            state: state.clone(),
            session_id: session_id.clone(),
            task_id,
            queue_id: None,
            claimed: Arc::new(AtomicBool::new(false)),
            queue_finalized: Arc::new(AtomicBool::new(false)),
        };
        drop(cleanup_again);
        assert_run_status_idle(&state, &session_id).await;
    }

    /// 残骸防止バリアの回帰: 親が AbortHandle を登録してからタスク本体を
    /// 解放する順序なら、早期 return 経路でも cleanup が自分のハンドルを
    /// 見つけて running_prompt_tasks に残骸が残らない。
    #[tokio::test]
    async fn registration_barrier_leaves_no_handle_on_early_return() {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id.clone()
        };
        set_session_run_status(&state, &session_id, SessionRunStatus::Busy).await;
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let task_state = state.clone();
        let task_session = session_id.clone();
        let task = tokio::spawn(async move {
            let _cleanup = PromptTaskCleanup {
                state: task_state.clone(),
                session_id: task_session.clone(),
                task_id: tokio::task::id(),
                queue_id: None,
                claimed: Arc::new(AtomicBool::new(false)),
                queue_finalized: Arc::new(AtomicBool::new(true)),
            };
            let _ = start_rx.await;
            // 早期 return 相当（登録済みロックを触らずに終了）。
        });
        state.register_prompt_task(session_id.clone(), task.abort_handle());
        let _ = start_tx.send(());
        let _ = task.await;
        assert!(state
            .running_prompt_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
        assert_run_status_idle(&state, &session_id).await;
    }

    /// spawn_prompt_execution / stream_message が「登録 → 解放」の順序を
    /// 守っていることを静的にガードする。旧実装（spawn 直後に登録）では
    /// 早期 return の Drop が登録より先に clear して残骸が残っていた。
    #[test]
    fn prompt_tasks_release_only_after_registration() {
        let source = include_str!("routes.rs");
        for marker in ["fn spawn_prompt_execution(", "async fn stream_message("] {
            let start = source
                .find(marker)
                .unwrap_or_else(|| panic!("missing {marker}"));
            let body = &source[start..];
            let body = &body[..body.find("\n}\n").expect("function closing brace")];
            let wait = body
                .find("start_rx.await")
                .unwrap_or_else(|| panic!("{marker}: task must wait for registration"));
            let register = body
                .find("register_prompt_task(")
                .unwrap_or_else(|| panic!("{marker}: parent must register handle"));
            let release = body
                .find("start_tx.send")
                .unwrap_or_else(|| panic!("{marker}: parent must release after registration"));
            assert!(
                wait < register,
                "{marker}: barrier wait must precede registration"
            );
            assert!(
                register < release,
                "{marker}: registration must precede release"
            );
        }
    }
}

#[cfg(test)]
mod experimental_not_implemented_tests {
    use super::*;

    async fn assert_not_implemented(response: Response, operation: &str) {
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("body should be JSON");
        assert_eq!(value["error"]["type"], "not_implemented");
        assert!(value["error"]["message"]
            .as_str()
            .expect("message should be a string")
            .contains(operation));
        assert!(
            value.get("status").is_none(),
            "placeholder `accepted` status must not be reported"
        );
    }

    #[tokio::test]
    async fn experimental_analyze_returns_501() {
        let payload = ExperimentalTaskRequest {
            prompt: Some("inspect this".to_string()),
            context: None,
        };
        assert_not_implemented(experimental_analyze(Json(payload)).await, "analyze").await;
    }

    #[tokio::test]
    async fn experimental_generate_returns_501() {
        let payload = ExperimentalTaskRequest {
            prompt: Some("generate this".to_string()),
            context: None,
        };
        assert_not_implemented(experimental_generate(Json(payload)).await, "generate").await;
    }

    #[tokio::test]
    async fn experimental_refactor_returns_501() {
        let payload = ExperimentalTaskRequest {
            prompt: Some("refactor this".to_string()),
            context: None,
        };
        assert_not_implemented(experimental_refactor(Json(payload)).await, "refactor").await;
    }

    #[tokio::test]
    async fn experimental_test_returns_501() {
        let payload = ExperimentalTaskRequest {
            prompt: Some("test this".to_string()),
            context: None,
        };
        assert_not_implemented(experimental_test(Json(payload)).await, "test").await;
    }

    // 以下、固定値・キュー投入のみで実処理がないハンドラの 501 化検証。

    #[tokio::test]
    async fn execute_shell_returns_501() {
        let state = Arc::new(ServerState::new());
        let response = execute_shell(
            State(state),
            Path("session-1".to_string()),
            Json(ExecuteShellRequest {
                command: "echo hi".to_string(),
                workdir: None,
            }),
        )
        .await;
        assert_not_implemented(response, "shell").await;
    }

    #[tokio::test]
    async fn init_session_returns_501() {
        let response = init_session(
            Path("session-1".to_string()),
            Json(InitSessionRequest { force: Some(true) }),
        )
        .await;
        assert_not_implemented(response, "init").await;
    }

    #[tokio::test]
    async fn summarize_session_returns_501() {
        let response = summarize_session(
            Path("session-1".to_string()),
            Json(SummarizeSessionRequest {
                provider_id: None,
                model_id: None,
            }),
        )
        .await;
        assert_not_implemented(response, "summarize").await;
    }

    #[tokio::test]
    async fn session_unrevert_returns_501() {
        let response = session_unrevert(Path("session-1".to_string())).await;
        assert_not_implemented(response, "unrevert").await;
    }

    #[tokio::test]
    async fn find_symbols_returns_501() {
        let response = find_symbols(Query(FindSymbolsQuery {
            query: "needle".to_string(),
        }))
        .await;
        assert_not_implemented(response, "find_symbols").await;
    }

    #[tokio::test]
    async fn dispose_all_returns_501() {
        assert_not_implemented(dispose_all().await, "dispose").await;
    }

    #[tokio::test]
    async fn list_experimental_returns_501() {
        assert_not_implemented(list_experimental().await, "experimental list").await;
    }

    #[tokio::test]
    async fn enable_experimental_returns_501() {
        let response = enable_experimental(Path("feature".to_string())).await;
        assert_not_implemented(response, "experimental enable").await;
    }

    #[tokio::test]
    async fn disable_experimental_returns_501() {
        let response = disable_experimental(Path("feature".to_string())).await;
        assert_not_implemented(response, "experimental disable").await;
    }

    #[tokio::test]
    async fn list_resources_returns_501() {
        assert_not_implemented(list_resources().await, "resource").await;
    }

    #[tokio::test]
    async fn list_skills_returns_501() {
        assert_not_implemented(list_skills().await, "skill").await;
    }

    #[tokio::test]
    async fn get_lsp_status_returns_501() {
        assert_not_implemented(get_lsp_status().await, "lsp status").await;
    }

    #[tokio::test]
    async fn get_formatter_status_returns_501() {
        assert_not_implemented(get_formatter_status().await, "formatter status").await;
    }

    #[tokio::test]
    async fn get_vcs_info_returns_501() {
        assert_not_implemented(get_vcs_info().await, "vcs").await;
    }

    // 以下、R2 で追加した固定値ハンドラの 501 化検証。

    #[tokio::test]
    async fn get_global_config_returns_501() {
        let state = Arc::new(ServerState::new());
        let response = get_global_config(State(state)).await;
        assert_not_implemented(response, "global config").await;
    }

    #[tokio::test]
    async fn list_tool_ids_returns_501() {
        assert_not_implemented(list_tool_ids().await, "tool ids").await;
    }

    #[tokio::test]
    async fn list_tools_returns_501() {
        let response = list_tools(Query(HashMap::new())).await;
        assert_not_implemented(response, "tools").await;
    }

    #[tokio::test]
    async fn get_doc_returns_501() {
        assert_not_implemented(get_doc().await, "doc").await;
    }

    #[tokio::test]
    async fn list_commands_returns_501() {
        assert_not_implemented(list_commands().await, "commands").await;
    }

    #[tokio::test]
    async fn not_implemented_message_distinguishes_generic_and_experimental() {
        // 一般ルートは "Experimental endpoint" と誤表示しない。
        let generic = get_vcs_info().await;
        assert_eq!(generic.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(generic.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body should be JSON");
        assert_eq!(
            value["error"]["message"]
                .as_str()
                .expect("message should be a string"),
            "Endpoint `vcs` is not implemented in the Rust server."
        );
        assert_eq!(value["error"]["type"], "not_implemented");

        // /experimental/* は従来の "Experimental endpoint" 表記を維持。
        let experimental = experimental_not_implemented("analyze");
        assert_eq!(experimental.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(experimental.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body should be JSON");
        assert_eq!(
            value["error"]["message"]
                .as_str()
                .expect("message should be a string"),
            "Experimental endpoint `analyze` is not implemented in the Rust server."
        );
        assert_eq!(value["error"]["type"], "not_implemented");
    }
}

#[cfg(test)]
mod project_metadata_tests {
    use super::*;

    #[tokio::test]
    async fn project_metadata_is_scoped_to_server_state_instance() {
        let state_a = Arc::new(ServerState::new());
        let state_b = Arc::new(ServerState::new());

        let project_id = get_current_project(State(state_a.clone()))
            .await
            .expect("current project should resolve")
            .0
            .id;

        let updated = update_project(
            State(state_a.clone()),
            Path(project_id.clone()),
            Json(UpdateProjectRequest {
                name: Some("instance-a-name".to_string()),
                icon: None,
            }),
        )
        .await
        .expect("update should succeed");
        assert_eq!(updated.0.name, "instance-a-name");

        assert_eq!(
            state_a
                .project_metadata
                .read()
                .await
                .get(&project_id)
                .and_then(|m| m.name.clone()),
            Some("instance-a-name".to_string())
        );
        assert!(state_b.project_metadata.read().await.is_empty());

        let baseline_b = get_current_project(State(state_b))
            .await
            .expect("current project should resolve")
            .0;
        assert_ne!(baseline_b.name, "instance-a-name");
    }
}

#[cfg(test)]
mod message_info_wire_format_tests {
    use super::*;
    use serde_json::json;

    fn sample_message_info(parts: Vec<PartInfo>) -> MessageInfo {
        MessageInfo {
            id: "msg-1".to_string(),
            session_id: "ses-1".to_string(),
            role: "assistant".to_string(),
            parts,
            created_at: 1000,
            completed_at: Some(2000),
            agent: Some("dummy-agent".to_string()),
            model: Some("dummy-model".to_string()),
            mode: Some("primary".to_string()),
            finish: Some("stop".to_string()),
            error: Some("dummy error".to_string()),
            cost: 0.125,
            tokens: MessageTokensInfo {
                input: 10,
                output: 20,
                reasoning: 5,
                cache_read: 3,
                cache_write: 4,
            },
        }
    }

    #[test]
    fn message_info_serializes_snake_case_keys() {
        let value = serde_json::to_value(sample_message_info(Vec::new()))
            .expect("MessageInfo should serialize");

        // MessageInfo には rename_all 指定がなく、Rust フィールド名がそのまま wire キーになる。
        assert_eq!(value.get("id"), Some(&json!("msg-1")));
        assert_eq!(value.get("session_id"), Some(&json!("ses-1")));
        assert_eq!(value.get("role"), Some(&json!("assistant")));
        assert_eq!(value.get("created_at"), Some(&json!(1000)));
        assert_eq!(value.get("completed_at"), Some(&json!(2000)));
        assert_eq!(value.get("agent"), Some(&json!("dummy-agent")));
        assert_eq!(value.get("model"), Some(&json!("dummy-model")));
        assert_eq!(value.get("mode"), Some(&json!("primary")));
        assert_eq!(value.get("finish"), Some(&json!("stop")));
        assert_eq!(value.get("error"), Some(&json!("dummy error")));
        assert_eq!(value.get("cost"), Some(&json!(0.125)));

        let parts = value
            .get("parts")
            .and_then(|parts| parts.as_array())
            .expect("parts should be an array");
        assert!(parts.is_empty());

        // camelCase の wire キーは出力されない（list_messages の応答契約）。
        assert!(value.get("sessionId").is_none());
        assert!(value.get("createdAt").is_none());
        assert!(value.get("completedAt").is_none());

        let tokens = value.get("tokens").expect("tokens should be present");
        assert_eq!(tokens.get("input"), Some(&json!(10)));
        assert_eq!(tokens.get("output"), Some(&json!(20)));
        assert_eq!(tokens.get("reasoning"), Some(&json!(5)));
        assert_eq!(tokens.get("cache_read"), Some(&json!(3)));
        assert_eq!(tokens.get("cache_write"), Some(&json!(4)));
        assert!(tokens.get("cacheRead").is_none());
        assert!(tokens.get("cacheWrite").is_none());
    }

    #[test]
    fn message_info_parts_serialize_snake_case_keys() {
        let part = PartInfo {
            id: "part-1".to_string(),
            part_type: "tool_result".to_string(),
            text: None,
            file: None,
            tool_call: None,
            tool_result: Some(ToolResultInfo {
                tool_call_id: "call-1".to_string(),
                content: "done".to_string(),
                is_error: false,
                attachments: None,
            }),
            synthetic: None,
            ignored: Some(true),
        };

        let value = serde_json::to_value(sample_message_info(vec![part]))
            .expect("MessageInfo with parts should serialize");
        let parts = value
            .get("parts")
            .and_then(|parts| parts.as_array())
            .expect("parts should be an array");
        assert_eq!(parts.len(), 1);
        let first = &parts[0];

        // PartInfo.part_type は rename = "type" で出力される。
        assert_eq!(first.get("type"), Some(&json!("tool_result")));
        assert!(first.get("part_type").is_none());
        assert_eq!(first.get("id"), Some(&json!("part-1")));

        let result = first
            .get("tool_result")
            .expect("tool_result should be present");
        assert_eq!(result.get("tool_call_id"), Some(&json!("call-1")));
        assert_eq!(result.get("content"), Some(&json!("done")));
        assert_eq!(result.get("is_error"), Some(&json!(false)));
        assert!(result.get("toolCallId").is_none());
        assert!(result.get("isError").is_none());

        // synthetic は skip_serializing_if、ignored は Some のまま出力される。
        assert!(first.get("synthetic").is_none());
        assert_eq!(first.get("ignored"), Some(&json!(true)));
    }
}

#[cfg(test)]
mod get_message_wire_format_tests {
    use super::*;
    use serde_json::json;

    /// 1 件の user メッセージ（text part を 1 つ持つ）を持つセッションを用意する。
    async fn state_with_one_message() -> (Arc<ServerState>, String, String) {
        let state = Arc::new(ServerState::new());
        let (session_id, msg_id) = {
            let mut sessions = state.sessions.lock().await;
            let session_id = sessions.create("default", ".").id;
            let session = sessions
                .get_mut(&session_id)
                .expect("created session should be available for mutation");
            let msg_id = session.add_user_message("hello").id.clone();
            (session_id, msg_id)
        };
        (state, session_id, msg_id)
    }

    /// R2/API-09: 単一取得と一覧取得で wire スキーマが同一であることを検証する回帰テスト。
    #[tokio::test]
    async fn get_message_matches_list_messages_wire_schema() {
        let (state, session_id, msg_id) = state_with_one_message().await;

        let single = get_message(
            State(state.clone()),
            Path((session_id.clone(), msg_id.clone())),
        )
        .await
        .expect("get_message should succeed for an existing message");

        let list = list_messages(
            State(state.clone()),
            Path(session_id.clone()),
            Query(ListMessagesQuery {
                limit: None,
                offset: None,
            }),
        )
        .await
        .expect("list_messages should succeed for an existing session");

        let single_value = serde_json::to_value(single.0).expect("MessageInfo should serialize");
        let list_value = serde_json::to_value(list.0).expect("Vec<MessageInfo> should serialize");
        let list = list_value
            .as_array()
            .expect("list_messages payload should be an array");

        // get_message のペイロードは list_messages の要素と完全に同一構造。
        assert_eq!(list.len(), 1);
        assert_eq!(&single_value, &list[0]);

        // snake_case キーであり、旧 camelCase / 旧外包 {"info", "parts"} が残らない。
        assert_eq!(single_value.get("id"), Some(&json!(msg_id)));
        assert_eq!(single_value.get("session_id"), Some(&json!(session_id)));
        assert_eq!(single_value.get("role"), Some(&json!("user")));
        assert!(single_value.get("sessionID").is_none());
        assert!(single_value.get("createdAt").is_none());
        assert!(single_value.get("info").is_none());

        // parts は PartInfo に正規化され、type/text 等の snake_case キーで出力される。
        let parts = single_value
            .get("parts")
            .and_then(|parts| parts.as_array())
            .expect("parts should be an array");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].get("type"), Some(&json!("text")));
        assert_eq!(parts[0].get("text"), Some(&json!("hello")));
        assert!(parts[0].get("part_type").is_none());
        assert!(parts[0].get("partType").is_none());
    }

    #[tokio::test]
    async fn get_message_not_found_keeps_existing_error_contract() {
        // 存在しない ID は 404 NotFound（既存契約を維持）。
        // トークン値やパスをログ・エラーへ出力する処理はハンドラに追加していない。
        let (state, session_id, _msg_id) = state_with_one_message().await;
        let error = get_message(State(state), Path((session_id, "msg_missing".to_string())))
            .await
            .expect_err("unknown message id must be rejected");
        assert!(matches!(&error, ApiError::NotFound(_)));
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }
}

#[cfg(test)]
mod limit_clamp_tests {
    use super::*;

    #[test]
    fn unspecified_limit_stays_none() {
        // 未指定は None のまま（各ハンドラの「全件/デフォルト」意味論を維持）。
        assert_eq!(clamp_limit(None), None);
    }

    #[test]
    fn limit_below_max_is_unchanged() {
        assert_eq!(clamp_limit(Some(1)), Some(1));
        assert_eq!(clamp_limit(Some(3)), Some(3));
        assert_eq!(clamp_limit(Some(MAX_LIMIT - 1)), Some(MAX_LIMIT - 1));
    }

    #[test]
    fn limit_at_max_is_unchanged() {
        // 上限ちょうどは境界値としてそのまま通す。
        assert_eq!(clamp_limit(Some(MAX_LIMIT)), Some(MAX_LIMIT));
    }

    #[test]
    fn limit_above_max_is_clamped() {
        assert_eq!(clamp_limit(Some(MAX_LIMIT + 1)), Some(MAX_LIMIT));
        // 攻撃ベクトル（usize::MAX = 18446744073709551615 on 64-bit）。
        assert_eq!(clamp_limit(Some(usize::MAX)), Some(MAX_LIMIT));
    }

    #[test]
    fn limit_zero_is_preserved() {
        // limit=0 の既存意味論（空結果）を維持する。
        assert_eq!(clamp_limit(Some(0)), Some(0));
    }

    /// 回帰ガード: クランプ通知ヘッダは上限超過時のみ固定値 "true" で出現し、
    /// 未指定・上限以下・上限ちょうどでは出ないこと。
    #[test]
    fn clamp_header_only_when_exceeding_max() {
        let header = |limit: Option<usize>| {
            build_limit_clamp_headers(limit)
                .get(LIMIT_CLAMPED_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        assert_eq!(header(None), None);
        assert_eq!(header(Some(0)), None);
        assert_eq!(header(Some(1)), None);
        assert_eq!(header(Some(MAX_LIMIT - 1)), None);
        assert_eq!(header(Some(MAX_LIMIT)), None);
        assert_eq!(header(Some(MAX_LIMIT + 1)), Some("true".to_owned()));
        assert_eq!(header(Some(usize::MAX)), Some("true".to_owned()));
    }

    /// 回帰ガード: ヘッダ値は固定文字列のみで、要求値・パス・トークンを出さない。
    #[test]
    fn clamp_header_value_is_fixed_and_non_sensitive() {
        let secret_token = "sk-supersecret-token-value";
        let secret_path = "/home/user/.secrets/auth.json";
        let headers = build_limit_clamp_headers(Some(usize::MAX));
        let value = headers
            .get(LIMIT_CLAMPED_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("clamped request must carry the header");
        assert_eq!(value, LIMIT_CLAMPED_VALUE);
        assert_eq!(value, "true");
        let max_value = usize::MAX.to_string();
        let serialized = format!("{headers:?}");
        for forbidden in [secret_token, secret_path, max_value.as_str()] {
            assert!(
                !serialized.contains(forbidden),
                "header leaked {forbidden}: {serialized}"
            );
        }
    }

    /// 回帰ガード: list_sessions は JSON ボディを Vec<SessionInfo> のまま返し、
    /// クランプ時のみヘッダが出る（ハンドラ経由の確認）。
    #[tokio::test]
    async fn list_sessions_keeps_body_shape_and_signals_clamp() {
        let state = Arc::new(ServerState::new());
        {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".");
        }

        let query = |limit: Option<usize>| {
            Query(ListSessionsQuery {
                directory: None,
                roots: None,
                start: None,
                search: None,
                limit,
            })
        };

        // クランプ時: ヘッダ "true" + ボディは Vec<SessionInfo> のまま。
        let (headers, Json(infos)) =
            list_sessions(State(state.clone()), query(Some(MAX_LIMIT + 1)))
                .await
                .expect("list_sessions must succeed");
        assert_eq!(
            headers
                .get(LIMIT_CLAMPED_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("true")
        );
        let infos: Vec<SessionInfo> = infos;
        assert_eq!(infos.len(), 1);
        assert!(!infos[0].id.is_empty());

        // 上限ちょうど: ヘッダなし、ボディ形状不変。
        let (headers, Json(infos)) = list_sessions(State(state.clone()), query(Some(MAX_LIMIT)))
            .await
            .expect("list_sessions must succeed");
        assert!(headers.get(LIMIT_CLAMPED_HEADER).is_none());
        let _: Vec<SessionInfo> = infos;

        // 未指定: ヘッダなし、ボディ形状不変。
        let (headers, Json(infos)) = list_sessions(State(state.clone()), query(None))
            .await
            .expect("list_sessions must succeed");
        assert!(headers.get(LIMIT_CLAMPED_HEADER).is_none());
        let _: Vec<SessionInfo> = infos;
    }
}

#[cfg(test)]
mod list_messages_pagination_tests {
    use super::*;

    /// 指定件数のメッセージを持つセッションを用意し、(state, session_id) を返す。
    async fn state_with_messages(count: usize) -> (Arc<ServerState>, String) {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            let session_id = sessions.create("default", ".").id.clone();
            {
                // create は Session を値で返すため、格納済みの実体を get_mut で更新する。
                let session = sessions
                    .get_mut(&session_id)
                    .expect("created session should exist");
                for i in 0..count {
                    session.add_user_message(format!("message-{i}"));
                }
            }
            session_id
        };
        (state, session_id)
    }

    /// ハンドラを通さず、格納済みセッションから全メッセージ id を取り出す（フィルタ無しベースライン）。
    async fn all_stored_message_ids(state: &Arc<ServerState>, session_id: &str) -> Vec<String> {
        let sessions = state.sessions.lock().await;
        let session = sessions.get(session_id).expect("session should exist");
        session.messages.iter().map(|m| m.id.clone()).collect()
    }

    /// 実クエリ文字列を axum の Query エクストラクタと同じ経路でデシリアライズする。
    fn parse_list_messages_query(uri: &str) -> ListMessagesQuery {
        let uri: axum::http::Uri = uri.parse().expect("valid URI");
        Query::<ListMessagesQuery>::try_from_uri(&uri)
            .expect("query should deserialize into ListMessagesQuery")
            .0
    }

    /// list_messages ハンドラを直接呼び、結果の id 列を順序付きで返す。
    async fn list_ids(
        state: &Arc<ServerState>,
        session_id: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Vec<String> {
        let response = list_messages(
            State(state.clone()),
            Path(session_id.to_string()),
            Query(ListMessagesQuery { limit, offset }),
        )
        .await
        .expect("list_messages should succeed");
        response.0.into_iter().map(|info| info.id).collect()
    }

    #[tokio::test]
    async fn no_query_params_returns_all_messages() {
        let (state, session_id) = state_with_messages(5).await;
        let expected = all_stored_message_ids(&state, &session_id).await;
        let ids = list_ids(&state, &session_id, None, None).await;
        assert_eq!(
            ids, expected,
            "未指定時はフィルタ無しベースラインと完全一致（wire format 維持）"
        );
    }

    #[tokio::test]
    async fn offset_only_skips_first_messages() {
        let (state, session_id) = state_with_messages(5).await;
        let all = list_ids(&state, &session_id, None, None).await;
        let ids = list_ids(&state, &session_id, None, Some(2)).await;
        assert_eq!(ids, all[2..].to_vec(), "offset のみ指定時は先頭をスキップ");
    }

    #[tokio::test]
    async fn limit_only_takes_first_messages() {
        let (state, session_id) = state_with_messages(5).await;
        let all = list_ids(&state, &session_id, None, None).await;
        let ids = list_ids(&state, &session_id, Some(3), None).await;
        assert_eq!(ids, all[..3].to_vec(), "limit のみ指定時は先頭から指定件数");
    }

    #[tokio::test]
    async fn offset_and_limit_slice_messages() {
        let (state, session_id) = state_with_messages(5).await;
        let all = list_ids(&state, &session_id, None, None).await;
        let ids = list_ids(&state, &session_id, Some(2), Some(1)).await;
        assert_eq!(ids, all[1..3].to_vec(), "offset と limit の両方を適用");
    }

    #[tokio::test]
    async fn offset_beyond_range_returns_empty() {
        let (state, session_id) = state_with_messages(3).await;
        let ids = list_ids(&state, &session_id, None, Some(10)).await;
        assert!(ids.is_empty(), "範囲外 offset は空結果");
    }

    #[tokio::test]
    async fn limit_zero_returns_empty() {
        let (state, session_id) = state_with_messages(3).await;
        let ids = list_ids(&state, &session_id, Some(0), None).await;
        assert!(ids.is_empty(), "limit=0 は空結果");
    }

    #[tokio::test]
    async fn huge_limit_is_clamped_and_keeps_wire_compat() {
        // 上限超の limit はクランプされ、実データ件数（ここでは 5 件）に
        // 対して未指定時と同じ全件結果になる（レスポンス型・順序は不変）。
        let (state, session_id) = state_with_messages(5).await;
        let expected = list_ids(&state, &session_id, None, None).await;
        let ids = list_ids(&state, &session_id, Some(usize::MAX), None).await;
        assert_eq!(ids, expected, "上限超 limit はクランプされ全件取得と同等");
    }

    // --- Query エクストラクタのデシリアライズ経路（実 HTTP サーバは起動しない） ---

    #[test]
    fn query_empty_string_deserializes_to_all_none() {
        let query = parse_list_messages_query("http://example.com/session/ses-1/message");
        assert_eq!(query.limit, None);
        assert_eq!(query.offset, None);
    }

    #[test]
    fn query_offset_only_deserializes() {
        let query = parse_list_messages_query("http://example.com/session/ses-1/message?offset=2");
        assert_eq!(query.limit, None);
        assert_eq!(query.offset, Some(2));
    }

    #[test]
    fn query_limit_only_deserializes() {
        let query = parse_list_messages_query("http://example.com/session/ses-1/message?limit=3");
        assert_eq!(query.limit, Some(3));
        assert_eq!(query.offset, None);
    }

    #[test]
    fn query_limit_and_offset_deserializes() {
        let query =
            parse_list_messages_query("http://example.com/session/ses-1/message?limit=2&offset=1");
        assert_eq!(query.limit, Some(2));
        assert_eq!(query.offset, Some(1));
    }

    #[test]
    fn query_unknown_keys_are_ignored() {
        // deny_unknown_fields を付けていないため、未知キーが混在しても既存キーは解析される。
        let query = parse_list_messages_query(
            "http://example.com/session/ses-1/message?limit=2&offset=1&unknown=ignored&foo=bar",
        );
        assert_eq!(query.limit, Some(2));
        assert_eq!(query.offset, Some(1));
    }
}

#[cfg(test)]
mod api_alias_tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn router_builds_with_api_alias_without_panic() {
        // axum の nest/merge は経路重複時に構築時 panic するため、
        // /api エイリアス追加で既存ルートと衝突しないことを検証する。
        let _root = api_routes();
        let _app = router();
    }

    #[test]
    fn api_alias_routing_test_does_not_skip_on_bind_failure() {
        // 回帰ガード: bind 失敗時に return して黙って成功する（false green）ことを禁止。
        let source = include_str!("routes.rs");
        assert!(
            source.contains(".expect(\"loopback bind must succeed in tests"),
            "bind failure must fail the test"
        );
        assert!(
            !source.contains("eprintln!(\"SKIPPED"),
            "silent skip must be removed"
        );
    }

    /// ループバックのテストサーバへ素の HTTP/1.1 GET を送り、(status, body) を返す。
    ///
    /// tower が workspace から削除済みのため `ServiceExt::oneshot` は使えず、
    /// 既存依存（tokio + axum）のみで実際のルーティングを通す。
    async fn http_get(addr: SocketAddr, path: &str) -> (StatusCode, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = TcpStream::connect(addr)
                .await
                .expect("connect to loopback test server");
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            stream
                .write_all(request.as_bytes())
                .await
                .expect("write request");
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.expect("read response");
            let raw = String::from_utf8(raw).expect("response should be utf-8");
            let status: u16 = raw
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("status line")
                .parse()
                .expect("numeric status");
            let body = raw
                .split_once("\r\n\r\n")
                .map(|(_, body)| body.to_string())
                .unwrap_or_default();
            (StatusCode::from_u16(status).expect("valid status"), body)
        })
        .await
        .expect("http exchange should not hang")
    }

    #[tokio::test]
    async fn api_alias_routes_reach_same_handlers() {
        let app = router().with_state(Arc::new(ServerState::new()));
        // loopback bind 失敗時に return すると false green になるため失敗させる。
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed in tests; skipping hides failures");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("test server stopped: {err}");
            }
        });

        // 既存ルートと /api エイリアスが同じハンドラへ到達し、同一の応答を返すことを検証する。
        for path in ["/health", "/session", "/session/status"] {
            let plain = http_get(addr, path).await;
            let aliased = http_get(addr, &format!("/api{path}")).await;
            assert_eq!(plain.0, StatusCode::OK, "GET {path} should succeed");
            assert!(!plain.1.is_empty(), "GET {path} should return a body");
            assert_eq!(aliased.0, plain.0, "GET /api{path} status should match");
            assert_eq!(aliased.1, plain.1, "GET /api{path} body should match");
        }

        server.abort();
    }
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use std::fmt::Write as _;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// `tracing-subscriber` を依存追加せずに warn の構造化フィールドを捕捉する
    /// 最小バッファ。
    #[derive(Clone, Default)]
    struct ReasonLogBuffer(std::sync::Arc<std::sync::Mutex<String>>);

    impl ReasonLogBuffer {
        fn text(&self) -> String {
            self.0.lock().expect("log buffer lock").clone()
        }
    }

    struct ReasonVisitor(ReasonLogBuffer);

    impl Visit for ReasonVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let mut guard = self.0 .0.lock().expect("log buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let mut guard = self.0 .0.lock().expect("log buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={} ", field.name(), value);
        }
    }

    struct ReasonCaptureSubscriber {
        buffer: ReasonLogBuffer,
    }

    impl Subscriber for ReasonCaptureSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            event.record(&mut ReasonVisitor(self.buffer.clone()));
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    /// レスポンスボディを JSON として読み取る。
    async fn response_body(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("health body must be readable");
        serde_json::from_slice(&bytes).expect("health body must be json")
    }

    #[tokio::test]
    async fn liveness_returns_static_ok_without_database() {
        // DB 未初期化でも liveness は従来どおり 200 を返す（CLI 起動待ち互換）。
        let resp = health().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["healthy"].as_bool(), Some(true));
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn health_and_global_health_share_one_schema() {
        let plain = response_body(health().await.into_response()).await;
        let global = response_body(global_health().await.into_response()).await;
        assert_eq!(
            plain, global,
            "both endpoints must return the unified health schema"
        );
        // 旧クライアントが参照するフィールドが残っていることを明示的に守る。
        assert!(plain.get("status").is_some());
        assert!(plain.get("version").is_some());
        assert!(global.get("healthy").is_some());
    }

    #[tokio::test]
    async fn readiness_returns_ok_when_database_is_available() {
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let repo = opencode_storage::InputRepository::new(db.pool().clone());
        let mut state = ServerState::new();
        state.input_repo = Some(repo);

        let resp = readiness(State(Arc::new(state))).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["healthy"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn readiness_returns_service_unavailable_when_database_is_closed() {
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let pool = db.pool().clone();
        let repo = opencode_storage::InputRepository::new(pool.clone());
        // DB 停止を模擬: プールを閉じて全クエリを失敗させる。
        pool.close().await;

        let mut state = ServerState::new();
        state.input_repo = Some(repo);
        let resp = readiness(State(Arc::new(state))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_body(resp).await;
        assert_eq!(body["status"], "error");
        assert_eq!(body["healthy"].as_bool(), Some(false));
    }

    #[tokio::test]
    async fn readiness_returns_service_unavailable_when_database_is_not_initialized() {
        // `ServerState::new()` は repo を持たないため、DB 未初期化として 503。
        let state = Arc::new(ServerState::new());
        let resp = readiness(State(state)).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_body(resp).await;
        assert_eq!(body["status"], "error");
        assert_eq!(body["healthy"].as_bool(), Some(false));
    }

    /// 回帰ガード: ping 失敗を固定 reason コードへ分類できること。
    /// ここでは sqlx 0.9 の Display 形式を模した文字列を使い、文言依存を明示する。
    #[test]
    fn readiness_failure_reason_maps_known_ping_errors() {
        use opencode_storage::DatabaseError;

        assert_eq!(
            readiness_failure_reason(&DatabaseError::QueryError(
                "Query error: error returned from database: (code: 5) database is locked".into()
            )),
            "busy"
        );
        assert_eq!(
            readiness_failure_reason(&DatabaseError::QueryError(
                "Query error: error returned from database: (code: 11) database disk image is malformed"
                    .into()
            )),
            "corrupt"
        );
        assert_eq!(
            readiness_failure_reason(&DatabaseError::CorruptData("payload".into())),
            "corrupt"
        );
        assert_eq!(
            readiness_failure_reason(&DatabaseError::QueryError(
                "Query error: attempted to acquire a connection on a closed pool".into()
            )),
            "closed"
        );
        assert_eq!(
            readiness_failure_reason(&DatabaseError::QueryError(
                "Query error: unexpected failure".into()
            )),
            "other"
        );
    }

    /// 回帰ガード: ping 失敗時に構造化 `reason` が warn へ記録され、
    /// 同一コードが 503 ボディにも載ること。生エラー文言は漏れないこと。
    #[tokio::test]
    async fn readiness_warn_records_reason_for_ping_failure() {
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let pool = db.pool().clone();
        let mut state = ServerState::new();
        state.input_repo = Some(opencode_storage::InputRepository::new(pool.clone()));
        pool.close().await;

        let buffer = ReasonLogBuffer::default();
        let subscriber = ReasonCaptureSubscriber {
            buffer: buffer.clone(),
        };
        let _tracing_guard = tracing::subscriber::set_default(subscriber);
        let resp = readiness(State(Arc::new(state))).await;

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let text = buffer.text();
        assert!(
            text.contains("reason=closed"),
            "structured reason missing: {text}"
        );
        assert!(
            !text.contains("closed pool"),
            "raw sqlx error text must not reach logs: {text}"
        );

        let body = response_body(resp).await;
        assert_eq!(body["reason"], "closed");
        assert_eq!(body["status"], "error");
        assert_eq!(body["healthy"].as_bool(), Some(false));
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }

    /// 回帰ガード: repo 未初期化も `uninitialized` として記録されること。
    #[tokio::test]
    async fn readiness_warn_records_uninitialized_reason() {
        let buffer = ReasonLogBuffer::default();
        let subscriber = ReasonCaptureSubscriber {
            buffer: buffer.clone(),
        };
        let _tracing_guard = tracing::subscriber::set_default(subscriber);
        let resp = readiness(State(Arc::new(ServerState::new()))).await;

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_body(resp).await;
        assert_eq!(body["reason"], "uninitialized");
        let text = buffer.text();
        assert!(text.contains("reason=uninitialized"), "{text}");
    }

    /// 回帰ガード: 成功時は既存の `{status, version, healthy}` のみで、
    /// `reason` フィールドが現れないこと（wire format 非破壊）。
    #[tokio::test]
    async fn readiness_success_keeps_existing_health_schema() {
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let mut state = ServerState::new();
        state.input_repo = Some(opencode_storage::InputRepository::new(db.pool().clone()));

        let resp = readiness(State(Arc::new(state))).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_body(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["healthy"].as_bool(), Some(true));
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(
            body.get("reason").is_none(),
            "reason must be omitted on success"
        );
    }
}

#[cfg(test)]
mod find_files_type_tests {
    use super::*;

    /// テストごとに固有のルートを作る（並列実行時の衝突を避ける）。
    fn temp_root(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opencode-find-file-type-{}-{}",
            std::process::id(),
            suffix
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `alpha_file.txt`（ファイル）と `alpha_dir`（ディレクトリ）を作る。
    /// 両方ともクエリ `alpha` に一致し、内側の `inner.txt` は一致しない。
    fn setup(root: &FsPath) {
        std::fs::write(root.join("alpha_file.txt"), "content").unwrap();
        std::fs::create_dir_all(root.join("alpha_dir")).unwrap();
        std::fs::write(root.join("alpha_dir/inner.txt"), "content").unwrap();
    }

    fn names(results: &[String]) -> Vec<String> {
        results
            .iter()
            .map(|p| {
                FsPath::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn parse_find_file_type_maps_known_and_invalid_values() {
        assert_eq!(
            parse_find_file_type(Some("file")),
            Some(FindFileTypeFilter::File)
        );
        assert_eq!(
            parse_find_file_type(Some("directory")),
            Some(FindFileTypeFilter::Directory)
        );
        assert_eq!(parse_find_file_type(None), None);
        // 不正値はエラーにせず未指定扱い（both）にする。
        assert_eq!(parse_find_file_type(Some("bogus")), None);
    }

    #[test]
    fn type_file_returns_only_files() {
        let root = temp_root("file");
        setup(&root);

        let results = collect_find_files(
            root.clone(),
            "alpha".to_string(),
            Some(FindFileTypeFilter::File),
            100,
        );
        let names = names(&results);
        assert!(names.iter().any(|n| n == "alpha_file.txt"));
        assert!(!names.iter().any(|n| n == "alpha_dir"));
        assert_eq!(names.len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn type_directory_returns_only_directories() {
        let root = temp_root("directory");
        setup(&root);

        let results = collect_find_files(
            root.clone(),
            "alpha".to_string(),
            Some(FindFileTypeFilter::Directory),
            100,
        );
        let names = names(&results);
        assert!(names.iter().any(|n| n == "alpha_dir"));
        assert!(!names.iter().any(|n| n == "alpha_file.txt"));
        assert_eq!(names.len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn type_unspecified_returns_both_files_and_directories() {
        let root = temp_root("unspecified");
        setup(&root);

        let results = collect_find_files(root.clone(), "alpha".to_string(), None, 100);
        let names = names(&results);
        assert!(names.iter().any(|n| n == "alpha_file.txt"));
        assert!(names.iter().any(|n| n == "alpha_dir"));
        assert_eq!(names.len(), 2);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn type_invalid_value_is_treated_as_unspecified() {
        let root = temp_root("invalid");
        setup(&root);

        let file_type = parse_find_file_type(Some("bogus"));
        let results = collect_find_files(root.clone(), "alpha".to_string(), file_type, 100);
        let names = names(&results);
        assert!(names.iter().any(|n| n == "alpha_file.txt"));
        assert!(names.iter().any(|n| n == "alpha_dir"));
        assert_eq!(names.len(), 2);

        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod delete_message_tests {
    use super::*;

    async fn state_with_one_message() -> (Arc<ServerState>, String, String) {
        let state = Arc::new(ServerState::new());
        let (session_id, msg_id) = {
            let mut sessions = state.sessions.lock().await;
            let session_id = sessions.create("default", ".").id;
            let session = sessions
                .get_mut(&session_id)
                .expect("created session should be available for mutation");
            let msg_id = session.add_user_message("hello").id.clone();
            (session_id, msg_id)
        };
        (state, session_id, msg_id)
    }

    #[tokio::test]
    async fn delete_message_returns_not_found_for_unknown_id() {
        // L-15: 存在しない ID を指定しても 200 deleted:true を返さず、
        // delete_part と同じ 404 NotFound 契約になることを検証する。
        let (state, session_id, msg_id) = state_with_one_message().await;

        let error = delete_message(
            State(state.clone()),
            Path((session_id.clone(), "msg_missing".to_string())),
        )
        .await
        .expect_err("deleting an unknown message must be rejected");
        assert!(matches!(&error, ApiError::NotFound(_)));
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);

        // 失敗時に既存メッセージが削除されていないことも確認する。
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&session_id)
            .expect("session should still exist");
        assert!(session.get_message(&msg_id).is_some());
    }

    #[tokio::test]
    async fn delete_message_removes_existing_message() {
        let (state, session_id, msg_id) = state_with_one_message().await;

        let response = delete_message(
            State(state.clone()),
            Path((session_id.clone(), msg_id.clone())),
        )
        .await
        .expect("deleting an existing message should succeed");
        assert_eq!(response.0["deleted"], true);

        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&session_id)
            .expect("session should still exist");
        assert!(session.get_message(&msg_id).is_none());
    }
}

#[cfg(test)]
mod queued_retry_tests {
    use super::*;

    /// session_input の外部キー制約を満たす保存済みセッションを作る。
    async fn store_session(db: &opencode_storage::Database) -> String {
        let (session, stored) = {
            let mut manager = opencode_session::SessionManager::new();
            let session = manager.create("default", ".");
            let stored = crate::server::stored_session_from_session(&session);
            (session, stored)
        };
        opencode_storage::SessionRepository::new(db.pool().clone())
            .create(&stored)
            .await
            .expect("stored session must exist for the foreign key");
        session.id
    }

    /// 連続失敗が上限に達すると再スケジュールが終端することを検証する。
    ///
    /// 各失敗で `retry_queued_prompts` に渡す attempt が 1 ずつ進み、
    /// `queued_retry_delay` が `None` を返した時点で task が起動されない。
    #[test]
    fn repeated_failures_reach_terminal_state() {
        let mut failed_attempt = 0;
        while let Some(delay) = queued_retry_delay(failed_attempt + 1) {
            assert!(delay >= QUEUED_PROMPT_INITIAL_RETRY_DELAY);
            failed_attempt += 1;
        }
        assert_eq!(failed_attempt, QUEUED_PROMPT_MAX_RETRIES);
        // 上限超過後は終端のまま（無限リトライしない）。
        assert_eq!(queued_retry_delay(failed_attempt + 1), None);
        assert_eq!(queued_retry_delay(u32::MAX), None);
    }

    /// 待機時間が指数バックオフで増加し、成功後のリセットで再びフル予算が
    /// 得られることを検証する（受け入れ条件 (1)(3)）。
    #[test]
    fn backoff_grows_exponentially_and_success_resets_counter() {
        assert_eq!(
            queued_retry_delay(0),
            None,
            "成功後（リセット済み）は再試行なし"
        );
        assert_eq!(queued_retry_delay(1), Some(Duration::from_secs(1)));
        assert_eq!(queued_retry_delay(2), Some(Duration::from_secs(2)));
        assert_eq!(queued_retry_delay(3), Some(Duration::from_secs(4)));
        let max_delay = QUEUED_PROMPT_INITIAL_RETRY_DELAY
            .saturating_mul(1u32 << (QUEUED_PROMPT_MAX_RETRIES - 1));
        assert_eq!(
            queued_retry_delay(QUEUED_PROMPT_MAX_RETRIES),
            Some(max_delay)
        );

        // 上限まで消費した後、成功を経て attempt が 0 にリセットされると、
        // 次の失敗は初回（1 秒）からやり直し、再び全回数が使える。
        let reset_attempt = 0;
        assert_eq!(
            queued_retry_delay(reset_attempt + 1),
            Some(Duration::from_secs(1))
        );
        let mut failed_attempt = reset_attempt;
        while queued_retry_delay(failed_attempt + 1).is_some() {
            failed_attempt += 1;
        }
        assert_eq!(failed_attempt, QUEUED_PROMPT_MAX_RETRIES);
    }

    /// 回帰ガード (RETRY-02): 上限超過時に当該キュー項目が `mark_as_failed`
    /// で terminal 化され、再起動時の resume 対象（pending）から外れること。
    #[tokio::test]
    async fn exhausted_retries_mark_queued_prompt_failed() {
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let repo = opencode_storage::InputRepository::new(db.pool().clone());
        let session_id = store_session(&db).await;
        let secret_prompt = "do-not-leak-secret-token-value";
        let input = repo
            .enqueue(&session_id, secret_prompt, None, None)
            .await
            .expect("enqueue should succeed");

        // 上限超過 attempt を直接渡す。`mark_as_failed` は spawn 内で非同期に
        // 呼ばれるため、完了までポーリングする。
        let mut base = ServerState::new();
        base.set_input_repo_for_test(repo.clone());
        let state = Arc::new(base);
        retry_queued_prompts(
            state.clone(),
            session_id.to_string(),
            QUEUED_PROMPT_MAX_RETRIES + 1,
            Some(input.id.clone()),
        );

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let pending = repo
                .list_pending(&session_id)
                .await
                .expect("list_pending should succeed");
            if pending.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "exhausted queue item was not terminalized"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // failed 行は resume が読む list_pending_all に現れないこと。
        let all_pending = repo
            .list_pending_all()
            .await
            .expect("list_pending_all should succeed");
        assert!(
            !all_pending.iter().any(|item| item.id == input.id),
            "failed row must not be resumed on restart"
        );
    }

    /// 回帰ガード (RETRY-02): 上限内の失敗では DB 行を failed にせず、通常の
    /// 再スケジュール経路が不変であることを検証する。
    #[tokio::test]
    async fn retries_within_limit_keep_prompt_pending() {
        let db = opencode_storage::Database::in_memory()
            .await
            .expect("in-memory database initializes");
        let repo = opencode_storage::InputRepository::new(db.pool().clone());
        let session_id = store_session(&db).await;
        let input = repo
            .enqueue(&session_id, "keep pending", None, None)
            .await
            .expect("enqueue should succeed");

        let mut base = ServerState::new();
        base.set_input_repo_for_test(repo.clone());
        let state = Arc::new(base);
        // 上限内の attempt では sleep 後に再スケジュールが走るが、repo は
        // 変更されないため pending のままであることを確認する。
        retry_queued_prompts(
            state.clone(),
            session_id.to_string(),
            1,
            Some(input.id.clone()),
        );

        let pending = repo
            .list_pending(&session_id)
            .await
            .expect("list_pending should succeed");
        assert!(
            pending.iter().any(|item| item.id == input.id),
            "in-limit retry must not terminalize the queued prompt"
        );
    }
}

#[cfg(test)]
mod patch_summary_persistence_tests {
    use super::*;

    /// R4(high) 回帰: PATCH 要約リクエストに diffs が無い (None) 場合でも、
    /// 永続化同期の `to_value(session)` -> `from_value::<opencode_types::Session>`
    /// が "missing field `diffs`" で失敗せず、要約が失われないことを検証する。
    #[tokio::test]
    async fn patch_summary_without_diffs_survives_persistence_roundtrip() {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id.clone()
        };

        // PATCH /session/:id/summary と同じ入力（diffs はリクエストに存在しない）。
        let response = set_session_summary(
            State(state.clone()),
            Path(session_id.clone()),
            Json(SetSummaryRequest {
                additions: Some(11),
                deletions: Some(7),
                files: Some(3),
            }),
        )
        .await
        .expect("PATCH summary should succeed");
        assert_eq!(response.0.summary.as_ref().map(|s| s.additions), Some(11));
        assert_eq!(response.0.summary.as_ref().map(|s| s.deletions), Some(7));
        assert_eq!(response.0.summary.as_ref().map(|s| s.files), Some(3));

        // sync_sessions_to_storage と同一の変換。ここで diffs=None のままなら
        // "missing field `diffs`" で from_value が失敗していた。
        let stored: opencode_types::Session = {
            let sessions = state.sessions.lock().await;
            let session = sessions
                .get(&session_id)
                .expect("session should exist after PATCH");
            let value = serde_json::to_value(session).expect("session should serialize");
            serde_json::from_value(value).expect("session should deserialize into storage type")
        };

        let stored_summary = stored
            .summary
            .as_ref()
            .expect("summary should reach storage (not be skipped)");
        assert_eq!(stored_summary.additions, 11);
        assert_eq!(stored_summary.deletions, 7);
        assert_eq!(stored_summary.files, 3);
        assert!(stored_summary.diffs.is_empty());

        // load_sessions_from_storage の restore_session_summary と同じ復元を行い、
        // 要約が残ることを確認する（diffs は Some(空配列) として戻る）。
        let reloaded_summary: opencode_session::SessionSummary = serde_json::from_value(
            serde_json::to_value(stored_summary).expect("stored summary should serialize"),
        )
        .expect("stored summary should deserialize into session summary");
        assert_eq!(reloaded_summary.additions, 11);
        assert_eq!(reloaded_summary.deletions, 7);
        assert_eq!(reloaded_summary.files, 3);
        assert!(reloaded_summary.diffs.as_ref().is_some_and(Vec::is_empty));
    }
}

#[cfg(test)]
mod worktree_error_mapping_tests {
    use super::*;

    /// git の stderr / io エラーに含まれるパスをレスポンスへ漏らさず、
    /// バリアントごとの固定ステータスへ変換することを検証する。
    #[tokio::test]
    async fn map_worktree_error_maps_statuses_and_hides_paths() {
        let secret_path = "/home/private/secret-worktree";
        let cases = [
            (
                WorktreeError::NotGitRepo,
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
            (
                WorktreeError::GitError(format!("fatal: cannot open '{}'", secret_path)),
                StatusCode::BAD_GATEWAY,
                "provider_error",
            ),
            (
                WorktreeError::NotFound(secret_path.to_string()),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                WorktreeError::IoError(std::io::Error::other(secret_path.to_string())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ];

        for (err, expected_status, expected_type) in cases {
            let response = map_worktree_error(err).into_response();
            assert_eq!(response.status(), expected_status);

            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body should be readable");
            let body = String::from_utf8(bytes.to_vec()).expect("body should be utf-8");
            assert!(
                !body.contains(secret_path),
                "error body leaked a path: {}",
                body
            );
            let value: serde_json::Value =
                serde_json::from_str(&body).expect("body should be JSON");
            assert_eq!(value["error"]["type"], expected_type);
        }
    }
}

#[cfg(test)]
mod permission_always_tests {
    use super::*;

    fn spawn_permission(
        state: &Arc<ServerState>,
        session_id: &str,
        permission: &str,
        pattern: &str,
    ) -> tokio::task::JoinHandle<std::result::Result<(), opencode_tool::ToolError>> {
        let state = state.clone();
        let broker = state.permission_broker.clone();
        let sid = session_id.to_string();
        let req = opencode_tool::PermissionRequest::new(permission).with_pattern(pattern);
        tokio::spawn(async move { handle_permission_request(&state, &broker, &sid, req).await })
    }

    async fn wait_for_pending(
        broker: &crate::server::PermissionBroker,
    ) -> crate::server::PermissionRequestInfo {
        for _ in 0..100 {
            if let Some(info) = broker.list().await.first().cloned() {
                return info;
            }
            tokio::task::yield_now().await;
        }
        panic!("permission request was never brokered");
    }

    /// Always 応答の回帰テスト:
    /// - Always 後は同一 (permission, patterns) がプロンプトなしでスキップ
    /// - 別ツールは依然としてプロンプトが出る
    /// - セッション終了で動的 allow はクリアされる
    #[tokio::test]
    async fn always_skips_same_tool_only_and_clears_on_session_end() {
        let state = Arc::new(ServerState::new());
        let session = state.sessions.lock().await.create("project", "/work");
        let session_id = session.id.clone();
        let broker = state.permission_broker.clone();

        // 1) 初回はプロンプトが出て Always で応答する。
        let first = spawn_permission(&state, &session_id, "bash", "git status");
        let pending = wait_for_pending(&broker).await;
        assert_eq!(pending.permission_type, "bash");
        assert_eq!(pending.patterns, vec!["git status".to_string()]);
        assert!(
            broker
                .reply(&pending.id, crate::server::PermissionResponse::Always)
                .await
        );
        assert!(first.await.expect("task should join").is_ok());

        // 2) 同一ツール・同一パターンは待たずにスキップされ、新規プロンプトも出ない。
        let second = tokio::time::timeout(Duration::from_secs(1), async {
            handle_permission_request(
                &state,
                &broker,
                &session_id,
                opencode_tool::PermissionRequest::new("bash").with_pattern("git status"),
            )
            .await
        })
        .await
        .expect("approved request must not wait for a prompt");
        assert!(second.is_ok());
        assert!(
            broker.list().await.is_empty(),
            "no new permission prompt should be queued"
        );

        // 3) 別ツールは依然としてプロンプトが出る（Once で応答）。
        let third = spawn_permission(&state, &session_id, "write", "src/lib.rs");
        let pending = wait_for_pending(&broker).await;
        assert_eq!(pending.permission_type, "write");
        assert!(
            broker
                .reply(&pending.id, crate::server::PermissionResponse::Once)
                .await
        );
        assert!(third.await.expect("task should join").is_ok());

        // 4) セッション削除で動的 allow が消え、同一ツールが再びプロンプトを出す。
        assert!(state.sessions.lock().await.delete(&session_id).is_some());
        let fourth = spawn_permission(&state, &session_id, "bash", "git status");
        let pending = wait_for_pending(&broker).await;
        assert_eq!(pending.permission_type, "bash");
        assert!(
            broker
                .reply(&pending.id, crate::server::PermissionResponse::Once)
                .await
        );
        assert!(fourth.await.expect("task should join").is_ok());
    }

    /// 承認時に批准されていないパターンへワイルドカード拡張されないことを検証する。
    #[tokio::test]
    async fn always_does_not_expand_patterns() {
        let state = Arc::new(ServerState::new());
        let session = state.sessions.lock().await.create("project", "/work");
        let session_id = session.id.clone();
        let broker = state.permission_broker.clone();

        let first = spawn_permission(&state, &session_id, "bash", "git status");
        let pending = wait_for_pending(&broker).await;
        assert!(
            broker
                .reply(&pending.id, crate::server::PermissionResponse::Always)
                .await
        );
        assert!(first.await.expect("task should join").is_ok());

        // 別パターンは別リクエストとしてプロンプトが出る。
        let second = spawn_permission(&state, &session_id, "bash", "git push --force");
        let pending = wait_for_pending(&broker).await;
        assert_eq!(pending.patterns, vec!["git push --force".to_string()]);
        assert!(
            broker
                .reply(&pending.id, crate::server::PermissionResponse::Once)
                .await
        );
        assert!(second.await.expect("task should join").is_ok());
    }

    /// R4(high) 回帰: broker.ask() 待機中にセッションが削除されても、
    /// Always 応答で孤立した動的 allow が作られないこと。
    #[tokio::test]
    async fn always_after_session_deletion_does_not_record_orphan() {
        let state = Arc::new(ServerState::new());
        let session = state.sessions.lock().await.create("project", "/work");
        let session_id = session.id.clone();
        let broker = state.permission_broker.clone();

        // 1) プロンプトを broker に積んだ状態で待機させる。
        let pending_task = spawn_permission(&state, &session_id, "bash", "git status");
        let pending = wait_for_pending(&broker).await;

        // 2) 応答前にセッションを削除する（競合の再現）。
        assert!(state.sessions.lock().await.delete(&session_id).is_some());

        // 3) Always で応答しても孤立エントリは作られない。
        assert!(
            broker
                .reply(&pending.id, crate::server::PermissionResponse::Always)
                .await
        );
        assert!(pending_task.await.expect("task should join").is_ok());
        assert!(
            !state.sessions.lock().await.is_permission_approved(
                &session_id,
                "bash",
                &["git status".to_string()]
            ),
            "orphaned dynamic allow must not be recorded for a deleted session"
        );
    }
}

#[cfg(test)]
mod config_patch_runtime_tests {
    use super::*;

    fn config_with_openai_model(model_id: &str) -> AppConfig {
        let model = opencode_config::schema::ModelConfig {
            name: Some(model_id.to_string()),
            ..Default::default()
        };
        let provider = opencode_config::schema::ProviderConfig {
            // テスト専用のダミー値。ログ・エラーへは出さない。
            api_key: Some("dummy-key-for-test".to_string()),
            models: Some(HashMap::from([(model_id.to_string(), model)])),
            ..Default::default()
        };
        AppConfig {
            provider: Some(HashMap::from([("openai".to_string(), provider)])),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn patch_rebuild_changes_model_resolution() {
        let state = Arc::new(ServerState::new());
        *state.config.write().await = config_with_openai_model("model-a");
        rebuild_provider_registry_after_patch(&state).await;

        let (provider, resolved_provider_id, model_id) =
            resolve_provider_and_model(&state, Some("model-a"), None, None)
                .expect("model-a must resolve before PATCH");
        assert_eq!(resolved_provider_id, "openai");
        assert_eq!(model_id, "model-a");
        assert!(
            !provider.id().is_empty(),
            "resolved provider must be usable"
        );

        let patch = AppConfig {
            disabled_providers: vec!["openai".to_string()].into(),
            ..Default::default()
        };
        state.config.write().await.merge(patch);
        rebuild_provider_registry_after_patch(&state).await;

        assert!(
            resolve_provider_and_model(&state, Some("model-a"), None, None).is_err(),
            "model-a must stop resolving after PATCH disables its provider"
        );
    }

    #[test]
    fn restart_required_keys_lists_only_server_fields() {
        let patch = AppConfig::default();
        assert!(restart_required_keys(&patch).is_empty());

        let patch = AppConfig {
            server: Some(opencode_config::schema::ServerConfig {
                port: Some(41234),
                hostname: Some("127.0.0.1".to_string()),
                cors: Some(vec!["http://example.test".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let keys = restart_required_keys(&patch);
        assert_eq!(
            keys,
            vec![
                "server.port".to_string(),
                "server.hostname".to_string(),
                "server.cors".to_string(),
            ]
        );
        // キー名だけを返し、設定値（CORS オリジン等）はレスポンスへ混ぜない。
        assert!(!format!("{keys:?}").contains("example.test"));
    }

    #[test]
    fn restart_required_keys_includes_log_level() {
        // 回帰ガード (OBS-04): log_level は起動時確定のため再起動が必要。
        let patch = AppConfig {
            log_level: Some("debug".to_string()),
            ..Default::default()
        };
        assert_eq!(restart_required_keys(&patch), vec!["log_level".to_string()]);

        let patch = AppConfig {
            log_level: Some("debug".to_string()),
            server: Some(opencode_config::schema::ServerConfig {
                port: Some(41234),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            restart_required_keys(&patch),
            vec!["log_level".to_string(), "server.port".to_string()]
        );
        // 設定値（"debug"）はキー一覧へ混ぜない。
        assert!(!format!("{:?}", restart_required_keys(&patch)).contains("debug"));
    }

    #[test]
    fn patch_config_rebuilds_registry_after_persistence() {
        let source = include_str!("routes.rs");
        let start = source
            .find("async fn patch_config(")
            .expect("patch_config must exist");
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("patch_config closing brace");
        let body = &rest[..end];
        let persist_at = body.find("update_config(").expect("persist call");
        let rebuild_at = body
            .find("rebuild_provider_registry_after_patch(")
            .expect("registry rebuild call");
        assert!(
            persist_at < rebuild_at,
            "provider registry must be rebuilt after config persistence: {body}"
        );
    }

    #[test]
    fn patch_config_applies_post_load_transforms_to_merged_config() {
        // R2: PATCH 後のメモリ上設定にも起動時と同じ変換を適用すること。
        let source = include_str!("routes.rs");
        let start = source
            .find("async fn patch_config(")
            .expect("patch_config must exist");
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("patch_config closing brace");
        let body = &rest[..end];
        let merge_at = body.find("config.merge(").expect("merge call");
        let transform_at = body
            .find("apply_post_load_transforms(")
            .expect("post-load transform call");
        let clone_at = body.find("let updated = config.clone()").expect("clone");
        assert!(
            merge_at < transform_at && transform_at < clone_at,
            "transforms must run after merge and before response snapshot: {body}"
        );
        // R4: PATCH のインメモリ経路では環境由来 username を注入しない。
        let call = &body[transform_at..];
        assert!(
            call.starts_with("apply_post_load_transforms(&mut config, false)"),
            "PATCH transforms must not inject environment username: {body}"
        );
    }

    #[tokio::test]
    async fn patch_adding_plural_provider_rebuilds_registry_with_it() {
        let state = Arc::new(ServerState::new());
        assert!(
            resolve_provider_and_model(&state, Some("patched-model"), None, None).is_err(),
            "precondition: patched-model must not resolve before PATCH"
        );

        let model = opencode_config::schema::ModelConfig {
            name: Some("patched-model".to_string()),
            ..Default::default()
        };
        let patch = AppConfig {
            providers: Some(HashMap::from([(
                "openai".to_string(),
                opencode_config::schema::ProviderConfig {
                    // テスト専用のダミー値。ログ・エラーへは出さない。
                    api_key: Some("dummy-patched-key".to_string()),
                    models: Some(HashMap::from([("patched-model".to_string(), model)])),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };
        state.config.write().await.merge(patch);
        rebuild_provider_registry_after_patch(&state).await;

        let registry = state.providers.read().expect("provider registry lock");
        assert!(
            registry.get("openai").is_some(),
            "PATCH-added providers.openai must be present in the rebuilt registry"
        );
        drop(registry);

        let (_, provider_id, model_id) =
            resolve_provider_and_model(&state, Some("patched-model"), None, None)
                .expect("patched model must resolve after PATCH");
        assert_eq!(provider_id, "openai");
        assert_eq!(model_id, "patched-model");
    }

    fn models_data_with_provider(provider_id: &str) -> ModelsData {
        // テスト専用のダミー値。トークン値やパスは含めず、ログ・エラーへも出さない。
        let info = opencode_provider::ModelsProviderInfo {
            api: None,
            name: provider_id.to_string(),
            env: Vec::new(),
            id: provider_id.to_string(),
            npm: None,
            models: HashMap::new(),
        };
        HashMap::from([(provider_id.to_string(), info)])
    }

    async fn fresh_models_data() -> ModelsData {
        models_data_with_provider("fresh-provider")
    }

    #[tokio::test]
    async fn invalidate_model_variant_lookup_forces_recomputation() {
        let _serial = VARIANT_LOOKUP_TEST_LOCK.lock().await;
        let sentinel_provider = "state04-sentinel-provider";
        let seeded: ModelVariantLookup =
            HashMap::from([(sentinel_provider.to_string(), HashMap::new())]);
        let previous = {
            let seeded_cell: VariantLookupCell = Arc::new(OnceCell::new());
            let _ = seeded_cell.set(Arc::new(seeded));
            let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
            let previous = guard.replace(seeded_cell);
            // シードと検証を同じロック区内で行い、並列テストによる割り込みを避ける。
            assert!(
                guard
                    .as_ref()
                    .and_then(|cell| cell.get())
                    .is_some_and(|lookup| lookup.contains_key(sentinel_provider)),
                "precondition: seeded lookup must be cached"
            );
            previous
        };

        invalidate_model_variant_lookup().await;

        let recomputed = get_model_variant_lookup().await;
        // 並列実行される他テストへキャッシュを残さないよう、直前の状態を復元する。
        *MODEL_VARIANT_LOOKUP.lock().await = previous;
        assert!(
            !recomputed.contains_key(sentinel_provider),
            "cached lookup must be recomputed after invalidation"
        );
    }

    #[tokio::test]
    async fn concurrent_lookup_initialization_loads_data_only_once() {
        let _serial = VARIANT_LOOKUP_TEST_LOCK.lock().await;
        {
            let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
            let _ = guard.take();
        }

        let load_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&load_count);
        let loader = move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                models_data_with_provider("concurrent-provider")
            }
        };

        let (a, b, c, d) = tokio::join!(
            get_model_variant_lookup_with(&loader),
            get_model_variant_lookup_with(&loader),
            get_model_variant_lookup_with(&loader),
            get_model_variant_lookup_with(&loader),
        );

        assert_eq!(
            load_count.load(Ordering::SeqCst),
            1,
            "parallel cold init must load data only once"
        );
        assert!(
            Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &c) && Arc::ptr_eq(&a, &d),
            "all concurrent callers must share the single initialized lookup"
        );
        assert!(a.contains_key("concurrent-provider"));

        {
            let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
            let _ = guard.take();
        }
    }

    #[tokio::test]
    async fn invalidate_during_load_prevents_stale_lookup_from_reviving() {
        let _serial = VARIANT_LOOKUP_TEST_LOCK.lock().await;
        {
            let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
            let _ = guard.take();
        }

        let started = Arc::new(AtomicBool::new(false));
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel::<()>();
        let resume_rx = Arc::new(std::sync::Mutex::new(Some(resume_rx)));
        let started_for_loader = Arc::clone(&started);
        let stale_loader = move || {
            let started = Arc::clone(&started_for_loader);
            let resume_rx = Arc::clone(&resume_rx);
            async move {
                started.store(true, Ordering::SeqCst);
                let receiver = {
                    let mut guard = resume_rx.lock().expect("resume receiver lock");
                    guard.take().expect("resume receiver used once")
                };
                let _ = receiver.await;
                models_data_with_provider("stale-provider")
            }
        };

        // 同じタスク上で「古い load」と「invalidate → 再計算」を交互に進める。
        let control = async {
            // 古い load が開始するまで待つ（開始しなければ 2 秒で失敗）。
            tokio::time::timeout(Duration::from_secs(2), async {
                while !started.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("stale loader must start");

            // load が完了する前に invalidate → 再計算。
            invalidate_model_variant_lookup().await;
            let fresh = get_model_variant_lookup_with(fresh_models_data).await;
            assert!(
                fresh.contains_key("fresh-provider"),
                "recomputation after invalidate must use fresh data"
            );

            // ここで初めて古い load を完了させる。
            let _ = resume_tx.send(());
            fresh
        };

        let (stale, fresh) = tokio::join!(get_model_variant_lookup_with(stale_loader), control);
        assert!(stale.contains_key("stale-provider"));

        // 古い load が遅れて完了しても、キャッシュは再計算後の値のまま。
        let final_lookup = get_model_variant_lookup_with(fresh_models_data).await;
        assert!(
            Arc::ptr_eq(&fresh, &final_lookup) && final_lookup.contains_key("fresh-provider"),
            "stale in-flight load must not overwrite the invalidated-then-recomputed lookup"
        );
        assert!(!final_lookup.contains_key("stale-provider"));

        {
            let mut guard = MODEL_VARIANT_LOOKUP.lock().await;
            let _ = guard.take();
        }
    }

    #[test]
    fn rebuild_provider_registry_after_patch_invalidates_model_variant_lookup() {
        let source = include_str!("routes.rs");
        let start = source
            .find("async fn rebuild_provider_registry_after_patch(")
            .expect("rebuild function must exist");
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("rebuild function closing brace");
        let body = &rest[..end];
        assert!(
            body.contains("invalidate_model_variant_lookup()"),
            "config re-bootstrap must drop the cached variant lookup: {body}"
        );
    }
}

#[cfg(test)]
mod tui_response_queue_tests {
    use super::*;

    /// RES-02 regression: the externally writable `/control/response` inbox
    /// must stay bounded, keeping the newest entries and the `Json<bool>`
    /// response shape.
    #[tokio::test]
    async fn submit_tui_response_bounds_queue_and_drops_oldest() {
        let state = Arc::new(ServerState::new());
        let extra = 3usize;
        let total = MAX_TUI_RESPONSE_QUEUE + extra;

        for index in 0..total {
            let response = submit_tui_response(
                State(state.clone()),
                Json(serde_json::json!({ "n": index })),
            )
            .await;
            assert!(response.0, "external response shape must stay Json<bool>");
        }

        let queue = state.tui_response_queue.lock().await;
        assert_eq!(queue.len(), MAX_TUI_RESPONSE_QUEUE);
        assert_eq!(
            queue
                .front()
                .and_then(|value| value.get("n"))
                .and_then(|n| n.as_u64()),
            Some(extra as u64),
            "oldest entries must be dropped first"
        );
        assert_eq!(
            queue
                .back()
                .and_then(|value| value.get("n"))
                .and_then(|n| n.as_u64()),
            Some((total - 1) as u64),
            "newest entry must be retained"
        );
    }
}

#[cfg(test)]
mod todo_route_stable_id_tests {
    use super::*;
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// tower が workspace から削除済みのため、ループバックの素の HTTP/1.1 GET で
    /// 実際のルーティングを通す（api_alias_tests と同じ方式）。
    async fn http_get(addr: SocketAddr, path: &str) -> (StatusCode, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = TcpStream::connect(addr)
                .await
                .expect("connect to loopback test server");
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            stream
                .write_all(request.as_bytes())
                .await
                .expect("write request");
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.expect("read response");
            let raw = String::from_utf8(raw).expect("response should be utf-8");
            let status: u16 = raw
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("status line")
                .parse()
                .expect("numeric status");
            let body = raw
                .split_once("\r\n\r\n")
                .map(|(_, body)| body.to_string())
                .unwrap_or_default();
            (StatusCode::from_u16(status).expect("valid status"), body)
        })
        .await
        .expect("http exchange should not hang")
    }

    /// GET /session/{id}/todo が `{session}_{position}` ではなく安定 ID を返し、
    /// todo 削除後も残った todo の ID が変化しないことを検証する
    /// (ID-TODO-POSITIONAL-01)。wire 型 (TodoInfo) の形は不変。
    #[tokio::test]
    async fn get_session_todos_returns_stable_ids_after_remove() {
        let state = Arc::new(ServerState::new());
        let session_id = {
            let mut sessions = state.sessions.lock().await;
            sessions.create("default", ".").id
        };
        state
            .todo_manager
            .update(
                &session_id,
                vec![
                    opencode_session::TodoInfo {
                        content: "A".to_string(),
                        status: "pending".to_string(),
                        priority: "high".to_string(),
                    },
                    opencode_session::TodoInfo {
                        content: "B".to_string(),
                        status: "pending".to_string(),
                        priority: "medium".to_string(),
                    },
                    opencode_session::TodoInfo {
                        content: "C".to_string(),
                        status: "pending".to_string(),
                        priority: "low".to_string(),
                    },
                ],
            )
            .await;

        let app = router().with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed in tests; skipping hides failures");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("test server stopped: {err}");
            }
        });

        let path = format!("/session/{session_id}/todo");
        let (status, body) = http_get(addr, &path).await;
        assert_eq!(status, StatusCode::OK, "GET {path} should succeed");
        let before: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("todo response should be a JSON array");
        assert_eq!(before.len(), 3);
        let ids: Vec<String> = before
            .iter()
            .map(|item| {
                item.get("id")
                    .and_then(|id| id.as_str())
                    .expect("id field")
                    .to_string()
            })
            .collect();
        assert_eq!(
            ids.iter().map(String::as_str).collect::<HashSet<_>>().len(),
            3,
            "todo ids must be unique"
        );
        // 旧実装の位置インデックス形式にはならないこと。
        for (position, id) in ids.iter().enumerate() {
            assert_ne!(id, &format!("{session_id}_{position}"));
        }
        // wire 型の他フィールドは従来どおり。
        assert_eq!(before[0]["content"], "A");
        assert_eq!(before[0]["status"], "pending");
        assert_eq!(before[0]["priority"], "high");

        // 先頭を削除して再取得しても、残った B/C の ID は不変。
        assert!(state.todo_manager.remove(&session_id, 0).await);
        let (status, body) = http_get(addr, &path).await;
        assert_eq!(status, StatusCode::OK);
        let after: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("todo response should be a JSON array");
        assert_eq!(after.len(), 2);
        let after_ids: Vec<String> = after
            .iter()
            .map(|item| {
                item.get("id")
                    .and_then(|id| id.as_str())
                    .expect("id field")
                    .to_string()
            })
            .collect();
        assert_eq!(after_ids, vec![ids[1].clone(), ids[2].clone()]);
        assert_eq!(
            after
                .iter()
                .map(|item| item["content"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["B", "C"]
        );

        server.abort();
    }
}
