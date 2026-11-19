use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub slug: String,
    pub project_id: String,
    pub directory: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub version: String,
    pub time: SessionTimeInfo,
    #[serde(default)]
    pub revert: Option<SessionRevertInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTimeInfo {
    pub created: i64,
    pub updated: i64,
    pub compacting: Option<i64>,
    pub archived: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRevertInfo {
    pub message_id: String,
    #[serde(default)]
    pub part_id: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatusInfo {
    pub status: String,
    pub idle: bool,
    pub busy: bool,
    #[serde(default)]
    pub attempt: Option<u32>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub next: Option<i64>,
}

/// `GET /session/{id}/todo` が返す TODO 項目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoInfo {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOptionInfo {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionInfo {
    pub question: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<QuestionOptionInfo>,
    #[serde(default)]
    pub multiple: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRequestInfo {
    pub id: String,
    pub session_id: String,
    pub questions: Vec<QuestionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePart {
    pub id: String,
    #[serde(rename = "type")]
    pub part_type: String,
    pub text: Option<String>,
    pub file: Option<FileInfo>,
    #[serde(alias = "toolCall")]
    pub tool_call: Option<ToolCall>,
    #[serde(alias = "toolResult")]
    pub tool_result: Option<ToolResult>,
    #[serde(default)]
    pub synthetic: Option<bool>,
    #[serde(default)]
    pub ignored: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub url: String,
    pub filename: String,
    pub mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    #[serde(alias = "toolCallId")]
    pub tool_call_id: String,
    pub content: String,
    #[serde(alias = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageInfo {
    pub id: String,
    #[serde(alias = "sessionId")]
    pub session_id: String,
    pub role: String,
    pub created_at: i64,
    #[serde(default, alias = "completedAt")]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub finish: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub tokens: MessageTokensInfo,
    #[serde(default)]
    pub parts: Vec<MessagePart>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageTokensInfo {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(default, alias = "cacheRead")]
    pub cache_read: u64,
    #[serde(default, alias = "cacheWrite")]
    pub cache_write: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    pub message: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub command: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteShellRequest {
    pub command: String,
    pub workdir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSessionRequest {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderInfo>,
    #[serde(rename = "default")]
    pub default_model: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<ProviderModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub variants: Vec<String>,
    #[serde(
        default,
        alias = "context_window",
        alias = "contextWindow",
        alias = "contextLength"
    )]
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStatusInfo {
    pub name: String,
    pub status: String,
    pub tools: usize,
    pub resources: usize,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpAuthStartInfo {
    pub authorization_url: String,
    pub client_id: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareResponse {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactResponse {
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertRequest {
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertResponse {
    pub success: bool,
}

/// `DELETE /session/{id}` の応答。キー欠落・型不一致はデシリアライズ失敗とする（M-52）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeletedResponse {
    deleted: bool,
}

/// `{"success": bool}` 形式の応答（MCP 認証解除など）。緩やかなデフォルト値は持たない（M-52）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SuccessResponse {
    success: bool,
}

/// `DELETE /session/{id}/share` の応答。サーバは `unshared` を返す（M-52）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnsharedResponse {
    unshared: bool,
}

/// サーバが 501 Not Implemented を返した操作を表す型付きエラー。
///
/// TUI 側で `anyhow::Error::downcast_ref` により、通信失敗（500 等）ではなく
/// 「この機能はサーバで利用不可」であるを識別するために使用する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedEndpointError {
    /// 501 を返した操作名（例: `"skill list"`）。
    pub operation: &'static str,
}

impl std::fmt::Display for UnsupportedEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` is not supported by the server (501 Not Implemented)",
            self.operation
        )
    }
}

impl std::error::Error for UnsupportedEndpointError {}

/// レスポンスが 501 Not Implemented かどうかを判定する純粋関数。
///
/// HTTP 通信を行わないため、単体テストで直接検証できる。
fn is_not_implemented(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::NOT_IMPLEMENTED
}

/// 501 用の型付きエラーを構築する。
fn unsupported(operation: &'static str) -> anyhow::Error {
    anyhow::Error::new(UnsupportedEndpointError { operation })
}

pub struct ApiClient {
    client: Client,
    base_url: String,
    pub current_session: Arc<RwLock<Option<SessionInfo>>>,
}

impl ApiClient {
    pub fn new(base_url: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url,
            current_session: Arc::new(RwLock::new(None)),
        }
    }

    pub fn create_session(&self, parent_id: Option<String>) -> anyhow::Result<SessionInfo> {
        let url = format!("{}/session", self.base_url);
        let request = CreateSessionRequest { parent_id };

        let response = self.client.post(&url).json(&request).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to create session: {} - {}", status, text);
        }

        let session: SessionInfo = response.json()?;
        Ok(session)
    }

    pub fn get_session(&self, session_id: &str) -> anyhow::Result<SessionInfo> {
        let url = format!("{}/session/{}", self.base_url, session_id);

        let response = self.client.get(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get session: {} - {}", status, text);
        }

        let session: SessionInfo = response.json()?;
        Ok(session)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        self.list_sessions_filtered(None, None)
    }

    pub fn list_sessions_filtered(
        &self,
        search: Option<&str>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let url = format!("{}/session", self.base_url);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(search) = search.map(str::trim).filter(|s| !s.is_empty()) {
            params.push(("search", search.to_string()));
        }
        if let Some(limit) = limit.filter(|l| *l > 0) {
            params.push(("limit", limit.to_string()));
        }

        let request = if params.is_empty() {
            self.client.get(&url)
        } else {
            self.client.get(&url).query(&params)
        };
        let response = request.send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to list sessions: {} - {}", status, text);
        }

        let sessions: Vec<SessionInfo> = response.json()?;
        Ok(sessions)
    }

    pub fn get_session_status(&self) -> anyhow::Result<HashMap<String, SessionStatusInfo>> {
        let url = format!("{}/session/status", self.base_url);
        let response = self.client.get(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get session status: {} - {}", status, text);
        }
        Ok(response.json::<HashMap<String, SessionStatusInfo>>()?)
    }

    pub fn update_session_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> anyhow::Result<SessionInfo> {
        let url = format!("{}/session/{}", self.base_url, session_id);
        let request = UpdateSessionRequest {
            title: Some(title.to_string()),
        };
        let response = self.client.patch(&url).json(&request).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to update session `{}` title: {} - {}",
                session_id,
                status,
                text
            );
        }
        let session: SessionInfo = response.json()?;
        Ok(session)
    }

    pub fn delete_session(&self, session_id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/session/{}", self.base_url, session_id);
        let response = self.client.delete(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to delete session `{}`: {} - {}",
                session_id,
                status,
                text
            );
        }
        // M-52: 旧実装は Value::get(...).unwrap_or(true) で、
        // キー欠落や型不一致を「削除成功」として報告していた。
        // 厳密な型付きデシリアライズで解析失敗を Err として返す。
        let result: DeletedResponse = response.json()?;
        Ok(result.deleted)
    }

    pub fn send_prompt(
        &self,
        session_id: &str,
        content: String,
        agent: Option<String>,
        model: Option<String>,
        variant: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/session/{}/prompt", self.base_url, session_id);
        let request = PromptRequest {
            message: content,
            agent,
            model,
            variant,
            command: None,
            arguments: None,
        };

        let response = self.client.post(&url).json(&request).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to send prompt to {}: {} - {}", url, status, text);
        }

        let result: serde_json::Value = response.json()?;
        Ok(result)
    }

    pub fn execute_shell(
        &self,
        session_id: &str,
        command: String,
        workdir: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/session/{}/shell", self.base_url, session_id);
        let request = ExecuteShellRequest { command, workdir };
        let response = self.client.post(&url).json(&request).send()?;

        let status = response.status();
        if is_not_implemented(status) {
            // 501 = 明示的な unsupported。シェル実行は空結果へフォールバックできないため、
            // 型付きエラーとして呼び出し側へ「利用不可」を伝える。
            return Err(unsupported("shell execution"));
        }
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to execute shell command: {} - {}", status, text);
        }

        Ok(response.json::<serde_json::Value>()?)
    }

    pub fn abort_session(&self, session_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/session/{}/abort", self.base_url, session_id);
        let response = self.client.post(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to abort session: {} - {}", status, text);
        }

        Ok(response.json::<serde_json::Value>()?)
    }

    pub fn get_config_providers(&self) -> anyhow::Result<ProviderListResponse> {
        let url = format!("{}/config/providers", self.base_url);

        let response = self.client.get(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get providers: {} - {}", status, text);
        }

        let providers: ProviderListResponse = response.json()?;
        Ok(providers)
    }

    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        let url = format!("{}/agent", self.base_url);

        let response = self.client.get(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to list agents: {} - {}", status, text);
        }

        let agents: Vec<AgentInfo> = response.json()?;
        Ok(agents)
    }

    pub fn list_skills(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/skill", self.base_url);
        let response = self.client.get(&url).send()?;

        let status = response.status();
        if is_not_implemented(status) {
            // 501 = 明示的な unsupported。一覧自体は空だが、TUI が「スキルなし」と
            // 「機能自体が利用不可」を区別できるよう型付きエラーで伝える。
            return Err(unsupported("skill list"));
        }
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to list skills: {} - {}", status, text);
        }

        Ok(response.json::<Vec<String>>()?)
    }

    pub fn get_mcp_status(&self) -> anyhow::Result<Vec<McpStatusInfo>> {
        let url = format!("{}/mcp", self.base_url);

        let response = self.client.get(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to fetch MCP status: {} - {}", status, text);
        }

        let mut servers: Vec<McpStatusInfo> = response
            .json::<HashMap<String, McpStatusInfo>>()?
            .into_values()
            .collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(servers)
    }

    pub fn start_mcp_auth(&self, name: &str) -> anyhow::Result<McpAuthStartInfo> {
        let url = format!("{}/mcp/{}/auth", self.base_url, name);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to start MCP auth `{}`: {} - {}", name, status, text);
        }
        Ok(response.json::<McpAuthStartInfo>()?)
    }

    pub fn authenticate_mcp(&self, name: &str) -> anyhow::Result<McpStatusInfo> {
        let url = format!("{}/mcp/{}/auth/authenticate", self.base_url, name);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to authenticate MCP `{}`: {} - {}",
                name,
                status,
                text
            );
        }
        Ok(response.json::<McpStatusInfo>()?)
    }

    pub fn remove_mcp_auth(&self, name: &str) -> anyhow::Result<bool> {
        let url = format!("{}/mcp/{}/auth", self.base_url, name);
        let response = self.client.delete(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to remove MCP auth `{}`: {} - {}",
                name,
                status,
                text
            );
        }
        // M-52: `success` の欠落・型不一致は Err として返す。
        let result: SuccessResponse = response.json()?;
        Ok(result.success)
    }

    pub fn connect_mcp(&self, name: &str) -> anyhow::Result<bool> {
        let url = format!("{}/mcp/{}/connect", self.base_url, name);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to connect MCP `{}`: {} - {}", name, status, text);
        }
        // M-52: サーバは Json<bool> を返す。解析失敗は Err として返す。
        Ok(response.json::<bool>()?)
    }

    pub fn disconnect_mcp(&self, name: &str) -> anyhow::Result<bool> {
        let url = format!("{}/mcp/{}/disconnect", self.base_url, name);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to disconnect MCP `{}`: {} - {}", name, status, text);
        }
        // M-52: サーバは Json<bool> を返す。解析失敗は Err として返す。
        Ok(response.json::<bool>()?)
    }

    pub fn get_messages(&self, session_id: &str) -> anyhow::Result<Vec<MessageInfo>> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);

        let response = self.client.get(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get messages: {} - {}", status, text);
        }

        let messages: Vec<MessageInfo> = response.json()?;
        Ok(messages)
    }

    /// セッションに紐付いた TODO 一覧を `GET /session/{id}/todo` で取得する。
    ///
    /// `/event` の `todo.updated` は fire-and-forget のため再接続中の更新を
    /// 取りこぼす可能性がある。接続確立時にこのメソッドで再同期する。
    pub fn get_todos(&self, session_id: &str) -> anyhow::Result<Vec<TodoInfo>> {
        let url = format!("{}/session/{}/todo", self.base_url, session_id);
        let response = self.client.get(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get todos: {} - {}", status, text);
        }
        Ok(response.json::<Vec<TodoInfo>>()?)
    }

    /// `/lsp` はサーバ側で未実装（常に 501）のため、型付きレスポンスへの変換は行わない。
    /// 501 は空結果へフォールバックし、それ以外の通信失敗は従来どおり Err を返す。
    pub fn get_lsp_servers(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/lsp", self.base_url);
        let response = self.client.get(&url).send()?;
        let http_status = response.status();
        if is_not_implemented(http_status) {
            // 501 = 明示的な unsupported。一覧系は空結果へフォールバックする。
            return Ok(Vec::new());
        }
        if !http_status.is_success() {
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get LSP status: {} - {}", http_status, text);
        }
        // 到達不能な成功パス。解析すべきレスポンス型は存在しないため空結果を返す。
        Ok(Vec::new())
    }

    /// `/formatter` はサーバ側で未実装（常に 501）のため、型付きレスポンスへの変換は行わない。
    /// 501 は空結果へフォールバックし、それ以外の通信失敗は従来どおり Err を返す。
    pub fn get_formatters(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/formatter", self.base_url);
        let response = self.client.get(&url).send()?;
        let http_status = response.status();
        if is_not_implemented(http_status) {
            // 501 = 明示的な unsupported。一覧系は空結果へフォールバックする。
            return Ok(Vec::new());
        }
        if !http_status.is_success() {
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get formatter status: {} - {}", http_status, text);
        }
        // 到達不能な成功パス。解析すべきレスポンス型は存在しないため空結果を返す。
        Ok(Vec::new())
    }

    pub fn share_session(&self, session_id: &str) -> anyhow::Result<ShareResponse> {
        let url = format!("{}/session/{}/share", self.base_url, session_id);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to share session `{}`: {} - {}",
                session_id,
                status,
                text
            );
        }
        Ok(response.json::<ShareResponse>()?)
    }

    pub fn unshare_session(&self, session_id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/session/{}/share", self.base_url, session_id);
        let response = self.client.delete(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to unshare session `{}`: {} - {}",
                session_id,
                status,
                text
            );
        }
        // M-52: サーバの unshare_session は `{"unshared": true}` を返す。
        // 旧実装は `success` を読んで unwrap_or(true) に逃げており、
        // キー名の不一致が成功報告として隠れていた。
        let result: UnsharedResponse = response.json()?;
        Ok(result.unshared)
    }

    pub fn compact_session(&self, session_id: &str) -> anyhow::Result<CompactResponse> {
        let url = format!("{}/session/{}/compact", self.base_url, session_id);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to compact session `{}`: {} - {}",
                session_id,
                status,
                text
            );
        }
        Ok(response.json::<CompactResponse>()?)
    }

    pub fn revert_session(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<RevertResponse> {
        let url = format!("{}/session/{}/revert", self.base_url, session_id);
        let request = RevertRequest {
            message_id: message_id.to_string(),
        };
        let response = self.client.post(&url).json(&request).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to revert session `{}`: {} - {}",
                session_id,
                status,
                text
            );
        }
        Ok(response.json::<RevertResponse>()?)
    }

    pub fn fork_session(
        &self,
        session_id: &str,
        message_id: Option<&str>,
    ) -> anyhow::Result<SessionInfo> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(msg_id) = message_id {
            params.push(("message_id", msg_id.to_string()));
        }
        let url = format!("{}/session/{}/fork", self.base_url, session_id);
        let request = if params.is_empty() {
            self.client.post(&url)
        } else {
            self.client.post(&url).query(&params)
        };
        let response = request.send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to fork session `{}`: {} - {}",
                session_id,
                status,
                text
            );
        }
        Ok(response.json::<SessionInfo>()?)
    }

    pub fn set_current_session(&self, session: SessionInfo) {
        let mut current = futures::executor::block_on(self.current_session.write());
        *current = Some(session);
    }

    pub fn get_current_session(&self) -> Option<SessionInfo> {
        let current = futures::executor::block_on(self.current_session.read());
        current.clone()
    }

    pub fn reply_permission(&self, request_id: &str, reply: &str) -> anyhow::Result<()> {
        let url = format!("{}/permission/{}/reply", self.base_url, request_id);
        let body = serde_json::json!({
            "reply": reply,
        });
        let response = self.client.post(&url).json(&body).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to reply to permission request: {} - {}",
                status,
                text
            );
        }
        Ok(())
    }

    pub fn reply_question(&self, request_id: &str, answers: &[Vec<String>]) -> anyhow::Result<()> {
        let url = format!("{}/question/{}/reply", self.base_url, request_id);
        let body = serde_json::json!({
            "answers": answers,
        });
        let response = self.client.post(&url).json(&body).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to reply to question: {} - {}", status, text);
        }
        Ok(())
    }

    pub fn reject_question(&self, request_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/question/{}/reject", self.base_url, request_id);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to reject question: {} - {}", status, text);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- SessionInfo / SessionTimeInfo / SessionRevertInfo ---

    #[test]
    fn session_info_deserializes_all_fields_from_wire_json() {
        let session: SessionInfo = serde_json::from_value(json!({
            "id": "ses-1",
            "slug": "dummy-slug",
            "project_id": "proj-1",
            "directory": "/tmp/dummy",
            "parent_id": "ses-parent",
            "title": "dummy title",
            "version": "v1",
            "time": {
                "created": 1000,
                "updated": 2000,
                "compacting": 1500,
                "archived": null
            },
            "revert": {
                "message_id": "msg-1",
                "part_id": "part-1",
                "snapshot": "snap-1",
                "diff": "dummy diff"
            }
        }))
        .expect("session payload should deserialize");

        assert_eq!(session.id, "ses-1");
        assert_eq!(session.slug, "dummy-slug");
        assert_eq!(session.project_id, "proj-1");
        assert_eq!(session.directory, "/tmp/dummy");
        assert_eq!(session.parent_id.as_deref(), Some("ses-parent"));
        assert_eq!(session.title, "dummy title");
        assert_eq!(session.version, "v1");
        assert_eq!(session.time.created, 1000);
        assert_eq!(session.time.updated, 2000);
        assert_eq!(session.time.compacting, Some(1500));
        assert!(session.time.archived.is_none());

        // SessionInfo は PartialEq を実装しないため、フィールド単位で検証する。
        let revert = session.revert.as_ref().expect("revert should be present");
        assert_eq!(revert.message_id, "msg-1");
        assert_eq!(revert.part_id.as_deref(), Some("part-1"));
        assert_eq!(revert.snapshot.as_deref(), Some("snap-1"));
        assert_eq!(revert.diff.as_deref(), Some("dummy diff"));
    }

    #[test]
    fn session_info_defaults_revert_and_optional_time_fields() {
        let session: SessionInfo = serde_json::from_value(json!({
            "id": "ses-2",
            "slug": "slug-2",
            "project_id": "proj-2",
            "directory": "/tmp/dummy2",
            "parent_id": null,
            "title": "title-2",
            "version": "v2",
            "time": { "created": 1, "updated": 2 }
        }))
        .expect("session without revert should deserialize");

        assert!(session.revert.is_none());
        assert!(session.time.compacting.is_none());
        assert!(session.time.archived.is_none());

        let partial: SessionInfo = serde_json::from_value(json!({
            "id": "ses-3",
            "slug": "slug-3",
            "project_id": "proj-3",
            "directory": "/tmp/dummy3",
            "parent_id": null,
            "title": "title-3",
            "version": "v3",
            "time": { "created": 3, "updated": 4 },
            "revert": { "message_id": "msg-3" }
        }))
        .expect("partial revert should deserialize");

        let revert = partial.revert.as_ref().expect("revert should be present");
        assert_eq!(revert.message_id, "msg-3");
        assert!(revert.part_id.is_none());
        assert!(revert.snapshot.is_none());
        assert!(revert.diff.is_none());
    }

    #[test]
    fn session_info_requires_time_field() {
        let result = serde_json::from_value::<SessionInfo>(json!({
            "id": "ses-x",
            "slug": "slug-x",
            "project_id": "proj-x",
            "directory": "/tmp/x",
            "parent_id": null,
            "title": "title-x",
            "version": "v-x"
        }));
        assert!(result.is_err(), "session without `time` must fail");
    }

    // --- SessionStatusInfo ---

    #[test]
    fn session_status_info_deserializes_and_defaults_optional_fields() {
        let status: SessionStatusInfo = serde_json::from_value(json!({
            "status": "busy",
            "idle": false,
            "busy": true,
            "attempt": 2,
            "message": "dummy message",
            "next": 1234
        }))
        .expect("status should deserialize");

        assert_eq!(status.status, "busy");
        assert!(!status.idle);
        assert!(status.busy);
        assert_eq!(status.attempt, Some(2));
        assert_eq!(status.message.as_deref(), Some("dummy message"));
        assert_eq!(status.next, Some(1234));

        let minimal: SessionStatusInfo = serde_json::from_value(json!({
            "status": "idle",
            "idle": true,
            "busy": false
        }))
        .expect("minimal status should deserialize");

        assert_eq!(minimal.status, "idle");
        assert!(minimal.idle);
        assert!(!minimal.busy);
        assert!(minimal.attempt.is_none());
        assert!(minimal.message.is_none());
        assert!(minimal.next.is_none());
    }

    // --- MessagePart / FileInfo / ToolCall / ToolResult ---

    #[test]
    fn message_part_type_uses_rename_type_on_wire() {
        let part: MessagePart = serde_json::from_value(json!({
            "id": "part-1",
            "type": "text",
            "text": "dummy text",
            "file": null,
            "toolCall": null,
            "toolResult": null
        }))
        .expect("text part should deserialize from the `type` key");

        assert_eq!(part.id, "part-1");
        assert_eq!(part.part_type, "text");
        assert_eq!(part.text.as_deref(), Some("dummy text"));
        assert!(part.file.is_none());
        assert!(part.tool_call.is_none());
        assert!(part.tool_result.is_none());
        assert!(part.synthetic.is_none());
        assert!(part.ignored.is_none());

        // `part_type` は rename = "type" のため、元のフィールド名では読めない。
        assert!(
            serde_json::from_value::<MessagePart>(json!({
                "id": "part-x",
                "part_type": "text"
            }))
            .is_err(),
            "`part_type` must not be accepted as the wire key"
        );

        // シリアライズは rename 後の `type` を出し、`part_type` は出さない。
        let value = serde_json::to_value(&part).expect("part should serialize");
        assert_eq!(value.get("type"), Some(&json!("text")));
        assert!(value.get("part_type").is_none());
    }

    #[test]
    fn message_part_deserializes_tool_call_alias_and_defaults() {
        let part: MessagePart = serde_json::from_value(json!({
            "id": "part-2",
            "type": "tool-call",
            "toolCall": {
                "id": "call-1",
                "name": "dummy-tool",
                "input": { "path": "/tmp/x" }
            },
            "synthetic": true
        }))
        .expect("tool call part should deserialize from the `toolCall` alias");

        assert_eq!(part.part_type, "tool-call");
        assert_eq!(part.synthetic, Some(true));
        assert!(part.ignored.is_none());
        let call = part
            .tool_call
            .as_ref()
            .expect("tool call should be present");
        assert_eq!(call.id, "call-1");
        assert_eq!(call.name, "dummy-tool");
        assert_eq!(call.input, json!({ "path": "/tmp/x" }));
        assert!(part.tool_result.is_none());

        // 元の snake_case キーも受け付ける。
        let snake: MessagePart = serde_json::from_value(json!({
            "id": "part-3",
            "type": "tool-call",
            "tool_call": { "id": "call-2", "name": "dummy", "input": {} }
        }))
        .expect("snake_case tool_call should also deserialize");
        assert_eq!(
            snake.tool_call.as_ref().map(|c| c.id.as_str()),
            Some("call-2")
        );

        // シリアライズは alias ではなく元のフィールド名 `tool_call` を使う。
        let value = serde_json::to_value(&part).expect("part should serialize");
        assert!(value.get("tool_call").is_some());
        assert!(value.get("toolCall").is_none());
    }

    #[test]
    fn message_part_deserializes_tool_result_alias() {
        let part: MessagePart = serde_json::from_value(json!({
            "id": "part-4",
            "type": "tool-result",
            "toolResult": {
                "toolCallId": "call-1",
                "content": "done",
                "isError": false
            },
            "ignored": true
        }))
        .expect("toolResult alias should deserialize");

        assert_eq!(part.part_type, "tool-result");
        assert_eq!(part.ignored, Some(true));
        let result = part
            .tool_result
            .as_ref()
            .expect("tool result should be present");
        assert_eq!(result.tool_call_id, "call-1");
        assert_eq!(result.content, "done");
        assert!(!result.is_error);
        assert!(part.tool_call.is_none());

        let value = serde_json::to_value(&part).expect("part should serialize");
        assert!(value.get("tool_result").is_some());
        assert!(value.get("toolResult").is_none());
    }

    #[test]
    fn message_part_deserializes_file_field() {
        let part: MessagePart = serde_json::from_value(json!({
            "id": "part-5",
            "type": "file",
            "file": {
                "url": "https://example.invalid/dummy.png",
                "filename": "dummy.png",
                "mime": "image/png"
            }
        }))
        .expect("file part should deserialize");

        let file = part.file.as_ref().expect("file should be present");
        assert_eq!(file.url, "https://example.invalid/dummy.png");
        assert_eq!(file.filename, "dummy.png");
        assert_eq!(file.mime, "image/png");
    }

    #[test]
    fn tool_result_uses_camel_case_aliases_and_snake_case_on_serialize() {
        let result: ToolResult = serde_json::from_value(json!({
            "toolCallId": "call-1",
            "content": "dummy output",
            "isError": false
        }))
        .expect("camelCase tool result should deserialize via aliases");

        assert_eq!(result.tool_call_id, "call-1");
        assert_eq!(result.content, "dummy output");
        assert!(!result.is_error);

        let snake: ToolResult = serde_json::from_value(json!({
            "tool_call_id": "call-2",
            "content": "oops",
            "is_error": true
        }))
        .expect("snake_case tool result should deserialize");
        assert_eq!(snake.tool_call_id, "call-2");
        assert!(snake.is_error);

        let value = serde_json::to_value(&result).expect("tool result should serialize");
        assert_eq!(value.get("tool_call_id"), Some(&json!("call-1")));
        assert_eq!(value.get("is_error"), Some(&json!(false)));
        assert!(value.get("toolCallId").is_none());
        assert!(value.get("isError").is_none());
    }

    // --- MessageInfo / MessageTokensInfo ---

    #[test]
    fn message_info_deserializes_aliases_defaults_and_nested_parts() {
        let message: MessageInfo = serde_json::from_value(json!({
            "id": "msg-1",
            "sessionId": "ses-1",
            "role": "assistant",
            "created_at": 1000,
            "completedAt": 2000,
            "agent": "dummy-agent",
            "model": "dummy-model",
            "mode": "primary",
            "finish": "stop",
            "error": null,
            "cost": 0.125,
            "tokens": {
                "input": 10,
                "output": 20,
                "reasoning": 5,
                "cacheRead": 3,
                "cacheWrite": 4
            },
            "parts": [
                { "id": "part-1", "type": "text", "text": "dummy reply" }
            ]
        }))
        .expect("message should deserialize");

        assert_eq!(message.id, "msg-1");
        assert_eq!(message.session_id, "ses-1");
        assert_eq!(message.role, "assistant");
        assert_eq!(message.created_at, 1000);
        assert_eq!(message.completed_at, Some(2000));
        assert_eq!(message.agent.as_deref(), Some("dummy-agent"));
        assert_eq!(message.model.as_deref(), Some("dummy-model"));
        assert_eq!(message.mode.as_deref(), Some("primary"));
        assert_eq!(message.finish.as_deref(), Some("stop"));
        assert!(message.error.is_none());
        assert_eq!(message.cost, 0.125);
        assert_eq!(message.tokens.input, 10);
        assert_eq!(message.tokens.output, 20);
        assert_eq!(message.tokens.reasoning, 5);
        assert_eq!(message.tokens.cache_read, 3);
        assert_eq!(message.tokens.cache_write, 4);
        assert_eq!(message.parts.len(), 1);
        assert_eq!(message.parts[0].part_type, "text");
        assert_eq!(message.parts[0].text.as_deref(), Some("dummy reply"));

        // alias はデシリアライズ専用。シリアライズは snake_case を出す。
        let value = serde_json::to_value(&message).expect("message should serialize");
        assert_eq!(value.get("session_id"), Some(&json!("ses-1")));
        assert!(value.get("sessionId").is_none());
        assert_eq!(value.get("completed_at"), Some(&json!(2000)));
        assert!(value.get("completedAt").is_none());
    }

    #[test]
    fn message_info_accepts_snake_case_keys_and_applies_defaults() {
        let message: MessageInfo = serde_json::from_value(json!({
            "id": "msg-2",
            "session_id": "ses-2",
            "role": "user",
            "created_at": 3000
        }))
        .expect("minimal message should deserialize");

        assert_eq!(message.session_id, "ses-2");
        assert!(message.completed_at.is_none());
        assert!(message.agent.is_none());
        assert!(message.model.is_none());
        assert!(message.mode.is_none());
        assert!(message.finish.is_none());
        assert!(message.error.is_none());
        assert_eq!(message.cost, 0.0);
        assert_eq!(message.tokens.input, 0);
        assert_eq!(message.tokens.cache_read, 0);
        assert!(message.parts.is_empty());
    }

    #[test]
    fn message_info_deserializes_server_list_messages_payload() {
        // サーバ `list_messages`（routes.rs）は `message_to_info` が組み立てた
        // `MessageInfo` を serde 既定の snake_case キーで直列化した配列を返す。
        // 具体的には `session_id` / `created_at` / `completed_at` と、トークンの
        // `cache_read` / `cache_write` 等がそのまま wire キーになる。
        // なお同一 routes.rs の `get_message` が返す
        // `{"info": {"sessionID", "createdAt", ...}, "parts": [...]}` は手組みの
        // 別契約で、この `MessageInfo` のデシリアライズ対象外（`createdAt` の
        // alias も持たない）。
        let messages: Vec<MessageInfo> = serde_json::from_value(json!([
            {
                "id": "msg-list-1",
                "session_id": "ses-list-1",
                "role": "assistant",
                "parts": [
                    {
                        "id": "prt-text",
                        "type": "text",
                        "text": "dummy reply",
                        "file": null,
                        "tool_call": null,
                        "tool_result": null
                    },
                    {
                        "id": "prt-call",
                        "type": "tool_call",
                        "text": null,
                        "file": null,
                        "tool_call": {
                            "id": "call-1",
                            "name": "dummy-tool",
                            "input": { "path": "/tmp/x" }
                        },
                        "tool_result": null
                    },
                    {
                        "id": "prt-result",
                        "type": "tool_result",
                        "text": null,
                        "file": null,
                        "tool_call": null,
                        "tool_result": {
                            "tool_call_id": "call-1",
                            "content": "done",
                            "is_error": false
                        }
                    }
                ],
                "created_at": 1700000000123i64,
                "completed_at": 1700000000456i64,
                "agent": "dummy-agent",
                "model": "dummy-provider/dummy-model",
                "mode": "primary",
                "finish": "stop",
                "error": null,
                "cost": 0.125,
                "tokens": {
                    "input": 10,
                    "output": 20,
                    "reasoning": 5,
                    "cache_read": 3,
                    "cache_write": 4
                }
            },
            {
                "id": "msg-list-2",
                "session_id": "ses-list-1",
                "role": "user",
                "parts": [
                    {
                        "id": "prt-file",
                        "type": "file",
                        "text": null,
                        "file": {
                            "url": "https://example.invalid/dummy.png",
                            "filename": "dummy.png",
                            "mime": "image/png"
                        },
                        "tool_call": null,
                        "tool_result": null
                    }
                ],
                "created_at": 1700000001000i64,
                "completed_at": null,
                "agent": null,
                "model": null,
                "mode": null,
                "finish": null,
                "error": null,
                "cost": 0.0,
                "tokens": {
                    "input": 0,
                    "output": 0,
                    "reasoning": 0,
                    "cache_read": 0,
                    "cache_write": 0
                }
            }
        ]))
        .expect("server list_messages payload should deserialize into Vec<MessageInfo>");

        assert_eq!(messages.len(), 2);

        // MessageInfo / MessagePart 等は PartialEq を実装しないため、フィールド単位で検証する。
        let first = &messages[0];
        assert_eq!(first.id, "msg-list-1");
        assert_eq!(first.session_id, "ses-list-1");
        assert_eq!(first.role, "assistant");
        assert_eq!(first.created_at, 1_700_000_000_123);
        assert_eq!(first.completed_at, Some(1_700_000_000_456));
        assert_eq!(first.agent.as_deref(), Some("dummy-agent"));
        assert_eq!(first.model.as_deref(), Some("dummy-provider/dummy-model"));
        assert_eq!(first.mode.as_deref(), Some("primary"));
        assert_eq!(first.finish.as_deref(), Some("stop"));
        assert!(first.error.is_none());
        assert_eq!(first.cost, 0.125);
        assert_eq!(first.tokens.input, 10);
        assert_eq!(first.tokens.output, 20);
        assert_eq!(first.tokens.reasoning, 5);
        assert_eq!(first.tokens.cache_read, 3);
        assert_eq!(first.tokens.cache_write, 4);

        assert_eq!(first.parts.len(), 3);
        assert_eq!(first.parts[0].part_type, "text");
        assert_eq!(first.parts[0].text.as_deref(), Some("dummy reply"));
        assert!(first.parts[0].file.is_none());
        assert!(first.parts[0].tool_call.is_none());
        assert!(first.parts[0].tool_result.is_none());

        let call = first.parts[1]
            .tool_call
            .as_ref()
            .expect("tool_call part should carry a ToolCall");
        assert_eq!(call.id, "call-1");
        assert_eq!(call.name, "dummy-tool");
        assert_eq!(call.input, json!({ "path": "/tmp/x" }));
        assert!(first.parts[1].tool_result.is_none());

        let result = first.parts[2]
            .tool_result
            .as_ref()
            .expect("tool_result part should carry a ToolResult");
        assert_eq!(result.tool_call_id, "call-1");
        assert_eq!(result.content, "done");
        assert!(!result.is_error);
        assert!(first.parts[2].tool_call.is_none());

        let second = &messages[1];
        assert_eq!(second.id, "msg-list-2");
        assert_eq!(second.session_id, "ses-list-1");
        assert_eq!(second.role, "user");
        assert_eq!(second.created_at, 1_700_000_001_000);
        assert!(second.completed_at.is_none());
        assert!(second.agent.is_none());
        assert!(second.model.is_none());
        assert!(second.mode.is_none());
        assert!(second.finish.is_none());
        assert!(second.error.is_none());
        assert_eq!(second.cost, 0.0);
        assert_eq!(second.tokens.input, 0);
        assert_eq!(second.tokens.output, 0);
        assert_eq!(second.tokens.reasoning, 0);
        assert_eq!(second.tokens.cache_read, 0);
        assert_eq!(second.tokens.cache_write, 0);

        assert_eq!(second.parts.len(), 1);
        assert_eq!(second.parts[0].part_type, "file");
        let file = second.parts[0]
            .file
            .as_ref()
            .expect("file part should carry a FileInfo");
        assert_eq!(file.url, "https://example.invalid/dummy.png");
        assert_eq!(file.filename, "dummy.png");
        assert_eq!(file.mime, "image/png");
    }

    #[test]
    fn message_tokens_info_cache_aliases_and_snake_case_wire_format() {
        let tokens: MessageTokensInfo = serde_json::from_value(json!({
            "input": 1,
            "output": 2,
            "reasoning": 3,
            "cacheRead": 4,
            "cacheWrite": 5
        }))
        .expect("tokens should deserialize from camelCase aliases");

        assert_eq!(tokens.input, 1);
        assert_eq!(tokens.output, 2);
        assert_eq!(tokens.reasoning, 3);
        assert_eq!(tokens.cache_read, 4);
        assert_eq!(tokens.cache_write, 5);

        let value = serde_json::to_value(&tokens).expect("tokens should serialize");
        assert_eq!(value.get("cache_read"), Some(&json!(4)));
        assert_eq!(value.get("cache_write"), Some(&json!(5)));
        assert!(value.get("cacheRead").is_none());
        assert!(value.get("cacheWrite").is_none());

        let empty: MessageTokensInfo =
            serde_json::from_value(json!({})).expect("empty tokens object should deserialize");
        assert_eq!(empty.input, 0);
        assert_eq!(empty.output, 0);
        assert_eq!(empty.reasoning, 0);
        assert_eq!(empty.cache_read, 0);
        assert_eq!(empty.cache_write, 0);
    }

    // --- ProviderListResponse / ProviderModelInfo ---

    #[test]
    fn provider_list_response_default_model_uses_rename_default() {
        let response: ProviderListResponse = serde_json::from_value(json!({
            "providers": [
                {
                    "id": "dummy-provider",
                    "name": "Dummy Provider",
                    "models": [
                        {
                            "id": "dummy-model",
                            "name": "Dummy Model",
                            "provider": "dummy-provider",
                            "contextWindow": 128000
                        },
                        {
                            "id": "dummy-model-2",
                            "name": "Dummy Model 2",
                            "provider": "dummy-provider",
                            "contextLength": 64000
                        }
                    ]
                }
            ],
            "default": { "dummy-provider": "dummy-model" }
        }))
        .expect("provider list should deserialize with the `default` key");

        assert_eq!(response.providers.len(), 1);
        assert_eq!(response.providers[0].id, "dummy-provider");
        assert_eq!(response.providers[0].name, "Dummy Provider");
        assert_eq!(response.providers[0].models.len(), 2);
        let first = &response.providers[0].models[0];
        assert_eq!(first.id, "dummy-model");
        assert_eq!(first.name, "Dummy Model");
        assert_eq!(first.provider, "dummy-provider");
        assert!(first.variants.is_empty());
        assert_eq!(first.context_window, Some(128000));
        let second = &response.providers[0].models[1];
        assert_eq!(second.context_window, Some(64000));
        assert_eq!(
            response
                .default_model
                .get("dummy-provider")
                .map(String::as_str),
            Some("dummy-model")
        );

        let value = serde_json::to_value(&response).expect("provider list should serialize");
        assert_eq!(
            value.get("default"),
            Some(&json!({ "dummy-provider": "dummy-model" }))
        );
        assert!(value.get("default_model").is_none());

        // rename = "default" のため、`default_model` は wire キーとして読めない。
        assert!(
            serde_json::from_value::<ProviderListResponse>(json!({
                "providers": [],
                "default_model": {}
            }))
            .is_err(),
            "`default_model` must not be accepted as the wire key"
        );
    }

    // --- M-52: bool 応答の厳密デシリアライズ（実 HTTP 通信は行わない） ---

    #[test]
    fn deleted_response_requires_bool_deleted_key() {
        let ok: DeletedResponse = serde_json::from_value(json!({ "deleted": true }))
            .expect("deleted response should deserialize");
        assert!(ok.deleted);

        let negative: DeletedResponse = serde_json::from_value(json!({ "deleted": false }))
            .expect("explicit false should deserialize");
        assert!(!negative.deleted);

        // 旧 unwrap_or(true) が成功と見なした入力はすべて Err になること。
        assert!(
            serde_json::from_value::<DeletedResponse>(json!({})).is_err(),
            "missing `deleted` must fail"
        );
        assert!(
            serde_json::from_value::<DeletedResponse>(json!({ "deleted": "true" })).is_err(),
            "non-bool `deleted` must fail"
        );
        assert!(
            serde_json::from_value::<DeletedResponse>(json!({ "success": true })).is_err(),
            "a different key must not silently count as success"
        );
    }

    #[test]
    fn success_response_requires_bool_success_key() {
        let ok: SuccessResponse = serde_json::from_value(json!({ "success": true }))
            .expect("success response should deserialize");
        assert!(ok.success);

        let negative: SuccessResponse = serde_json::from_value(json!({ "success": false }))
            .expect("explicit false should deserialize");
        assert!(!negative.success);

        assert!(
            serde_json::from_value::<SuccessResponse>(json!({})).is_err(),
            "missing `success` must fail"
        );
        assert!(
            serde_json::from_value::<SuccessResponse>(json!({ "success": 1 })).is_err(),
            "non-bool `success` must fail"
        );
    }

    #[test]
    fn unshared_response_uses_the_servers_unshared_key() {
        let ok: UnsharedResponse = serde_json::from_value(json!({ "unshared": true }))
            .expect("unshare response should deserialize via the server wire key");
        assert!(ok.unshared);

        let negative: UnsharedResponse = serde_json::from_value(json!({ "unshared": false }))
            .expect("explicit false should deserialize");
        assert!(!negative.unshared);

        // 旧実装は `success` を読んでいたが、サーバは `unshared` を返す。
        // 誤ったキーでは暗黙に成功させず、解析失敗として扱う。
        assert!(
            serde_json::from_value::<UnsharedResponse>(json!({ "success": true })).is_err(),
            "the old `success` key must not be accepted"
        );
        assert!(
            serde_json::from_value::<UnsharedResponse>(json!({})).is_err(),
            "missing `unshared` must fail"
        );
    }

    // --- 501 (NOT_IMPLEMENTED) を unsupported として扱う（HTTP 通信なし） ---

    #[test]
    fn is_not_implemented_matches_only_501() {
        assert!(is_not_implemented(reqwest::StatusCode::NOT_IMPLEMENTED));

        // 501 以外は unsupported と判定しない（500 等は従来どおり bail する）。
        assert!(!is_not_implemented(reqwest::StatusCode::OK));
        assert!(!is_not_implemented(reqwest::StatusCode::NO_CONTENT));
        assert!(!is_not_implemented(reqwest::StatusCode::NOT_FOUND));
        assert!(!is_not_implemented(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!is_not_implemented(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_not_implemented(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    #[test]
    fn unsupported_error_is_typed_downcastable_and_descriptive() {
        let err: anyhow::Error = unsupported("skill list");

        let typed = err
            .downcast_ref::<UnsupportedEndpointError>()
            .expect("unsupported() must produce a downcastable typed error");
        assert_eq!(typed.operation, "skill list");

        let message = err.to_string();
        assert!(message.contains("skill list"), "message: {message}");
        assert!(message.contains("not supported"), "message: {message}");
        assert!(
            message.contains("501 Not Implemented"),
            "message: {message}"
        );

        // 一般エラー（500 等）は型付きエラーへ誤ってダウンキャストされない。
        let generic = anyhow::anyhow!("Failed to list skills: 500 - boom");
        assert!(generic.downcast_ref::<UnsupportedEndpointError>().is_none());
    }
}
