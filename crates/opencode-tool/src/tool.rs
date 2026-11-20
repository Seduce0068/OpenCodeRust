use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ToolRegistry;

#[cfg(feature = "lsp")]
use opencode_lsp::LspClientRegistry;

pub type Metadata = HashMap<String, serde_json::Value>;

/// ファイルロックのレジストリ。
///
/// `Weak` で保持するため、呼び出し側が `Arc` を手放したエントリは
/// `prune_dead_file_locks` で回収できる。以前は `Arc` を残し続けていたため、
/// ツール入力のユニークパスを渡し続けると無限に成長していた。
type FileLockMap = HashMap<String, std::sync::Weak<Mutex<()>>>;

static FILE_LOCKS: std::sync::OnceLock<Arc<std::sync::Mutex<FileLockMap>>> =
    std::sync::OnceLock::new();

fn get_file_locks() -> Arc<std::sync::Mutex<FileLockMap>> {
    FILE_LOCKS
        .get_or_init(|| Arc::new(std::sync::Mutex::new(HashMap::new())))
        .clone()
}

/// 最後の強参照が消えたエントリをマップから取り除く。
///
/// `with_file_lock` の呼び出し中は `Arc` が必ず 1 つ以上生きているため、
/// 実行中・待機中の filepath が誤って削除されることはない。
/// filepath 自身はログにもエラーにも出さない。
fn prune_dead_file_locks(locks: &std::sync::Mutex<FileLockMap>) {
    let mut guard = locks.lock().unwrap();
    guard.retain(|_, weak| weak.strong_count() > 0);
}

pub async fn with_file_lock<F, Fut, T>(filepath: &str, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let lock: Arc<Mutex<()>> = {
        let locks = get_file_locks();
        let mut locks_guard = locks.lock().unwrap();
        match locks_guard.get(filepath).and_then(|weak| weak.upgrade()) {
            Some(existing) => existing,
            None => {
                let created = Arc::new(Mutex::new(()));
                locks_guard.insert(filepath.to_string(), Arc::downgrade(&created));
                created
            }
        }
    };

    // `lock` をガード取得後も `f().await` が終わるまで保持する。
    // これにより待機者がいる間に prune が走っても weak は死なず、
    // 同じ filepath の呼び出しは引き続き同じ Mutex を共有する。
    let guard = lock.lock().await;

    let locks = get_file_locks();
    prune_dead_file_locks(&locks);

    let result = f().await;
    // 最後のイテレーションでもエントリが残らないよう、guard と Arc を
    // 解放してからもう一度 prune する。
    drop(guard);
    drop(lock);
    prune_dead_file_locks(&locks);
    result
}

/// Parse an optional boolean argument. A missing or `null` value keeps
/// `default`; any other JSON type is rejected instead of being silently
/// coerced to `false`.
///
/// Shared by tools that accept `replace_all`-style flags (grep, edit).
/// Rejecting invalid types prevents a value such as `"yes"` or `1` from
/// collapsing to `false` and silently changing the tool's behaviour.
pub(crate) fn parse_bool_arg(
    args: &serde_json::Value,
    key: &str,
    default: bool,
) -> Result<bool, ToolError> {
    let value = &args[key];
    if value.is_null() {
        return Ok(default);
    }
    value
        .as_bool()
        .ok_or_else(|| ToolError::InvalidArguments(format!("{} must be a boolean", key)))
}

/// Parse an optional non-negative integer argument into `usize`.
///
/// Missing/`null` keeps `default`. Any other JSON type — including a
/// negative number such as `-1` — is rejected rather than silently coerced
/// to `1`. `usize::try_from` also guards the u64 -> usize narrowing on
/// targets where `usize` is smaller than `u64`.
pub(crate) fn parse_usize_arg(
    args: &serde_json::Value,
    key: &str,
    default: usize,
) -> Result<usize, ToolError> {
    let value = &args[key];
    if value.is_null() {
        return Ok(default);
    }
    let n = value.as_u64().ok_or_else(|| {
        ToolError::InvalidArguments(format!("{} must be a non-negative integer", key))
    })?;
    usize::try_from(n).map_err(|_| {
        ToolError::InvalidArguments(format!("{} is out of range for this platform", key))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionDef {
    pub question: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multiple: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

pub type AskCallback = Arc<
    dyn (Fn(
            PermissionRequest,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type QuestionCallback = Arc<
    dyn (Fn(
            Vec<QuestionDef>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<Vec<String>>, ToolError>> + Send>,
        >) + Send
        + Sync,
>;

pub type SwitchAgentCallback = Arc<
    dyn (Fn(
            String,
            Option<String>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type CreateSubsessionCallback = Arc<
    dyn (Fn(
            String,
            Option<String>,
            Option<String>,
            Vec<String>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ToolError>> + Send>>)
        + Send
        + Sync,
>;
pub type PromptSubsessionCallback = Arc<
    dyn (Fn(
            String,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type FileTimeAssertCallback = Arc<
    dyn (Fn(
            String,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type FileTimeReadCallback = Arc<
    dyn (Fn(
            String,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type PublishBusCallback = Arc<
    dyn (Fn(
            String,
            serde_json::Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>)
        + Send
        + Sync,
>;

pub type UpdatePartCallback = Arc<
    dyn (Fn(
            serde_json::Value,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type UpdateMessageCallback = Arc<
    dyn (Fn(
            serde_json::Value,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type LspTouchFileCallback = Arc<
    dyn (Fn(
            String,
            bool,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItemData {
    pub content: String,
    pub status: String,
    pub priority: String,
}

pub type TodoUpdateCallback = Arc<
    dyn (Fn(
            String,
            Vec<TodoItemData>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

pub type TodoGetCallback = Arc<
    dyn (Fn(
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<TodoItemData>, ToolError>> + Send>,
        >) + Send
        + Sync,
>;

pub type GetLastModelCallback = Arc<
    dyn (Fn(
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<String>, ToolError>> + Send>,
        >) + Send
        + Sync,
>;

pub type CreateSyntheticMessageCallback = Arc<
    dyn (Fn(
            String,
            Option<String>,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolError>> + Send>>)
        + Send
        + Sync,
>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub permission: String,
    pub patterns: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub always: Vec<String>,
}

impl PermissionRequest {
    pub fn new(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            patterns: Vec::new(),
            metadata: HashMap::new(),
            always: Vec::new(),
        }
    }

    pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.patterns.push(pattern.into());
        self
    }

    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        self.patterns = patterns;
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn with_always(mut self, always: impl Into<String>) -> Self {
        self.always.push(always.into());
        self
    }

    pub fn always_allow(mut self) -> Self {
        self.always.push("*".to_string());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub title: String,
    pub output: String,
    pub metadata: Metadata,
    pub truncated: bool,
}

impl ToolResult {
    pub fn simple(title: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            output: output.into(),
            metadata: Metadata::new(),
            truncated: false,
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub message_id: String,
    pub agent: String,
    pub call_id: Option<String>,
    pub directory: String,
    pub worktree: String,
    pub abort: CancellationToken,
    pub extra: HashMap<String, serde_json::Value>,
    pub ask: Option<AskCallback>,
    pub ask_question: Option<QuestionCallback>,
    pub switch_agent: Option<SwitchAgentCallback>,
    pub create_subsession: Option<CreateSubsessionCallback>,
    pub prompt_subsession: Option<PromptSubsessionCallback>,
    pub file_time_assert: Option<FileTimeAssertCallback>,
    pub file_time_read: Option<FileTimeReadCallback>,
    pub publish_bus: Option<PublishBusCallback>,
    pub update_part: Option<UpdatePartCallback>,
    pub update_message: Option<UpdateMessageCallback>,
    pub lsp_touch_file: Option<LspTouchFileCallback>,
    pub todo_update: Option<TodoUpdateCallback>,
    pub todo_get: Option<TodoGetCallback>,
    pub get_last_model: Option<GetLastModelCallback>,
    pub create_synthetic_message: Option<CreateSyntheticMessageCallback>,
    pub project_root: String,
    pub registry: Option<Arc<ToolRegistry>>,
    #[cfg(feature = "lsp")]
    pub lsp_registry: Option<Arc<LspClientRegistry>>,
}

impl ToolContext {
    pub fn new(session_id: String, message_id: String, directory: String) -> Self {
        Self {
            session_id,
            message_id,
            agent: String::new(),
            call_id: None,
            directory: directory.clone(),
            worktree: directory.clone(),
            abort: CancellationToken::new(),
            extra: HashMap::new(),
            ask: None,
            ask_question: None,
            switch_agent: None,
            create_subsession: None,
            prompt_subsession: None,
            file_time_assert: None,
            file_time_read: None,
            publish_bus: None,
            update_part: None,
            update_message: None,
            lsp_touch_file: None,
            todo_update: None,
            todo_get: None,
            get_last_model: None,
            create_synthetic_message: None,
            project_root: directory,
            registry: None,
            #[cfg(feature = "lsp")]
            lsp_registry: None,
        }
    }

    pub fn with_agent(mut self, agent: String) -> Self {
        self.agent = agent;
        self
    }

    pub fn with_abort(mut self, abort: CancellationToken) -> Self {
        self.abort = abort;
        self
    }

    pub fn with_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    #[cfg(feature = "lsp")]
    pub fn with_lsp_registry(mut self, lsp_registry: Arc<LspClientRegistry>) -> Self {
        self.lsp_registry = Some(lsp_registry);
        self
    }

    pub fn with_ask<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(PermissionRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.ask = Some(Arc::new(move |req| Box::pin(callback(req))));
        self
    }

    /// 個別ツールの詳細権限（コマンドパターンや外部ディレクトリ等）を確認する。
    ///
    /// # 安全性の根拠
    ///
    /// `ask` が未設定のときに `Ok(())` を返すのは fail-open に見えるが、
    /// 上位レイヤが既に権限判断を行っている経路では意図的な no-op である。
    ///
    /// - `opencode-agent::AgentExecutor` はツール実行前に
    ///   `ensure_tool_allowed` を呼び、`PermissionDecision::Ask` を
    ///   `ToolError::PermissionDenied` へ変換する（同クレートの
    ///   `executor_blocks_ask_permissions_without_user_approval` テストで検証）。
    ///   さらに `opencode-permission::ruleset::evaluate` はルール未マッチ時に
    ///   `Ask` を返すため、明示 `allow` がないツールは既定で拒否される。
    /// - HTTP 経路（`opencode-server::routes`）は常に `PermissionBroker` を
    ///   コールバックとして配線し、broker は応答が無ければ `Reject` を返す。
    ///
    /// つまり「拒否されるべき操作がこの no-op で通過する」因果は存在しない。
    /// このメソッド単体で拒否を保証したい場合は `with_ask` でコールバックを
    /// 設定し、拒否時に `ToolError::PermissionDenied` を返すこと。
    pub async fn ask_permission(&self, request: PermissionRequest) -> Result<(), ToolError> {
        if let Some(ref callback) = self.ask {
            callback(request).await
        } else {
            Ok(())
        }
    }

    pub fn with_ask_question<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(Vec<QuestionDef>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Vec<Vec<String>>, ToolError>> + Send + 'static,
    {
        self.ask_question = Some(Arc::new(move |questions| Box::pin(callback(questions))));
        self
    }

    pub async fn question(
        &self,
        questions: Vec<QuestionDef>,
    ) -> Result<Vec<Vec<String>>, ToolError> {
        if let Some(ref callback) = self.ask_question {
            callback(questions).await
        } else {
            Err(ToolError::ExecutionError(
                "Question callback not configured".to_string(),
            ))
        }
    }

    pub fn with_switch_agent<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, Option<String>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.switch_agent = Some(Arc::new(move |agent, model| {
            Box::pin(callback(agent, model))
        }));
        self
    }

    pub async fn do_switch_agent(
        &self,
        agent: String,
        model: Option<String>,
    ) -> Result<(), ToolError> {
        if let Some(ref callback) = self.switch_agent {
            callback(agent, model).await
        } else {
            Ok(())
        }
    }

    pub fn with_create_subsession<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, Option<String>, Option<String>, Vec<String>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, ToolError>> + Send + 'static,
    {
        self.create_subsession = Some(Arc::new(move |agent, title, model, disabled_tools| {
            Box::pin(callback(agent, title, model, disabled_tools))
        }));
        self
    }

    pub async fn do_create_subsession(
        &self,
        agent: String,
        title: Option<String>,
        model: Option<String>,
        disabled_tools: Vec<String>,
    ) -> Result<String, ToolError> {
        if let Some(ref callback) = self.create_subsession {
            callback(agent, title, model, disabled_tools).await
        } else {
            Ok(format!("task_{}_{}", agent, uuid::Uuid::new_v4()))
        }
    }

    pub fn with_prompt_subsession<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, ToolError>> + Send + 'static,
    {
        self.prompt_subsession = Some(Arc::new(move |session_id, prompt| {
            Box::pin(callback(session_id, prompt))
        }));
        self
    }

    pub async fn do_prompt_subsession(
        &self,
        session_id: String,
        prompt: String,
    ) -> Result<String, ToolError> {
        if let Some(ref callback) = self.prompt_subsession {
            callback(session_id, prompt).await
        } else {
            Err(ToolError::ExecutionError(
                "Subsession prompt callback not configured".to_string(),
            ))
        }
    }

    pub fn with_file_time_assert<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.file_time_assert = Some(Arc::new(move |session_id, file_path| {
            Box::pin(callback(session_id, file_path))
        }));
        self
    }

    pub async fn do_file_time_assert(&self, file_path: String) -> Result<(), ToolError> {
        if let Some(ref callback) = self.file_time_assert {
            callback(self.session_id.clone(), file_path).await
        } else {
            Ok(())
        }
    }

    pub fn with_file_time_read<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.file_time_read = Some(Arc::new(move |session_id, file_path| {
            Box::pin(callback(session_id, file_path))
        }));
        self
    }

    pub async fn do_file_time_read(&self, file_path: String) -> Result<(), ToolError> {
        if let Some(ref callback) = self.file_time_read {
            callback(self.session_id.clone(), file_path).await
        } else {
            Ok(())
        }
    }

    pub fn with_publish_bus<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.publish_bus = Some(Arc::new(move |event_type, properties| {
            Box::pin(callback(event_type, properties))
        }));
        self
    }

    pub async fn do_publish_bus(&self, event_type: &str, properties: serde_json::Value) {
        if let Some(ref callback) = self.publish_bus {
            callback(event_type.to_string(), properties).await;
        }
    }

    pub fn with_update_part<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.update_part = Some(Arc::new(move |part| Box::pin(callback(part))));
        self
    }

    pub async fn do_update_part(&self, part: serde_json::Value) -> Result<(), ToolError> {
        if let Some(ref callback) = self.update_part {
            callback(part).await
        } else {
            Ok(())
        }
    }

    pub fn with_update_message<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.update_message = Some(Arc::new(move |msg| Box::pin(callback(msg))));
        self
    }

    pub async fn do_update_message(&self, msg: serde_json::Value) -> Result<(), ToolError> {
        if let Some(ref callback) = self.update_message {
            callback(msg).await
        } else {
            Ok(())
        }
    }

    pub fn with_lsp_touch_file<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, bool) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.lsp_touch_file = Some(Arc::new(move |file_path, write| {
            Box::pin(callback(file_path, write))
        }));
        self
    }

    pub async fn do_lsp_touch_file(&self, file_path: String, write: bool) -> Result<(), ToolError> {
        if let Some(ref callback) = self.lsp_touch_file {
            callback(file_path, write).await
        } else {
            Ok(())
        }
    }

    pub fn with_todo_update<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, Vec<TodoItemData>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.todo_update = Some(Arc::new(move |session_id, todos| {
            Box::pin(callback(session_id, todos))
        }));
        self
    }

    pub async fn do_todo_update(&self, todos: Vec<TodoItemData>) -> Result<(), ToolError> {
        if let Some(ref callback) = self.todo_update {
            callback(self.session_id.clone(), todos).await
        } else {
            Ok(())
        }
    }

    pub fn with_todo_get<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Vec<TodoItemData>, ToolError>> + Send + 'static,
    {
        self.todo_get = Some(Arc::new(move |session_id| Box::pin(callback(session_id))));
        self
    }

    pub async fn do_todo_get(&self) -> Result<Vec<TodoItemData>, ToolError> {
        if let Some(ref callback) = self.todo_get {
            callback(self.session_id.clone()).await
        } else {
            Ok(Vec::new())
        }
    }

    pub fn with_get_last_model<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<String>, ToolError>> + Send + 'static,
    {
        self.get_last_model = Some(Arc::new(move |session_id| Box::pin(callback(session_id))));
        self
    }

    pub async fn do_get_last_model(&self) -> Option<String> {
        if let Some(ref callback) = self.get_last_model {
            callback(self.session_id.clone()).await.ok().flatten()
        } else {
            None
        }
    }

    pub fn with_create_synthetic_message<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(String, Option<String>, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ToolError>> + Send + 'static,
    {
        self.create_synthetic_message = Some(Arc::new(move |session_id, agent, text| {
            Box::pin(callback(session_id, agent, text))
        }));
        self
    }

    pub async fn do_create_synthetic_message(
        &self,
        agent: Option<String>,
        text: String,
    ) -> Result<(), ToolError> {
        if let Some(ref callback) = self.create_synthetic_message {
            callback(self.session_id.clone(), agent, text).await
        } else {
            Ok(())
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.abort.is_cancelled()
    }

    /// Whether `path` resolves outside the project root.
    ///
    /// Both the candidate and the project root are canonicalized before
    /// comparison, so `..` segments, sibling directories that share a string
    /// prefix with the root, and symlink escapes are not mistaken for paths
    /// inside the project. Comparison is done on path components via
    /// [`std::path::Path::starts_with`], not on the raw string prefix.
    ///
    /// If either path cannot be canonicalized (for example, the target does
    /// not exist), the path is treated as external. That is the safe side for
    /// the permission checks that call this method.
    pub fn is_external_path(&self, path: &str) -> bool {
        let candidate = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            std::path::Path::new(&self.directory).join(path)
        };

        let candidate = match candidate.canonicalize() {
            Ok(resolved) => resolved,
            Err(_) => return true,
        };
        let root = match std::path::Path::new(&self.project_root).canonicalize() {
            Ok(resolved) => resolved,
            Err(_) => return true,
        };

        !candidate.starts_with(&root)
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("session_id", &self.session_id)
            .field("message_id", &self.message_id)
            .field("agent", &self.agent)
            .field("directory", &self.directory)
            .field("worktree", &self.worktree)
            .finish()
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError>;

    fn validate(&self, args: &serde_json::Value) -> Result<(), ToolError> {
        let _ = args;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    /// 下位エラーを `source()` チェーンとして保持する実行エラー。
    ///
    /// `ExecutionError` は原因を文字列へ平坦化するためチェーンが失われる。
    /// 原因を保持したい呼び出し元は `ToolError::execution_with_source` を使う。
    #[error("Execution error: {message}")]
    ExecutionWithSource {
        /// 利用者向けの説明文。
        message: String,
        /// 保持された下位エラー。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Question rejected: {0}")]
    QuestionRejected(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Binary file: {0}")]
    BinaryFile(String),

    #[error("Invalid arguments: {0}")]
    InvalidArguments(String),

    #[error("Cancelled")]
    Cancelled,
}

impl ToolError {
    /// 実行エラーを、下位エラーを `source()` に保持した状態で構築する。
    pub fn execution_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::ExecutionWithSource {
            message: message.into(),
            source: Box::new(source),
        }
    }

    pub fn with_suggestions(msg: impl Into<String>, suggestions: &[String]) -> Self {
        let msg = msg.into();
        if suggestions.is_empty() {
            ToolError::FileNotFound(msg)
        } else {
            ToolError::FileNotFound(format!(
                "{}\n\nDid you mean one of these?\n{}",
                msg,
                suggestions.join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_request_builder() {
        let request = PermissionRequest::new("bash")
            .with_pattern("ls *")
            .with_patterns(vec!["git status".to_owned()])
            .with_metadata("reason", serde_json::json!("test"))
            .with_always("ls")
            .always_allow();

        assert_eq!(request.permission, "bash");
        assert_eq!(request.patterns, vec!["git status"]);
        assert_eq!(request.metadata["reason"], "test");
        assert_eq!(request.always, vec!["ls", "*"]);
    }

    #[test]
    fn tool_result_simple_and_metadata() {
        let result =
            ToolResult::simple("title", "output").with_metadata("count", serde_json::json!(2));

        assert_eq!(result.title, "title");
        assert_eq!(result.output, "output");
        assert!(!result.truncated);
        assert_eq!(result.metadata["count"], 2);
    }

    #[test]
    fn tool_context_defaults() {
        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned());
        assert_eq!(ctx.session_id, "s");
        assert_eq!(ctx.message_id, "m");
        assert_eq!(ctx.directory, "/work");
        assert_eq!(ctx.worktree, "/work");
        assert!(!ctx.is_cancelled());
    }

    #[test]
    fn tool_context_with_agent() {
        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned())
            .with_agent("plan".to_owned());
        assert_eq!(ctx.agent, "plan");
    }

    /// `ask_permission` の既定動作を固定する回帰テスト。
    ///
    /// コールバック未設定時は上位ゲート（executor の Ask→Deny／server の
    /// PermissionBroker）が権限判断を担うため no-op であることを明示する。
    #[tokio::test]
    async fn ask_permission_without_callback_is_documented_no_op() {
        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned());
        assert!(ctx.ask.is_none());

        let result = ctx
            .ask_permission(PermissionRequest::new("bash").with_pattern("rm -rf *"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ask_permission_propagates_callback_rejection() {
        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned()).with_ask(
            |_req| async { Err(ToolError::PermissionDenied("rejected by broker".to_owned())) },
        );

        let error = ctx
            .ask_permission(PermissionRequest::new("bash"))
            .await
            .expect_err("callback rejection must be propagated");
        assert!(matches!(error, ToolError::PermissionDenied(_)));
    }

    #[test]
    fn is_external_path_checks_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        let ctx = ToolContext::new(
            "s".to_owned(),
            "m".to_owned(),
            root.to_string_lossy().to_string(),
        );

        assert!(!ctx.is_external_path("src/main.rs"));
        assert!(!ctx.is_external_path(&root.join("src/main.rs").to_string_lossy()));
        assert!(ctx.is_external_path("/etc/passwd"));
    }

    #[test]
    fn is_external_path_rejects_sibling_with_shared_string_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let sibling = tmp.path().join("project-sibling");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("file.txt"), "").unwrap();
        let ctx = ToolContext::new(
            "s".to_owned(),
            "m".to_owned(),
            root.to_string_lossy().to_string(),
        );

        // The absolute sibling path shares the root's string prefix but is outside it.
        let sibling_file = sibling.join("file.txt").to_string_lossy().to_string();
        assert!(ctx.is_external_path(&sibling_file));
        // The same path reached through ".." must not be accepted either.
        assert!(ctx.is_external_path("../project-sibling/file.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn is_external_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        let ctx = ToolContext::new(
            "s".to_owned(),
            "m".to_owned(),
            root.to_string_lossy().to_string(),
        );

        assert!(ctx.is_external_path("escape/secret.txt"));
    }

    #[test]
    fn is_external_path_treats_canonicalize_failure_as_external() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ToolContext::new(
            "s".to_owned(),
            "m".to_owned(),
            root.to_string_lossy().to_string(),
        );

        assert!(ctx.is_external_path("does-not-exist.rs"));

        let missing_root = ToolContext::new(
            "s".to_owned(),
            "m".to_owned(),
            tmp.path()
                .join("missing-root")
                .to_string_lossy()
                .to_string(),
        );
        assert!(missing_root.is_external_path("/etc/passwd"));
    }

    #[test]
    fn tool_error_execution_with_source_preserves_error_chain() {
        let cause = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let error = ToolError::execution_with_source("Failed to read file", cause);

        assert_eq!(error.to_string(), "Execution error: Failed to read file");
        let source = std::error::Error::source(&error).expect("source must be preserved");
        assert_eq!(source.to_string(), "missing file");
        assert!(matches!(
            source.downcast_ref::<std::io::Error>(),
            Some(io_error) if io_error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn tool_error_with_suggestions() {
        let suggestions = vec!["a.rs".to_owned(), "b.rs".to_owned()];
        let error = ToolError::with_suggestions("missing", &suggestions);
        let message = error.to_string();
        assert!(message.contains("missing"));
        assert!(message.contains("a.rs"));
        assert!(message.contains("b.rs"));

        let plain = ToolError::with_suggestions("missing", &[]);
        assert_eq!(plain.to_string(), "File not found: missing");
    }

    fn count_file_lock_entries(prefix: &str) -> usize {
        get_file_locks()
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(prefix))
            .count()
    }

    /// 多数のユニーク filepath を渡しても FILE_LOCKS が無限成長しない回帰テスト。
    #[tokio::test]
    async fn file_locks_do_not_grow_for_unique_filepaths() {
        let namespace = format!("leak-regression-{}", uuid::Uuid::new_v4());
        assert_eq!(count_file_lock_entries(&namespace), 0);

        for index in 0..256 {
            let path = format!("{namespace}/{index}");
            with_file_lock(&path, || async {}).await;
        }

        assert_eq!(
            count_file_lock_entries(&namespace),
            0,
            "dead lock entries must be pruned after guard release"
        );
    }

    /// 同じ filepath の待機者がいる間に prune が走っても、
    /// 全呼び出しが同じ Mutex を共有し相互排他が維持されること。
    #[tokio::test]
    async fn concurrent_file_locks_stay_exclusive_while_pruning() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let path = format!("shared-lock-{}", uuid::Uuid::new_v4());
        let completed = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            let completed = Arc::clone(&completed);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                with_file_lock(&path, || async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    completed.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            }));
        }
        for task in tasks {
            task.await.expect("lock task must not panic");
        }

        assert_eq!(completed.load(Ordering::SeqCst), 8);
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "all callers for one filepath must share a single mutex"
        );
        assert_eq!(count_file_lock_entries(&path), 0);
    }
}
