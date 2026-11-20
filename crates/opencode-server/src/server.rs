use async_trait::async_trait;
use axum::{
    extract::{ConnectInfo, State},
    http::{header, header::HeaderValue, request::Parts, HeaderName, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::sync::RwLock;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, Mutex, Notify, RwLock as TokioRwLock};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::{OnResponse, TraceLayer};
use tracing::{Instrument, Span};
use url::Url;
use uuid::Uuid;

use opencode_config::{load_config, Config as AppConfig};
use opencode_plugin::init_global;
use opencode_plugin::subprocess::{
    PluginAuthBridge, PluginContext, PluginFetchRequest, PluginLoader,
};
use opencode_provider::{
    bootstrap_config_from_raw, create_registry_from_bootstrap_config, register_custom_fetch_proxy,
    unregister_custom_fetch_proxy, AuthInfo, AuthManager, ConfigModel as BootstrapConfigModel,
    ConfigProvider as BootstrapConfigProvider, CustomFetchProxy, CustomFetchRequest,
    CustomFetchResponse, CustomFetchStreamResponse, ProviderError, ProviderRegistry,
    RotationReason,
};
use opencode_session::SessionManager;
use opencode_storage::InputRepository;
use opencode_storage::{Database, MessageRepository, SessionRepository};

use crate::error::ApiError;
use crate::pty::PtyManager;
use crate::routes;

const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:4096";

/// `x-request-id` ヘッダ値の最大長（文字数）。超過分は先頭のみ採用する。
const MAX_REQUEST_ID_LEN: usize = 128;

/// `ask()` の待機 future が abort 等でドロップされたときに pending map から
/// エントリを削除するガード。
///
/// pending map は `std::sync::Mutex` で保持し、`Drop` 内では同期ロック Only で
/// 削除する。これにより `Drop` から非同期ロックを取得せず（＝ロックを跨がず）、
/// ランタイム上でのデッドロックを回避する。通常の応答経路（reply/reject）では
/// 先にエントリが取り出されるため、ここでの `remove` は no-op になる。
struct PendingEntryGuard<'a, T> {
    id: String,
    pending: &'a StdMutex<HashMap<String, T>>,
}

impl<T> Drop for PendingEntryGuard<'_, T> {
    fn drop(&mut self) {
        let mut map = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.remove(&self.id);
    }
}

fn generate_auth_token() -> String {
    use base64::Engine;
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `OPENCODE_NO_AUTH` の値をパースし、認証を無効化すべきかどうかを返す純粋関数。
///
/// `true` / `1` / `yes`（大小文字無視、前後の空白は除去）の場合のみ `true` を返す。
/// 未設定・空文字・`0` / `false`・不正値はすべて fail-closed として認証を有効にする。
/// 環境変数を直接読まないため、単体テストから値を渡して検証できる。
fn parse_no_auth(value: Option<&str>) -> bool {
    value.is_some_and(|raw| {
        let normalized = raw.trim();
        normalized == "1"
            || normalized.eq_ignore_ascii_case("true")
            || normalized.eq_ignore_ascii_case("yes")
    })
}

/// 起動時に `OPENCODE_NO_AUTH` から認証状態を読み取る。
///
/// `OPENCODE_NO_AUTH` が明示的に無効化を要求する値の場合のみ `false`。
/// 結果は `ServerState::auth_enabled` に一度だけ保持し、リクエスト処理中は
/// 環境変数を再読しない（起動後の env 変更で認証判定が変わらないようにする）。
fn auth_enabled_from_env() -> bool {
    !parse_no_auth(std::env::var("OPENCODE_NO_AUTH").ok().as_deref())
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionRequestInfo {
    pub id: String,
    pub session_id: String,
    pub tool: String,
    pub input: serde_json::Value,
    pub message: String,
    pub permission_type: String,
    pub patterns: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub always: Vec<String>,
}

#[derive(Debug)]
struct PendingPermission {
    info: PermissionRequestInfo,
    responder: oneshot::Sender<PermissionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionResponse {
    Once,
    Always,
    Reject { message: Option<String> },
}

pub struct PermissionBroker {
    pending: StdMutex<HashMap<String, PendingPermission>>,
    event_bus: broadcast::Sender<String>,
}

impl PermissionBroker {
    pub fn new(event_bus: broadcast::Sender<String>) -> Self {
        Self {
            pending: StdMutex::new(HashMap::new()),
            event_bus,
        }
    }

    pub async fn ask(&self, info: PermissionRequestInfo) -> PermissionResponse {
        let (tx, rx) = oneshot::channel();
        let pending = PendingPermission {
            info: info.clone(),
            responder: tx,
        };
        let id = info.id.clone();

        {
            let mut pending_map = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending_map.insert(id.clone(), pending);
        }

        // 待機 future がドロップされても pending map に残骸が残らないよう、
        // 待機期間全体をガードで覆う（Drop でエントリを削除する）。
        let _cleanup = PendingEntryGuard {
            id: id.clone(),
            pending: &self.pending,
        };

        // Broadcast permission request event
        let _ = self.event_bus.send(
            serde_json::json!({
                "type": "permission.requested",
                "request": info
            })
            .to_string(),
        );

        // Wait for response
        match rx.await {
            Ok(response) => response,
            Err(_) => PermissionResponse::Reject {
                message: Some("Permission request cancelled".to_string()),
            },
        }
    }

    pub async fn reply(&self, id: &str, response: PermissionResponse) -> bool {
        let pending = {
            let mut pending_map = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending_map.remove(id)
        };

        if let Some(pending) = pending {
            // 受け手が既にドロップされている場合は配信失敗とし、false を返す。
            let delivered = pending.responder.send(response.clone()).is_ok();

            // Broadcast permission reply event
            let _ = self.event_bus.send(
                serde_json::json!({
                    "type": "permission.replied",
                    "requestID": id,
                    "sessionID": pending.info.session_id,
                    "reply": match response {
                        PermissionResponse::Once => "once",
                        PermissionResponse::Always => "always",
                        PermissionResponse::Reject { .. } => "reject",
                    },
                    "message": match response {
                        PermissionResponse::Reject { message } => message,
                        _ => None,
                    },
                })
                .to_string(),
            );
            delivered
        } else {
            false
        }
    }

    pub async fn list(&self) -> Vec<PermissionRequestInfo> {
        let pending_map = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending_map
            .values()
            // 受け手が消失した残骸は返さない（ガードが効かなかった場合の防御）。
            .filter(|p| !p.responder.is_closed())
            .map(|p| p.info.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionRequestInfo {
    pub id: String,
    pub session_id: String,
    pub questions: Vec<opencode_tool::QuestionDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QuestionResponse {
    Answered(Vec<Vec<String>>),
    Rejected,
}

#[derive(Debug)]
struct PendingQuestion {
    info: QuestionRequestInfo,
    responder: oneshot::Sender<QuestionResponse>,
}

pub struct QuestionBroker {
    pending: StdMutex<HashMap<String, PendingQuestion>>,
    event_bus: broadcast::Sender<String>,
}

impl QuestionBroker {
    pub fn new(event_bus: broadcast::Sender<String>) -> Self {
        Self {
            pending: StdMutex::new(HashMap::new()),
            event_bus,
        }
    }

    pub async fn ask(&self, info: QuestionRequestInfo) -> QuestionResponse {
        let (tx, rx) = oneshot::channel();
        let pending = PendingQuestion {
            info: info.clone(),
            responder: tx,
        };
        let id = info.id.clone();

        {
            let mut pending_map = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending_map.insert(id.clone(), pending);
        }

        // 待機 future がドロップされても pending map に残骸が残らないよう、
        // 待機期間全体をガードで覆う（Drop でエントリを削除する）。
        let _cleanup = PendingEntryGuard {
            id: id.clone(),
            pending: &self.pending,
        };

        tracing::info!(
            question_id = %info.id,
            session_id = %info.session_id,
            questions = info.questions.len(),
            "question requested; waiting for an answer"
        );

        // Broadcast question request event
        let _ = self.event_bus.send(
            serde_json::json!({
                "type": "question.asked",
                "request": info
            })
            .to_string(),
        );

        // Wait for an answer from the API client
        match rx.await {
            Ok(response) => response,
            Err(_) => QuestionResponse::Rejected,
        }
    }

    pub async fn reply(&self, id: &str, answers: Vec<Vec<String>>) -> bool {
        let pending = {
            let mut pending_map = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending_map.remove(id)
        };

        if let Some(pending) = pending {
            tracing::info!(
                question_id = %id,
                session_id = %pending.info.session_id,
                answered = answers.len(),
                "question answered"
            );

            // 受け手が既にドロップされている場合は配信失敗とし、false を返す。
            let delivered = pending
                .responder
                .send(QuestionResponse::Answered(answers.clone()))
                .is_ok();

            // Broadcast question reply event
            let _ = self.event_bus.send(
                serde_json::json!({
                    "type": "question.replied",
                    "requestID": id,
                    "sessionID": pending.info.session_id,
                    "answers": answers,
                })
                .to_string(),
            );
            delivered
        } else {
            tracing::warn!(question_id = %id, "reply for unknown question request");
            false
        }
    }

    pub async fn reject(&self, id: &str) -> bool {
        let pending = {
            let mut pending_map = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending_map.remove(id)
        };

        if let Some(pending) = pending {
            tracing::info!(
                question_id = %id,
                session_id = %pending.info.session_id,
                "question rejected"
            );

            // 受け手が既にドロップされている場合は配信失敗とし、false を返す。
            let delivered = pending.responder.send(QuestionResponse::Rejected).is_ok();

            // Broadcast question rejection event
            let _ = self.event_bus.send(
                serde_json::json!({
                    "type": "question.rejected",
                    "requestID": id,
                    "sessionID": pending.info.session_id,
                })
                .to_string(),
            );
            delivered
        } else {
            tracing::warn!(question_id = %id, "reject for unknown question request");
            false
        }
    }

    pub async fn list(&self) -> Vec<QuestionRequestInfo> {
        let pending_map = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut result: Vec<_> = pending_map
            .values()
            // 受け手が消失した残骸は返さない（ガードが効かなかった場合の防御）。
            .filter(|p| !p.responder.is_closed())
            .map(|p| p.info.clone())
            .collect();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }
}

struct PluginBridgeFetchProxy {
    bridge: Arc<PluginAuthBridge>,
}

#[async_trait]
impl CustomFetchProxy for PluginBridgeFetchProxy {
    async fn fetch(
        &self,
        request: CustomFetchRequest,
    ) -> Result<CustomFetchResponse, ProviderError> {
        let response = self
            .bridge
            .fetch_proxy(PluginFetchRequest {
                url: request.url,
                method: request.method,
                headers: request.headers,
                body: request.body,
            })
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        Ok(CustomFetchResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }

    async fn fetch_stream(
        &self,
        request: CustomFetchRequest,
    ) -> Result<CustomFetchStreamResponse, ProviderError> {
        let response = self
            .bridge
            .fetch_proxy_stream(PluginFetchRequest {
                url: request.url,
                method: request.method,
                headers: request.headers,
                body: request.body,
            })
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let stream = ReceiverStream::new(response.chunks)
            .map(|item| item.map_err(|e| ProviderError::NetworkError(e.to_string())));
        Ok(CustomFetchStreamResponse {
            status: response.status,
            headers: response.headers,
            stream: Box::pin(stream),
        })
    }
}

pub(crate) fn sync_custom_fetch_proxy(
    provider_id: &str,
    bridge: Arc<PluginAuthBridge>,
    enabled: bool,
) {
    if enabled {
        register_custom_fetch_proxy(
            provider_id.to_string(),
            Arc::new(PluginBridgeFetchProxy {
                bridge: bridge.clone(),
            }),
        );
        if provider_id == "github-copilot" {
            register_custom_fetch_proxy(
                "github-copilot-enterprise",
                Arc::new(PluginBridgeFetchProxy { bridge }),
            );
        }
    } else {
        unregister_custom_fetch_proxy(provider_id);
        if provider_id == "github-copilot" {
            unregister_custom_fetch_proxy("github-copilot-enterprise");
        }
    }
}

pub struct ServerState {
    /// `SessionManager` を所有する。`ServerState` の生成/破棄にそのまま追従し、
    /// 破棄時は `SessionManager::drop` が plugin hook ハンドルを自動 abort する
    /// （本番コードからの明示的な `shutdown()` 呼び出しは不要）。
    pub sessions: Mutex<SessionManager>,
    pub(crate) config: TokioRwLock<AppConfig>,
    /// provider レジストリ。`&ServerState` を取る HTTP ハンドラからも差し替えられる
    /// よう interior mutability を持たせる（`patch_config` の再 bootstrap と
    /// 認証キーローテーションで使用）。
    pub providers: RwLock<ProviderRegistry>,
    pub auth_manager: Arc<AuthManager>,
    pub event_bus: broadcast::Sender<String>,
    pub auth_token: Option<String>,
    /// 起動時に確定した認証有効/無効。`auth_middleware` は毎回 env を読まずに
    /// この値だけを参照する（起動後の `OPENCODE_NO_AUTH` 変更に影響されない）。
    pub(crate) auth_enabled: bool,
    pub permission_broker: Arc<PermissionBroker>,
    pub question_broker: Arc<QuestionBroker>,
    /// Per-instance queue of pending TUI requests (was a process-global static).
    pub(crate) tui_request_queue: Mutex<VecDeque<routes::TuiRequest>>,
    /// Wakeup signal for `get_next_tui_request` waiters (per-instance).
    pub(crate) tui_request_notify: Notify,
    /// Per-instance response inbox for `/control/response`.
    ///
    /// Kept as an instance field rather than removed: `/control/response` is a
    /// registered HTTP route (external API) documented as part of the TUI
    /// bridge, so removing it would be a breaking change even though this
    /// codebase currently has no consumer that pops from the queue.
    pub(crate) tui_response_queue: Mutex<VecDeque<serde_json::Value>>,
    /// Wakeup signal for future response consumers (per-instance).
    pub(crate) tui_response_notify: Notify,
    /// セッションごとの実行中プロンプトタスクのキャンセルハンドル。
    /// `prompt_locks` により 1 セッション同時 1 本だけが登録される。
    pub(crate) running_prompt_tasks: StdMutex<HashMap<String, tokio::task::AbortHandle>>,
    /// 実行中プロンプトがキュー由来の場合のキュー項目 ID。
    /// abort 時に項目を終端化するため、`running_prompt_tasks` と同じセッション単位で管理する。
    pub(crate) running_prompt_queue_ids: StdMutex<HashMap<String, String>>,
    /// セッションごとの実行状態（旧プロセス共有 static `SESSION_RUN_STATUS`）。
    pub(crate) session_run_status: TokioRwLock<HashMap<String, routes::SessionRunStatus>>,
    /// セッションごとのプロンプト直列化ロック（旧プロセス共有 static `PROMPT_LOCKS`）。
    pub(crate) prompt_locks: TokioRwLock<HashMap<String, Arc<Mutex<()>>>>,
    /// プロジェクトごとのメタデータ（旧プロセス共有 static `PROJECT_METADATA`）。
    pub(crate) project_metadata: TokioRwLock<HashMap<String, routes::ProjectMetadata>>,
    /// セッションごとの TODO リスト（旧プロセス共有 static `TODO_MANAGER`）。
    pub(crate) todo_manager: opencode_session::TodoManager,
    /// PTY セッション管理（旧プロセス共有 static `PTY_MANAGER`）。
    pub(crate) pty_manager: PtyManager,
    session_repo: Option<SessionRepository>,
    message_repo: Option<MessageRepository>,
    pub(crate) input_repo: Option<InputRepository>,
    /// このインスタンス専用の CORS 追加許可オリジン。
    /// 旧 `EXTRA_CORS_WHITELIST`（プロセス共有 static）から移行し、
    /// リクエスト判定は常にこのフィールドのみを参照する。
    pub(crate) extra_cors_whitelist: Arc<RwLock<HashSet<String>>>,
}

impl ServerState {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        let auth_token = generate_auth_token();
        let auth_enabled = auth_enabled_from_env();
        let permission_broker = Arc::new(PermissionBroker::new(tx.clone()));
        let question_broker = Arc::new(QuestionBroker::new(tx.clone()));
        Self {
            sessions: Mutex::new(SessionManager::new()),
            config: TokioRwLock::new(AppConfig::default()),
            providers: RwLock::new(ProviderRegistry::new()),
            auth_manager: Arc::new(AuthManager::new()),
            event_bus: tx,
            auth_token: Some(auth_token),
            auth_enabled,
            permission_broker,
            question_broker,
            tui_request_queue: Mutex::new(VecDeque::new()),
            tui_request_notify: Notify::new(),
            tui_response_queue: Mutex::new(VecDeque::new()),
            tui_response_notify: Notify::new(),
            running_prompt_tasks: StdMutex::new(HashMap::new()),
            running_prompt_queue_ids: StdMutex::new(HashMap::new()),
            session_run_status: TokioRwLock::new(HashMap::new()),
            prompt_locks: TokioRwLock::new(HashMap::new()),
            project_metadata: TokioRwLock::new(HashMap::new()),
            todo_manager: opencode_session::TodoManager::new(),
            pty_manager: PtyManager::new(),
            session_repo: None,
            message_repo: None,
            input_repo: None,
            // CLI の起動時設定は legacy シードから 1 度だけコピーする。
            // 以降の変更はこのインスタンス内に閉じる。
            extra_cors_whitelist: Arc::new(RwLock::new(snapshot_legacy_cors_seed())),
        }
    }

    /// 実行中プロンプトタスクのキャンセルハンドルをセッション単位で登録する。
    pub(crate) fn register_prompt_task(
        &self,
        session_id: String,
        handle: tokio::task::AbortHandle,
    ) {
        self.running_prompt_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id, handle);
    }

    /// セッションの実行中プロンプトを取り出してキャンセルする。
    /// ハンドルが無い場合は `false` を返す（セッション自体の存在確認は呼び出し側で行う）。
    pub(crate) fn abort_prompt_task(&self, session_id: &str) -> bool {
        let handle = self
            .running_prompt_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id);
        if let Some(handle) = handle {
            handle.abort();
            true
        } else {
            false
        }
    }

    /// 実行中プロンプトに対応するキュー項目 ID を登録する。
    pub(crate) fn register_prompt_queue_id(&self, session_id: String, queue_id: String) {
        self.running_prompt_queue_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id, queue_id);
    }

    /// abort 時にキュー項目 ID を取り出す（取り出し後は登録を解除する）。
    pub(crate) fn take_prompt_queue_id(&self, session_id: &str) -> Option<String> {
        self.running_prompt_queue_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
    }

    /// 終了したタスクのキュー項目 ID を削除する。後続タスクの登録は残す。
    pub(crate) fn clear_prompt_queue_id(&self, session_id: &str, queue_id: &str) {
        let mut ids = self
            .running_prompt_queue_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if ids.get(session_id).is_some_and(|id| id == queue_id) {
            ids.remove(session_id);
        }
    }

    /// 終了したタスクのハンドルを削除する。後続タスクのハンドルは残す。
    pub(crate) fn clear_prompt_task(&self, session_id: &str, task_id: tokio::task::Id) {
        let mut tasks = self
            .running_prompt_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tasks
            .get(session_id)
            .is_some_and(|handle| handle.id() == task_id)
        {
            tasks.remove(session_id);
        }
    }

    pub async fn new_with_storage() -> anyhow::Result<Self> {
        Self::new_with_storage_for_url(DEFAULT_SERVER_URL.to_string()).await
    }

    pub async fn new_with_storage_for_url(server_url: String) -> anyhow::Result<Self> {
        let mut state = Self::new();
        let auth_dir = auth_data_dir();
        let auth_path = auth_dir.join("auth.json");
        let auth_manager = Arc::new(match AuthManager::try_load_from_file(&auth_dir).await {
            Ok(manager) => manager,
            // A missing auth file is the normal first-run state, not a load failure.
            Err(_) if !auth_path.exists() => AuthManager::with_filepath(auth_path),
            Err(error) => return Err(error),
        });
        state.auth_manager = auth_manager.clone();
        load_plugin_auth_store(&server_url, auth_manager.clone()).await;

        // 起動時の config 読み込みと BootstrapConfig 変換は
        // `build_initial_provider_registry` に集約する。401/429 ローテーション後の
        // 再 bootstrap は `build_provider_registry_from_auth` を使う（下記 NOTE 参照）。
        let initial_registry = state.build_initial_provider_registry().await;
        state.replace_provider_registry(initial_registry);
        let db = Database::new().await?;
        let pool = db.pool().clone();
        state.session_repo = Some(SessionRepository::new(pool.clone()));
        state.message_repo = Some(MessageRepository::new(pool.clone()));
        state.input_repo = Some(InputRepository::new(pool));
        state.load_sessions_from_storage().await?;
        if let Some(input_repo) = &state.input_repo {
            input_repo.reset_processing().await?;
        }
        Ok(state)
    }

    /// HTTP status をローテーション理由へ変換する純粋関数（401/429 のみ対象）。
    ///
    /// キー値や provider の応答本文を一切受け取らないため、ここから秘密情報が
    /// ログ・エラーへ漏れる経路は存在しない。
    fn rotation_reason_for_status(status: StatusCode) -> Option<RotationReason> {
        match status {
            StatusCode::UNAUTHORIZED => Some(RotationReason::Unauthorized),
            StatusCode::TOO_MANY_REQUESTS => Some(RotationReason::RateLimited),
            _ => None,
        }
    }

    /// `AppConfig` → `BootstrapConfig` の変換を起動時と再 bootstrap で共有する。
    fn bootstrap_config_from_app_config(config: &AppConfig) -> opencode_provider::BootstrapConfig {
        let providers = convert_config_providers_for_bootstrap(config);
        bootstrap_config_from_raw(
            providers,
            config.disabled_providers.clone(),
            config.enabled_providers.clone(),
            config.model.clone(),
            config.small_model.clone(),
        )
    }

    /// 起動時専用: config を読み込み `state.config` へ同期しつつ、
    /// `AuthManager` のアクティブ認証から初期 `ProviderRegistry` を構築する。
    pub async fn build_initial_provider_registry(&mut self) -> ProviderRegistry {
        let cwd = std::env::current_dir().unwrap_or_default();
        let loaded_config = load_config(&cwd).ok();
        if let Some(config) = loaded_config.as_ref() {
            // Keep this instance's config in sync with the loaded config so
            // routes that read it (model resolution, /config) see the same
            // values as the provider bootstrap.
            *self.config.write().await = config.clone();
        }
        let bootstrap_config = match loaded_config {
            Some(config) => Self::bootstrap_config_from_app_config(&config),
            None => {
                tracing::warn!("failed to load config for provider bootstrap, using defaults");
                opencode_provider::BootstrapConfig::default()
            }
        };
        let auth_store = self.auth_manager.list().await;
        create_registry_from_bootstrap_config(&bootstrap_config, &auth_store)
    }

    /// 現在の `state.config` と `AuthManager.list()` から `ProviderRegistry` を
    /// 作り直す（PATCH 後の再 bootstrap とローテーション後の再 bootstrap 経路）。
    ///
    /// キー値や provider の応答本文は受け取らず、ログにも出さない。
    pub async fn build_provider_registry_from_auth(&self) -> ProviderRegistry {
        let auth_store = self.auth_manager.list().await;
        let config = self.config.read().await.clone();
        let bootstrap_config = Self::bootstrap_config_from_app_config(&config);
        create_registry_from_bootstrap_config(&bootstrap_config, &auth_store)
    }

    /// 新しいレジストリで `ServerState.providers` を差し替える。
    /// `&ServerState` からのみ呼ばれ、トークン値は受け取らない。
    pub fn replace_provider_registry(&self, registry: ProviderRegistry) {
        let mut current = self
            .providers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current = registry;
    }

    // NOTE(R2 主実行) — `patch_config` は永続化後に
    // `build_provider_registry_from_auth()` と本差し替えを呼ぶよう配線済み。
    // provider の HTTP 401/429 自動ローテーションは routes.rs の provider
    // 呼び出し側（`state.providers.read()...get_provider(...)`）と
    // opencode-provider クレート内の検出に依存し、本メソッドは API としてのみ
    // 提供する（自動発火の配線は担当外）。いずれにせよ受け取るのは
    // provider_id と StatusCode のみで、トークン値・provider 応答本文を
    // ログ/エラーへ出す経路は存在しない。

    /// provider の 401/429 でキーをローテーションする API（現状は HTTP 経路へ未配線）。
    ///
    /// 呼び出し側（`routes.rs` の provider 呼び出し）は本変更の担当外のため、
    /// 本メソッドは API としてのみ提供され、実運用ではまだ自動発火しない。
    ///
    /// `provider_id` と `StatusCode` のみを受け取り、`RotationReason` を付けて
    /// `AuthManager::rotate_key` を呼ぶ（同メソッドは provider_id と理由だけを
    /// 構造化ログに出し、キー値は出さない）。ローテーションが起きた場合は
    /// `auth_manager.list()` から再 bootstrap した `ProviderRegistry` を返す。
    /// 401/429 以外、または代替キーが無い場合は `None`。
    pub async fn rotate_provider_key(
        &self,
        provider_id: &str,
        status: StatusCode,
    ) -> Option<ProviderRegistry> {
        let reason = Self::rotation_reason_for_status(status)?;
        if !self.auth_manager.rotate_key(provider_id, reason).await {
            return None;
        }
        Some(self.build_provider_registry_from_auth().await)
    }

    pub fn broadcast(&self, event: &str) {
        let _ = self.event_bus.send(event.to_string());
    }

    async fn load_sessions_from_storage(&self) -> anyhow::Result<()> {
        let (Some(session_repo), Some(message_repo)) = (&self.session_repo, &self.message_repo)
        else {
            return Ok(());
        };

        // 起動時一覧は tolerant 版を使う。1 行の破損した permission / summary 等で
        // サーバ全体の起動が止まらないようにする（個別 get() は fail-loud を維持）。
        let stored_sessions = session_repo.list_skipping_corrupt(None, 100_000).await?;
        let mut manager = self.sessions.lock().await;

        for stored in stored_sessions {
            // session 側 MessageRole には serde rename が無いため、JSON 往復では
            // types 側の小文字 role を読めない。明示変換で enum を直接写す。
            let stored_messages: Vec<opencode_session::SessionMessage> = message_repo
                .list_for_session(&stored.id.0)
                .await?
                .into_iter()
                .map(session_message_from_stored)
                .collect();
            let session = opencode_session::Session {
                id: stored.id.0,
                slug: stored.slug,
                project_id: stored.project_id.0,
                directory: stored.directory,
                parent_id: stored.parent_id.map(|id| id.0),
                title: stored.title,
                version: stored.version,
                time: opencode_session::SessionTime {
                    created: stored.time.created as i64,
                    updated: stored.time.updated as i64,
                    compacting: stored.time.compacting.map(|t| t as i64),
                    archived: stored.time.archived.map(|t| t as i64),
                },
                messages: stored_messages,
                summary: stored.summary.map(restore_session_summary),
                share: stored
                    .share
                    .map(|s| opencode_session::SessionShare { url: s.url }),
                revert: stored.revert.map(|r| opencode_session::SessionRevert {
                    message_id: r.message_id,
                    part_id: r.part_id,
                    snapshot: r.snapshot,
                    diff: r.diff,
                }),
                permission: stored
                    .permission
                    .map(|p| opencode_session::PermissionRuleset {
                        allow: p.allow,
                        deny: p.deny,
                        mode: p.mode,
                    }),
                usage: stored.usage.map(|u| opencode_session::SessionUsage {
                    input_tokens: u.input as u64,
                    output_tokens: u.output as u64,
                    reasoning_tokens: u.reasoning as u64,
                    cache_write_tokens: u.cache.write as u64,
                    cache_read_tokens: u.cache.read as u64,
                    // usage_total_cost 列には実コストのみを保存しているため、
                    // トークン数をコストとして誤表示せずそのまま復元する。
                    // トークン総数は input_tokens / output_tokens で賄う。
                    total_cost: u.total_cost,
                }),
                status: match stored.status {
                    opencode_types::SessionStatus::Active => {
                        opencode_session::SessionStatus::Active
                    }
                    opencode_types::SessionStatus::Completed => {
                        opencode_session::SessionStatus::Completed
                    }
                    opencode_types::SessionStatus::Archived => {
                        opencode_session::SessionStatus::Archived
                    }
                    opencode_types::SessionStatus::Compacting => {
                        opencode_session::SessionStatus::Compacting
                    }
                },
                // 従来は空 HashMap で上書きしており、保存済み metadata を
                // 起動時に失していた。保存値をそのまま復元する。
                metadata: stored.metadata.clone(),
                created_at: stored.created_at,
                updated_at: stored.updated_at,
            };
            manager.update(session);
        }

        Ok(())
    }

    pub async fn sync_sessions_to_storage(&self) -> anyhow::Result<()> {
        let (Some(session_repo), Some(message_repo)) = (&self.session_repo, &self.message_repo)
        else {
            return Ok(());
        };

        let snapshot: Vec<opencode_session::Session> = {
            let manager = self.sessions.lock().await;
            manager.list().into_iter().cloned().collect()
        };

        let snapshot_ids: HashSet<String> = snapshot.iter().map(|s| s.id.clone()).collect();
        let persisted = session_repo.list(None, 100_000).await?;

        for stale in persisted {
            if !snapshot_ids.contains(&stale.id.0) {
                message_repo.delete_for_session(&stale.id.0).await?;
                session_repo.delete(&stale.id.0).await?;
            }
        }

        for session in snapshot {
            // serde_json 往復では usage / metadata / synthetic / ignored が落ち、
            // session 側 SessionUsage と types 側 TokenUsage の JSON スキーマ差で
            // 変換自体が失敗しうる。フィールド単位の明示変換を使う。
            let stored_session = stored_session_from_session(&session);

            // Convert messages to opencode_types::SessionMessage
            let messages: Vec<opencode_types::SessionMessage> = session
                .messages
                .iter()
                .map(stored_message_from_session)
                .collect();

            if session_repo.get(&stored_session.id.0).await?.is_some() {
                session_repo.update(&stored_session).await?;
            } else {
                session_repo.create(&stored_session).await?;
            }

            // delete + create を単一トランザクションで実行し、途中失敗で
            // メッセージが欠損しないようにする (H-10)。
            message_repo
                .replace_for_session(&stored_session.id.0, &messages)
                .await?;
        }

        Ok(())
    }
}

/// 永続化された要約 (`opencode_types::SessionSummary`) を in-memory の
/// `opencode_session::SessionSummary` へ変換する。
///
/// storage 側は `diffs: Vec<FileDiff>`、session 側は `Option<Vec<FileDiff>>` と
/// 型が異なるため明示的な変換が必要。空の diffs も「要約あり」として保持し、
/// `stored.summary` が `Some` のとき復元後の `Session.summary` も `Some` になるようにする。
fn restore_session_summary(
    summary: opencode_types::SessionSummary,
) -> opencode_session::SessionSummary {
    opencode_session::SessionSummary {
        additions: summary.additions,
        deletions: summary.deletions,
        files: summary.files,
        diffs: Some(
            summary
                .diffs
                .into_iter()
                .map(|diff| opencode_session::FileDiff {
                    path: diff.path,
                    additions: diff.additions,
                    deletions: diff.deletions,
                })
                .collect(),
        ),
    }
}

/// in-memory の role を永続化用 role へ変換する（文字列を経由しない）。
fn stored_role(role: &opencode_session::MessageRole) -> opencode_types::MessageRole {
    match role {
        opencode_session::MessageRole::User => opencode_types::MessageRole::User,
        opencode_session::MessageRole::Assistant => opencode_types::MessageRole::Assistant,
        opencode_session::MessageRole::System => opencode_types::MessageRole::System,
        opencode_session::MessageRole::Tool => opencode_types::MessageRole::Tool,
    }
}

/// 永続化用 role を in-memory の role へ変換する（文字列を経由しない）。
/// session 側 `MessageRole` には serde rename が無いため、JSON 往復に依存すると
/// types 側の小文字 wire 形式を読めない。
fn session_role(role: &opencode_types::MessageRole) -> opencode_session::MessageRole {
    match role {
        opencode_types::MessageRole::User => opencode_session::MessageRole::User,
        opencode_types::MessageRole::Assistant => opencode_session::MessageRole::Assistant,
        opencode_types::MessageRole::System => opencode_session::MessageRole::System,
        opencode_types::MessageRole::Tool => opencode_session::MessageRole::Tool,
    }
}

/// in-memory のメッセージ部分を永続化用へ変換する。
///
/// `PartType::Text` の `synthetic` / `ignored` は session 側にしか無かったため、
/// serde の往復では失われていた。types 側へ同じフィールドを追加した上で、
/// ここでも明示的に受け渡す。
fn stored_part_from_session(part: &opencode_session::MessagePart) -> opencode_types::MessagePart {
    use opencode_session::PartType as SessionPart;
    use opencode_types::PartType as StoredPart;

    let part_type = match &part.part_type {
        SessionPart::Text {
            text,
            synthetic,
            ignored,
        } => StoredPart::Text {
            text: text.clone(),
            synthetic: *synthetic,
            ignored: *ignored,
        },
        SessionPart::ToolCall { id, name, input } => StoredPart::ToolCall {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        SessionPart::ToolResult {
            tool_call_id,
            content,
            is_error,
        } => StoredPart::ToolResult {
            tool_call_id: tool_call_id.clone(),
            content: content.clone(),
            is_error: *is_error,
        },
        SessionPart::Reasoning { text } => StoredPart::Reasoning { text: text.clone() },
        SessionPart::File {
            url,
            filename,
            mime,
        } => StoredPart::File {
            url: url.clone(),
            filename: filename.clone(),
            mime: mime.clone(),
        },
        SessionPart::StepStart { id, name } => StoredPart::StepStart {
            id: id.clone(),
            name: name.clone(),
        },
        SessionPart::StepFinish { id, output } => StoredPart::StepFinish {
            id: id.clone(),
            output: output.clone(),
        },
        SessionPart::Snapshot { content } => StoredPart::Snapshot {
            content: content.clone(),
        },
        SessionPart::Patch {
            old_string,
            new_string,
            filepath,
        } => StoredPart::Patch {
            old_string: old_string.clone(),
            new_string: new_string.clone(),
            filepath: filepath.clone(),
        },
        SessionPart::Agent { name, status } => StoredPart::Agent {
            name: name.clone(),
            status: status.clone(),
        },
        SessionPart::Subtask {
            id,
            description,
            status,
        } => StoredPart::Subtask {
            id: id.clone(),
            description: description.clone(),
            status: status.clone(),
        },
        SessionPart::Retry { count, reason } => StoredPart::Retry {
            count: *count,
            reason: reason.clone(),
        },
        SessionPart::Compaction { summary } => StoredPart::Compaction {
            summary: summary.clone(),
        },
    };

    opencode_types::MessagePart {
        id: part.id.clone(),
        part_type,
        created_at: part.created_at,
        message_id: part.message_id.clone(),
    }
}

/// 永続化用メッセージ部分を in-memory の表現へ変換する。
fn session_part_from_stored(part: opencode_types::MessagePart) -> opencode_session::MessagePart {
    use opencode_session::PartType as SessionPart;
    use opencode_types::PartType as StoredPart;

    let part_type = match part.part_type {
        StoredPart::Text {
            text,
            synthetic,
            ignored,
        } => SessionPart::Text {
            text,
            synthetic,
            ignored,
        },
        StoredPart::ToolCall { id, name, input } => SessionPart::ToolCall { id, name, input },
        StoredPart::ToolResult {
            tool_call_id,
            content,
            is_error,
        } => SessionPart::ToolResult {
            tool_call_id,
            content,
            is_error,
        },
        StoredPart::Reasoning { text } => SessionPart::Reasoning { text },
        StoredPart::File {
            url,
            filename,
            mime,
        } => SessionPart::File {
            url,
            filename,
            mime,
        },
        StoredPart::StepStart { id, name } => SessionPart::StepStart { id, name },
        StoredPart::StepFinish { id, output } => SessionPart::StepFinish { id, output },
        StoredPart::Snapshot { content } => SessionPart::Snapshot { content },
        StoredPart::Patch {
            old_string,
            new_string,
            filepath,
        } => SessionPart::Patch {
            old_string,
            new_string,
            filepath,
        },
        StoredPart::Agent { name, status } => SessionPart::Agent { name, status },
        StoredPart::Subtask {
            id,
            description,
            status,
        } => SessionPart::Subtask {
            id,
            description,
            status,
        },
        StoredPart::Retry { count, reason } => SessionPart::Retry { count, reason },
        StoredPart::Compaction { summary } => SessionPart::Compaction { summary },
    };

    opencode_session::MessagePart {
        id: part.id,
        part_type,
        created_at: part.created_at,
        message_id: part.message_id,
    }
}

/// in-memory のメッセージを永続化用へ変換する。
///
/// `serde_json::to_value -> from_value` の往復では `usage` / `metadata` /
/// `synthetic` / `ignored` が失われるため、フィールド単位で写す。
fn stored_message_from_session(
    message: &opencode_session::SessionMessage,
) -> opencode_types::SessionMessage {
    opencode_types::SessionMessage {
        id: message.id.clone(),
        session_id: message.session_id.clone(),
        role: stored_role(&message.role),
        parts: message.parts.iter().map(stored_part_from_session).collect(),
        created_at: message.created_at,
        metadata: message.metadata.clone(),
        usage: message
            .usage
            .as_ref()
            .map(|usage| opencode_types::MessageUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                total_cost: usage.total_cost,
            }),
    }
}

/// 永続化用メッセージを in-memory の表現へ変換する。
fn session_message_from_stored(
    message: opencode_types::SessionMessage,
) -> opencode_session::SessionMessage {
    opencode_session::SessionMessage {
        id: message.id,
        session_id: message.session_id,
        role: session_role(&message.role),
        parts: message
            .parts
            .into_iter()
            .map(session_part_from_stored)
            .collect(),
        created_at: message.created_at,
        metadata: message.metadata,
        usage: message.usage.map(|usage| opencode_session::MessageUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            total_cost: usage.total_cost,
        }),
    }
}

/// in-memory のセッションを永続化用 `opencode_types::Session` へ変換する。
///
/// `serde_json` を挟むと session 側 `SessionUsage` の JSON 形
/// (`input_tokens` 等）と types 側 `TokenUsage` の JSON 形（`input` 等）が
/// 一致せず `missing field` で変換が失敗する。加えて metadata / メッセージの
/// usage / Text の synthetic / ignored が落ちるため、全フィールドを明示的に写す。
/// enum は直接 match するため、status / role の wire 形式変更にも影響されない。
fn stored_session_from_session(session: &opencode_session::Session) -> opencode_types::Session {
    use opencode_session::SessionStatus as SessionStatusInMemory;
    use opencode_types::SessionStatus as StoredSessionStatus;

    let status = match &session.status {
        SessionStatusInMemory::Active => StoredSessionStatus::Active,
        SessionStatusInMemory::Completed => StoredSessionStatus::Completed,
        SessionStatusInMemory::Archived => StoredSessionStatus::Archived,
        SessionStatusInMemory::Compacting => StoredSessionStatus::Compacting,
    };

    opencode_types::Session {
        id: opencode_types::SessionID(session.id.clone()),
        slug: session.slug.clone(),
        project_id: opencode_types::ProjectID(session.project_id.clone()),
        directory: session.directory.clone(),
        parent_id: session.parent_id.clone().map(opencode_types::SessionID),
        title: session.title.clone(),
        version: session.version.clone(),
        time: opencode_types::TimeInfo {
            created: session.time.created as u64,
            updated: session.time.updated as u64,
            compacting: session.time.compacting.map(|t| t as u64),
            archived: session.time.archived.map(|t| t as u64),
        },
        messages: session
            .messages
            .iter()
            .map(stored_message_from_session)
            .collect(),
        summary: session
            .summary
            .as_ref()
            .map(|summary| opencode_types::SessionSummary {
                additions: summary.additions,
                deletions: summary.deletions,
                files: summary.files,
                diffs: summary
                    .diffs
                    .as_ref()
                    .map(|diffs| {
                        diffs
                            .iter()
                            .map(|diff| opencode_types::FileDiff {
                                path: diff.path.clone(),
                                additions: diff.additions,
                                deletions: diff.deletions,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
        share: session
            .share
            .as_ref()
            .map(|share| opencode_types::SessionShareInfo {
                url: share.url.clone(),
            }),
        revert: session
            .revert
            .as_ref()
            .map(|revert| opencode_types::SessionRevert {
                message_id: revert.message_id.clone(),
                part_id: revert.part_id.clone(),
                snapshot: revert.snapshot.clone(),
                diff: revert.diff.clone(),
            }),
        permission: session.permission.as_ref().map(|permission| {
            opencode_types::PermissionRuleset {
                allow: permission.allow.clone(),
                deny: permission.deny.clone(),
                mode: permission.mode.clone(),
            }
        }),
        usage: session
            .usage
            .as_ref()
            .map(|usage| opencode_types::TokenUsage {
                input: usage.input_tokens as f64,
                output: usage.output_tokens as f64,
                reasoning: usage.reasoning_tokens as f64,
                cache: opencode_types::CacheUsage {
                    read: usage.cache_read_tokens as f64,
                    write: usage.cache_write_tokens as f64,
                },
                // 実コストは input/output のトークン数とは別に保存する。
                total_cost: usage.total_cost,
            }),
        status,
        metadata: session.metadata.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

/// Convert both opencode_config provider maps to a bootstrap ConfigProvider map.
///
/// `ConfigLoader::load_all` normally folds the current-schema `providers` map
/// into the runtime `provider` map before the server sees it. A PATCH merges
/// straight into `state.config` without re-running the loader, so both
/// spellings can reach this function. Mirror the loader's rules: legacy
/// `provider` wins on an id conflict, and current-schema entries are normalized
/// the same way before the bootstrap conversion.
fn convert_config_providers_for_bootstrap(
    config: &opencode_config::Config,
) -> HashMap<String, BootstrapConfigProvider> {
    let mut converted: HashMap<String, BootstrapConfigProvider> = HashMap::new();

    if let Some(providers) = &config.provider {
        for (id, provider) in providers {
            let normalized = normalize_provider_for_bootstrap(provider.clone());
            converted.insert(id.clone(), provider_to_bootstrap(&normalized));
        }
    }

    if let Some(providers) = &config.providers {
        for (id, provider) in providers {
            if converted.contains_key(id) {
                // Same-id conflict: the legacy `provider` map keeps its
                // historical priority, matching `normalize_provider_configs`.
                continue;
            }
            let normalized = normalize_provider_for_bootstrap(provider.clone());
            converted.insert(id.clone(), provider_to_bootstrap(&normalized));
        }
    }

    converted
}

/// Mirror of `opencode_config::loader::normalize_provider_config`: flatten the
/// current-schema fields into the ones consumed by `provider_to_bootstrap`.
/// Values are only moved between fields; no value is ever logged or returned.
fn normalize_provider_for_bootstrap(
    mut provider: opencode_config::ProviderConfig,
) -> opencode_config::ProviderConfig {
    if provider.npm.is_none() {
        provider.npm = provider.package.take();
    } else {
        provider.package = None;
    }

    if let Some(settings) = provider.settings.take() {
        for (key, value) in settings {
            let target = match key.as_str() {
                "baseURL" | "baseUrl" | "base_url" | "api" => &mut provider.base_url,
                "apiKey" | "api_key" => &mut provider.api_key,
                _ => {
                    provider
                        .options
                        .get_or_insert_with(HashMap::new)
                        .insert(key, value);
                    continue;
                }
            };
            if target.is_none() {
                *target = value.as_str().map(ToString::to_string);
            }
        }
    }

    for (key, value) in [
        ("headers", provider.headers.take()),
        ("compatibility", provider.compatibility.take()),
    ] {
        if let Some(values) = value {
            provider.options.get_or_insert_with(HashMap::new).insert(
                key.to_string(),
                serde_json::Value::Object(values.into_iter().collect()),
            );
        }
    }

    provider
}

fn provider_to_bootstrap(provider: &opencode_config::ProviderConfig) -> BootstrapConfigProvider {
    let mut options = provider.options.clone().unwrap_or_default();
    if let Some(api_key) = &provider.api_key {
        options
            .entry("apiKey".to_string())
            .or_insert_with(|| serde_json::Value::String(api_key.clone()));
    }
    if let Some(base_url) = &provider.base_url {
        options
            .entry("baseURL".to_string())
            .or_insert_with(|| serde_json::Value::String(base_url.clone()));
    }

    let models = provider.models.as_ref().map(|models| {
        models
            .iter()
            .map(|(id, model)| (id.clone(), model_to_bootstrap(id, model)))
            .collect()
    });

    BootstrapConfigProvider {
        name: provider.name.clone(),
        api: provider.base_url.clone(),
        npm: provider.npm.clone(),
        options: (!options.is_empty()).then_some(options),
        models,
        blacklist: (!provider.blacklist.is_empty()).then_some(provider.blacklist.clone()),
        whitelist: (!provider.whitelist.is_empty()).then_some(provider.whitelist.clone()),
        ..Default::default()
    }
}

fn model_to_bootstrap(id: &str, model: &opencode_config::ModelConfig) -> BootstrapConfigModel {
    let mut options = HashMap::new();
    if let Some(api_key) = &model.api_key {
        options.insert(
            "apiKey".to_string(),
            serde_json::Value::String(api_key.clone()),
        );
    }

    let variants = model.variants.as_ref().map(|variants| {
        variants
            .iter()
            .map(|(name, variant)| (name.clone(), variant_to_bootstrap(variant)))
            .collect()
    });

    let limit = model
        .limit
        .as_ref()
        .map(|limit| opencode_provider::bootstrap::ConfigModelLimit {
            context: limit.context,
            output: limit.output,
        });
    let modalities = model.capabilities.as_ref().and_then(|capabilities| {
        if capabilities.input.is_none() && capabilities.output.is_none() {
            return None;
        }
        Some(opencode_provider::bootstrap::ConfigModalities {
            input: capabilities.input.clone(),
            output: capabilities.output.clone(),
        })
    });
    let capabilities = model.capabilities.as_ref();

    BootstrapConfigModel {
        id: model.model.clone().or_else(|| Some(id.to_string())),
        name: model.name.clone(),
        provider: model.base_url.as_ref().map(|url| {
            opencode_provider::bootstrap::ConfigModelProvider {
                api: Some(url.clone()),
                npm: None,
            }
        }),
        options: (!options.is_empty()).then_some(options),
        variants,
        limit,
        modalities,
        tool_call: capabilities.and_then(|capabilities| capabilities.tools),
        reasoning: capabilities.and_then(|capabilities| capabilities.reasoning_supported()),
        attachment: capabilities.and_then(|capabilities| capabilities.attachment),
        ..Default::default()
    }
}

fn variant_to_bootstrap(
    variant: &opencode_config::ModelVariantConfig,
) -> HashMap<String, serde_json::Value> {
    let mut values = variant.extra.clone();
    if let Some(disabled) = variant.disabled {
        values.insert("disabled".to_string(), serde_json::Value::Bool(disabled));
    }
    values
}

fn auth_data_dir() -> PathBuf {
    if let Ok(path) = std::env::var("OPENCODE_DATA_DIR") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("opencode")
        .join("data")
}

async fn load_plugin_auth_store(server_url: &str, auth_manager: Arc<AuthManager>) {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            tracing::warn!(%error, "failed to get current directory for plugin bootstrap");
            return;
        }
    };

    let config = match load_config(&cwd) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "failed to load config for plugin bootstrap");
            return;
        }
    };

    let loader = match PluginLoader::new() {
        Ok(loader) => Arc::new(loader),
        Err(error) => {
            tracing::warn!(%error, "failed to initialize plugin loader");
            return;
        }
    };
    init_global(loader.hook_system());

    let directory = cwd.to_string_lossy().to_string();
    let context = PluginContext {
        worktree: directory.clone(),
        directory,
        server_url: server_url.to_string(),
    };

    if let Err(error) = loader.load_builtins(&context).await {
        tracing::warn!(%error, "failed to load builtin auth plugins");
    }

    if !config.plugin.is_empty() {
        if let Err(error) = loader.load_all(&config.plugin, &context).await {
            tracing::warn!(%error, "failed to load configured plugins");
            return;
        }
    }

    routes::set_plugin_loader(loader.clone());

    let bridges = loader.auth_bridges().await;
    for (provider_id, bridge) in bridges {
        match bridge.load().await {
            Ok(result) => {
                sync_custom_fetch_proxy(&provider_id, bridge.clone(), result.has_custom_fetch);

                if let Some(api_key) = result.api_key {
                    if let Err(error) = auth_manager
                        .try_set(
                            &provider_id,
                            AuthInfo::Api {
                                key: api_key.clone(),
                            },
                        )
                        .await
                    {
                        tracing::warn!(provider = %provider_id, %error, "failed to persist plugin auth credentials");
                    }
                    // TS parity: copilot auth can power both standard and enterprise providers.
                    if provider_id == "github-copilot" {
                        if let Err(error) = auth_manager
                            .try_set("github-copilot-enterprise", AuthInfo::Api { key: api_key })
                            .await
                        {
                            tracing::warn!(%error, "failed to persist enterprise plugin auth credentials");
                        }
                    }
                }
            }
            Err(error) => {
                sync_custom_fetch_proxy(&provider_id, bridge.clone(), false);
                tracing::warn!(provider = provider_id, %error, "failed to load plugin auth");
            }
        }
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

/// CLI 起動時に `set_cors_whitelist()` へ渡されたオリジンのシード。
///
/// リクエスト判定には使わず、`ServerState::new()` が初期値をコピーするためだけに
/// 保持する。process-global な状態をリクエスト経路から排除するため、
/// 判定は常に各 `ServerState::extra_cors_whitelist` を参照する。
static LEGACY_CORS_SEED: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

fn normalize_origin(origin: &str) -> Option<String> {
    let trimmed = origin.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_cors_origins(origins: Vec<String>) -> HashSet<String> {
    origins
        .iter()
        .filter_map(|origin| normalize_origin(origin))
        .collect()
}

/// Legacy シードのスナップショットを返す（poisoned でも読み出す）。
fn snapshot_legacy_cors_seed() -> HashSet<String> {
    match LEGACY_CORS_SEED.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// 後方互換 API。CLI の起動時設定を受け取り、`ServerState` 生成時のシードとして
/// 保持するだけであり、リクエストごとの CORS 判定には関与しない。
/// `ServerState` 生成後に本関数を再呼び出しても既存インスタンスには反映されない。
///
/// 新しいコードでは `ServerState::set_cors_whitelist()` を優先すること。
/// オリジン値のみを扱い、認証トークンは受け取らないため、ログやエラーへ
/// 秘密情報が出る経路は存在しない。
pub fn set_cors_whitelist(origins: Vec<String>) {
    let next = normalize_cors_origins(origins);
    match LEGACY_CORS_SEED.write() {
        Ok(mut guard) => *guard = next,
        Err(poisoned) => *poisoned.into_inner() = next,
    }
}

fn is_extra_allowed_origin_exact_in(origin: &str, whitelist: &RwLock<HashSet<String>>) -> bool {
    let normalized = normalize_origin(origin).unwrap_or_else(|| origin.to_string());
    match whitelist.read() {
        Ok(guard) => guard.contains(&normalized),
        Err(poisoned) => poisoned.into_inner().contains(&normalized),
    }
}

fn is_allowed_origin_with_extra(origin: &str, whitelist: &RwLock<HashSet<String>>) -> bool {
    let parsed = Url::parse(origin);
    let Ok(parsed) = parsed else {
        return false;
    };

    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");

    // Allow localhost with any port, including when the port is omitted.
    // Browsers omit the port for the scheme's default (http://localhost => 80,
    // https://localhost => 443), so requiring an explicit port rejected valid
    // loopback origins. The host/scheme checks below are the security boundary:
    // only the loopback hosts over http/https pass, never a remote host.
    let is_localhost =
        matches!(host, "localhost" | "127.0.0.1" | "[::1]") && matches!(scheme, "http" | "https");

    // Allow tauri
    let is_tauri = matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    );

    // Allow exact opencode.ai subdomains (no wildcard suffix)
    let is_opencode_ai = scheme == "https" && matches!(host, "opencode.ai" | "www.opencode.ai");

    // Check the per-instance extra whitelist with exact match.
    let is_extra = is_extra_allowed_origin_exact_in(origin, whitelist);

    is_localhost || is_tauri || is_opencode_ai || is_extra
}

impl ServerState {
    /// このインスタンスの CORS 追加許可オリジンを設定する（完全置換）。
    /// オリジンのみを扱い、認証トークンは受け取らない。
    pub fn set_cors_whitelist(&self, origins: Vec<String>) {
        let next = normalize_cors_origins(origins);
        match self.extra_cors_whitelist.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// このインスタンスの追加許可リストにオリジンが完全一致するかを返す。
    #[cfg(test)]
    pub(crate) fn is_extra_allowed_origin_exact(&self, origin: &str) -> bool {
        is_extra_allowed_origin_exact_in(origin, &self.extra_cors_whitelist)
    }

    /// このインスタンスの許可リストを用いて Origin ヘッダ値を検証する。
    #[cfg(test)]
    pub(crate) fn is_allowed_origin(&self, origin: &str) -> bool {
        is_allowed_origin_with_extra(origin, &self.extra_cors_whitelist)
    }

    /// このインスタンスに結びついた CORS ミドルウェアを構築する。
    ///
    /// `AllowOrigin::predicate` は `'static` のクロージャを要求するため、
    /// リクエスト判定には `Arc<RwLock<_>>` をクローンして捕捉する。
    /// これにより同じプロセスの別インスタンスへ設定が漏れない。
    pub(crate) fn cors_layer(&self) -> CorsLayer {
        let extra = Arc::clone(&self.extra_cors_whitelist);
        CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(
                move |origin: &HeaderValue, _parts: &Parts| {
                    origin
                        .to_str()
                        .is_ok_and(|value| is_allowed_origin_with_extra(value, &extra))
                },
            ))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers(cors_allow_headers())
            // ブラウザの fetch から相関 ID を読めるようにする（allow_headers は
            // リクエスト側の許可のみでレスポンスヘッダの露出は制御しない）。
            .expose_headers([HeaderName::from_static("x-request-id")])
    }
}

/// `/session/{id}/...` および `/api/session/{id}/...` パスから session id を取り出す。
///
/// P-01 の `/api` エイリアス経由リクエストでも session id がスパンへ記録されるよう
/// `/api/session/` を優先して照合し、従来の `/session/` も維持する。
fn session_id_from_path(path: &str) -> Option<&str> {
    let rest = path
        .strip_prefix("/api/session/")
        .or_else(|| path.strip_prefix("/session/"))?;
    let id = rest.split('/').next().unwrap_or_default();
    (!id.is_empty()).then_some(id)
}

/// `x-request-id` ヘッダ値をログ安全な形式へ正規化する。
///
/// 制御文字を除去し前後の空白を落としたうえで最大 `MAX_REQUEST_ID_LEN` 文字へ
/// 切り詰める。空になった場合は `None` を返し、呼び出し側で UUID を生成する。
fn sanitize_request_id(raw: &str) -> Option<String> {
    let without_controls: String = raw.chars().filter(|ch| !ch.is_control()).collect();
    let cleaned = without_controls.trim();
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.chars().take(MAX_REQUEST_ID_LEN).collect())
}

/// 完了ログの深刻度。
///
/// 5xx はアラート対象のため ERROR、4xx は WARN、それ以外は INFO。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionLogLevel {
    Info,
    Warn,
    Error,
}

/// `outcome` から完了ログのレベルを決める純粋関数（回帰テストから直接検証可能）。
fn completion_log_level(outcome: &str) -> CompletionLogLevel {
    match outcome {
        "server_error" => CompletionLogLevel::Error,
        "client_error" => CompletionLogLevel::Warn,
        _ => CompletionLogLevel::Info,
    }
}

/// HTTP status を完了ログの outcome 文字列へ変換する純粋関数。
fn completion_outcome(status: StatusCode) -> &'static str {
    if status.is_server_error() {
        "server_error"
    } else if status.is_client_error() {
        "client_error"
    } else if status.is_redirection() {
        "redirect"
    } else {
        "success"
    }
}

/// レスポンス完了時に status / latency / outcome をリクエストスパンへ記録する。
#[derive(Clone, Debug)]
struct RequestCompletionLog;

impl<B> OnResponse<B> for RequestCompletionLog {
    fn on_response(self, response: &Response<B>, latency: Duration, span: &Span) {
        let status = response.status();
        let outcome = completion_outcome(status);
        let status_code = status.as_u16();
        let latency_ms = latency.as_millis() as u64;

        // 5xx を INFO のまま埋もれさせず、ERROR/WARN としてアラートに拾わせる。
        // トークンやクエリ文字列はフィールドに含めない。
        match completion_log_level(outcome) {
            CompletionLogLevel::Error => tracing::error!(
                parent: span,
                status = status_code,
                latency_ms = latency_ms,
                outcome = outcome,
                "http_request completed"
            ),
            CompletionLogLevel::Warn => tracing::warn!(
                parent: span,
                status = status_code,
                latency_ms = latency_ms,
                outcome = outcome,
                "http_request completed"
            ),
            CompletionLogLevel::Info => tracing::info!(
                parent: span,
                status = status_code,
                latency_ms = latency_ms,
                outcome = outcome,
                "http_request completed"
            ),
        }
    }
}

/// `into_make_service_with_connect_info` が挿入したピアアドレスから IP だけを取り出す。
///
/// ConnectInfo が無い経路（例: 直接 Router を呼ぶテスト）でも panic せず `None` を返す。
/// 実装差を吸収するため `ConnectInfo<SocketAddr>` と `SocketAddr` の両方を確認する。
fn client_ip_from_request(request: &Request<axum::body::Body>) -> Option<IpAddr> {
    if let Some(info) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        return Some(info.0.ip());
    }
    request.extensions().get::<SocketAddr>().map(SocketAddr::ip)
}

/// サニタイズ済み request id をレスポンスヘッダへ安全に変換する。
///
/// `sanitize_request_id` は制御文字を除くが、非 ASCII 等は残り得るため
/// `HeaderValue` 変換に失敗した場合は固定値へフォールバックする（入力を反射しない）。
fn request_id_header_value(request_id: &str) -> HeaderValue {
    if !request_id.is_ascii() {
        return HeaderValue::from_static("invalid-request-id");
    }
    match HeaderValue::from_str(request_id) {
        Ok(value) => value,
        Err(_) => HeaderValue::from_static("invalid-request-id"),
    }
}

/// リクエストごとの構造化スパンを生成し、request id / session id / client ip を
/// ログ文脈に伝播させる。また決定した request id をレスポンスヘッダで返す。
///
/// トークン・クエリ文字列・ボディはスパン属性に含めない。
async fn request_span_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .and_then(sanitize_request_id)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let session_id = session_id_from_path(request.uri().path()).map(str::to_owned);
    let client_ip = client_ip_from_request(&request);
    let method = request.method().clone();
    let path = request.uri().path().to_owned();

    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %method,
        path = %path,
        session_id = tracing::field::Empty,
        client_ip = tracing::field::Empty,
    );
    if let Some(session_id) = &session_id {
        span.record("session_id", session_id);
    }
    if let Some(ip) = &client_ip {
        span.record("client_ip", tracing::field::display(ip));
    }

    let mut response = next.run(request).instrument(span).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        request_id_header_value(&request_id),
    );
    response
}

/// CORS の `Access-Control-Allow-Headers` に明示的に含めるヘッダ名。
///
/// - `x-opencode-directory`: cross-origin の session create が作業ディレクトリを
///   指定するために送るヘッダ。
/// - `x-request-id`: フロントエンドから渡す相関ログ ID（`request_span_middleware`
///   がスパン属性として記録する）。
///
/// `AllowHeaders::mirror_request()` はリクエストが要求したヘッダを無条件に許可
/// するため、ここでは許可ヘッダを明示的に列挙する方式を採用した。
fn cors_allow_headers() -> [HeaderName; 5] {
    [
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
        header::ACCEPT,
        HeaderName::from_static("x-opencode-directory"),
        HeaderName::from_static("x-request-id"),
    ]
}

/// 2 つの文字列を固定時間で比較する（新規依存なしの自前実装）。
///
/// `subtle` 等の定数時間比較クレートは workspace に存在しないため、ここでは
/// 長さ不一致でも早期 return せず、両入力のうち長い方の長さまで常に同じ回数
/// ループさせ、各バイトの XOR 差をアキュムレータへ畳み込む。長さの差自体も
/// 累積値に XOR で含めるため、末尾が一致していても長さが違えば不一致になる。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut diff = a_bytes.len() ^ b_bytes.len();
    for index in 0..a_bytes.len().max(b_bytes.len()) {
        let left = a_bytes.get(index).copied().unwrap_or_default();
        let right = b_bytes.get(index).copied().unwrap_or_default();
        diff |= (left ^ right) as usize;
    }
    diff == 0
}

/// `auth_middleware` の判定を外部状態なしで検証するための純粋関数。
///
/// `Misconfigured` は fail-open を防ぐための分岐（認証有効・トークン未設定）で、
/// 呼び出し側は 500 を返す。トークン値は判定にのみ使い、ログへは出さない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthDecision {
    Pass,
    Unauthorized,
    Misconfigured,
}

fn auth_decision(
    auth_enabled: bool,
    expected_token: Option<&str>,
    supplied_bearer: Option<&str>,
) -> AuthDecision {
    if !auth_enabled {
        return AuthDecision::Pass;
    }

    let Some(expected) = expected_token else {
        return AuthDecision::Misconfigured;
    };

    match supplied_bearer {
        Some(token) if constant_time_eq(token, expected) => AuthDecision::Pass,
        _ => AuthDecision::Unauthorized,
    }
}

/// 認証 middleware の対象外にする liveness プローブのパス（完全一致）。
///
/// `starts_with("/health")` のような前方一致は readiness (`/health/ready`)
/// まで誤って除外してしまうため使わない。readiness (`/health/ready`,
/// `/global/ready`) は DB に触れるため認証対象のままにする。
const UNAUTHENTICATED_LIVENESS_PATHS: &[&str] = &["/health", "/health/live", "/global/health"];

/// クエリ文字列を除いたパスが認証不要の liveness プローブかどうかを判定する。
///
/// トークン値は一切扱わない。呼び出し側の `Uri::path()` は既にクエリを除くが、
/// 完全な request target が渡されても安全なようにここでも防御的に除去する。
fn is_unauthenticated_liveness_path(path: &str) -> bool {
    let path_only = path.split('?').next().unwrap_or(path);
    UNAUTHENTICATED_LIVENESS_PATHS.contains(&path_only)
}

async fn auth_middleware(
    state: Arc<ServerState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // CLI/TUI の起動待ちは Bearer を送らず `/health` を叩く。既定（認証有効）
    // でここを通すと 401 になりタイムアウトするため liveness だけ素通しする。
    if is_unauthenticated_liveness_path(request.uri().path()) {
        return next.run(request).await;
    }

    let supplied_bearer = request
        .headers()
        .get(HeaderName::from_static("authorization"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let client_ip = client_ip_from_request(&request);
    let decision = auth_decision(
        state.auth_enabled,
        state.auth_token.as_deref(),
        supplied_bearer,
    );

    match decision {
        AuthDecision::Pass => next.run(request).await,
        AuthDecision::Unauthorized => {
            // 送信元 IP を構造化フィールドとして残し、トークン値は決して出さない。
            match client_ip {
                Some(ip) => tracing::warn!(
                    client_ip = %ip,
                    "Authentication failed: invalid or missing token"
                ),
                None => tracing::warn!("Authentication failed: invalid or missing token"),
            }
            ApiError::Unauthorized("Invalid or missing authentication token".to_owned())
                .into_response()
        }
        AuthDecision::Misconfigured => {
            // 認証が有効なのにトークンが未設定なら全リクエストを素通しさせず
            // 500 で拒否する（設定不変条件違反）。トークン値は出力しない。
            tracing::error!(
                "authentication is enabled but no auth token is configured; refusing request"
            );
            ApiError::InternalError(
                "Authentication is enabled but no token is configured".to_owned(),
            )
            .into_response()
        }
    }
}

/// Redact an auth token for logging: never emit any part of the secret.
///
/// 先頭 prefix や長さを出すと、短いトークンや低エントロピーのトークンが
/// ログから復元できてしまう。入力値によらず固定のマスクのみを返す。
fn redact_token(_token: &str) -> String {
    "[REDACTED]".to_string()
}

pub async fn run_server(addr: SocketAddr) -> anyhow::Result<()> {
    let state = Arc::new(ServerState::new_with_storage_for_url(format!("http://{}", addr)).await?);
    routes::resume_queued_prompts(state.clone()).await?;

    if state.auth_enabled {
        if let Some(token) = &state.auth_token {
            tracing::info!("Authentication enabled. Token: {}", redact_token(token));
            tracing::info!("Set Authorization: Bearer <token> header for all requests");
        }
    }
    // 認証無効化の警告は CLI 側（run_server_command）に一本化している。

    let app = routes::router()
        .layer(middleware::from_fn_with_state(
            state.clone(),
            |State(state): State<Arc<ServerState>>,
             request: Request<axum::body::Body>,
             next: Next| async move { auth_middleware(state, request, next).await },
        ))
        .layer(state.cors_layer())
        .layer(TraceLayer::new_for_http().on_response(RequestCompletionLog))
        .layer(middleware::from_fn(request_span_middleware))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("Server listening on {}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

pub async fn run_server_with_state(
    addr: SocketAddr,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    routes::resume_queued_prompts(state.clone()).await?;
    if state.auth_enabled {
        if let Some(token) = &state.auth_token {
            tracing::info!("Authentication enabled. Token: {}", redact_token(token));
        }
    }
    // 認証無効化の警告は CLI 側（run_server_command）に一本化している。

    let app = routes::router()
        .layer(middleware::from_fn_with_state(
            state.clone(),
            |State(state): State<Arc<ServerState>>,
             request: Request<axum::body::Body>,
             next: Next| async move { auth_middleware(state, request, next).await },
        ))
        .layer(state.cors_layer())
        .layer(TraceLayer::new_for_http().on_response(RequestCompletionLog))
        .layer(middleware::from_fn(request_span_middleware))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("Server listening on {}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn convert_config_providers_reads_both_spellings_and_normalizes() {
        // テスト専用のダミー値。ログ・エラーへは出さない。
        let config = AppConfig {
            provider: Some(HashMap::from([(
                "legacy".to_string(),
                opencode_config::schema::ProviderConfig {
                    api_key: Some("dummy-legacy-key".to_string()),
                    ..Default::default()
                },
            )])),
            providers: Some(HashMap::from([
                (
                    "added_by_patch".to_string(),
                    opencode_config::schema::ProviderConfig {
                        settings: Some(HashMap::from([(
                            "apiKey".to_string(),
                            serde_json::json!("dummy-patched-key"),
                        )])),
                        package: Some("@ai-sdk/openai-compatible".to_string()),
                        base_url: Some("https://example.test/v1".to_string()),
                        ..Default::default()
                    },
                ),
                (
                    "legacy".to_string(),
                    opencode_config::schema::ProviderConfig {
                        api_key: Some("dummy-shadowed-key".to_string()),
                        ..Default::default()
                    },
                ),
            ])),
            ..Default::default()
        };

        let converted = convert_config_providers_for_bootstrap(&config);
        assert!(converted.contains_key("added_by_patch"));

        // 同一 id の衝突では legacy `provider` map が優先される（loader と同規則）。
        let legacy = converted.get("legacy").expect("legacy provider");
        let legacy_options = legacy.options.as_ref().expect("legacy options");
        assert_eq!(legacy_options["apiKey"], "dummy-legacy-key");

        // current-schema の settings.apiKey / baseURL / package が
        // loader の normalize_provider_config と同じく平坦化される。
        let added = converted.get("added_by_patch").expect("patched provider");
        assert_eq!(added.npm.as_deref(), Some("@ai-sdk/openai-compatible"));
        let options = added.options.as_ref().expect("patched options");
        assert_eq!(options["apiKey"], "dummy-patched-key");
        assert_eq!(options["baseURL"], "https://example.test/v1");
    }

    #[test]
    fn session_id_from_path_extracts_only_session_routes() {
        assert_eq!(
            session_id_from_path("/session/abc-123/message"),
            Some("abc-123")
        );
        assert_eq!(session_id_from_path("/session/abc-123"), Some("abc-123"));
        assert_eq!(session_id_from_path("/session"), None);
        assert_eq!(session_id_from_path("/health"), None);
        assert_eq!(session_id_from_path("/session//message"), None);
    }

    #[test]
    fn session_id_from_path_extracts_api_alias_session_routes() {
        assert_eq!(
            session_id_from_path("/api/session/abc-123/message"),
            Some("abc-123")
        );
        assert_eq!(
            session_id_from_path("/api/session/abc-123"),
            Some("abc-123")
        );
        assert_eq!(session_id_from_path("/api/session"), None);
        assert_eq!(session_id_from_path("/api/session//message"), None);
        assert_eq!(session_id_from_path("/api/health"), None);
    }

    #[test]
    fn sanitize_request_id_removes_controls_and_bounds_length() {
        let long_id = format!("req-{}-id", "x".repeat(200));
        let with_controls = format!("  {}\n\r\u{0000}  ", long_id);
        let sanitized = sanitize_request_id(&with_controls).expect("non-empty request id");
        assert_eq!(sanitized.chars().count(), MAX_REQUEST_ID_LEN);
        assert!(sanitized.chars().all(|ch| !ch.is_control()));
        assert_eq!(
            sanitized,
            long_id.chars().take(MAX_REQUEST_ID_LEN).collect::<String>()
        );

        assert_eq!(
            sanitize_request_id("ab\ncd\tef"),
            Some("abcdef".to_string())
        );
        assert_eq!(
            sanitize_request_id("plain-id"),
            Some("plain-id".to_string())
        );
        assert_eq!(sanitize_request_id("  "), None);
        assert_eq!(sanitize_request_id("\n\r\u{0000}"), None);
    }

    #[test]
    fn parse_no_auth_disables_auth_only_for_explicit_true_values() {
        // 明示的な無効化値（大小文字無視・前後空白許容）のみ true。
        let disabling: [Option<&str>; 8] = [
            Some("1"),
            Some("true"),
            Some("yes"),
            Some("TRUE"),
            Some("YES"),
            Some("True"),
            Some(" true "),
            Some("\tyes\n"),
        ];
        for value in disabling {
            assert!(
                parse_no_auth(value),
                "explicit NO_AUTH value must disable auth: {value:?}"
            );
        }
    }

    #[test]
    fn parse_no_auth_keeps_auth_enabled_for_all_other_values() {
        // 未設定・空・0/false・不正値は fail-closed（認証有効）。
        let not_disabling: [Option<&str>; 9] = [
            None,
            Some(""),
            Some("  "),
            Some("0"),
            Some("false"),
            Some("FALSE"),
            Some("no"),
            Some("garbage"),
            Some("2"),
        ];
        for value in not_disabling {
            assert!(
                !parse_no_auth(value),
                "non-explicit NO_AUTH value must keep auth enabled: {value:?}"
            );
        }
    }

    /// 環境変数を書き換えるテストを直列化するためのロック。
    static AUTH_ENV_LOCK: StdMutex<()> = StdMutex::new(());

    /// テスト終了時に `OPENCODE_NO_AUTH` を元の状態へ戻すガード。
    struct RestoreNoAuth(Option<String>);

    impl Drop for RestoreNoAuth {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("OPENCODE_NO_AUTH", value),
                None => std::env::remove_var("OPENCODE_NO_AUTH"),
            }
        }
    }

    #[test]
    fn auth_enabled_from_env_matches_parsed_no_auth_value() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());

        // 未設定
        std::env::remove_var("OPENCODE_NO_AUTH");
        assert!(
            auth_enabled_from_env(),
            "unset OPENCODE_NO_AUTH must keep auth enabled"
        );

        let cases: [(&str, bool); 9] = [
            ("1", false),
            ("true", false),
            ("yes", false),
            ("TRUE", false),
            ("YES", false),
            ("0", true),
            ("false", true),
            ("", true),
            ("garbage", true),
        ];
        for (value, expected_enabled) in cases {
            std::env::set_var("OPENCODE_NO_AUTH", value);
            assert_eq!(
                auth_enabled_from_env(),
                expected_enabled,
                "OPENCODE_NO_AUTH={value:?} should yield auth_enabled_from_env={expected_enabled}"
            );
        }
    }

    fn state_with_repos(
        session_repo: SessionRepository,
        message_repo: MessageRepository,
    ) -> ServerState {
        let mut state = ServerState::new();
        state.session_repo = Some(session_repo);
        state.message_repo = Some(message_repo);
        state
    }

    #[tokio::test]
    async fn storage_roundtrip_restores_sessions_and_messages() {
        let db = Database::in_memory()
            .await
            .expect("in-memory db should initialize");
        let pool = db.pool().clone();

        let state = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool.clone()),
        );
        let (session_id, user_created_at, assistant_created_at) = {
            let mut manager = state.sessions.lock().await;
            let session = manager.create("default", ".");
            let session_id = session.id.clone();

            let fixed_user_time = chrono::Utc
                .timestamp_millis_opt(1_700_000_000_000)
                .single()
                .expect("valid user timestamp");
            let fixed_assistant_time = chrono::Utc
                .timestamp_millis_opt(1_700_000_000_123)
                .single()
                .expect("valid assistant timestamp");

            let session = manager
                .get_mut(&session_id)
                .expect("session should be available for mutation");
            let user = session.add_user_message("hello");
            user.created_at = fixed_user_time;
            if let Some(part) = user.parts.first_mut() {
                part.created_at = fixed_user_time;
            }

            let assistant = session.add_assistant_message();
            assistant.created_at = fixed_assistant_time;
            assistant.add_text("world");
            if let Some(part) = assistant.parts.first_mut() {
                part.created_at = fixed_assistant_time;
            }

            // storage に保存された summary が復元時に破棄されないことを検証するため、
            // 既存の roundtrip テストに summary を乗せる。
            session.set_summary(opencode_session::SessionSummary {
                additions: 5,
                deletions: 3,
                files: 2,
                diffs: Some(vec![opencode_session::FileDiff {
                    path: "src/main.rs".to_string(),
                    additions: 5,
                    deletions: 3,
                }]),
            });

            (session_id, fixed_user_time, fixed_assistant_time)
        };

        state
            .sync_sessions_to_storage()
            .await
            .expect("session snapshot should sync to storage");

        let reloaded = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool),
        );
        reloaded
            .load_sessions_from_storage()
            .await
            .expect("sessions should reload from storage");

        let manager = reloaded.sessions.lock().await;
        let session = manager
            .get(&session_id)
            .expect("session should exist after reload");
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].created_at, user_created_at);
        assert_eq!(session.messages[1].created_at, assistant_created_at);
        assert_eq!(session.messages[0].get_text(), "hello");
        assert_eq!(session.messages[1].get_text(), "world");

        // stored.summary が Some のとき server 側 Session.summary も Some になること。
        let summary = session
            .summary
            .as_ref()
            .expect("summary should survive storage roundtrip");
        assert_eq!(summary.additions, 5);
        assert_eq!(summary.deletions, 3);
        assert_eq!(summary.files, 2);
        let diffs = summary.diffs.as_ref().expect("diffs should be restored");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "src/main.rs");
        assert_eq!(diffs[0].additions, 5);
        assert_eq!(diffs[0].deletions, 3);
    }

    #[tokio::test]
    async fn storage_restore_keeps_persisted_total_cost() {
        // 復元時に input/output のトークン数を total_cost へ持ち込まず、
        // 永続化済みの実コストだけを復元することを検証する回帰テスト。
        let db = Database::in_memory()
            .await
            .expect("in-memory db should initialize");
        let pool = db.pool().clone();
        let session_repo = SessionRepository::new(pool.clone());

        let state = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool.clone()),
        );
        let session_id = {
            let mut manager = state.sessions.lock().await;
            manager.create("default", ".").id
        };

        state
            .sync_sessions_to_storage()
            .await
            .expect("initial snapshot should sync");

        // 永続化表現 (opencode_types::TokenUsage) に非ゼロのトークン数を設定する。
        // opencode_session::SessionUsage とフィールド名が異なるため、
        // リポジトリ経由で直接書き込む (実ユーザー DB は使わない)。
        {
            let mut stored = session_repo
                .get(&session_id)
                .await
                .expect("get should succeed")
                .expect("stored session should exist");
            stored.usage = Some(opencode_types::TokenUsage {
                input: 150.0,
                output: 75.0,
                reasoning: 0.0,
                cache: opencode_types::CacheUsage {
                    read: 0.0,
                    write: 0.0,
                },
                total_cost: 0.125,
            });
            session_repo
                .update(&stored)
                .await
                .expect("update should succeed");
        }

        let reloaded = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool),
        );
        reloaded
            .load_sessions_from_storage()
            .await
            .expect("sessions should reload from storage");

        let manager = reloaded.sessions.lock().await;
        let session = manager
            .get(&session_id)
            .expect("session should exist after reload");
        let usage = session.usage.as_ref().expect("usage should be restored");
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(
            usage.total_cost, 0.125,
            "トークン数ではなく実コストが復元されること"
        );
    }

    /// 起動時復元で保存済み session.metadata が空 HashMap に上書きされないこと。
    #[tokio::test]
    async fn storage_restore_keeps_session_metadata() {
        let db = Database::in_memory()
            .await
            .expect("in-memory db should initialize");
        let pool = db.pool().clone();
        let state = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool.clone()),
        );

        let session_id = {
            let mut manager = state.sessions.lock().await;
            let id = manager.create("default", ".").id.clone();
            let session = manager.get_mut(&id).expect("session available");
            session
                .metadata
                .insert("source".to_string(), serde_json::json!("startup-restore"));
            session
                .metadata
                .insert("priority".to_string(), serde_json::json!(7));
            id
        };

        state
            .sync_sessions_to_storage()
            .await
            .expect("session snapshot should sync");

        // 別リポジトリインスタンス（= DB 読み出し）でも metadata が読めること。
        let stored = SessionRepository::new(pool.clone())
            .get(&session_id)
            .await
            .expect("get works")
            .expect("stored session exists");
        assert_eq!(
            stored.metadata.get("source"),
            Some(&serde_json::json!("startup-restore"))
        );

        let reloaded = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool),
        );
        reloaded
            .load_sessions_from_storage()
            .await
            .expect("sessions should reload from storage");

        let manager = reloaded.sessions.lock().await;
        let session = manager
            .get(&session_id)
            .expect("session should exist after reload");
        assert_eq!(
            session.metadata.get("source"),
            Some(&serde_json::json!("startup-restore")),
            "起動時復元で metadata が失われないこと"
        );
        assert_eq!(
            session.metadata.get("priority"),
            Some(&serde_json::json!(7))
        );
    }

    #[tokio::test]
    async fn sync_removes_deleted_sessions_from_storage() {
        let db = Database::in_memory()
            .await
            .expect("in-memory db should initialize");
        let pool = db.pool().clone();
        let session_repo = SessionRepository::new(pool.clone());

        let state = state_with_repos(
            SessionRepository::new(pool.clone()),
            MessageRepository::new(pool),
        );
        let session_id = {
            let mut manager = state.sessions.lock().await;
            manager.create("default", ".").id
        };

        state
            .sync_sessions_to_storage()
            .await
            .expect("initial snapshot should sync");
        assert_eq!(
            session_repo
                .list(None, 10)
                .await
                .expect("list should succeed")
                .len(),
            1
        );

        {
            let mut manager = state.sessions.lock().await;
            manager.delete(&session_id);
        }

        state
            .sync_sessions_to_storage()
            .await
            .expect("delete sync should succeed");
        assert!(session_repo
            .get(&session_id)
            .await
            .expect("get should succeed")
            .is_none());
    }

    fn question_defs() -> Vec<opencode_tool::QuestionDef> {
        vec![opencode_tool::QuestionDef {
            question: "Continue?".to_string(),
            header: Some("Confirm".to_string()),
            options: vec![opencode_tool::QuestionOption {
                label: "Yes".to_string(),
                description: None,
            }],
            multiple: false,
        }]
    }

    async fn next_question_event(rx: &mut broadcast::Receiver<String>) -> serde_json::Value {
        let event = rx.recv().await.expect("broker should broadcast an event");
        serde_json::from_str(&event).expect("broadcast payload should be valid JSON")
    }

    #[tokio::test]
    async fn question_broker_ask_resolves_with_reply_answers() {
        let (tx, mut rx) = broadcast::channel(16);
        let broker = Arc::new(QuestionBroker::new(tx));
        let info = QuestionRequestInfo {
            id: "qst_1".to_string(),
            session_id: "ses_1".to_string(),
            questions: question_defs(),
        };

        let ask_broker = broker.clone();
        let ask_task = tokio::spawn(async move { ask_broker.ask(info).await });

        let event = next_question_event(&mut rx).await;
        assert_eq!(event["type"], "question.asked");
        assert_eq!(event["request"]["id"], "qst_1");
        assert_eq!(event["request"]["session_id"], "ses_1");

        let pending = broker.list().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].questions[0].question, "Continue?");

        assert!(broker.reply("qst_1", vec![vec!["Yes".to_string()]]).await);

        match ask_task.await.expect("ask task should join") {
            QuestionResponse::Answered(answers) => {
                assert_eq!(answers, vec![vec!["Yes".to_string()]]);
            }
            other => panic!("expected answered response, got {:?}", other),
        }

        let event = next_question_event(&mut rx).await;
        assert_eq!(event["type"], "question.replied");
        assert_eq!(event["requestID"], "qst_1");
        assert_eq!(event["sessionID"], "ses_1");
        assert!(broker.list().await.is_empty());
    }

    #[tokio::test]
    async fn question_broker_reject_resolves_with_rejection() {
        let (tx, mut rx) = broadcast::channel(16);
        let broker = Arc::new(QuestionBroker::new(tx));
        let info = QuestionRequestInfo {
            id: "qst_2".to_string(),
            session_id: "ses_2".to_string(),
            questions: question_defs(),
        };

        let ask_broker = broker.clone();
        let ask_task = tokio::spawn(async move { ask_broker.ask(info).await });

        let event = next_question_event(&mut rx).await;
        assert_eq!(event["type"], "question.asked");

        assert!(broker.reject("qst_2").await);
        assert!(matches!(
            ask_task.await.expect("ask task should join"),
            QuestionResponse::Rejected
        ));

        let event = next_question_event(&mut rx).await;
        assert_eq!(event["type"], "question.rejected");
        assert_eq!(event["requestID"], "qst_2");
        assert!(broker.list().await.is_empty());
    }

    #[tokio::test]
    async fn question_broker_unknown_ids_return_false() {
        let (tx, _rx) = broadcast::channel(16);
        let broker = QuestionBroker::new(tx);

        assert!(!broker.reply("missing", Vec::new()).await);
        assert!(!broker.reject("missing").await);
        assert!(broker.list().await.is_empty());
    }

    /// 非同期条件が満たされるまで待つテストヘルパ（タイムアウトで失敗させる）。
    async fn wait_until<F, Fut>(mut predicate: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !predicate().await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition should be satisfied within timeout");
    }

    fn permission_info(id: &str) -> PermissionRequestInfo {
        PermissionRequestInfo {
            id: id.to_string(),
            session_id: "ses_test".to_string(),
            tool: "bash".to_string(),
            input: serde_json::json!({}),
            message: "test permission".to_string(),
            permission_type: "bash".to_string(),
            patterns: Vec::new(),
            metadata: HashMap::new(),
            always: Vec::new(),
        }
    }

    fn question_info(id: &str) -> QuestionRequestInfo {
        QuestionRequestInfo {
            id: id.to_string(),
            session_id: "ses_test".to_string(),
            questions: question_defs(),
        }
    }

    #[tokio::test]
    async fn permission_broker_removes_pending_when_ask_task_is_aborted() {
        let (tx, _rx) = broadcast::channel(16);
        let broker = Arc::new(PermissionBroker::new(tx));
        let ask_broker = broker.clone();
        let handle =
            tokio::spawn(async move { ask_broker.ask(permission_info("per_aborted")).await });

        wait_until(|| async { !broker.list().await.is_empty() }).await;
        assert_eq!(broker.list().await.len(), 1);

        handle.abort();
        let _ = handle.await;

        // Drop ガードにより pending map 自体から削除されていること。
        wait_until(|| async { broker.pending.lock().unwrap().is_empty() }).await;
        assert!(broker.list().await.is_empty());
    }

    #[tokio::test]
    async fn question_broker_removes_pending_when_ask_task_is_aborted() {
        let (tx, _rx) = broadcast::channel(16);
        let broker = Arc::new(QuestionBroker::new(tx));
        let ask_broker = broker.clone();
        let handle =
            tokio::spawn(async move { ask_broker.ask(question_info("qst_aborted")).await });

        wait_until(|| async { !broker.list().await.is_empty() }).await;
        assert_eq!(broker.list().await.len(), 1);

        handle.abort();
        let _ = handle.await;

        wait_until(|| async { broker.pending.lock().unwrap().is_empty() }).await;
        assert!(broker.list().await.is_empty());
    }

    #[tokio::test]
    async fn permission_broker_reply_returns_false_when_receiver_is_dropped() {
        let (tx, _rx) = broadcast::channel(16);
        let broker = PermissionBroker::new(tx);
        let info = permission_info("per_dead");
        let (sender, receiver) = oneshot::channel::<PermissionResponse>();
        drop(receiver);
        broker.pending.lock().unwrap().insert(
            info.id.clone(),
            PendingPermission {
                info,
                responder: sender,
            },
        );

        // 受け手消失エントリは list() に露出しない（防御レイヤ）。
        assert!(broker.list().await.is_empty());
        // send() の Err を検知して false を返す（false success の回帰）。
        assert!(!broker.reply("per_dead", PermissionResponse::Once).await);
        assert!(broker.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn question_broker_reply_and_reject_return_false_when_receiver_is_dropped() {
        for id in ["qst_dead_reply", "qst_dead_reject"] {
            let (tx, _rx) = broadcast::channel(16);
            let broker = QuestionBroker::new(tx);
            let info = question_info(id);
            let (sender, receiver) = oneshot::channel::<QuestionResponse>();
            drop(receiver);
            broker.pending.lock().unwrap().insert(
                info.id.clone(),
                PendingQuestion {
                    info,
                    responder: sender,
                },
            );

            assert!(broker.list().await.is_empty());
            let delivered = if id == "qst_dead_reply" {
                broker.reply(id, vec![vec!["Yes".to_string()]]).await
            } else {
                broker.reject(id).await
            };
            assert!(!delivered, "send to a dropped receiver must report failure");
            assert!(broker.pending.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn list_excludes_only_closed_pending_entries() {
        let (tx, _rx) = broadcast::channel(16);
        let broker = QuestionBroker::new(tx);
        let live = question_info("qst_live");
        let dead = question_info("qst_dead_list");
        let (live_sender, _live_receiver) = oneshot::channel::<QuestionResponse>();
        let (dead_sender, dead_receiver) = oneshot::channel::<QuestionResponse>();
        drop(dead_receiver);
        {
            let mut map = broker.pending.lock().unwrap();
            map.insert(
                live.id.clone(),
                PendingQuestion {
                    info: live,
                    responder: live_sender,
                },
            );
            map.insert(
                dead.id.clone(),
                PendingQuestion {
                    info: dead,
                    responder: dead_sender,
                },
            );
        }

        let listed = broker.list().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "qst_live");
    }

    #[test]
    fn redact_token_hides_secret_body() {
        let token = "abcd1234secret-secret-secret";
        let redacted = redact_token(token);
        assert_eq!(redacted, "[REDACTED]");
        assert!(!redacted.contains("abcd"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("28")); // 長さも漏らさない
    }

    #[test]
    fn redact_token_handles_short_tokens() {
        // 短い入力でも元値を返さない（従来の prefix 表示は廃止）。
        assert_eq!(redact_token("abc"), "[REDACTED]");
        assert_eq!(redact_token("x"), "[REDACTED]");
        assert_eq!(redact_token(""), "[REDACTED]");
    }

    #[tokio::test]
    async fn config_is_isolated_between_server_states() {
        let first = ServerState::new();
        let second = ServerState::new();

        {
            let mut config = first.config.write().await;
            config.model = Some("first-only-model".to_string());
        }
        assert_eq!(
            first.config.read().await.model.as_deref(),
            Some("first-only-model")
        );
        assert_eq!(second.config.read().await.model.as_deref(), None);

        {
            let mut config = second.config.write().await;
            config.model = Some("second-only-model".to_string());
        }
        assert_eq!(
            first.config.read().await.model.as_deref(),
            Some("first-only-model")
        );
        assert_eq!(
            second.config.read().await.model.as_deref(),
            Some("second-only-model")
        );
    }

    #[tokio::test]
    async fn tui_queues_are_isolated_between_server_states() {
        let first = ServerState::new();
        let second = ServerState::new();

        first
            .tui_request_queue
            .lock()
            .await
            .push_back(routes::TuiRequest {
                path: "/tui/first-only".to_string(),
                body: serde_json::json!({"text": "first"}),
            });
        first
            .tui_response_queue
            .lock()
            .await
            .push_back(serde_json::json!({"reply": "first"}));

        assert_eq!(first.tui_request_queue.lock().await.len(), 1);
        assert_eq!(
            first.tui_request_queue.lock().await[0].path,
            "/tui/first-only"
        );
        assert_eq!(first.tui_response_queue.lock().await.len(), 1);
        assert!(second.tui_request_queue.lock().await.is_empty());
        assert!(second.tui_response_queue.lock().await.is_empty());

        second
            .tui_request_queue
            .lock()
            .await
            .push_back(routes::TuiRequest {
                path: "/tui/second-only".to_string(),
                body: serde_json::json!({"text": "second"}),
            });
        assert_eq!(first.tui_request_queue.lock().await.len(), 1);
        assert_eq!(second.tui_request_queue.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn session_run_state_and_prompt_locks_are_isolated_between_server_states() {
        let first = ServerState::new();
        let second = ServerState::new();

        first
            .session_run_status
            .write()
            .await
            .insert("session-a".to_string(), routes::SessionRunStatus::Busy);
        first
            .prompt_locks
            .write()
            .await
            .insert("session-a".to_string(), Arc::new(Mutex::new(())));

        assert!(first
            .session_run_status
            .read()
            .await
            .contains_key("session-a"));
        assert!(first.prompt_locks.read().await.contains_key("session-a"));
        assert!(second.session_run_status.read().await.is_empty());
        assert!(second.prompt_locks.read().await.is_empty());
    }

    #[tokio::test]
    async fn todo_manager_is_isolated_between_server_states() {
        let first = ServerState::new();
        let second = ServerState::new();

        first
            .todo_manager
            .update(
                "session-a",
                vec![opencode_session::TodoInfo {
                    content: "first-only".to_string(),
                    status: "pending".to_string(),
                    priority: "high".to_string(),
                }],
            )
            .await;

        let first_todos = first.todo_manager.get("session-a").await;
        let second_todos = second.todo_manager.get("session-a").await;
        assert_eq!(first_todos.len(), 1);
        assert_eq!(first_todos[0].content, "first-only");
        assert!(second_todos.is_empty());
    }

    #[tokio::test]
    async fn pty_manager_is_isolated_between_server_states() {
        let first = ServerState::new();
        let second = ServerState::new();

        assert!(!std::ptr::eq(&first.pty_manager, &second.pty_manager));
        assert!(first.pty_manager.list_sessions().await.is_empty());
        assert!(second.pty_manager.list_sessions().await.is_empty());
    }

    #[test]
    fn cors_allows_loopback_origins_with_and_without_explicit_port() {
        let state = ServerState::new();
        // L-17: ブラウザは既定ポートを省略するため、port なしの localhost Origin も許可する。
        // 許可範囲は loopback ホスト + http/https に限定したまま拡張する。
        for origin in [
            "http://localhost",
            "http://localhost/",
            "http://localhost:80",
            "http://localhost:3000",
            "https://localhost",
            "https://localhost:443",
            "https://localhost:8443",
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://[::1]",
            "http://[::1]:3000",
        ] {
            assert!(
                state.is_allowed_origin(origin),
                "loopback origin should be allowed: {origin}"
            );
        }
    }

    #[test]
    fn cors_rejects_remote_hosts_and_non_http_schemes() {
        let state = ServerState::new();
        // セキュリティ境界: localhost 以外は port の有無にかかわらず決して許可しない。
        for origin in [
            "http://example.com",
            "http://example.com:3000",
            "https://example.com",
            "http://localhost.example.com",
            "http://localhost.example.com:3000",
            "http://127.0.0.1.example.com",
            "ftp://localhost",
        ] {
            assert!(
                !state.is_allowed_origin(origin),
                "remote/non-http origin must be rejected: {origin}"
            );
        }
    }

    #[test]
    fn cors_preserves_existing_tauri_and_opencode_ai_allowlist() {
        let state = ServerState::new();
        assert!(state.is_allowed_origin("tauri://localhost"));
        assert!(state.is_allowed_origin("http://tauri.localhost"));
        assert!(state.is_allowed_origin("https://opencode.ai"));
        assert!(state.is_allowed_origin("https://www.opencode.ai"));
        assert!(!state.is_allowed_origin("http://opencode.ai"));
    }

    #[test]
    fn cors_whitelist_is_isolated_between_server_states() {
        let first = ServerState::new();
        let second = ServerState::new();
        let first_origin = "https://first.example.com";
        let second_origin = "https://second.example.com";

        assert!(!first.is_allowed_origin(first_origin));
        assert!(!second.is_allowed_origin(first_origin));

        // 末尾スラッシュ付きで設定しても正規化されて完全一致する。
        first.set_cors_whitelist(vec![format!("{first_origin}/"), "   ".to_string()]);

        assert!(first.is_extra_allowed_origin_exact(first_origin));
        assert!(first.is_allowed_origin(first_origin));
        assert!(!second.is_extra_allowed_origin_exact(first_origin));
        assert!(!second.is_allowed_origin(first_origin));

        // 逆方向の変更も波及せず、既に設定したエントリも維持される。
        second.set_cors_whitelist(vec![second_origin.to_string()]);
        assert!(second.is_allowed_origin(second_origin));
        assert!(!first.is_allowed_origin(second_origin));
        assert!(first.is_allowed_origin(first_origin));
        assert!(!second.is_allowed_origin(first_origin));
    }

    // ---- R2 回帰テスト: CORS カスタムヘッダ / 401 スキーマ / fail-open ----
    // StatusCode は上位モジュールの import を `use super::*` で共有する。
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn cors_allow_headers_include_custom_headers_for_session_and_telemetry() {
        let headers = cors_allow_headers();
        let names: Vec<&str> = headers.iter().map(|h| h.as_str()).collect();
        assert!(names.contains(&"x-opencode-directory"));
        assert!(names.contains(&"x-request-id"));
        assert!(names.contains(&"authorization"));
        assert!(names.contains(&"content-type"));
        assert!(names.contains(&"accept"));
    }

    /// ループバックのテストサーバへ素の HTTP/1.1 リクエストを送り、
    /// (status, ヘッダ(小文字), body) を返す。
    ///
    /// tower が workspace から削除済みのため `ServiceExt::oneshot` は使えず、
    /// routes.rs の既存パターンに倣い tokio + axum のみで実ミドルウェアを通す。
    async fn http_exchange(addr: SocketAddr, raw_request: &str) -> (StatusCode, String, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = TcpStream::connect(addr)
                .await
                .expect("connect to loopback test server");
            stream
                .write_all(raw_request.as_bytes())
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
            let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
            (
                StatusCode::from_u16(status).expect("valid status"),
                head.to_ascii_lowercase(),
                body.to_string(),
            )
        })
        .await
        .expect("http exchange should not hang")
    }

    async fn start_test_server(app: axum::Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        // loopback bind 失敗時にテストをスキップすると false green になるため、
        // 失敗時は明示的に失敗させる（R2 回帰ガード）。
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed in tests; skipping hides failures");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            // 本番と同じく ConnectInfo を注入し、client_ip 経路もテストで通す。
            if let Err(err) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                eprintln!("test server stopped: {err}");
            }
        });
        (addr, server)
    }

    fn auth_test_app(state: Arc<ServerState>) -> axum::Router {
        axum::Router::new()
            .route("/protected", axum::routing::get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(
                state,
                |State(state): State<Arc<ServerState>>,
                 request: Request<axum::body::Body>,
                 next: Next| async move { auth_middleware(state, request, next).await },
            ))
    }

    /// liveness / readiness / 通常ルートを同じ middleware 下に並べた検証用ルータ。
    fn auth_liveness_test_app(state: Arc<ServerState>) -> axum::Router {
        axum::Router::new()
            .route("/health", axum::routing::get(|| async { "liveness" }))
            .route("/health/live", axum::routing::get(|| async { "liveness" }))
            .route(
                "/health/ready",
                axum::routing::get(|| async { "readiness" }),
            )
            .route(
                "/global/health",
                axum::routing::get(|| async { "liveness" }),
            )
            .route(
                "/global/ready",
                axum::routing::get(|| async { "readiness" }),
            )
            .route("/protected", axum::routing::get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(
                state,
                |State(state): State<Arc<ServerState>>,
                 request: Request<axum::body::Body>,
                 next: Next| async move { auth_middleware(state, request, next).await },
            ))
    }

    fn parse_json_body(body: &str) -> serde_json::Value {
        // chunked 転送のフレームが付いても JSON オブジェクト部分だけを拾う。
        let start = body.find('{').expect("JSON object in body");
        let end = body.rfind('}').expect("JSON object in body");
        serde_json::from_str(&body[start..=end]).expect("valid JSON body")
    }

    #[tokio::test]
    async fn cors_preflight_allows_session_directory_and_request_id_headers() {
        // 設定そのものの検証は loopback 可否に依存させない（必ず走るガード）。
        cors_allow_headers_include_custom_headers_for_session_and_telemetry();

        let state = Arc::new(ServerState::new());
        let app = axum::Router::new()
            .route("/session", axum::routing::get(|| async { "ok" }))
            .layer(state.cors_layer());
        let (addr, server) = start_test_server(app).await;

        let request = concat!(
            "OPTIONS /session HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "Origin: http://localhost:3000\r\n",
            "Access-Control-Request-Method: POST\r\n",
            "Access-Control-Request-Headers: x-opencode-directory, x-request-id, content-type\r\n",
            "Connection: close\r\n",
            "\r\n"
        );
        let (status, headers, _) = http_exchange(addr, request).await;
        assert_eq!(status, StatusCode::OK, "preflight should be permitted");
        let allow_headers = headers
            .lines()
            .find_map(|line| line.strip_prefix("access-control-allow-headers:"))
            .expect("preflight must return Access-Control-Allow-Headers")
            .trim()
            .to_ascii_lowercase();
        assert!(
            allow_headers.contains("x-opencode-directory"),
            "cross-origin session create must be allowed: {allow_headers}"
        );
        assert!(
            allow_headers.contains("x-request-id"),
            "correlation header must be allowed: {allow_headers}"
        );
        assert!(allow_headers.contains("content-type"));
        server.abort();
    }

    #[test]
    fn auth_decision_fails_closed_when_auth_enabled_but_token_missing() {
        assert_eq!(
            auth_decision(true, None, Some("candidate")),
            AuthDecision::Misconfigured
        );
        assert_eq!(auth_decision(true, None, None), AuthDecision::Misconfigured);
        // 明示的な認証無効化は既存どおり素通し（後方互換）。
        assert_eq!(auth_decision(false, None, None), AuthDecision::Pass);
        assert_eq!(auth_decision(false, Some("t"), None), AuthDecision::Pass);
    }

    #[test]
    fn auth_decision_accepts_only_exact_bearer_token() {
        assert_eq!(
            auth_decision(true, Some("test-token-abc123"), Some("test-token-abc123")),
            AuthDecision::Pass
        );
        assert_eq!(
            auth_decision(true, Some("test-token-abc123"), None),
            AuthDecision::Unauthorized
        );
        assert_eq!(
            auth_decision(true, Some("test-token-abc123"), Some("wrong-token")),
            AuthDecision::Unauthorized
        );
    }

    #[test]
    fn constant_time_eq_accepts_only_identical_bytes() {
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq("same-token", "same-token"));
        // 途中の 1 バイト差と末尾の 1 バイト差はともに拒否する。
        assert!(!constant_time_eq("abcdef", "abXdef"));
        assert!(!constant_time_eq("abcdef", "abcdeg"));
        // 長さ違い（共通接頭辞あり／なし、前後逆）も拒否する。
        assert!(!constant_time_eq("abcdef", "abcde"));
        assert!(!constant_time_eq("abcde", "abcdef"));
        assert!(!constant_time_eq("", "x"));
        assert!(!constant_time_eq("x", ""));
        // auth_decision の分岐も同じ比較を使うことを固定する。
        assert_eq!(
            auth_decision(true, Some("abcdef"), Some("abcde")),
            AuthDecision::Unauthorized
        );
        assert_eq!(
            auth_decision(true, Some("abcdef"), Some("abcdef")),
            AuthDecision::Pass
        );
    }

    #[test]
    fn constant_time_eq_is_correct_for_all_mismatch_positions_and_lengths() {
        // 戻り値のブラックボックス検証。不一致位置・入力長の全域で正確であることを
        // 確認する。なお戻り値だけでは定数時間性（短絡の有無）は検証できない:
        // `a == b` や早期 return 付きループでも戻り値は同じだからである。
        // タイミング側チャネル耐性は実装（`XOR` アキュムレータと最大長までの走査）
        // と clippy/レビューで担保し、実時間計測は環境依存のため行わない。
        // 比較対象は固定のテスト文字列（実トークンではない）であり、値そのものは
        // ログやアサーション失敗メッセージに出さない。
        let expected = format!("ct-{}", "abc123XYZ".repeat(8)); // 75 バイト

        // 完全一致のみ受け入れる。
        assert!(constant_time_eq(&expected, &expected));
        assert!(constant_time_eq("", ""));

        // 不一致位置を先頭から末尾までずらした単一バイト差は、どの位置でも
        // 同じく拒否されなければならない（早期 return の有無を問わず結果は同一）。
        for position in 0..expected.len() {
            let mut bytes = expected.as_bytes().to_vec();
            bytes[position] = if bytes[position] == b'a' { b'b' } else { b'a' };
            let candidate = String::from_utf8(bytes).expect("ASCII fixture must stay valid UTF-8");

            assert!(
                !constant_time_eq(&candidate, &expected),
                "mismatch at byte index {position} must be rejected"
            );
            assert!(
                !constant_time_eq(&expected, &candidate),
                "mismatch at byte index {position} must be rejected (reversed operands)"
            );
        }

        // 長さが異なる入力でも結果は同じく拒否される（共通接頭辞の長さに依存しない）。
        for len in 0..expected.len() {
            let shorter = &expected[..len];
            assert!(
                !constant_time_eq(shorter, &expected),
                "shorter input (len {len}) must be rejected"
            );
            assert!(
                !constant_time_eq(&expected, shorter),
                "shorter input (len {len}) must be rejected (reversed operands)"
            );
        }

        let mut appended = expected.clone();
        appended.push('!');
        let mut prepended = String::from("!");
        prepended.push_str(&expected);
        for longer in [&appended, &prepended] {
            assert!(
                !constant_time_eq(longer, &expected),
                "longer input must be rejected"
            );
            assert!(
                !constant_time_eq(&expected, longer),
                "longer input must be rejected (reversed operands)"
            );
        }

        // マルチバイト文字はバイト列として比較され、文字数が同じでも長さ差は拒否される。
        assert!(!constant_time_eq("あ", "a"));
        assert!(!constant_time_eq("a", "あ"));
        assert!(constant_time_eq("あ", "あ"));

        // 認証分岐も同じ比較結果に従う（先頭・末尾の不一致の両方を拒否）。
        let first_mismatch = format!("!{}", &expected[1..]);
        let last_mismatch = format!("{}!", &expected[..expected.len() - 1]);
        for supplied in [first_mismatch.as_str(), last_mismatch.as_str()] {
            assert_eq!(
                auth_decision(true, Some(expected.as_str()), Some(supplied)),
                AuthDecision::Unauthorized,
                "auth_decision must reject a token with a byte mismatch"
            );
        }
        assert_eq!(
            auth_decision(true, Some(expected.as_str()), Some(expected.as_str())),
            AuthDecision::Pass
        );
    }

    #[test]
    fn auth_enabled_is_frozen_at_server_state_construction() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());

        std::env::set_var("OPENCODE_NO_AUTH", "1");
        let disabled = ServerState::new();
        assert!(!disabled.auth_enabled);

        // 起動後の env 変更は既存 state の auth_enabled を書き換えない。
        std::env::set_var("OPENCODE_NO_AUTH", "0");
        assert!(!disabled.auth_enabled);
        assert_eq!(
            auth_decision(
                disabled.auth_enabled,
                disabled.auth_token.as_deref(),
                Some("bogus")
            ),
            AuthDecision::Pass,
            "an auth-disabled state must stay disabled after env changes"
        );

        std::env::set_var("OPENCODE_NO_AUTH", "0");
        let enabled = ServerState::new();
        assert!(enabled.auth_enabled);
        std::env::set_var("OPENCODE_NO_AUTH", "1");
        assert!(enabled.auth_enabled);
        assert_eq!(
            auth_decision(
                enabled.auth_enabled,
                enabled.auth_token.as_deref(),
                Some("bogus")
            ),
            AuthDecision::Unauthorized,
            "an auth-enabled state must stay enabled after env changes"
        );
    }

    #[test]
    fn liveness_paths_use_exact_match_and_ignore_query_strings() {
        for path in ["/health", "/health/live", "/global/health"] {
            assert!(
                is_unauthenticated_liveness_path(path),
                "documented liveness path must be exempt: {path}"
            );
            let with_query = format!("{path}?probe=cli&ts=1");
            assert!(
                is_unauthenticated_liveness_path(&with_query),
                "query string must not affect the exemption: {with_query}"
            );
        }

        for path in [
            "/health/ready",
            "/global/ready",
            "/health/live/extra",
            "/health/",
            "/HEALTH",
            "/protected",
        ] {
            assert!(
                !is_unauthenticated_liveness_path(path),
                "only exact liveness paths may be exempt: {path}"
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn auth_middleware_exempts_liveness_but_not_readiness() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());
        std::env::remove_var("OPENCODE_NO_AUTH"); // auth enabled

        let state = Arc::new(ServerState::new());
        let token = state.auth_token.clone().expect("generated token");
        let (addr, server) = start_test_server(auth_liveness_test_app(state)).await;

        // Bearer なしで liveness は 200。クエリ付きも同様（CLI 起動待ちの想定）。
        for target in [
            "/health",
            "/health?from=cli",
            "/health/live",
            "/health/live?from=cli",
            "/global/health",
            "/global/health?from=cli",
        ] {
            let request =
                format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let (status, _, body) = http_exchange(addr, &request).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "liveness must not require a token: {target}"
            );
            assert!(
                body.contains("liveness"),
                "unexpected liveness response for {target}: {body}"
            );
            assert!(
                !body.contains(&token),
                "token value must never appear in a response body"
            );
        }

        // readiness と通常ルートは従来どおり 401（前方一致で誤除外しないこと）。
        for target in ["/health/ready", "/global/ready", "/protected"] {
            let request =
                format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let (status, _, body) = http_exchange(addr, &request).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "readiness and protected routes must stay authenticated: {target}"
            );
            assert!(
                !body.contains(&token),
                "token value must never appear in an error body"
            );
        }
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn auth_middleware_uses_frozen_auth_enabled_not_live_env() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());
        let request = "GET /protected HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";

        // 起動時に無効 → 起動後に有効へ変更してもトークン無しで通る。
        std::env::set_var("OPENCODE_NO_AUTH", "1");
        let disabled_state = Arc::new(ServerState::new());
        std::env::set_var("OPENCODE_NO_AUTH", "0");
        let app = auth_test_app(disabled_state);
        let (addr, server) = start_test_server(app).await;
        let (status, _, _) = http_exchange(addr, request).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "startup-disabled auth must not be re-enabled by a later env change"
        );
        server.abort();

        // 起動時に有効 → 起動後に無効へ変更してもトークン無しは 401。
        std::env::set_var("OPENCODE_NO_AUTH", "0");
        let enabled_state = Arc::new(ServerState::new());
        std::env::set_var("OPENCODE_NO_AUTH", "1");
        let app = auth_test_app(enabled_state);
        let (addr, server) = start_test_server(app).await;
        let (status, _, _) = http_exchange(addr, request).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "startup-enabled auth must not be disabled by a later env change"
        );
        server.abort();
    }

    // current_thread ランタイムで実行され、ガードは環境変数の復元のためだけに
    // 短く保持する。他テストとの直列化が必要なため allow を局所的に付与する。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn auth_middleware_returns_common_error_schema_on_401() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());
        std::env::remove_var("OPENCODE_NO_AUTH"); // auth enabled

        let state = Arc::new(ServerState::new());
        let token = state.auth_token.clone().expect("generated token");
        let app = auth_test_app(state);
        let (addr, server) = start_test_server(app).await;

        let request = "GET /protected HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        let (status, _, body) = http_exchange(addr, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let parsed = parse_json_body(&body);
        assert_eq!(parsed["error"]["type"], "unauthorized");
        assert_eq!(
            parsed["error"]["message"],
            "Invalid or missing authentication token"
        );
        assert_eq!(
            parsed["message"], "Invalid or missing authentication token",
            "legacy top-level message must remain for existing clients"
        );
        assert!(
            !parsed.to_string().contains(&token),
            "auth token value must never appear in an error body"
        );
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn auth_middleware_fails_closed_with_500_when_token_is_missing() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());
        std::env::remove_var("OPENCODE_NO_AUTH"); // auth enabled

        let mut state = ServerState::new();
        let token = state.auth_token.take().expect("generated token");
        let app = auth_test_app(Arc::new(state));
        let (addr, server) = start_test_server(app).await;

        let request = "GET /protected HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        let (status, _, body) = http_exchange(addr, request).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing token must fail closed with 500"
        );
        let parsed = parse_json_body(&body);
        assert_eq!(parsed["error"]["type"], "internal_error");
        assert!(
            !parsed.to_string().contains(&token),
            "auth token value must never appear in an error body"
        );
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn auth_middleware_still_allows_valid_bearer_token() {
        let _guard = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreNoAuth(std::env::var("OPENCODE_NO_AUTH").ok());
        std::env::remove_var("OPENCODE_NO_AUTH"); // auth enabled

        let state = Arc::new(ServerState::new());
        let token = state.auth_token.clone().expect("generated token");
        let app = auth_test_app(state);
        let (addr, server) = start_test_server(app).await;

        let request = format!(
            "GET /protected HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        );
        let (status, _, body) = http_exchange(addr, &request).await;
        assert_eq!(status, StatusCode::OK, "valid token must still pass");
        assert!(body.contains("ok"));
        server.abort();
    }

    // ---- R2 観測性回帰テスト: 5xx レベル / request id 返却 / client_ip ----

    #[test]
    fn completion_log_level_escalates_server_and_client_errors() {
        assert_eq!(
            completion_log_level("server_error"),
            CompletionLogLevel::Error,
            "5xx must be ERROR so alerts can pick it up"
        );
        assert_eq!(
            completion_log_level("client_error"),
            CompletionLogLevel::Warn
        );
        assert_eq!(completion_log_level("redirect"), CompletionLogLevel::Info);
        assert_eq!(completion_log_level("success"), CompletionLogLevel::Info);
    }

    #[test]
    fn completion_outcome_classifies_http_status() {
        assert_eq!(
            completion_outcome(StatusCode::INTERNAL_SERVER_ERROR),
            "server_error"
        );
        assert_eq!(completion_outcome(StatusCode::BAD_GATEWAY), "server_error");
        assert_eq!(completion_outcome(StatusCode::UNAUTHORIZED), "client_error");
        assert_eq!(completion_outcome(StatusCode::NOT_FOUND), "client_error");
        assert_eq!(completion_outcome(StatusCode::FOUND), "redirect");
        assert_eq!(completion_outcome(StatusCode::OK), "success");
        assert_eq!(completion_outcome(StatusCode::CREATED), "success");
    }

    #[test]
    fn client_ip_from_request_reads_connect_info_extension() {
        let addr: SocketAddr = "127.0.0.1:45678".parse().expect("valid socket addr");
        let mut request = Request::new(axum::body::Body::empty());
        assert_eq!(client_ip_from_request(&request), None);

        request.extensions_mut().insert(ConnectInfo(addr));
        assert_eq!(client_ip_from_request(&request), Some(addr.ip()));
    }

    #[test]
    fn request_id_header_value_never_reflects_invalid_bytes() {
        assert_eq!(
            request_id_header_value("correlation-abc-123")
                .to_str()
                .unwrap(),
            "correlation-abc-123"
        );
        // 非 ASCII は HeaderValue にできないため固定値へフォールバックする。
        assert_eq!(
            request_id_header_value("日本語-id").to_str().unwrap(),
            "invalid-request-id"
        );
    }

    /// 指定した関数定義の本文を `include_str!` から切り出す（補助）。
    fn source_fn_body<'a>(source: &'a str, marker: &str) -> &'a str {
        let start = source.find(marker).expect("function must exist in source");
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("function closing brace");
        &rest[..end]
    }

    #[test]
    fn start_test_server_fails_instead_of_skipping_on_bind_error() {
        // 回帰ガード: bind 失敗時に None を返してテストを黙って通す（false green）ことを禁止。
        let source = include_str!("server.rs");
        assert!(
            source.contains(".expect(\"loopback bind must succeed in tests"),
            "loopback bind failure must fail the test"
        );
        assert!(
            !source.contains("eprintln!(\"SKIPPED"),
            "silent skip must be removed"
        );
        assert!(
            !source.contains(
                "async fn start_test_server(\n        app: axum::Router,\n    ) -> Option<"
            ),
            "test server helper must not return Option"
        );
    }

    #[test]
    fn server_wires_connect_info_and_records_client_ip() {
        let source = include_str!("server.rs");
        let connect_info_calls = source
            .matches("into_make_service_with_connect_info::<SocketAddr>()")
            .count();
        assert!(
            connect_info_calls >= 2,
            "both run_server and run_server_with_state must inject ConnectInfo"
        );

        let span_body = source_fn_body(source, "async fn request_span_middleware(");
        assert!(
            span_body.contains("client_ip"),
            "HTTP span must carry client_ip"
        );

        let auth_body = source_fn_body(source, "async fn auth_middleware(");
        assert!(
            auth_body.contains("client_ip"),
            "authentication failure warn must carry client_ip"
        );
        assert!(
            !auth_body.contains("supplied_bearer = %"),
            "token value must not be logged"
        );
    }

    fn request_id_test_app() -> axum::Router {
        axum::Router::new()
            .route("/ping", axum::routing::get(|| async { "pong" }))
            .layer(middleware::from_fn(request_span_middleware))
    }

    #[tokio::test]
    async fn response_echoes_supplied_request_id_header() {
        let (addr, server) = start_test_server(request_id_test_app()).await;
        let request = concat!(
            "GET /ping HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "x-request-id: correlation-abc-123\r\n",
            "Connection: close\r\n",
            "\r\n"
        );
        let (status, headers, _) = http_exchange(addr, request).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers.contains("x-request-id: correlation-abc-123"),
            "input request id must be echoed verbatim: {headers}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn response_returns_generated_uuid_when_request_id_is_absent() {
        let (addr, server) = start_test_server(request_id_test_app()).await;
        let request = "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        let (status, headers, _) = http_exchange(addr, request).await;
        assert_eq!(status, StatusCode::OK);
        let value = headers
            .lines()
            .find_map(|line| line.strip_prefix("x-request-id:"))
            .expect("response must carry x-request-id")
            .trim();
        assert!(
            Uuid::parse_str(value).is_ok(),
            "generated request id must be a UUID: {value}"
        );
        server.abort();
    }

    // ---- R2 主実行 / R4 残存 medium: 認証キーローテーション回帰テスト ----

    #[test]
    fn rotation_reason_accepts_only_401_and_429() {
        assert_eq!(
            ServerState::rotation_reason_for_status(StatusCode::UNAUTHORIZED),
            Some(RotationReason::Unauthorized)
        );
        assert_eq!(
            ServerState::rotation_reason_for_status(StatusCode::TOO_MANY_REQUESTS),
            Some(RotationReason::RateLimited)
        );
        for status in [
            StatusCode::OK,
            StatusCode::BAD_REQUEST,
            StatusCode::FORBIDDEN,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert_eq!(
                ServerState::rotation_reason_for_status(status),
                None,
                "non 401/429 status must not trigger rotation"
            );
        }
    }

    /// アクティブキー + プール済みキーを 1 つずつ持つ状態を作る。
    /// ダミー値のみを使い、実の秘密情報は一切読まない。
    async fn state_with_rotatable_key(provider_id: &str) -> ServerState {
        let state = ServerState::new();
        state
            .auth_manager
            .try_add_key(
                provider_id,
                AuthInfo::Api {
                    key: "test-primary-key".to_string(),
                },
            )
            .await
            .expect("primary key registration");
        state
            .auth_manager
            .try_add_key(
                provider_id,
                AuthInfo::Api {
                    key: "test-next-key".to_string(),
                },
            )
            .await
            .expect("pooled key registration");
        state
    }

    async fn active_key(auth: &AuthManager, provider_id: &str) -> Option<String> {
        match auth.get(provider_id).await? {
            AuthInfo::Api { key } => Some(key),
            _ => None,
        }
    }

    #[tokio::test]
    async fn unauthorized_rotates_key_and_rebuilds_registry() {
        let state = state_with_rotatable_key("openai").await;
        assert_eq!(
            active_key(&state.auth_manager, "openai").await.as_deref(),
            Some("test-primary-key")
        );
        assert_eq!(state.auth_manager.key_count("openai").await, 2);

        let rebuilt = state
            .rotate_provider_key("openai", StatusCode::UNAUTHORIZED)
            .await;

        assert!(
            rebuilt.is_some(),
            "401 must rotate when a pooled key exists"
        );
        assert_eq!(
            active_key(&state.auth_manager, "openai").await.as_deref(),
            Some("test-next-key"),
            "the next key must become active"
        );
        assert_eq!(state.auth_manager.key_count("openai").await, 2);
        // 返されたレジストリは auth_manager.list() を読んだ再 bootstrap 結果。
        let _ = rebuilt.expect("rebuilt registry").list();
    }

    #[tokio::test]
    async fn rate_limited_rotates_key_and_rebuilds_registry() {
        let state = state_with_rotatable_key("openai").await;

        let rebuilt = state
            .rotate_provider_key("openai", StatusCode::TOO_MANY_REQUESTS)
            .await;

        assert!(
            rebuilt.is_some(),
            "429 must rotate when a pooled key exists"
        );
        assert_eq!(
            active_key(&state.auth_manager, "openai").await.as_deref(),
            Some("test-next-key"),
            "the next key must become active on 429 too"
        );
        let _ = rebuilt.expect("rebuilt registry").list();
    }

    #[tokio::test]
    async fn rotation_is_noop_without_alternate_or_on_other_statuses() {
        let single = ServerState::new();
        single
            .auth_manager
            .try_add_key(
                "openai",
                AuthInfo::Api {
                    key: "only-key".to_string(),
                },
            )
            .await
            .expect("primary key registration");
        assert!(
            single
                .rotate_provider_key("openai", StatusCode::UNAUTHORIZED)
                .await
                .is_none(),
            "no alternate key means no rotation"
        );
        assert_eq!(
            active_key(&single.auth_manager, "openai").await.as_deref(),
            Some("only-key")
        );

        let state = state_with_rotatable_key("openai").await;
        assert!(
            state
                .rotate_provider_key("openai", StatusCode::INTERNAL_SERVER_ERROR)
                .await
                .is_none(),
            "non 401/429 status must not rotate"
        );
        assert_eq!(
            active_key(&state.auth_manager, "openai").await.as_deref(),
            Some("test-primary-key")
        );
    }

    #[test]
    fn rotation_helper_accepts_no_key_or_response_body() {
        // 秘密情報がヘルパへ入らないことをシグネチャ面でも固定する。
        let source = include_str!("server.rs");
        let start = source
            .find("pub async fn rotate_provider_key(")
            .expect("helper must exist");
        let end = source[start..]
            .find(") -> Option<ProviderRegistry>")
            .expect("helper signature");
        let signature = &source[start..start + end];
        assert!(signature.contains("provider_id: &str"));
        assert!(signature.contains("status: StatusCode"));
        assert!(
            !signature.contains("body"),
            "response body must not be passed in"
        );
        assert!(
            !signature.contains("key:"),
            "key value must not be passed in"
        );
    }

    /// R2(high) 回帰: session -> types の明示変換で usage / metadata /
    /// synthetic / ignored が落ちず、JSON 往復でも保持されること。
    #[test]
    fn stored_session_conversion_preserves_wire_fields() {
        let mut session = opencode_session::Session::new("proj-wire", "/work");
        session
            .metadata
            .insert("source".to_string(), serde_json::json!("wire-test"));
        session.usage = Some(opencode_session::SessionUsage {
            input_tokens: 101,
            output_tokens: 202,
            reasoning_tokens: 303,
            cache_write_tokens: 404,
            cache_read_tokens: 505,
            total_cost: 0.75,
        });

        let mut message = opencode_session::SessionMessage::user(&session.id, "hello");
        message
            .metadata
            .insert("tag".to_string(), serde_json::json!("kept"));
        message.usage = Some(opencode_session::MessageUsage {
            input_tokens: 11,
            output_tokens: 22,
            reasoning_tokens: 33,
            cache_write_tokens: 44,
            cache_read_tokens: 55,
            total_cost: 0.125,
        });
        message.parts = vec![opencode_session::MessagePart {
            id: "prt_flags".to_string(),
            part_type: opencode_session::PartType::Text {
                text: "flagged".to_string(),
                synthetic: Some(true),
                ignored: Some(false),
            },
            created_at: chrono::Utc::now(),
            message_id: None,
        }];
        session.messages.push(message);

        let stored = stored_session_from_session(&session);

        // wire 形式は小文字に正規化される（alias で旧形式も読めることは types 側で検証）。
        let stored_value = serde_json::to_value(&stored).unwrap();
        assert_eq!(stored_value["status"], serde_json::json!("active"));
        assert_eq!(
            serde_json::to_value(&stored.messages[0]).unwrap()["role"],
            serde_json::json!("user")
        );

        // セッション単位 usage / metadata が保持される。
        assert_eq!(stored.metadata, session.metadata);
        let usage = stored.usage.as_ref().expect("session usage preserved");
        assert_eq!(usage.input, 101.0);
        assert_eq!(usage.output, 202.0);
        assert_eq!(usage.reasoning, 303.0);
        assert_eq!(usage.cache.write, 404.0);
        assert_eq!(usage.cache.read, 505.0);
        assert_eq!(usage.total_cost, 0.75, "実コストが型変換で落ちないこと");

        // メッセージ単位 usage / metadata / Text フラグが保持される。
        let stored_message = &stored.messages[0];
        assert_eq!(stored_message.metadata, session.messages[0].metadata);
        let message_usage = stored_message
            .usage
            .as_ref()
            .expect("message usage preserved");
        assert_eq!(message_usage.input_tokens, 11);
        assert_eq!(message_usage.output_tokens, 22);
        assert_eq!(message_usage.reasoning_tokens, 33);
        assert_eq!(message_usage.cache_write_tokens, 44);
        assert_eq!(message_usage.cache_read_tokens, 55);
        assert_eq!(message_usage.total_cost, 0.125);
        match &stored_message.parts[0].part_type {
            opencode_types::PartType::Text {
                synthetic, ignored, ..
            } => {
                assert_eq!(*synthetic, Some(true));
                assert_eq!(*ignored, Some(false));
            }
            other => panic!("unexpected part type: {other:?}"),
        }

        // その後の JSON 往復でも情報が落ちない。
        // Session.messages は #[serde(skip)] なので、メッセージは個別に往復検証する。
        let decoded: opencode_types::Session = serde_json::from_value(stored_value).unwrap();
        assert_eq!(decoded.metadata, stored.metadata);
        assert_eq!(decoded.usage, stored.usage);

        let message_value = serde_json::to_value(stored_message).unwrap();
        let decoded_message: opencode_types::SessionMessage =
            serde_json::from_value(message_value).unwrap();
        assert_eq!(decoded_message.usage, stored_message.usage);
        assert_eq!(decoded_message.metadata, stored_message.metadata);
        assert_eq!(
            decoded_message.parts[0].part_type,
            stored_message.parts[0].part_type
        );
    }

    /// R2(high) 回帰: types -> session の明示変換が role の wire 形式に依存せず、
    /// PascalCase / lowercase どちらの入力 JSON からでも同じ enum へ復元されること。
    #[test]
    fn session_message_conversion_is_independent_of_role_wire_case() {
        for raw in ["user", "User"] {
            let message: opencode_types::SessionMessage =
                serde_json::from_value(serde_json::json!({
                    "id": "msg_wire",
                    "session_id": "sess_wire",
                    "role": raw,
                    "parts": [{
                        "id": "prt_wire",
                        "part_type": { "type": "text", "text": "hi" },
                        "created_at": "2024-01-01T00:00:00Z"
                    }],
                    "created_at": "2024-01-01T00:00:00Z",
                    "metadata": {},
                }))
                .unwrap_or_else(|error| panic!("role {raw} must deserialize: {error}"));

            let restored = session_message_from_stored(message);
            assert_eq!(restored.role, opencode_session::MessageRole::User);
            assert_eq!(restored.get_text(), "hi");
        }

        // 逆方向の enum 変換も全バリアントで一致する。
        for (stored, expected) in [
            (
                opencode_types::MessageRole::User,
                opencode_session::MessageRole::User,
            ),
            (
                opencode_types::MessageRole::Assistant,
                opencode_session::MessageRole::Assistant,
            ),
            (
                opencode_types::MessageRole::System,
                opencode_session::MessageRole::System,
            ),
            (
                opencode_types::MessageRole::Tool,
                opencode_session::MessageRole::Tool,
            ),
        ] {
            assert_eq!(session_role(&stored), expected);
            assert_eq!(stored_role(&expected), stored);
        }
    }
}
