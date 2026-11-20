use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::{ChatRequest, ChatResponse, StreamResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub cost_per_million_input: f64,
    pub cost_per_million_output: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: HashMap<String, ModelInfo>,
    pub source: String,
    pub options: HashMap<String, serde_json::Value>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    fn models(&self) -> Vec<ModelInfo>;
    fn get_model(&self, id: &str) -> Option<&ModelInfo>;

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError>;
    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("API error (status {status_code}): {message}")]
    ApiErrorWithStatus { message: String, status_code: u16 },

    /// HTTP ステータスに加えて、レスポンスヘッダと本文 preview を構造化保持する API エラー。
    ///
    /// `Retry-After` を `opencode-session::retry::delay` へ接続するため、
    /// ヘッダが判明している呼び出し側は [`ProviderError::api_error_with_headers`]
    /// で構築する。既存の `ApiErrorWithStatus` / `api_error_from_status` の形は
    /// 変更しないため、従来の呼び出し側はそのままコンパイルできる。
    #[error("API error (status {status_code}): {message}")]
    ApiErrorWithHeaders {
        message: String,
        status_code: u16,
        response_headers: Option<HashMap<String, String>>,
        response_body: Option<String>,
    },

    #[error("API error: {0}")]
    ApiError(String),

    /// 下位エラーを `source()` チェーンとして保持する API エラー。
    #[error("API error: {message}")]
    ApiErrorWithSource {
        /// 利用者向けの説明文。
        message: String,
        /// 保持された下位エラー。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Network error: {0}")]
    NetworkError(String),

    /// 下位エラーを `source()` チェーンとして保持するネットワークエラー。
    #[error("Network error: {message}")]
    NetworkErrorWithSource {
        /// 利用者向けの説明文。
        message: String,
        /// 保持された下位エラー。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Authentication error: {0}")]
    AuthError(String),

    #[error("Rate limit exceeded")]
    RateLimit,

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Stream error: {0}")]
    StreamError(String),

    #[error("Timeout")]
    Timeout,

    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Context overflow: {0}")]
    ContextOverflow(String),
}

impl crate::retry::IsRetryable for ProviderError {
    fn is_retryable(&self) -> Option<String> {
        match self {
            ProviderError::RateLimit => Some("Rate limited".to_string()),
            ProviderError::Timeout => Some("Request timed out".to_string()),
            ProviderError::NetworkError(msg) => Some(format!("Network error: {msg}")),
            ProviderError::NetworkErrorWithSource { message, .. } => {
                Some(format!("Network error: {message}"))
            }
            ProviderError::ApiErrorWithStatus {
                status_code,
                message,
            }
            | ProviderError::ApiErrorWithHeaders {
                status_code,
                message,
                ..
            } => {
                if is_status_retryable("", *status_code) {
                    Some(format!("API error {status_code}: {message}"))
                } else {
                    None
                }
            }
            // source が reqwest の一時エラー（connect / timeout / request）なら
            // リトライ可。下位エラーの本文や URL はメッセージへ含めない。
            ProviderError::ApiErrorWithSource { message, source } => {
                if source_is_transient_reqwest_error(source.as_ref()) {
                    Some(format!("API error: {message}"))
                } else {
                    None
                }
            }
            ProviderError::ApiError(_)
            | ProviderError::AuthError(_)
            | ProviderError::ModelNotFound(_)
            | ProviderError::InvalidRequest(_)
            | ProviderError::StreamError(_)
            | ProviderError::ProviderNotFound(_)
            | ProviderError::ConfigError(_)
            | ProviderError::ContextOverflow(_) => None,
        }
    }
}

/// ステータスコードに基づくリトライ可否の単一情報源。
///
/// `IsRetryable::is_retryable` と `parse_api_call_error_with_headers` の双方が
/// この関数を呼び、経路による判定の分岐を防ぐ。`provider_id` は呼び出し側の
/// 文脈を保つため引数として残すが、判定は全プロバイダ共通である。
/// 従前 OpenAI のみ 404 をリトライ可としていた特例は廃止して統一した。
pub fn is_status_retryable(provider_id: &str, status_code: u16) -> bool {
    let _ = provider_id; // 全プロバイダ共通のステータス集合へ統一（404 特例なし）。
    matches!(status_code, 429 | 500 | 502 | 503 | 504)
}

/// `ApiErrorWithSource` の下位エラーが reqwest の一時エラーかを判定する。
///
/// `reqwest::Error` へ downcast し、`is_connect` / `is_timeout` / `is_request`
/// のいずれかが真なら一時エラーとしてリトライ可とする。それ以外の
/// （`std::io::Error` や HTTP ステータス由来の）エラーはリトライ可にしない。
fn source_is_transient_reqwest_error(
    source: &(dyn std::error::Error + Send + Sync + 'static),
) -> bool {
    source
        .downcast_ref::<reqwest::Error>()
        .is_some_and(|error| error.is_connect() || error.is_timeout() || error.is_request())
}

static OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)prompt is too long").unwrap(),
        Regex::new(r"(?i)input is too long for requested model").unwrap(),
        Regex::new(r"(?i)exceeds the context window").unwrap(),
        Regex::new(r"(?i)input token count.*exceeds the maximum").unwrap(),
        Regex::new(r"(?i)maximum prompt length is \d+").unwrap(),
        Regex::new(r"(?i)reduce the length of the messages").unwrap(),
        Regex::new(r"(?i)maximum context length is \d+ tokens").unwrap(),
        Regex::new(r"(?i)exceeds the limit of \d+").unwrap(),
        Regex::new(r"(?i)exceeds the available context size").unwrap(),
        Regex::new(r"(?i)greater than the context length").unwrap(),
        Regex::new(r"(?i)context window exceeds limit").unwrap(),
        Regex::new(r"(?i)exceeded model token limit").unwrap(),
        Regex::new(r"(?i)context[_ ]length[_ ]exceeded").unwrap(),
    ]
});

static NO_BODY_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^4(00|13)\s*(status code)?\s*\(no body\)").unwrap());

/// credential クエリパラメータの値をエラー文字列から伏せる中央ヘルパ。
///
/// reqwest エラーやレスポンス本文 preview にはリクエスト URL が含まれることがあり、
/// `?key=` / `&api_key=` / `?token=` 等の形で秘密がログへ流出する。すべての
/// `ProviderError` 生成経路はこの関数を通し、値を `<redacted>` へ置換する。
/// パラメータ名・ホスト・それ以外のクエリは保持し、障害追跡性を落とさない。
pub(crate) fn redact_credential_query_params(input: &str) -> String {
    static CREDENTIAL_QUERY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?i)([?&](?:x-goog-api-key|x-api-key|x-auth-token|api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|client[_-]?secret|authorization|sig|signature|key|token|secret|code)=)[^&\s#"<>]+"#,
        )
        .expect("credential query regex must compile")
    });

    CREDENTIAL_QUERY
        .replace_all(input, "$1<redacted>")
        .into_owned()
}

impl ProviderError {
    /// API エラーを、下位エラーを `source()` に保持した状態で構築する。
    pub fn api_error_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        ProviderError::ApiErrorWithSource {
            message: redact_credential_query_params(&message.into()),
            source: Box::new(source),
        }
    }

    /// ネットワークエラーを、下位エラーを `source()` に保持した状態で構築する。
    pub fn network_error_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        ProviderError::NetworkErrorWithSource {
            message: redact_credential_query_params(&message.into()),
            source: Box::new(source),
        }
    }

    /// HTTP ステータスを構造化して保持する API エラーを構築する共通ヘルパ。
    ///
    /// ステータス付きレスポンスを `ApiError(String)` へ文字列化するとリトライ判定と
    /// `ParsedAPICallError::status_code` が失われるため、ステータスが判明している
    /// エラーは必ずこのヘルパを通す。
    pub fn api_error_with_status(message: impl Into<String>, status_code: u16) -> Self {
        ProviderError::ApiErrorWithStatus {
            message: redact_credential_query_params(&message.into()),
            status_code,
        }
    }

    /// `api_error_with_status` の引数順を HTTP ステータス優先にした別名。
    pub fn api_error_from_status(status_code: u16, message: impl Into<String>) -> Self {
        ProviderError::api_error_with_status(message, status_code)
    }

    /// レスポンスヘッダと本文を構造化保持する API エラーを構築する。
    ///
    /// `response_body` には完全な本文ではなく 500 バイトの preview を渡すこと。
    /// [`parse_api_call_error_with_headers`] が最終的な 500 バイト制限を再適用するため、
    /// 全文が session 側のエラー・ログへ流出することはない。
    pub fn api_error_with_headers(
        status_code: u16,
        message: impl Into<String>,
        response_headers: Option<HashMap<String, String>>,
        response_body: Option<String>,
    ) -> Self {
        ProviderError::ApiErrorWithHeaders {
            message: redact_credential_query_params(&message.into()),
            status_code,
            response_headers,
            response_body: response_body.map(|body| redact_credential_query_params(&body)),
        }
    }

    /// 構造化保持されたレスポンスヘッダを返す。平坦化されたエラーは `None`。
    pub fn response_headers(&self) -> Option<&HashMap<String, String>> {
        match self {
            ProviderError::ApiErrorWithHeaders {
                response_headers, ..
            } => response_headers.as_ref(),
            _ => None,
        }
    }

    /// 構造化保持されたレスポンス本文 preview を返す。平坦化されたエラーは `None`。
    pub fn response_body(&self) -> Option<&str> {
        match self {
            ProviderError::ApiErrorWithHeaders { response_body, .. } => response_body.as_deref(),
            _ => None,
        }
    }

    /// リトライ対象の HTTP ステータス集合を単一の情報源として判定する。
    ///
    /// 実体はモジュール級の [`is_status_retryable`] に集約済み。既存の呼び出し
    /// 互換のために残した薄いラッパーであり、判定ロジックは持たない。
    pub fn is_retryable_status_code(status_code: u16) -> bool {
        is_status_retryable("", status_code)
    }

    /// 構造化されたステータスコードを取り出す。平坦化された `ApiError` は `None`。
    pub fn status_code(&self) -> Option<u16> {
        match self {
            ProviderError::ApiErrorWithStatus { status_code, .. }
            | ProviderError::ApiErrorWithHeaders { status_code, .. } => Some(*status_code),
            _ => None,
        }
    }

    pub fn context_overflow(message: impl Into<String>) -> Self {
        ProviderError::ContextOverflow(redact_credential_query_params(&message.into()))
    }

    pub fn is_overflow(message: &str) -> bool {
        if OVERFLOW_PATTERNS.iter().any(|p| p.is_match(message)) {
            return true;
        }
        NO_BODY_PATTERN.is_match(message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ParsedAPICallError {
    #[serde(rename = "context_overflow")]
    ContextOverflow {
        message: String,
        response_body: Option<String>,
    },
    #[serde(rename = "api_error")]
    ApiError {
        message: String,
        status_code: Option<u16>,
        is_retryable: bool,
        response_headers: Option<HashMap<String, String>>,
        response_body: Option<String>,
        metadata: Option<HashMap<String, String>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ParsedStreamError {
    #[serde(rename = "context_overflow")]
    ContextOverflow {
        message: String,
        response_body: String,
    },
    #[serde(rename = "api_error")]
    ApiError {
        message: String,
        is_retryable: bool,
        response_body: String,
    },
}

pub fn parse_api_call_error(provider_id: &str, error: &ProviderError) -> ParsedAPICallError {
    parse_api_call_error_with_headers(provider_id, error, None)
}

/// `response_headers` / `response_body` を保持した `ParsedAPICallError` を構築する。
///
/// 既往の `parse_api_call_error` は常に `None` を渡していたため、
/// `opencode-session::retry::delay` が `Retry-After` を解釈できなかった。
/// 呼び出し側が明示したヘッダを優先し、未指定なら
/// [`ProviderError::ApiErrorWithHeaders`] の構造化値から取り出す。
/// 本文は 500 バイトの preview に制限して引き継ぐ。
pub fn parse_api_call_error_with_headers(
    provider_id: &str,
    error: &ProviderError,
    response_headers: Option<HashMap<String, String>>,
) -> ParsedAPICallError {
    let message = format_error_message(provider_id, error);

    if ProviderError::is_overflow(&message) {
        return ParsedAPICallError::ContextOverflow {
            message,
            response_body: None,
        };
    }

    let (status_code, is_retryable) = match error {
        ProviderError::ApiErrorWithStatus { status_code, .. }
        | ProviderError::ApiErrorWithHeaders { status_code, .. } => {
            // IsRetryable と同じ単一関数を呼ぶ（OpenAI 404 特例は廃止して統一）。
            (
                Some(*status_code),
                is_status_retryable(provider_id, *status_code),
            )
        }
        ProviderError::RateLimit => (Some(429), true),
        ProviderError::Timeout => (None, true),
        ProviderError::NetworkError(_) => (None, true),
        ProviderError::NetworkErrorWithSource { .. } => (None, true),
        ProviderError::ApiErrorWithSource { source, .. } => {
            (None, source_is_transient_reqwest_error(source.as_ref()))
        }
        _ => (None, false),
    };

    // 呼び出し側が明示したヘッダを優先し、無ければ構造化値から取り出す。
    let response_headers = response_headers.or_else(|| error.response_headers().cloned());
    // 本文全文をログ・エラーへ出さないよう 500 バイト preview に制限する。
    let response_body = error
        .response_body()
        .map(|body| crate::openai::error_body_preview(body, 500));

    ParsedAPICallError::ApiError {
        message,
        status_code,
        is_retryable,
        response_headers,
        response_body,
        metadata: None,
    }
}

fn format_error_message(provider_id: &str, error: &ProviderError) -> String {
    // GitHub Copilot 403 special case
    if provider_id.contains("github-copilot")
        && matches!(
            error,
            ProviderError::ApiErrorWithStatus {
                status_code: 403,
                ..
            } | ProviderError::ApiErrorWithHeaders {
                status_code: 403,
                ..
            }
        )
    {
        return "Please reauthenticate with the copilot provider to ensure your credentials work properly with OpenCode.".to_string();
    }
    // 直接バリアントを構築する呼び出し側（文字列化済みのエラー）もここで
    // credential クエリを落とし、parse 結果・session ログへの漏洩を防ぐ。
    redact_credential_query_params(&error.to_string())
}

/// 構造化エラーへ渡すレスポンスヘッダをサニタイズする共通ヘルパ。
///
/// 全 13 アダプタの `response_error_headers` / `response_header_map` はこの
/// 1 関数へ委譲する。2 段構えで機微情報を構造化エラーへ渡さない:
///
/// 1. **allowlist（主防御）**: リトライ判定・レート制限・リクエスト識別に
///    必要な既知のヘッダのみ保持する。将来ベンダが未知の認証ヘッダを
///    追加しても、allowlist に無い限り構造的に漏れない。
/// 2. **denylist（多層防御）**: 群 A / 群 B の除外リストの和集合を明示的に
///    弾く。allowlist を誤って拡張した場合の保険として機能する。
///
/// 値は `to_str()` で UTF-8 変換し、obs-text 等非 UTF-8 はスキップする。
/// ヘッダ名の比較は大小文字を区別しない。
pub(crate) fn sanitize_response_headers(
    headers: &reqwest::header::HeaderMap,
) -> HashMap<String, String> {
    /// 保持を許可するヘッダ名（完全一致）。
    const ALLOWED_HEADERS: &[&str] = &[
        "retry-after",    // opencode-session::retry が秒指定 / HTTP-date を解釈
        "retry-after-ms", // 同上（ミリ秒指定）
        "x-request-id",   // 障害追跡用の識別子（値自体は機微情報ではない）
    ];
    /// 保持を許可するヘッダ名の前方一致（レート制限情報は全般を保持）。
    const ALLOWED_PREFIXES: &[&str] = &["x-ratelimit-"];

    /// 群 A / 群 B の機微ヘッダ除外リストの和集合（大小文字無視）。
    const SENSITIVE_HEADERS: &[&str] = &[
        "authorization",
        "proxy-authorization",
        "authentication",
        "cookie",
        "set-cookie",
        "set-cookie2",
        "api-key",
        "x-api-key",
        "x-auth",
        "x-auth-token",
        "x-access-token",
        "x-api-token",
        "x-amz-security-token",
        "x-amz-credential",
        "x-goog-api-key",
        "x-goog-iam-authorization-token",
        "x-session-token",
        "x-csrf-token",
        "x-xsrf-token",
    ];

    headers
        .iter()
        .filter(|(name, _)| {
            let name = name.as_str();
            let lower = name.to_ascii_lowercase();
            let allowed = ALLOWED_HEADERS
                .iter()
                .any(|header| lower.eq_ignore_ascii_case(header))
                || ALLOWED_PREFIXES
                    .iter()
                    .any(|prefix| lower.starts_with(prefix));
            let denied = SENSITIVE_HEADERS
                .iter()
                .any(|header| lower.eq_ignore_ascii_case(header));
            allowed && !denied
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|text| (name.as_str().to_string(), text.to_string()))
        })
        .collect()
}

/// `Retry-After` の秒指定値をパースする純粋ヘルパ。
///
/// ヘッダ名は大文字小文字を区別しない。HTTP-date 形式は
/// `opencode-session::retry` 側で解釈されるため、ここでは扱わない。
/// 正の有限数以外（0・負・NaN・無限大・不正文字列）は `None` を返す。
pub fn parse_retry_after_seconds(headers: &HashMap<String, String>) -> Option<f64> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
}

/// `Retry-After` をバックオフ上限（ミリ秒）へ反映する純粋ヘルパ。
///
/// `opencode-session::retry::RETRY_MAX_DELAY` と同じ上限を既定値として使い、
/// 上限を超える値はクランプする。ヘッダが無い・不正な場合は `None`。
pub const DEFAULT_RETRY_AFTER_CAP_MS: u64 = 2_147_483_647;

pub fn retry_after_backoff_cap_ms(
    headers: Option<&HashMap<String, String>>,
    default_cap_ms: u64,
) -> Option<u64> {
    let seconds = parse_retry_after_seconds(headers?)?;
    let millis = seconds * 1000.0;
    if !millis.is_finite() || millis <= 0.0 {
        return None;
    }
    let cap = default_cap_ms.min(DEFAULT_RETRY_AFTER_CAP_MS);
    Some((millis.ceil() as u64).min(cap))
}

/// 共通 HTTP クライアントの接続タイムアウト既定値。
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 共通 HTTP クライアントのリクエスト全体タイムアウト既定値。
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 接続・全体タイムアウトを明示設定した `reqwest::Client` を構築する。
///
/// 各アダプタの `Client::new()` は既定タイムアウトが設定されないため、
/// 未設定のまま無期限に待つ可能性がある。アダプタはこの `pub(crate)` API
/// を利用してクライアントを構築する（個別ファイルの変更は別スコープ）。
pub(crate) fn build_http_client(
    connect_timeout: Duration,
    timeout: Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .build()
}

/// 既定タイムアウトで共通 HTTP クライアントを構築する。
pub(crate) fn default_http_client() -> Result<reqwest::Client, reqwest::Error> {
    build_http_client(DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT)
}

pub fn parse_stream_error(data: &str) -> Option<ParsedStreamError> {
    let body: serde_json::Value = serde_json::from_str(data).ok()?;

    if body.get("type")?.as_str()? != "error" {
        return None;
    }

    let error = body.get("error")?;
    let code = error.get("code")?.as_str()?;
    let response_body = serde_json::to_string(&body).unwrap_or_default();

    match code {
        "context_length_exceeded" => Some(ParsedStreamError::ContextOverflow {
            message: "Input exceeds context window of this model".to_string(),
            response_body,
        }),
        "insufficient_quota" => Some(ParsedStreamError::ApiError {
            message: "Quota exceeded. Check your plan and billing details.".to_string(),
            is_retryable: false,
            response_body,
        }),
        "usage_not_included" => Some(ParsedStreamError::ApiError {
            message: "To use Codex with your ChatGPT plan, upgrade to Plus: https://chatgpt.com/explore/plus.".to_string(),
            is_retryable: false,
            response_body,
        }),
        "invalid_prompt" => {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Invalid prompt.")
                .to_string();
            Some(ParsedStreamError::ApiError {
                message: msg,
                is_retryable: false,
                response_body,
            })
        }
        _ => None,
    }
}

pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    provider_info: HashMap<String, ProviderInfo>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            provider_info: HashMap::new(),
        }
    }

    pub fn register<P: Provider + 'static>(&mut self, provider: P) {
        let id = provider.id().to_string();
        let models = provider.models();
        let info = ProviderInfo {
            id: id.clone(),
            name: provider.name().to_string(),
            models: models.into_iter().map(|m| (m.id.clone(), m)).collect(),
            source: "bundled".to_string(),
            options: HashMap::new(),
        };
        self.provider_info.insert(id.clone(), info);
        self.providers.insert(id, Arc::new(provider));
    }

    pub fn register_arc(&mut self, provider: Arc<dyn Provider>) {
        let id = provider.id().to_string();
        let models = provider.models();
        let info = ProviderInfo {
            id: id.clone(),
            name: provider.name().to_string(),
            models: models.into_iter().map(|m| (m.id.clone(), m)).collect(),
            source: "bundled".to_string(),
            options: HashMap::new(),
        };
        self.provider_info.insert(id.clone(), info);
        self.providers.insert(id, provider);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).cloned()
    }

    pub fn get_info(&self, id: &str) -> Option<&ProviderInfo> {
        self.provider_info.get(id)
    }

    /// Provider IDs in a deterministic, `BTreeMap`-like order.
    ///
    /// `providers` is a `HashMap`, whose iteration order is randomized per
    /// process. Every model-resolution path must go through this helper so a
    /// model ID shared by several providers always resolves to the same one.
    /// Config providers are also stored in a `HashMap`, so no explicit config
    /// order is currently available; lexicographic ID order is the documented
    /// deterministic fallback. If a config layer later exposes an explicit
    /// order, it can be layered into this helper without touching callers.
    fn ordered_provider_ids(&self) -> Vec<&String> {
        let mut ids: Vec<&String> = self.providers.keys().collect();
        ids.sort();
        ids
    }

    pub fn list(&self) -> Vec<Arc<dyn Provider>> {
        self.ordered_provider_ids()
            .into_iter()
            .filter_map(|id| self.providers.get(id).cloned())
            .collect()
    }

    pub fn list_providers(&self) -> Vec<&ProviderInfo> {
        self.ordered_provider_ids()
            .into_iter()
            .filter_map(|id| self.provider_info.get(id))
            .collect()
    }

    pub fn list_models(&self) -> Vec<ModelInfo> {
        self.ordered_provider_ids()
            .into_iter()
            .filter_map(|id| self.providers.get(id))
            .flat_map(|provider| {
                let mut models = provider.models();
                models.sort_by(|a, b| a.id.cmp(&b.id));
                models
            })
            .collect()
    }

    /// Find a model by its bare ID, scanning providers in deterministic order.
    pub fn find_model(&self, model_id: &str) -> Option<(String, ModelInfo)> {
        for provider_id in self.ordered_provider_ids() {
            if let Some(provider) = self.providers.get(provider_id) {
                if let Some(model) = provider.get_model(model_id) {
                    return Some((provider_id.to_string(), model.clone()));
                }
            }
        }
        None
    }

    pub fn merge_config(&mut self, provider_id: &str, options: HashMap<String, serde_json::Value>) {
        if let Some(info) = self.provider_info.get_mut(provider_id) {
            info.options.extend(options);
        }
    }

    pub fn get_provider(&self, provider_id: &str) -> Result<Arc<dyn Provider>, ProviderError> {
        self.providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.to_string()))
    }

    pub fn get_language_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(Arc<dyn Provider>, ModelInfo), ProviderError> {
        let provider = self.get_provider(provider_id)?;
        let model = provider
            .get_model(model_id)
            .cloned()
            .ok_or_else(|| ProviderError::ModelNotFound(model_id.to_string()))?;
        Ok((provider, model))
    }

    pub fn parse_model_string(&self, model_string: &str) -> Option<(String, String)> {
        if let Some(pos) = model_string.find('/') {
            let provider_id = &model_string[..pos];
            let model_id = &model_string[pos + 1..];
            Some((provider_id.to_string(), model_id.to_string()))
        } else {
            self.find_model(model_string)
                .map(|(provider_id, _)| (provider_id, model_string.to_string()))
        }
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn create_default_registry() -> ProviderRegistry {
    ProviderRegistry::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Minimal `Provider` implementation for registry resolution tests.
    /// `chat` / `chat_stream` are never called and deliberately contain no
    /// credential-like values.
    struct StubProvider {
        id: &'static str,
        models: Vec<ModelInfo>,
    }

    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            self.id
        }

        fn name(&self) -> &str {
            self.id
        }

        fn models(&self) -> Vec<ModelInfo> {
            self.models.clone()
        }

        fn get_model(&self, id: &str) -> Option<&ModelInfo> {
            self.models.iter().find(|model| model.id == id)
        }

        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::ApiError(
                "stub provider does not implement chat".to_string(),
            ))
        }

        async fn chat_stream(&self, _request: ChatRequest) -> Result<StreamResult, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn registry_with_duplicate_model() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        // Register in reverse lexicographic order: HashMap iteration and
        // registration order must not influence resolution.
        for (provider_id, model_name) in
            [("z-provider", "Z Provider"), ("a-provider", "A Provider")]
        {
            registry.register(StubProvider {
                id: provider_id,
                models: vec![ModelInfo {
                    id: "gpt-4o".to_string(),
                    name: model_name.to_string(),
                    provider: provider_id.to_string(),
                    context_window: 128_000,
                    max_output_tokens: 16_384,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 0.0,
                    cost_per_million_output: 0.0,
                }],
            });
        }
        registry
    }

    #[test]
    fn find_model_is_deterministic_for_shared_model_ids() {
        let registry = registry_with_duplicate_model();

        for _ in 0..32 {
            let (provider_id, model) = registry
                .find_model("gpt-4o")
                .expect("shared model should resolve");
            assert_eq!(provider_id, "a-provider");
            assert_eq!(model.name, "A Provider");
        }
    }

    #[test]
    fn parse_model_string_resolves_bare_names_through_find_model() {
        let registry = registry_with_duplicate_model();

        for _ in 0..32 {
            assert_eq!(
                registry.parse_model_string("gpt-4o"),
                Some(("a-provider".to_string(), "gpt-4o".to_string()))
            );
        }
        assert_eq!(
            registry.parse_model_string("z-provider/gpt-4o"),
            Some(("z-provider".to_string(), "gpt-4o".to_string()))
        );
    }

    #[test]
    fn registry_listing_uses_deterministic_provider_order() {
        let registry = registry_with_duplicate_model();

        let ids: Vec<&str> = registry
            .list_providers()
            .iter()
            .map(|info| info.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a-provider", "z-provider"]);

        let providers = registry.list();
        let listed: Vec<&str> = providers.iter().map(|p| p.id()).collect();
        assert_eq!(listed, vec!["a-provider", "z-provider"]);
    }

    #[test]
    fn test_overflow_detection_anthropic() {
        assert!(ProviderError::is_overflow("prompt is too long"));
        assert!(ProviderError::is_overflow(
            "The prompt is too long for this model"
        ));
    }

    #[test]
    fn test_overflow_detection_openai() {
        assert!(ProviderError::is_overflow(
            "This model's maximum context length is 128000 tokens"
        ));
    }

    #[test]
    fn test_overflow_detection_bedrock() {
        assert!(ProviderError::is_overflow(
            "input is too long for requested model"
        ));
    }

    #[test]
    fn test_overflow_detection_no_body() {
        assert!(ProviderError::is_overflow("400 (no body)"));
        assert!(ProviderError::is_overflow("413 (no body)"));
        assert!(ProviderError::is_overflow("400 status code (no body)"));
    }

    #[test]
    fn test_no_false_positive_overflow() {
        assert!(!ProviderError::is_overflow("rate limit exceeded"));
        assert!(!ProviderError::is_overflow("authentication failed"));
    }

    #[test]
    fn test_parse_api_call_error_overflow() {
        let error = ProviderError::ApiError("prompt is too long".to_string());
        let parsed = parse_api_call_error("anthropic", &error);
        assert!(matches!(parsed, ParsedAPICallError::ContextOverflow { .. }));
    }

    #[test]
    fn test_parse_api_call_error_github_copilot_403() {
        let error = ProviderError::api_error_with_status("Forbidden", 403);
        let parsed = parse_api_call_error("github-copilot", &error);
        if let ParsedAPICallError::ApiError { message, .. } = parsed {
            assert!(message.contains("reauthenticate"));
        }
    }

    #[test]
    fn test_error_with_source_preserves_error_chain() {
        let api = ProviderError::api_error_with_source(
            "request failed",
            std::io::Error::other("underlying API cause"),
        );
        assert_eq!(api.to_string(), "API error: request failed");
        let source = std::error::Error::source(&api).expect("source must be preserved");
        assert_eq!(source.to_string(), "underlying API cause");
        assert!(source.downcast_ref::<std::io::Error>().is_some());

        let network = ProviderError::network_error_with_source(
            "request failed",
            std::io::Error::other("connection reset"),
        );
        assert_eq!(network.to_string(), "Network error: request failed");
        let source = std::error::Error::source(&network).expect("source must be preserved");
        assert_eq!(source.to_string(), "connection reset");
        assert!(source.downcast_ref::<std::io::Error>().is_some());
    }

    #[test]
    fn test_network_error_with_source_keeps_retryability() {
        use crate::retry::IsRetryable;

        let error = ProviderError::network_error_with_source(
            "request failed",
            std::io::Error::other("connection reset"),
        );
        assert_eq!(
            error.is_retryable().as_deref(),
            Some("Network error: request failed")
        );

        // 一時エラー判定は reqwest::Error の connect/timeout/request に限定した。
        // 汎用 std::io::Error は一時エラーではないため、非リトライを維持する
        // （期待値を変えず、理由をここに明記する）。
        let api = ProviderError::api_error_with_source("bad request", std::io::Error::other("bad"));
        assert!(api.is_retryable().is_none());
    }

    #[test]
    fn test_parse_api_call_error_network_with_source_is_retryable() {
        let network = ProviderError::network_error_with_source(
            "request failed",
            std::io::Error::other("connection reset"),
        );
        let parsed = parse_api_call_error("some-provider", &network);
        match parsed {
            ParsedAPICallError::ApiError {
                is_retryable,
                status_code,
                ..
            } => {
                assert!(
                    is_retryable,
                    "source 保持版のネットワークエラーは retryable であるべき"
                );
                assert_eq!(status_code, None);
            }
            other => panic!("expected ApiError, got {other:?}"),
        }

        // ApiErrorWithSource は source が一時 reqwest エラー（connect/timeout/request）
        // でない限り非リトライ。ここでは std::io::Error なので
        // is_retryable() と揃って非リトライのままであることを固定する。
        let api = ProviderError::api_error_with_source("bad request", std::io::Error::other("bad"));
        let parsed = parse_api_call_error("some-provider", &api);
        match parsed {
            ParsedAPICallError::ApiError { is_retryable, .. } => {
                assert!(
                    !is_retryable,
                    "ApiErrorWithSource は非リトライのままであるべき"
                );
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    /// 従前は `parse_api_call_error` のみ OpenAI の 404 をリトライ可としていたが、
    /// `IsRetryable` との乖離を解消するため特例を廃止し全プロバイダ共通の
    /// ステータス集合へ統一した（期待値を更新した理由）。
    #[test]
    fn test_openai_404_is_no_longer_special_cased() {
        use crate::retry::IsRetryable;

        assert!(!is_status_retryable("openai", 404));
        assert!(is_status_retryable("openai", 429));
        assert!(!is_status_retryable("openai", 401));

        let error = ProviderError::api_error_from_status(404, "not found");
        assert!(
            error.is_retryable().is_none(),
            "404 は IsRetryable 側で非リトライ"
        );
        match parse_api_call_error("openai", &error) {
            ParsedAPICallError::ApiError { is_retryable, .. } => assert!(
                !is_retryable,
                "parse 側も 404 を非リトライとし、経路で一致するべき"
            ),
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    /// `IsRetryable` と `parse_api_call_error` が全プロバイダ・全ステータスで
    /// 同じリトライ判定を返すことを固定する回帰テスト。
    #[test]
    fn test_is_retryable_and_parse_agree_on_all_statuses() {
        use crate::retry::IsRetryable;

        for provider_id in ["openai", "anthropic", "github-copilot", "some-provider"] {
            for status in [
                200, 400, 401, 403, 404, 408, 418, 422, 429, 500, 502, 503, 504,
            ] {
                let error = ProviderError::api_error_from_status(status, "boom");
                let via_trait = error.is_retryable().is_some();
                let via_parse = match parse_api_call_error(provider_id, &error) {
                    ParsedAPICallError::ApiError { is_retryable, .. } => is_retryable,
                    other => {
                        panic!("expected ApiError for {provider_id}/{status}, got {other:?}")
                    }
                };
                assert_eq!(
                    via_trait, via_parse,
                    "判定が分岐した: provider={provider_id}, status={status}"
                );
            }
        }
    }

    /// `ApiErrorWithSource` の source が reqwest の一時エラー
    /// （loopback への connect 失敗）なら、`IsRetryable` / `parse` の双方が
    /// リトライ可と判定することを固定する回帰テスト。
    #[tokio::test]
    async fn test_api_error_with_source_transient_reqwest_error_is_retryable() {
        use crate::retry::IsRetryable;

        // 一時的に listen して即座に閉じ、確実に connection refused を起こす。
        // 外部ホストへは送信しない（loopback のみ）。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let source = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("closed loopback port must fail to connect");
        assert!(
            source.is_connect() || source.is_request() || source.is_timeout(),
            "unexpected reqwest error kind: {source:?}"
        );

        let error = ProviderError::api_error_with_source("request failed", source);
        assert_eq!(
            error.is_retryable().as_deref(),
            Some("API error: request failed"),
            "source 由来の一時エラーはリトライ可であるべき"
        );
        match parse_api_call_error("some-provider", &error) {
            ParsedAPICallError::ApiError { is_retryable, .. } => {
                assert!(is_retryable, "parse 経由でもリトライ可であるべき")
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_stream_error_context_overflow() {
        let data =
            r#"{"type":"error","error":{"code":"context_length_exceeded","message":"too long"}}"#;
        let parsed = parse_stream_error(data).unwrap();
        assert!(matches!(parsed, ParsedStreamError::ContextOverflow { .. }));
    }

    #[test]
    fn test_parse_stream_error_quota() {
        let data =
            r#"{"type":"error","error":{"code":"insufficient_quota","message":"quota exceeded"}}"#;
        let parsed = parse_stream_error(data).unwrap();
        assert!(matches!(parsed, ParsedStreamError::ApiError { .. }));
    }

    #[test]
    fn test_parse_stream_error_non_error() {
        let data = r#"{"type":"message","content":"hello"}"#;
        assert!(parse_stream_error(data).is_none());
    }

    #[test]
    fn test_retryable_status_codes_are_fixed() {
        for status in [429, 500, 502, 503, 504] {
            assert!(
                ProviderError::is_retryable_status_code(status),
                "status {status} must be retryable"
            );
        }
        for status in [200, 400, 401, 403, 404, 408, 418, 422] {
            assert!(
                !ProviderError::is_retryable_status_code(status),
                "status {status} must not be retryable"
            );
        }
    }

    #[test]
    fn test_api_error_from_status_keeps_structured_status() {
        use crate::retry::IsRetryable;

        let error = ProviderError::api_error_from_status(503, "Service Unavailable");
        match &error {
            ProviderError::ApiErrorWithStatus {
                status_code,
                message,
            } => {
                assert_eq!(*status_code, 503);
                assert_eq!(message.as_str(), "Service Unavailable");
            }
            other => panic!("expected structured status error, got {other:?}"),
        }
        assert_eq!(error.status_code(), Some(503));
        assert!(
            error.is_retryable().is_some(),
            "503 must stay retryable when status is structured"
        );

        // 平坦化された ApiError はステータス情報を失う（回帰防止）。
        let flattened = ProviderError::ApiError("503: Service Unavailable".to_string());
        assert_eq!(flattened.status_code(), None);
        assert!(flattened.is_retryable().is_none());
    }

    #[test]
    fn test_parse_api_call_error_keeps_retryable_status() {
        for status in [429, 500, 502, 503, 504] {
            let error = ProviderError::api_error_from_status(status, "boom");
            let parsed = parse_api_call_error("anthropic", &error);
            match parsed {
                ParsedAPICallError::ApiError {
                    status_code: Some(actual),
                    is_retryable: true,
                    ..
                } => assert_eq!(actual, status),
                other => panic!("status {status} should be retryable, got {other:?}"),
            }
        }

        let error = ProviderError::api_error_from_status(400, "bad request");
        let parsed = parse_api_call_error("anthropic", &error);
        match parsed {
            ParsedAPICallError::ApiError { is_retryable, .. } => assert!(!is_retryable),
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_api_call_error_with_headers_passes_headers_through() {
        let error = ProviderError::api_error_from_status(429, "rate limited");
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "2".to_string());

        let parsed = parse_api_call_error_with_headers("openai", &error, Some(headers.clone()));
        match parsed {
            ParsedAPICallError::ApiError {
                response_headers, ..
            } => assert_eq!(response_headers, Some(headers)),
            other => panic!("expected ApiError, got {other:?}"),
        }

        let without = parse_api_call_error("openai", &error);
        match without {
            ParsedAPICallError::ApiError {
                response_headers, ..
            } => {
                assert!(response_headers.is_none())
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[test]
    fn test_api_error_with_headers_feeds_parsed_error() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "2".to_string());
        headers.insert("x-request-id".to_string(), "req-1".to_string());
        let long_body = format!("{{\"error\":\"{}\"}}", "x".repeat(2_000));
        let error = ProviderError::api_error_with_headers(
            429,
            "rate limited",
            Some(headers.clone()),
            Some(long_body.clone()),
        );

        // 構造化値が保持され、既存の status/retryable 判定も壊れていないこと。
        assert_eq!(error.status_code(), Some(429));
        assert_eq!(error.response_headers(), Some(&headers));
        assert_eq!(error.response_body(), Some(long_body.as_str()));
        use crate::retry::IsRetryable;
        assert!(error.is_retryable().is_some());

        // parse 経由で session 側へ届くこと。本文は 500 バイト preview に制限される。
        let parsed = parse_api_call_error("openai", &error);
        match parsed {
            ParsedAPICallError::ApiError {
                status_code,
                is_retryable,
                response_headers,
                response_body,
                ..
            } => {
                assert_eq!(status_code, Some(429));
                assert!(is_retryable);
                assert_eq!(response_headers, Some(headers));
                let body = response_body.expect("body must be propagated");
                assert!(
                    body.len() <= 503,
                    "full body leaked into preview: {} bytes",
                    body.len()
                );
                assert!(body.ends_with("..."));
                assert!(!body.contains(&long_body));
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_retry_after_seconds_seconds_only() {
        let mut headers = HashMap::new();
        headers.insert("Retry-After".to_string(), "2.5".to_string());
        assert_eq!(parse_retry_after_seconds(&headers), Some(2.5));

        // HTTP-date 形式は秒指定ではないためパースしない（session 側が担当）。
        let mut date_headers = HashMap::new();
        date_headers.insert(
            "retry-after".to_string(),
            "Wed, 21 Oct 2015 07:28:00 GMT".to_string(),
        );
        assert_eq!(parse_retry_after_seconds(&date_headers), None);

        for value in ["0", "-1", "NaN", "inf"] {
            let mut invalid = HashMap::new();
            invalid.insert("retry-after".to_string(), value.to_string());
            assert_eq!(parse_retry_after_seconds(&invalid), None, "value={value}");
        }
    }

    #[test]
    fn test_retry_after_backoff_cap_ms_is_clamped() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "1e9".to_string());
        assert_eq!(
            retry_after_backoff_cap_ms(Some(&headers), 5_000),
            Some(5_000)
        );

        headers.insert("retry-after".to_string(), "2".to_string());
        assert_eq!(
            retry_after_backoff_cap_ms(Some(&headers), DEFAULT_RETRY_AFTER_CAP_MS),
            Some(2_000)
        );

        assert_eq!(retry_after_backoff_cap_ms(None, 5_000), None);
    }

    #[test]
    fn test_default_http_client_builds_with_timeouts() {
        let client = default_http_client().expect("client with timeouts must build");
        // reqwest::Client はタイムアウト値を公開しないため、構築成功を固定する。
        let _ = &client;
        assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(120));
    }

    /// 群 A / 群 B の除外リスト和集合の全項目がサニタイズで落ちる回帰テスト。
    #[test]
    fn test_sanitize_response_headers_drops_every_sensitive_header() {
        const SENSITIVE_HEADERS: &[&str] = &[
            "authorization",
            "proxy-authorization",
            "authentication",
            "cookie",
            "set-cookie",
            "set-cookie2",
            "api-key",
            "x-api-key",
            "x-auth",
            "x-auth-token",
            "x-access-token",
            "x-api-token",
            "x-amz-security-token",
            "x-amz-credential",
            "x-goog-api-key",
            "x-goog-iam-authorization-token",
            "x-session-token",
            "x-csrf-token",
            "x-xsrf-token",
        ];

        let mut headers = reqwest::header::HeaderMap::new();
        for name in SENSITIVE_HEADERS {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .expect("sensitive header name must be valid"),
                reqwest::header::HeaderValue::from_static("secret-token-value"),
            );
        }

        let sanitized = sanitize_response_headers(&headers);
        assert!(
            sanitized.is_empty(),
            "機微ヘッダが 1 つも構造化エラーへ残ってはならない: {sanitized:?}"
        );
        assert!(!sanitized.values().any(|value| value.contains("secret")));
    }

    /// allowlist（retry-after / retry-after-ms / x-ratelimit-* / x-request-id）
    /// は保持され、それ以外の未知ヘッダは構造的に除外される回帰テスト。
    #[test]
    fn test_sanitize_response_headers_keeps_only_allowlisted_headers() {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in [
            ("retry-after", "2"),
            ("retry-after-ms", "1500"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset-requests", "1s"),
            ("x-request-id", "req-1"),
        ] {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_static(value),
            );
        }
        // 未知のヘッダ（認証ヘッダかどうかに関わらず）は allowlist に無いため落ちる。
        for name in ["x-vendor-secret", "x-new-auth-token", "x-binary"] {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_static("must-not-leak"),
            );
        }
        // obs-text は HeaderValue として有効だが to_str() は拒否するためスキップ。
        headers.insert(
            reqwest::header::HeaderName::from_bytes(b"x-ratelimit-weird").unwrap(),
            reqwest::header::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );

        let sanitized = sanitize_response_headers(&headers);
        assert_eq!(sanitized.get("retry-after").map(String::as_str), Some("2"));
        assert_eq!(
            sanitized.get("retry-after-ms").map(String::as_str),
            Some("1500")
        );
        assert_eq!(
            sanitized.get("x-ratelimit-remaining").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            sanitized
                .get("x-ratelimit-reset-requests")
                .map(String::as_str),
            Some("1s")
        );
        assert_eq!(
            sanitized.get("x-request-id").map(String::as_str),
            Some("req-1")
        );
        assert!(!sanitized.contains_key("x-vendor-secret"));
        assert!(!sanitized.contains_key("x-new-auth-token"));
        assert!(!sanitized.contains_key("x-binary"));
        assert!(!sanitized.contains_key("x-ratelimit-weird"));
        assert!(!sanitized
            .values()
            .any(|value| value.contains("must-not-leak")));
    }

    /// 機微ヘッダの値が構造化エラーの `response_headers` / `to_string()` の
    /// いずれにも現れないことを end-to-end で確認する回帰テスト。
    #[test]
    fn test_sensitive_headers_do_not_reach_structured_error() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("authorization"),
            reqwest::header::HeaderValue::from_static("Bearer secret-token-value"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            reqwest::header::HeaderValue::from_static("secret-token-value"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("retry-after"),
            reqwest::header::HeaderValue::from_static("3"),
        );

        let map = sanitize_response_headers(&headers);
        let error = ProviderError::api_error_with_headers(
            429,
            "rate limited",
            Some(map.clone()),
            Some("body-preview".to_string()),
        );

        let carried = error.response_headers().expect("headers must be carried");
        assert_eq!(carried, &map);
        assert_eq!(carried.get("retry-after").map(String::as_str), Some("3"));
        assert!(!carried.values().any(|value| value.contains("secret")));
        assert!(!error.to_string().contains("secret"));
    }

    /// 回帰ガード: credential クエリ値がエラー文字列へ残らないこと。
    #[test]
    fn test_redact_credential_query_params_hides_known_keys() {
        let secret = "supersecret-value-123";
        for input in [
            format!("https://api.example.com/v1?key={secret}&model=gpt-4"),
            format!("https://api.example.com/v1?api_key={secret}"),
            format!("https://api.example.com/v1?token={secret}"),
            format!("https://api.example.com/v1?access_token={secret}"),
            format!("https://api.example.com/v1&x-api-key={secret}"),
            format!("https://api.example.com/v1?x-goog-api-key={secret}"),
        ] {
            let redacted = redact_credential_query_params(&input);
            assert!(!redacted.contains(secret), "leak: {redacted}");
            assert!(redacted.contains("<redacted>"), "no marker: {redacted}");
        }

        // 非機微クエリとホストは保持し、障害追跡性を落とさない。
        let safe = redact_credential_query_params("https://api.example.com/v1?model=gpt-4&limit=2");
        assert_eq!(safe, "https://api.example.com/v1?model=gpt-4&limit=2");
    }

    /// 回帰ガード: 共通コンストラクタを通す全生成経路で redact されること。
    #[test]
    fn test_error_constructors_redact_credentials_in_message() {
        let secret = "supersecret-value-123";
        let network = ProviderError::network_error_with_source(
            format!("request to https://api.example.com/v1?key={secret} failed"),
            std::io::Error::other("boom"),
        );
        assert!(!network.to_string().contains(secret));
        assert!(network.to_string().contains("<redacted>"));

        let api = ProviderError::api_error_with_status(
            format!("call https://api.example.com?api_key={secret} failed"),
            400,
        );
        assert!(!api.to_string().contains(secret));

        let with_headers = ProviderError::api_error_with_headers(
            429,
            format!("rate limited at https://api.example.com?token={secret}"),
            None,
            Some(format!("see https://api.example.com?key={secret}")),
        );
        assert!(!with_headers.to_string().contains(secret));
        assert!(!with_headers.response_body().expect("body").contains(secret));

        let overflow = ProviderError::context_overflow(format!("too long ?token={secret}"));
        assert!(!overflow.to_string().contains(secret));
    }

    /// 回帰ガード: 直接バリアント構築されたエラーも parse 時に redact されること。
    #[test]
    fn test_parse_api_call_error_redacts_directly_constructed_error() {
        let secret = "supersecret-value-123";
        let error = ProviderError::ApiError(format!(
            "request to https://api.example.com/v1?token={secret} failed"
        ));
        match parse_api_call_error("openai", &error) {
            ParsedAPICallError::ApiError { message, .. } => {
                assert!(!message.contains(secret), "leak: {message}");
                assert!(message.contains("<redacted>"));
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }
}
