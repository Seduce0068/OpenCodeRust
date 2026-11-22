use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
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

/// `POST /session/{id}/fork` のリクエストボディ。
///
/// サーバの `ForkSessionRequest { message_id: Option<String> }` と同一形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ForkSessionRequest {
    /// `None` の場合はキーを省略し、空ボディではなく `{}` を送信する。
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertRequest {
    pub message_id: String,
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

/// UI（toast / 描画）側のマスク文字列は角括弧形式 `[redacted]` に固定する。
/// ログ・エラー側（opencode-plugin / opencode-server / opencode-mcp）は山括弧形式
/// `<redacted>` を用いる。共有定数を新設しないスコープのため、各クレートの
/// テストで値を固定して形式の漂移を検出する。
const REDACTED: &str = "[redacted]";
const SENSITIVE_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "authorization",
    "api_key",
    "apikey",
    "credential",
];

/// 既知プレフィックスのみを対象に、トークン値全体を `[redacted]` へ置換する。
///
/// 誤マスクを避けるため、一般の高エントロピー文字列は対象にしない。対象は
/// `sk-`、JWT（`eyJ` で始まる Base64URL 3 セグメント）、GitHub PAT
/// （`ghp_` / `gho_` / `github_pat_`）、AWS アクセスキー（`AKIA` / `ASIA`）、
/// および `token=` / `api_key=` に続く値に限定する。
fn redact_token_patterns(input: &str) -> String {
    #[derive(Clone, Copy)]
    enum Kind {
        /// プレフィックス後は Base64URL / `.` / `-` / `_` の連続。
        Word,
        /// プレフィックス後は区切り文字に達するまでの連続（キーと値が一体の表現）。
        Value,
        /// `eyJ` + Base64URL 3 セグメント。
        Jwt,
        /// `AKIA` / `ASIA` + 大文字数字 16 文字。
        Aws,
    }

    fn is_base64url(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
    }

    fn value_len(rest: &str, kind: Kind) -> usize {
        match kind {
            Kind::Word => rest
                .bytes()
                .take_while(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'-' | b'_')
                })
                .count(),
            Kind::Value => rest
                .bytes()
                .take_while(|byte| {
                    !byte.is_ascii_whitespace()
                        && !matches!(
                            *byte,
                            b'"' | b'\'' | b',' | b';' | b')' | b']' | b'}' | b'&'
                        )
                })
                .count(),
            Kind::Jwt => {
                let mut segments = rest.splitn(3, '.');
                let header = segments.next().unwrap_or("");
                let payload = segments.next().unwrap_or("");
                let signature = segments.next().unwrap_or("");
                if header.is_empty()
                    || payload.is_empty()
                    || signature.is_empty()
                    || !header.bytes().all(is_base64url)
                    || !payload.bytes().all(is_base64url)
                    || !signature.bytes().all(is_base64url)
                {
                    return 0;
                }
                header.len() + 1 + payload.len() + 1 + signature.len()
            }
            Kind::Aws => {
                let len = rest
                    .bytes()
                    .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                    .take(16)
                    .count();
                if len == 16 {
                    len
                } else {
                    0
                }
            }
        }
    }

    // 長いプレフィックスを先に並べ、同位置では最長のものを優先する。
    const PATTERNS: &[(&str, Kind)] = &[
        ("github_pat_", Kind::Word),
        ("api_key=", Kind::Value),
        ("apikey=", Kind::Value),
        ("token=", Kind::Value),
        ("ghp_", Kind::Word),
        ("gho_", Kind::Word),
        ("akia", Kind::Aws),
        ("asia", Kind::Aws),
        ("eyj", Kind::Jwt),
        ("sk-", Kind::Word),
    ];

    let mut out = String::with_capacity(input.len());
    let mut remaining = input;

    while !remaining.is_empty() {
        let lower = remaining.to_ascii_lowercase();
        let mut best: Option<(usize, usize, Kind)> = None;
        for (prefix, kind) in PATTERNS {
            let Some(index) = lower.find(*prefix) else {
                continue;
            };
            let better = match best {
                None => true,
                Some((best_index, best_len, _)) => {
                    index < best_index || (index == best_index && prefix.len() > best_len)
                }
            };
            if better {
                best = Some((index, prefix.len(), *kind));
            }
        }

        let Some((index, prefix_len, kind)) = best else {
            out.push_str(remaining);
            break;
        };

        out.push_str(&remaining[..index]);
        let after_prefix = &remaining[index + prefix_len..];
        let len = value_len(after_prefix, kind);
        if len == 0 {
            // 既知プレフィックスでも値が無い場合はマスクしない（誤マスク回避）。
            out.push_str(&remaining[..index + prefix_len]);
            remaining = after_prefix;
            continue;
        }
        out.push_str(REDACTED);
        remaining = &after_prefix[len..];
    }

    out
}

/// JSON の機微キーの値と、値に埋め込まれた既知トークンをマスクする。
///
/// キー名に依存しない既知プレフィックスのマスクも行うため、ログへトークン値が
/// 出ないようにする（一般の高エントロピー文字列はマスクしない）。
fn redact_sensitive(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let lower = key.to_ascii_lowercase();
                if SENSITIVE_KEY_PARTS.iter().any(|part| lower.contains(*part)) {
                    *child = serde_json::Value::String(REDACTED.to_string());
                } else {
                    redact_sensitive(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_sensitive(item);
            }
        }
        serde_json::Value::String(text) => {
            let redacted = redact_token_patterns(text);
            if redacted != text.as_str() {
                *text = redacted;
            }
        }
        _ => {}
    }
}

/// 改行・制御文字を畳み、既知のトークンをマスクして長さを制限する。
fn truncate_summary(raw: &str, max_chars: usize) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let scrubbed_tokens = redact_token_patterns(&collapsed);

    let mut out = String::with_capacity(scrubbed_tokens.len());
    let mut redact_next = false;
    for word in scrubbed_tokens.split(' ') {
        if redact_next {
            out.push_str(REDACTED);
            out.push(' ');
            redact_next = false;
            continue;
        }
        let lower = word.to_ascii_lowercase();
        if lower == "bearer" {
            out.push_str(REDACTED);
            out.push(' ');
            redact_next = true;
            continue;
        }
        out.push_str(word);
        out.push(' ');
    }
    let scrubbed = out.trim_end();
    if scrubbed.chars().count() <= max_chars {
        return scrubbed.to_string();
    }
    let mut truncated: String = scrubbed.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

/// 共通エラースキーマから `(type, message)` を取り出す。
///
/// `{"error": {"message": ..., "type": ...}}` を優先し、フラットな
/// `{"message": ..., "type": ...}` と `{"error": "..."}` にも対応する。
fn extract_error_fields(body: &str) -> (Option<String>, Option<String>) {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return (None, None),
    };
    let error = value.get("error");

    let message = error
        .and_then(|e| e.get("message"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
        .or_else(|| error.and_then(serde_json::Value::as_str))
        .map(|raw| truncate_summary(raw, 200));
    let kind = error
        .and_then(|e| e.get("type"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("type").and_then(serde_json::Value::as_str))
        .or_else(|| {
            error
                .and_then(|e| e.get("name"))
                .and_then(serde_json::Value::as_str)
        })
        .map(|raw| truncate_summary(raw, 80));
    (kind, message)
}

/// 4xx / 5xx ごとの対処文。
fn status_advice(status: reqwest::StatusCode) -> &'static str {
    if status.is_client_error() {
        " Check the request (parameters, authentication, permissions) and retry."
    } else if status.is_server_error() {
        " The server is temporarily unavailable; retry in a moment."
    } else {
        ""
    }
}

/// HTTP エラーレスポンスを要約して toast に出す一文へ変換する。
///
/// 生ボディはここでログにのみ残し（機微キーはマスク）、利用者には
/// `type` / `message` と status ごとの対処文だけを渡す。
fn error_detail(status: reqwest::StatusCode, body: &str) -> String {
    let (kind, message) = extract_error_fields(body);

    let logged_body = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut value) => {
            redact_sensitive(&mut value);
            value.to_string()
        }
        // プレーンテキストにはトークン等が混在し得るため全文はログに出さない。
        Err(_) => "<non-JSON body omitted>".to_string(),
    };
    tracing::warn!(
        status = %status,
        error_type = ?kind,
        message = ?message,
        body = %truncate_summary(&logged_body, 512),
        "API request failed"
    );

    let detail = match (kind, message) {
        (Some(kind), Some(message)) => format!("[{}] {}", kind, message),
        (None, Some(message)) => message,
        (Some(kind), None) => format!("[{}]", kind),
        (None, None) => "no error details returned".to_string(),
    };
    format!("{} - {}{}", status, detail, status_advice(status))
}

pub struct ApiClient {
    client: Client,
    base_url: String,
    pub current_session: Arc<RwLock<Option<SessionInfo>>>,
}

impl ApiClient {
    /// 認証なしのクライアントを生成する（従来どおりの挙動）。
    pub fn new(base_url: String) -> Self {
        Self::with_auth_token(base_url, None)
    }

    /// `Authorization: Bearer <token>` を全リクエストへ付与するクライアントを生成する。
    ///
    /// `token` が `None` の場合は `new` と同一（ヘッダを付与しない）。
    /// トークン値はログ・エラー・デバッグ出力へ一切含めない。
    pub fn with_auth_token(base_url: String, token: Option<&str>) -> Self {
        // app.rs の SSE URL 生成や cli/main.rs と同じく、末尾の `/` を除いてから保持する。
        // 末尾付き URL をそのまま使うと `format!("{}/session", base_url)` が `//session` になる。
        // 空文字列になった場合はルート `/` を維持し、末尾なし URL はそのまま不変。
        let base_url = {
            let trimmed = base_url.trim_end_matches('/');
            if trimmed.is_empty() {
                "/".to_string()
            } else {
                trimmed.to_string()
            }
        };
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            // SSE 側（app.rs の spawn_server_event_listener）と同一方針:
            // HeaderValue 生成が失敗したら panic せず、トークンを付けずに進む。
            // エラー文言・ログにはトークン値を一切含めない。
            if let Ok(header) = HeaderValue::from_str(&format!("Bearer {token}")) {
                headers.insert(AUTHORIZATION, header);
            } else {
                tracing::warn!("invalid auth token value; skipping Authorization header");
            }
        }
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .default_headers(headers)
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
            anyhow::bail!("Failed to create session: {}", error_detail(status, &text));
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
            anyhow::bail!("Failed to get session: {}", error_detail(status, &text));
        }

        let session: SessionInfo = response.json()?;
        Ok(session)
    }

    /// セッション一覧を取得する（追加フィルタなし、従来どおり）。
    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        self.list_sessions_filtered(None, None)
    }

    /// `search` / `limit` で絞り込む後方互換ラッパー。
    ///
    /// `directory` / `roots` / `start` は `None` を渡し、従来のクエリ形状を維持する。
    pub fn list_sessions_filtered(
        &self,
        search: Option<&str>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        self.list_sessions_with_filter(search, limit, None, None, None)
    }

    /// セッション一覧を pagination / 絞り込み引数付きで取得する。
    ///
    /// 各引数は `Some` のときだけクエリパラメータへ追加される（サーバの
    /// `ListSessionsQuery` と同じ形状）。
    ///
    /// - `search`: trim 後に空でない場合のみ送信（従来どおり）。
    /// - `limit`: `> 0` の場合のみ送信（従来どおり `0` は送らない）。
    /// - `directory`: パス。`Some` のときだけ送信し、ログ・エラーには出さない。
    /// - `roots`: `Some(flag)` のとき `"true"` / `"false"` として送信。
    /// - `start`: `Some` のときだけ送信。
    pub fn list_sessions_with_filter(
        &self,
        search: Option<&str>,
        limit: Option<usize>,
        directory: Option<&str>,
        roots: Option<bool>,
        start: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let url = format!("{}/session", self.base_url);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(search) = search.map(str::trim).filter(|s| !s.is_empty()) {
            params.push(("search", search.to_string()));
        }
        if let Some(limit) = limit.filter(|l| *l > 0) {
            params.push(("limit", limit.to_string()));
        }
        if let Some(directory) = directory {
            params.push(("directory", directory.to_string()));
        }
        if let Some(roots) = roots {
            params.push(("roots", roots.to_string()));
        }
        if let Some(start) = start {
            params.push(("start", start.to_string()));
        }

        let request = if params.is_empty() {
            self.client.get(&url)
        } else {
            self.client.get(&url).query(&params)
        };
        // reqwest の生エラーは完全な URL（directory パスのクエリを含む）を
        // 表示し得るため、固定文言へ変換して伝播させない（R4）。
        let response = request
            .send()
            .map_err(|_| anyhow::anyhow!("Failed to send list sessions request"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to list sessions: {}", error_detail(status, &text));
        }

        let sessions: Vec<SessionInfo> = response.json()?;
        Ok(sessions)
    }

    pub fn get_session_status(&self) -> anyhow::Result<HashMap<String, SessionStatusInfo>> {
        let url = format!("{}/session/status", self.base_url);
        // reqwest の生エラーは完全な URL（パスを含む）を表示し得るため、
        // list_sessions と同じく固定文言へ変換する（R4）。
        let response = self
            .client
            .get(&url)
            .send()
            .map_err(|_| anyhow::anyhow!("Failed to send session status request"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to get session status: {}",
                error_detail(status, &text)
            );
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
                "Failed to update session `{}` title: {}",
                session_id,
                error_detail(status, &text)
            );
        }
        let session: SessionInfo = response.json()?;
        Ok(session)
    }

    /// セッションを削除し、実際に削除が発生したかを返す。
    ///
    // API-04 方針: サーバは存在しないセッションへの `DELETE` に
    // `SessionNotFound`（HTTP 404）で応答する。この API 契約は変更しない。
    // TUI 側の解釈だけを冪等にし、404 を「既に削除済み」として `Ok(false)` に
    // 変換する（再試行時にエラー扱いになって失敗する問題を防ぐ）。
    // 404 以外の非成功ステータスは従来どおり `Err` とする。
    // エラー・ログにはセッション ID / トークン / パスを一切含めない。
    pub fn delete_session(&self, session_id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/session/{}", self.base_url, session_id);
        let response = self.client.delete(&url).send()?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            // 既に削除済み。ボディは内容を見ずに読み捨てる（Keep-Alive の
            // コネクションをプールへ戻すため）。ログ・エラーへは渡さない。
            let _ = response.bytes();
            return Ok(false);
        }
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to delete session: {}", error_detail(status, &text));
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
            anyhow::bail!("Failed to send prompt: {}", error_detail(status, &text));
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
            anyhow::bail!(
                "Failed to execute shell command: {}",
                error_detail(status, &text)
            );
        }

        Ok(response.json::<serde_json::Value>()?)
    }

    pub fn abort_session(&self, session_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/session/{}/abort", self.base_url, session_id);
        let response = self.client.post(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to abort session: {}", error_detail(status, &text));
        }

        Ok(response.json::<serde_json::Value>()?)
    }

    pub fn get_config_providers(&self) -> anyhow::Result<ProviderListResponse> {
        let url = format!("{}/config/providers", self.base_url);

        let response = self.client.get(&url).send()?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to get providers: {}", error_detail(status, &text));
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
            anyhow::bail!("Failed to list agents: {}", error_detail(status, &text));
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
            anyhow::bail!("Failed to list skills: {}", error_detail(status, &text));
        }

        Ok(response.json::<Vec<String>>()?)
    }

    pub fn get_mcp_status(&self) -> anyhow::Result<Vec<McpStatusInfo>> {
        let url = format!("{}/mcp", self.base_url);

        let response = self.client.get(&url).send()?;

        // 同期失敗は固定文言のヘッダで伝わる（値・パスは出ない）。
        if let Some(warning) = response.headers().get("x-mcp-sync-warning") {
            if let Ok(text) = warning.to_str() {
                tracing::warn!(%text, "MCP server list may be stale");
            }
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to fetch MCP status: {}",
                error_detail(status, &text)
            );
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
            anyhow::bail!(
                "Failed to start MCP auth `{}`: {}",
                name,
                error_detail(status, &text)
            );
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
                "Failed to authenticate MCP `{}`: {}",
                name,
                error_detail(status, &text)
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
                "Failed to remove MCP auth `{}`: {}",
                name,
                error_detail(status, &text)
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
            anyhow::bail!(
                "Failed to connect MCP `{}`: {}",
                name,
                error_detail(status, &text)
            );
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
            anyhow::bail!(
                "Failed to disconnect MCP `{}`: {}",
                name,
                error_detail(status, &text)
            );
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
            anyhow::bail!("Failed to get messages: {}", error_detail(status, &text));
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
            anyhow::bail!("Failed to get todos: {}", error_detail(status, &text));
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
            anyhow::bail!(
                "Failed to get LSP status: {}",
                error_detail(http_status, &text)
            );
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
            anyhow::bail!(
                "Failed to get formatter status: {}",
                error_detail(http_status, &text)
            );
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
                "Failed to share session `{}`: {}",
                session_id,
                error_detail(status, &text)
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
                "Failed to unshare session `{}`: {}",
                session_id,
                error_detail(status, &text)
            );
        }
        // M-52: サーバの unshare_session は `{"unshared": true}` を返す。
        // 旧実装は `success` を読んで unwrap_or(true) に逃げており、
        // キー名の不一致が成功報告として隠れていた。
        let result: UnsharedResponse = response.json()?;
        Ok(result.unshared)
    }

    /// `POST /session/{id}/compaction` を呼び、更新後の `SessionInfo` を返す。
    ///
    /// サーバは `/compact` ではなく `/compaction` を登録しており、戻り値も
    /// `{"success": bool}` ではなく `Json<SessionInfo>` である（routes.rs）。
    pub fn compact_session(&self, session_id: &str) -> anyhow::Result<SessionInfo> {
        let url = format!("{}/session/{}/compaction", self.base_url, session_id);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to compact session `{}`: {}",
                session_id,
                error_detail(status, &text)
            );
        }
        Ok(response.json::<SessionInfo>()?)
    }

    /// `POST /session/{id}/revert` を呼び、更新後の `SessionInfo` を返す。
    ///
    /// サーバの `session_revert` は `Json<SessionInfo>` を返すため、
    /// `{"success": bool}` を期待する旧契約では `missing field success` で失敗していた。
    pub fn revert_session(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<SessionInfo> {
        let url = format!("{}/session/{}/revert", self.base_url, session_id);
        let request = RevertRequest {
            message_id: message_id.to_string(),
        };
        let response = self.client.post(&url).json(&request).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to revert session `{}`: {}",
                session_id,
                error_detail(status, &text)
            );
        }
        Ok(response.json::<SessionInfo>()?)
    }

    /// `POST /session/{id}/fork` に JSON ボディを送り、新しい `SessionInfo` を返す。
    ///
    /// サーバはクエリではなく `Json<ForkSessionRequest>` を要求するため、
    /// `message_id` が `None` のときも空ボディではなく `{}` を送信する。
    pub fn fork_session(
        &self,
        session_id: &str,
        message_id: Option<&str>,
    ) -> anyhow::Result<SessionInfo> {
        let url = format!("{}/session/{}/fork", self.base_url, session_id);
        let request = ForkSessionRequest {
            message_id: message_id.map(|msg_id| msg_id.to_string()),
        };
        let response = self.client.post(&url).json(&request).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!(
                "Failed to fork session `{}`: {}",
                session_id,
                error_detail(status, &text)
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
                "Failed to reply to permission request: {}",
                error_detail(status, &text)
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
            anyhow::bail!(
                "Failed to reply to question: {}",
                error_detail(status, &text)
            );
        }
        Ok(())
    }

    pub fn reject_question(&self, request_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/question/{}/reject", self.base_url, request_id);
        let response = self.client.post(&url).send()?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("Failed to reject question: {}", error_detail(status, &text));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- HTTP エラー要約（生ボディを toast に出さない） ---

    #[test]
    fn error_detail_uses_common_error_schema_and_client_advice() {
        let detail = error_detail(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"invalid model id","type":"invalid_request"}}"#,
        );

        assert!(detail.contains("400 Bad Request"), "{detail}");
        assert!(detail.contains("[invalid_request]"), "{detail}");
        assert!(detail.contains("invalid model id"), "{detail}");
        assert!(detail.contains("Check the request"), "{detail}");
        assert!(
            !detail.contains("\""),
            "raw JSON body must not leak: {detail}"
        );
    }

    #[test]
    fn error_detail_supports_flat_schema_and_server_advice() {
        let detail = error_detail(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"message":"database unavailable","type":"internal_error"}"#,
        );

        assert!(detail.contains("500 Internal Server Error"), "{detail}");
        assert!(detail.contains("database unavailable"), "{detail}");
        assert!(detail.contains("retry in a moment"), "{detail}");
    }

    #[test]
    fn error_detail_never_includes_token_values() {
        let detail = error_detail(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"bad token sk-dummy-secret-123","type":"auth","token":"sk-dummy-secret-123"}}"#,
        );

        assert!(detail.contains("401 Unauthorized"), "{detail}");
        assert!(!detail.contains("sk-dummy-secret-123"), "{detail}");
        assert!(detail.contains("[redacted]"), "{detail}");
    }

    /// 回帰テスト（R2）: UI 表示側のマスクは角括弧形式 `[redacted]` に固定する。
    /// ログ側の `<redacted>` に漂移しないことを検出する。
    #[test]
    fn redacted_marker_is_ui_bracket_form() {
        assert_eq!(REDACTED, "[redacted]");
        assert_ne!(REDACTED, "<redacted>");

        let mut value = serde_json::json!({ "token": "sk-dummy-secret-123" });
        redact_sensitive(&mut value);
        assert_eq!(value["token"].as_str(), Some("[redacted]"));
        assert!(!value.to_string().contains("<redacted>"));
    }

    #[test]
    fn error_detail_redacts_jwt_github_pat_and_aws_access_key() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJkdW1teSJ9.ZHVtbXlzaWduYXR1cmViYXNlNjR1cmxfMTIz";
        let pat = "ghp_dummy1234567890abcdefghijklmnopqr";
        let oauth = "gho_dummy1234567890abcdefghijklmnopqr";
        let fine_grained = "github_pat_dummy1234567890_abcdefghijklmnopqrstuvwxyz";
        let aws = "AKIADUMMY12345678901";
        let aws_temp = "ASIADUMMY12345678901";
        let secrets = [jwt, pat, oauth, fine_grained, aws, aws_temp];
        let body = format!(
            r#"{{"error":{{"message":"denied jwt={jwt} pat={pat} oauth={oauth} fine={fine_grained} aws={aws} temp={aws_temp}","type":"auth"}},"hint":"also {jwt}"}}"#
        );

        // toast へ渡る要約にトークン値が出ないこと。
        let detail = error_detail(reqwest::StatusCode::UNAUTHORIZED, &body);
        assert!(detail.contains("401 Unauthorized"), "{detail}");
        for secret in secrets {
            assert!(
                !detail.contains(secret),
                "secret leaked into toast: {detail}"
            );
        }
        assert!(detail.contains(REDACTED), "{detail}");

        // error_detail が tracing::warn! へ渡す本文と同じ経路を検証する。
        let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
        redact_sensitive(&mut value);
        let logged = truncate_summary(&value.to_string(), 512);
        for secret in secrets {
            assert!(
                !logged.contains(secret),
                "secret leaked into log body: {logged}"
            );
        }
    }

    #[test]
    fn redact_token_patterns_keeps_generic_high_entropy_strings() {
        let generic = "aB3dEf7hJk9mNp2qRs5tUv8wXy0z";
        assert_eq!(redact_token_patterns(generic), generic);

        // 既知プレフィックスが無い文字列は、長くてもマスクしない。
        let long = "Q7xKp2mVn9Ld4Rf8Ws1Yt6Zh3Bc5Jg0Na";
        assert_eq!(redact_token_patterns(long), long);
    }

    #[test]
    fn error_detail_falls_back_for_non_json_body() {
        let detail = error_detail(reqwest::StatusCode::BAD_GATEWAY, "upstream went away");

        assert!(detail.contains("502 Bad Gateway"), "{detail}");
        assert!(detail.contains("no error details returned"), "{detail}");
        assert!(!detail.contains("upstream went away"), "{detail}");
    }

    #[test]
    fn error_detail_accepts_plain_error_string() {
        let detail = error_detail(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":"permission denied"}"#,
        );

        assert!(detail.contains("403 Forbidden"), "{detail}");
        assert!(detail.contains("permission denied"), "{detail}");
    }

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
        // 同一 routes.rs の `get_message` も `message_to_info` 経由で同じ
        // snake_case `MessageInfo` を返す（旧 `{"info","parts"}` 外包形状は廃止済み）。
        // そのため単一取得・一覧取得のどちらもこの型でデシリアライズできる。
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

    // --- attach の password が Authorization: Bearer として届くことの回帰テスト ---
    // loopback モックへ実際にリクエストを送り、ヘッダの有無を検証する（外部送信なし）。

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, Once};
    use std::thread;
    use std::time::{Duration, Instant};

    fn ensure_no_proxy() {
        static SET_NO_PROXY: Once = Once::new();
        SET_NO_PROXY.call_once(|| {
            std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
            std::env::set_var("no_proxy", "127.0.0.1,localhost");
        });
    }

    /// 1 リクエストを受け取り、ヘッダを記録して空 JSON 配列を返すモック。
    struct HeaderMock {
        base_url: String,
        seen: Arc<Mutex<Vec<String>>>,
        thread: thread::JoinHandle<()>,
    }

    impl HeaderMock {
        fn spawn() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
            let addr = listener.local_addr().expect("loopback addr must succeed");
            let base_url = format!("http://{addr}");
            let seen = Arc::new(Mutex::new(Vec::new()));
            let seen_for_thread = Arc::clone(&seen);
            let thread = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("mock listener must be nonblocking");
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                panic!("mock timed out waiting for a connection");
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(err) => panic!("mock accept failed: {err}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("mock stream read timeout");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let read = stream.read(&mut chunk).expect("mock must read request");
                    assert!(read > 0, "unexpected EOF while reading request headers");
                    buf.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                seen_for_thread
                    .lock()
                    .expect("mock seen lock")
                    .push(String::from_utf8_lossy(&buf[..header_end]).into_owned());
                let body = "[]";
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(headers.as_bytes())
                    .expect("mock must write headers");
                stream
                    .write_all(body.as_bytes())
                    .expect("mock must write body");
            });
            Self {
                base_url,
                seen,
                thread,
            }
        }

        fn request(self) -> String {
            self.thread.join().expect("mock thread must not panic");
            let seen = self.seen.lock().expect("mock seen lock");
            assert_eq!(seen.len(), 1, "exactly one request must be received");
            seen[0].clone()
        }
    }

    #[test]
    fn with_auth_token_sends_bearer_authorization_header() {
        ensure_no_proxy();
        let mock = HeaderMock::spawn();
        let client = ApiClient::with_auth_token(mock.base_url.clone(), Some("test-token-123"));

        let agents = client
            .list_agents()
            .expect("mock must return an empty agent list");
        assert!(agents.is_empty());

        let request = mock.request();
        assert!(
            request.lines().any(|line| line
                .trim()
                .eq_ignore_ascii_case("authorization: Bearer test-token-123")),
            "request must carry the bearer token: {request}"
        );
    }

    #[test]
    fn new_without_token_sends_no_authorization_header() {
        ensure_no_proxy();
        let mock = HeaderMock::spawn();
        let client = ApiClient::new(mock.base_url.clone());

        let agents = client
            .list_agents()
            .expect("mock must return an empty agent list");
        assert!(agents.is_empty());

        let request = mock.request();
        assert!(
            !request
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("authorization:")),
            "request must not carry an authorization header: {request}"
        );
    }

    #[test]
    fn with_auth_token_does_not_panic_on_invalid_header_values() {
        // CR/LF・NUL 等の制御文字を含むトークンは HeaderValue::from_str が拒否する。
        // panic せずにクライアントを生成できること（SSE 側と同一方針）を検証する。
        // ここでは外部送信せず、生成結果の保持のみを確認する。
        let invalid_tokens = [
            "bad\nvalue",
            "bad\rvalue",
            "bad\0value",
            "bad\r\ninjected: 1",
        ];
        for token in invalid_tokens {
            let client = ApiClient::with_auth_token("http://127.0.0.1:1".to_string(), Some(token));
            assert_eq!(client.base_url, "http://127.0.0.1:1");
        }
    }

    #[test]
    fn with_auth_token_skips_header_for_invalid_value() {
        // 不正値では panic せず、Authorization ヘッダを付けずにリクエストが送られる。
        ensure_no_proxy();
        let mock = HeaderMock::spawn();
        let client = ApiClient::with_auth_token(mock.base_url.clone(), Some("bad\r\ninjected: 1"));

        let agents = client
            .list_agents()
            .expect("invalid token must still allow the request to be sent");
        assert!(agents.is_empty());

        let request = mock.request();
        assert!(
            !request
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("authorization:")),
            "invalid token must not produce an Authorization header: {request}"
        );
    }

    // --- R2: base_url 末尾スラッシュ正規化の回帰テスト（loopback のみ） ---

    #[test]
    fn with_auth_token_trims_trailing_slash_from_base_url() {
        let client = ApiClient::with_auth_token("http://127.0.0.1:8080/api/".to_string(), None);
        assert_eq!(client.base_url, "http://127.0.0.1:8080/api");
    }

    #[test]
    fn with_auth_token_keeps_url_without_trailing_slash_unchanged() {
        let client = ApiClient::with_auth_token("http://127.0.0.1:8080/api".to_string(), None);
        assert_eq!(client.base_url, "http://127.0.0.1:8080/api");
    }

    #[test]
    fn with_auth_token_keeps_root_slash_when_all_slashes_trimmed() {
        let client = ApiClient::with_auth_token("///".to_string(), None);
        assert_eq!(client.base_url, "/");
    }

    #[test]
    fn trailing_slash_base_url_does_not_double_api_path() {
        ensure_no_proxy();
        let mock = HeaderMock::spawn();
        let client = ApiClient::with_auth_token(format!("{}/", mock.base_url.clone()), None);

        let agents = client
            .list_agents()
            .expect("mock must return an empty agent list");
        assert!(agents.is_empty());

        let request = mock.request();
        let request_line = request
            .lines()
            .next()
            .expect("request must have a request line");
        assert!(
            request_line.starts_with("GET /agent "),
            "trailing slash must not produce a doubled path: {request_line}"
        );
        assert!(
            !request_line.contains("//agent"),
            "API path must not contain a double slash: {request_line}"
        );
    }

    // --- R2: compact / revert / fork の API 契約回帰テスト（loopback のみ） ---

    /// サーバ `session_to_info` が返す SessionInfo の wire 形状。
    const SERVER_SESSION_JSON: &str = r#"{
        "id":"ses-1",
        "slug":"dummy-slug",
        "project_id":"proj-1",
        "directory":"/tmp/dummy",
        "parent_id":null,
        "title":"dummy title",
        "version":"v1",
        "time":{"created":1000,"updated":2000},
        "revert":{"message_id":"msg-1"}
    }"#;

    #[test]
    fn compact_and_revert_response_shape_is_session_info_not_success_flag() {
        let session: SessionInfo = serde_json::from_str(SERVER_SESSION_JSON)
            .expect("server-shaped SessionInfo must deserialize");
        assert_eq!(session.id, "ses-1");
        assert_eq!(
            session.revert.as_ref().map(|r| r.message_id.as_str()),
            Some("msg-1")
        );

        // 旧契約の `{"success": true}` は SessionInfo として解析失敗すること。
        assert!(
            serde_json::from_value::<SessionInfo>(json!({ "success": true })).is_err(),
            "legacy success-flag payload must not be treated as SessionInfo"
        );
    }

    #[test]
    fn fork_session_request_serializes_to_server_shape() {
        let with_id = ForkSessionRequest {
            message_id: Some("msg-1".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&with_id).expect("fork request should serialize"),
            json!({ "message_id": "msg-1" })
        );

        let without_id = ForkSessionRequest { message_id: None };
        assert_eq!(
            serde_json::to_value(&without_id).expect("empty fork request should serialize"),
            json!({})
        );

        // サーバ側も Option<String> なので `{}` と明示 null の両方を受け付ける。
        // TUI 側はキーを丸ごと省くため、空ボディにはならない。
        let from_empty: ForkSessionRequest =
            serde_json::from_value(json!({})).expect("server accepts `{}`");
        assert!(from_empty.message_id.is_none());
        let from_null: ForkSessionRequest = serde_json::from_value(json!({ "message_id": null }))
            .expect("server accepts explicit null");
        assert!(from_null.message_id.is_none());
    }

    // --- loopback モック: メソッド・パス・ボディを記録し、サーバ形式 JSON を返す ---

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        request_line: String,
        body: String,
    }

    struct RecordingMock {
        base_url: String,
        seen: Arc<Mutex<Vec<RecordedRequest>>>,
        thread: thread::JoinHandle<()>,
    }

    impl RecordingMock {
        fn spawn(response_body: &'static str) -> Self {
            Self::spawn_with_status("200 OK", response_body)
        }

        /// 任意のステータス行で応答するモック（loopback のみ、外部送信なし）。
        fn spawn_with_status(status_line: &'static str, response_body: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
            let addr = listener.local_addr().expect("loopback addr must succeed");
            let base_url = format!("http://{addr}");
            let seen = Arc::new(Mutex::new(Vec::new()));
            let seen_for_thread = Arc::clone(&seen);
            let thread = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("mock listener must be nonblocking");
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                panic!("mock timed out waiting for a connection");
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(err) => panic!("mock accept failed: {err}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("mock stream read timeout");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (header_end, content_len) = loop {
                    let read = stream.read(&mut chunk).expect("mock must read request");
                    assert!(read > 0, "unexpected EOF while reading request");
                    buf.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let end = pos + 4;
                        let headers = String::from_utf8_lossy(&buf[..end]);
                        let mut len = 0usize;
                        for line in headers.lines() {
                            let lower = line.to_ascii_lowercase();
                            if let Some(value) = lower.strip_prefix("content-length:") {
                                len = value.trim().parse().expect("valid content length");
                            }
                        }
                        break (end, len);
                    }
                };
                while buf.len() < header_end + content_len {
                    let read = stream.read(&mut chunk).expect("mock must read body");
                    assert!(read > 0, "unexpected EOF while reading body");
                    buf.extend_from_slice(&chunk[..read]);
                }
                let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                let request_line = headers.lines().next().unwrap_or("").to_string();
                let body = String::from_utf8_lossy(&buf[header_end..header_end + content_len])
                    .into_owned();
                seen_for_thread
                    .lock()
                    .expect("mock seen lock")
                    .push(RecordedRequest { request_line, body });

                let response_headers = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                );
                stream
                    .write_all(response_headers.as_bytes())
                    .expect("mock must write headers");
                stream
                    .write_all(response_body.as_bytes())
                    .expect("mock must write body");
            });
            Self {
                base_url,
                seen,
                thread,
            }
        }

        fn request(self) -> RecordedRequest {
            self.thread.join().expect("mock thread must not panic");
            let seen = self.seen.lock().expect("mock seen lock");
            assert_eq!(seen.len(), 1, "exactly one request must be received");
            seen[0].clone()
        }
    }

    #[test]
    fn compact_session_posts_to_compaction_and_reads_session_info() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn(SERVER_SESSION_JSON);
        let client = ApiClient::new(mock.base_url.clone());

        let session = client
            .compact_session("ses-1")
            .expect("compact response must deserialize into SessionInfo");
        assert_eq!(session.id, "ses-1");
        assert_eq!(session.title, "dummy title");

        let request = mock.request();
        assert!(
            request
                .request_line
                .starts_with("POST /session/ses-1/compaction "),
            "compact must POST to /compaction: {}",
            request.request_line
        );
    }

    #[test]
    fn revert_session_posts_message_id_and_reads_session_info() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn(SERVER_SESSION_JSON);
        let client = ApiClient::new(mock.base_url.clone());

        let session = client
            .revert_session("ses-1", "msg-1")
            .expect("revert response must deserialize into SessionInfo");
        assert_eq!(session.id, "ses-1");

        let request = mock.request();
        assert!(
            request
                .request_line
                .starts_with("POST /session/ses-1/revert "),
            "revert must POST to /revert: {}",
            request.request_line
        );
        let body: serde_json::Value =
            serde_json::from_str(&request.body).expect("revert body must be JSON");
        assert_eq!(body["message_id"], json!("msg-1"));
    }

    #[test]
    fn fork_session_sends_json_body_and_reads_session_info() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn(SERVER_SESSION_JSON);
        let client = ApiClient::new(mock.base_url.clone());

        let session = client
            .fork_session("ses-1", Some("msg-9"))
            .expect("fork response must deserialize into SessionInfo");
        assert_eq!(session.id, "ses-1");

        let request = mock.request();
        assert!(
            request
                .request_line
                .starts_with("POST /session/ses-1/fork "),
            "fork must POST to /fork: {}",
            request.request_line
        );
        assert!(
            !request.request_line.contains("message_id="),
            "message_id must be a JSON body, not a query param: {}",
            request.request_line
        );
        let body: serde_json::Value =
            serde_json::from_str(&request.body).expect("fork body must be JSON");
        assert_eq!(body["message_id"], json!("msg-9"));
    }

    #[test]
    fn fork_session_without_message_id_sends_empty_json_object() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn(SERVER_SESSION_JSON);
        let client = ApiClient::new(mock.base_url.clone());

        client
            .fork_session("ses-1", None)
            .expect("fork without message_id must still succeed");

        let request = mock.request();
        let body: serde_json::Value =
            serde_json::from_str(&request.body).expect("fork body must be JSON");
        assert_eq!(body, json!({}), "empty body must not be sent");
    }

    // --- API-04: DELETE の 404 を冪等な「既に削除済み」として扱う回帰テスト ---
    // サーバ契約（SessionNotFound = 404）は変更せず、TUI 側の解釈のみを検証する。

    #[test]
    fn delete_session_returns_ok_false_on_404() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn_with_status(
            "404 Not Found",
            r#"{"error":{"type":"SessionNotFound","message":"session not found"}}"#,
        );
        let client = ApiClient::new(mock.base_url.clone());

        let deleted = client
            .delete_session("ses-missing")
            .expect("404 must be interpreted as already deleted, not an error");
        assert!(!deleted, "404 must map to Ok(false)");

        let request = mock.request();
        assert!(
            request
                .request_line
                .starts_with("DELETE /session/ses-missing "),
            "delete must DELETE /session/{{id}}: {}",
            request.request_line
        );
    }

    #[test]
    fn delete_session_still_errors_on_500_without_leaking_session_id() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn_with_status(
            "500 Internal Server Error",
            r#"{"error":{"type":"internal_error","message":"boom"}}"#,
        );
        let client = ApiClient::new(mock.base_url.clone());

        let err = client
            .delete_session("ses-secret-id")
            .expect_err("500 must remain an error");
        let message = err.to_string();
        assert!(
            message.contains("500 Internal Server Error"),
            "message: {message}"
        );
        assert!(
            !message.contains("ses-secret-id"),
            "session id must not leak into error: {message}"
        );
        assert!(
            !message.contains("/session/"),
            "path must not leak into error: {message}"
        );
        let _request = mock.request();
    }

    #[test]
    fn delete_session_parses_deleted_response_on_200() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn_with_status("200 OK", r#"{"deleted":true}"#);
        let client = ApiClient::new(mock.base_url.clone());

        let deleted = client
            .delete_session("ses-1")
            .expect("200 must parse DeletedResponse");
        assert!(deleted, "`deleted: true` must map to Ok(true)");

        let request = mock.request();
        assert!(
            request.request_line.starts_with("DELETE /session/ses-1 "),
            "delete must DELETE /session/{{id}}: {}",
            request.request_line
        );
    }

    // --- R2 (API-08): セッション一覧の pagination 引数露出の回帰テスト（loopback のみ） ---

    /// テスト専用の percent-decode（`+` は空白として扱う）。
    fn percent_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i + 2 < bytes.len() => {
                    let hi = (bytes[i + 1] as char).to_digit(16);
                    let lo = (bytes[i + 2] as char).to_digit(16);
                    if let (Some(hi), Some(lo)) = (hi, lo) {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                    } else {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                b => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// リクエスト行からクエリを取り出し、デコード済みの (key, value) 組を返す。
    fn request_query(request_line: &str) -> Vec<(String, String)> {
        let target = request_line.split_whitespace().nth(1).unwrap_or("");
        let Some((_, query)) = target.split_once('?') else {
            return Vec::new();
        };
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (percent_decode(key), percent_decode(value))
            })
            .collect()
    }

    fn has_param(query: &[(String, String)], key: &str, value: &str) -> bool {
        query.iter().any(|(k, v)| k == key && v == value)
    }

    fn has_key(query: &[(String, String)], key: &str) -> bool {
        query.iter().any(|(k, _)| k == key)
    }

    #[test]
    fn list_sessions_with_filter_sends_all_query_params() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn("[]");
        let client = ApiClient::new(mock.base_url.clone());

        let sessions = client
            .list_sessions_with_filter(
                Some("keyword"),
                Some(30),
                Some("/home/user/my dir"),
                Some(true),
                Some(42),
            )
            .expect("mock must return an empty session list");
        assert!(sessions.is_empty());

        let request = mock.request();
        assert!(
            request.request_line.starts_with("GET /session?"),
            "list must GET /session with query: {}",
            request.request_line
        );
        let query = request_query(&request.request_line);
        assert!(has_param(&query, "search", "keyword"), "{query:?}");
        assert!(has_param(&query, "limit", "30"), "{query:?}");
        assert!(
            has_param(&query, "directory", "/home/user/my dir"),
            "{query:?}"
        );
        assert!(has_param(&query, "roots", "true"), "{query:?}");
        assert!(has_param(&query, "start", "42"), "{query:?}");
    }

    #[test]
    fn list_sessions_with_filter_serializes_false_and_omits_unset_params() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn("[]");
        let client = ApiClient::new(mock.base_url.clone());

        client
            .list_sessions_with_filter(None, Some(0), None, Some(false), None)
            .expect("mock must return an empty session list");

        let query = request_query(&mock.request().request_line);
        assert!(has_param(&query, "roots", "false"), "{query:?}");
        assert!(
            !has_key(&query, "limit"),
            "limit=0 must stay omitted: {query:?}"
        );
        for key in ["search", "directory", "start"] {
            assert!(!has_key(&query, key), "{key} must stay omitted: {query:?}");
        }

        // 全引数 None のときはクエリを付けない（list_sessions と同一の wire format）。
        let mock_none = RecordingMock::spawn("[]");
        let client_none = ApiClient::new(mock_none.base_url.clone());
        client_none
            .list_sessions_with_filter(None, None, None, None, None)
            .expect("mock must return an empty session list");
        let line = mock_none.request().request_line;
        assert!(
            line.starts_with("GET /session "),
            "no query params must be appended: {line}"
        );
    }

    #[test]
    fn list_sessions_filtered_keeps_legacy_query_shape() {
        ensure_no_proxy();
        let mock = RecordingMock::spawn("[]");
        let client = ApiClient::new(mock.base_url.clone());

        client
            .list_sessions_filtered(Some("  spaced  "), Some(7))
            .expect("mock must return an empty session list");

        let query = request_query(&mock.request().request_line);
        assert!(has_param(&query, "search", "spaced"), "{query:?}");
        assert!(has_param(&query, "limit", "7"), "{query:?}");
        for key in ["directory", "roots", "start"] {
            assert!(
                !has_key(&query, key),
                "legacy wrapper must not send {key}: {query:?}"
            );
        }

        // list_sessions() は従来どおり無クエリ。
        let mock_plain = RecordingMock::spawn("[]");
        let client_plain = ApiClient::new(mock_plain.base_url.clone());
        client_plain
            .list_sessions()
            .expect("mock must return an empty session list");
        let line = mock_plain.request().request_line;
        assert!(
            line.starts_with("GET /session "),
            "list_sessions must not append a query: {line}"
        );
    }

    #[test]
    fn list_sessions_with_filter_does_not_leak_directory_into_errors() {
        ensure_no_proxy();
        let secret_dir = "/home/secret/project";
        let mock = RecordingMock::spawn_with_status(
            "500 Internal Server Error",
            r#"{"error":{"type":"internal_error","message":"boom"}}"#,
        );
        let client = ApiClient::new(mock.base_url.clone());

        let err = client
            .list_sessions_with_filter(None, None, Some(secret_dir), None, None)
            .expect_err("500 must remain an error");
        let message = err.to_string();
        assert!(
            message.contains("500 Internal Server Error"),
            "message: {message}"
        );
        assert!(
            !message.contains(secret_dir),
            "directory path must not leak into error: {message}"
        );
        assert!(
            !message.contains("directory="),
            "query must not leak into error: {message}"
        );
        let _ = mock.request();
    }
}
