use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use opencode_plugin::{HookContext, HookEvent};
use opencode_provider::{
    get_model_context_limit, ChatRequest, Content, ImageUrl, IsRetryable, Message, Provider,
    ProviderError, StreamEvent, StreamUsage, ToolDefinition, ToolErrorKind as StreamToolErrorKind,
};

use crate::compaction::{CompactionConfig, CompactionEngine, ModelLimits, TokenUsage};
use crate::message_v2::{CompletedTime, ErrorTime, RunningTime, ToolState};
use crate::retry::{self, ApiErrorInfo};
use crate::snapshot::{Snapshot, SnapshotPatch};
use crate::{MessageError, MessageInfo, MessageUsage, MessageWithParts, Part};

pub const OUTPUT_TOKEN_MAX: u64 = 8192;

/// Doom loop detection threshold: if the last N tool calls are identical,
/// trigger a permission ask.
const DOOM_LOOP_THRESHOLD: usize = 3;

/// Retry limit used when a [`StreamInput`] does not specify `retries`.
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Hard bound for a single streaming LLM attempt.
///
/// Provider adapters construct their HTTP clients without a request timeout,
/// so a stalled provider could otherwise block the retry loop forever. The
/// value is intentionally generous to accommodate long-running generations;
/// cancellation via [`StreamInput::abort`] still applies during retry sleeps.
const LLM_STREAM_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct LlmAgent {
    pub name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LlmModelRef {
    pub model_id: String,
    pub provider_id: String,
}

pub struct StreamInput {
    pub user: MessageInfo,
    pub session_id: String,
    pub model: LlmModelRef,
    pub agent: LlmAgent,
    pub system: Vec<String>,
    pub abort: tokio_util::sync::CancellationToken,
    pub messages: Vec<Message>,
    pub small: bool,
    pub tools: HashMap<String, ToolDefinition>,
    pub retries: Option<u32>,
    pub tool_choice: Option<ToolChoice>,
    /// Model pricing: cost per million tokens (input, output, cache_read, cache_write)
    pub cost_rates: Option<CostRates>,
    /// Project working directory for git snapshot tracking.
    /// If None, snapshot tracking is skipped.
    pub work_dir: Option<PathBuf>,
}

/// Per-million-token cost rates for a model
#[derive(Debug, Clone, Default)]
pub struct CostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    /// Optional "experimentalOver200K" pricing tier used when total token volume exceeds 200k.
    pub over_200k: Option<CostRateTier>,
}

#[derive(Debug, Clone, Default)]
pub struct CostRateTier {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum ToolChoice {
    Auto,
    Required,
    None,
}

pub struct StreamOutput {
    pub events: tokio::sync::mpsc::Receiver<StreamEvent>,
}

// ============================================================================
// 構造化ストリームエラーの受け渡し
// ============================================================================
//
// `opencode_provider::StreamEvent::Error` のペイロードは `String` のみであり、
// 本タスクの書き込みスコープは opencode-session に限定されているため enum 自体へ
// バリアントを追加することはできない。そこで文字列へバージョン付き JSON
// エンベロープとして `ProviderError` の構造化情報（status / headers / message）を
// 埋め込み、processor 側で `ProviderError` へ復元する。
//
// これにより `error_from_anyhow` の `downcast_ref::<ProviderError>()` が成功し、
// `Retry-After` が session retry の `delay` へ届くようになる。
//
// セキュリティ:
// - 本文は 500 バイト preview のみ保持し、全文はエンベロープへ載せない。
//   `retry::retryable` の free-usage-limit 検出が streaming 経路でも機能する。
// - ヘッダは encode / decode 双方で allowlist を再適用し、カスタム provider が
//   混入させた `authorization` / `set-cookie` 等は復元時に除去する。
// - デコード失敗時の生ペイロードはログ・エラー文字列へ埋め込まない。

const STRUCTURED_STREAM_ERROR_MARKER: &str = "__opencode_session_provider_error_v1__";
const STRUCTURED_STREAM_ERROR_VERSION: u32 = 1;
const ERROR_BODY_PREVIEW_MAX_BYTES: usize = 500;

/// デコード失敗時にユーザー・ログへ渡す固定メッセージ。
const STREAM_ERROR_DECODE_FAILED: &str =
    "Stream error: upstream error payload could not be decoded";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StructuredStreamError {
    ApiErrorWithHeaders {
        status_code: u16,
        message: String,
        response_headers: Option<HashMap<String, String>>,
        response_body: Option<String>,
    },
    ApiErrorWithStatus {
        status_code: u16,
        message: String,
    },
    /// `ApiErrorWithSource` の再符号化形。source 自体は直列化できないため、
    /// transient 判定の結果だけを `retryable` に載せて往復で保持する。
    ApiErrorWithSource {
        message: String,
        retryable: bool,
    },
    /// `NetworkErrorWithSource` の再符号化形（判定結果の保持は API 側と同じ）。
    NetworkErrorWithSource {
        message: String,
        retryable: bool,
    },
    RateLimit,
    Timeout,
    NetworkError {
        message: String,
    },
    AuthError {
        message: String,
    },
    ContextOverflow {
        message: String,
    },
    Other {
        message: String,
    },
}

/// マーカー直後に続くバージョン付きラッパ。未知バージョンは復元を拒否する。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StructuredStreamEnvelope {
    _version: u32,
    error: StructuredStreamError,
}

/// provider 側の `error_body_preview` と同じ 500 バイト制限を、UTF-8 文字境界を
/// またがないように適用する。provider 側の実装は `pub(crate)` のため再利用できない。
fn error_body_preview(body: &str) -> String {
    if body.len() <= ERROR_BODY_PREVIEW_MAX_BYTES {
        return body.to_string();
    }
    let mut end = ERROR_BODY_PREVIEW_MAX_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &body[..end])
}

/// ストリーミング経路で復元するヘッダの allowlist。
///
/// provider.rs の `sanitize_response_headers` は `pub(crate)` かつ `HeaderMap`
/// 主体のため、書き込みスコープ内では `HashMap` 向けの最小実装を置く。
fn sanitize_stream_response_headers(
    headers: Option<HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    const ALLOWED_HEADERS: &[&str] = &["retry-after", "retry-after-ms", "x-request-id"];
    const ALLOWED_PREFIXES: &[&str] = &["x-ratelimit-"];

    headers.map(|headers| {
        headers
            .into_iter()
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                ALLOWED_HEADERS.iter().any(|allowed| lower == *allowed)
                    || ALLOWED_PREFIXES
                        .iter()
                        .any(|prefix| lower.starts_with(prefix))
            })
            .collect()
    })
}

/// credential クエリパラメータと Bearer 値を `<redacted>` へ伏せるローカルヘルパ。
///
/// `opencode_provider::redact_credential_query_params` は `pub(crate)` のため
/// 他クレートから参照できない。構造化エラーの復元経路では `ProviderError` の
/// タプルバリアント（`NetworkError` / `AuthError` / `ApiError`）へ文字列を直接
/// 渡すため、ここで同種の redact を必ず通してから組み立てる。
/// パラメータ名・ホスト・トークン種別は残し、値だけを伏せて障害追跡性を保つ。
fn redact_stream_error_message(input: &str) -> String {
    static CREDENTIAL_QUERY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
                r#"(?i)([?&\s"']?\b(?:x-goog-api-key|x-api-key|x-auth-token|api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|client[_-]?secret|authorization|sig|signature|key|token|secret|code)=)[^&\s#"<>']+"#,
            )
            .expect("credential query regex must compile")
    });
    static BEARER_VALUE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(\bbearer\s+)[^\s,;"'<>]+"#)
            .expect("bearer value regex must compile")
    });

    let redacted = CREDENTIAL_QUERY.replace_all(input, "$1<redacted>");
    BEARER_VALUE
        .replace_all(&redacted, "$1<redacted>")
        .into_owned()
}

/// `ProviderError` を `StreamEvent::Error(String)` へ載せられる構造化ペイロードへ変換する。
///
/// 本文は 500 バイト preview に制限して保持する（free-usage-limit 検出に必要）。
/// 全文がログやユーザー向けエラーへ流出しないよう、ここで切り詰める。
fn encode_stream_error(error: &ProviderError) -> String {
    let structured = match error {
        ProviderError::ApiErrorWithHeaders {
            status_code,
            message,
            response_headers,
            ..
        } => StructuredStreamError::ApiErrorWithHeaders {
            status_code: *status_code,
            message: message.clone(),
            response_headers: sanitize_stream_response_headers(response_headers.clone()),
            response_body: error.response_body().map(error_body_preview),
        },
        ProviderError::ApiErrorWithStatus {
            status_code,
            message,
        } => StructuredStreamError::ApiErrorWithStatus {
            status_code: *status_code,
            message: message.clone(),
        },
        ProviderError::ApiErrorWithSource { message, .. } => {
            StructuredStreamError::ApiErrorWithSource {
                message: message.clone(),
                // source を失っても reqwest 由来の transient 判定を保持する。
                retryable: error.is_retryable().is_some(),
            }
        }
        ProviderError::RateLimit => StructuredStreamError::RateLimit,
        ProviderError::Timeout => StructuredStreamError::Timeout,
        ProviderError::NetworkError(message) => StructuredStreamError::NetworkError {
            message: message.clone(),
        },
        ProviderError::NetworkErrorWithSource { message, .. } => {
            StructuredStreamError::NetworkErrorWithSource {
                message: message.clone(),
                retryable: error.is_retryable().is_some(),
            }
        }
        ProviderError::AuthError(message) => StructuredStreamError::AuthError {
            message: message.clone(),
        },
        ProviderError::ContextOverflow(message) => StructuredStreamError::ContextOverflow {
            message: message.clone(),
        },
        // source を保持しないバリアントは、利用者向けメッセージだけを
        // 保持して既存の文字列エラーと同じ扱いにする。
        _ => StructuredStreamError::Other {
            message: error.to_string(),
        },
    };

    let envelope = StructuredStreamEnvelope {
        _version: STRUCTURED_STREAM_ERROR_VERSION,
        error: structured,
    };
    match serde_json::to_string(&envelope) {
        Ok(json) => format!("{STRUCTURED_STREAM_ERROR_MARKER}{json}"),
        // エンコード失敗時は従来どおり文字列だけを渡し、工作流を止めない。
        Err(_) => error.to_string(),
    }
}

/// 既知 `kind` ごとに許されるキー集合。未知 kind / 余分フィールドの拒否に使う。
fn allowed_error_keys(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "api_error_with_headers" => Some(&[
            "kind",
            "status_code",
            "message",
            "response_headers",
            "response_body",
        ]),
        "api_error_with_status" => Some(&["kind", "status_code", "message"]),
        "api_error_with_source" | "network_error_with_source" => {
            Some(&["kind", "message", "retryable"])
        }
        "rate_limit" | "timeout" => Some(&["kind"]),
        "network_error" | "auth_error" | "context_overflow" | "other" => Some(&["kind", "message"]),
        _ => None,
    }
}

/// エンベロープを `ProviderError` へ復元する。通常の文字列エラーは `None`。
///
/// マーカー付きでも `_version` 不一致・未知 kind・余分フィールド・不正 JSON は
/// すべて `None` とし、生ペイロードをエラーやログへ渡さない。
fn decode_stream_error(payload: &str) -> Option<ProviderError> {
    let json = payload.strip_prefix(STRUCTURED_STREAM_ERROR_MARKER)?;
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let envelope = value.as_object()?;

    // トップレベルは `_version` と `error` のみ。余分フィールドは拒否する。
    if envelope.len() != 2 || !envelope.contains_key("_version") || !envelope.contains_key("error")
    {
        return None;
    }
    if envelope.get("_version")?.as_u64()? != STRUCTURED_STREAM_ERROR_VERSION as u64 {
        return None;
    }

    let error_value = envelope.get("error")?;
    let error_obj = error_value.as_object()?;
    let kind = error_obj.get("kind")?.as_str()?;
    let allowed = allowed_error_keys(kind)?;
    if error_obj.keys().any(|key| !allowed.contains(&key.as_str())) {
        return None;
    }

    let structured: StructuredStreamError = serde_json::from_value(error_value.clone()).ok()?;
    Some(match structured {
        StructuredStreamError::ApiErrorWithHeaders {
            status_code,
            message,
            response_headers,
            response_body,
        } => ProviderError::api_error_with_headers(
            status_code,
            redact_stream_error_message(&message),
            // カスタム provider が混入した機微ヘッダをここで確実に除去する。
            sanitize_stream_response_headers(response_headers),
            response_body.map(|body| error_body_preview(&body)),
        ),
        StructuredStreamError::ApiErrorWithStatus {
            status_code,
            message,
        } => {
            ProviderError::api_error_with_status(redact_stream_error_message(&message), status_code)
        }
        // source は直列化されないため、判定結果だけをマーカー化して復元する。
        // これにより decode 後の is_retryable() が元の判定を再現する。
        StructuredStreamError::ApiErrorWithSource { message, retryable } => {
            ProviderError::api_error_with_restored_source(
                redact_stream_error_message(&message),
                retryable,
            )
        }
        StructuredStreamError::NetworkErrorWithSource { message, retryable } => {
            ProviderError::network_error_with_restored_source(
                redact_stream_error_message(&message),
                retryable,
            )
        }
        StructuredStreamError::RateLimit => ProviderError::RateLimit,
        StructuredStreamError::Timeout => ProviderError::Timeout,
        // タプルバリアントは provider 側の redact を通らないため、ここで必ず伏せる。
        StructuredStreamError::NetworkError { message } => {
            ProviderError::NetworkError(redact_stream_error_message(&message))
        }
        StructuredStreamError::AuthError { message } => {
            ProviderError::AuthError(redact_stream_error_message(&message))
        }
        StructuredStreamError::ContextOverflow { message } => {
            ProviderError::context_overflow(redact_stream_error_message(&message))
        }
        StructuredStreamError::Other { message } => {
            ProviderError::ApiError(redact_stream_error_message(&message))
        }
    })
}

// ============================================================================
// Process result — mirrors TS "compact" | "stop" | "continue"
// ============================================================================

/// The outcome of a single `StreamProcessor::process` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessResult {
    /// Triggers session compaction.
    Compact,
    /// Stops processing (blocked by permission or error).
    Stop,
    /// Normal completion — caller may continue the agentic loop.
    Continue,
}

/// Additional processor-side signals to be consumed by callers (e.g. permission flow).
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessorEvent {
    DoomLoopDetected {
        tool_name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolExecutionErrorKind {
    PermissionDenied,
    QuestionRejected,
    Other,
}

// ============================================================================
// Stream Processor — full state machine matching TS SessionProcessor
// ============================================================================

/// Tracks reasoning blocks being streamed.
#[derive(Debug, Clone)]
struct ReasoningState {
    _part_id: String,
    text: String,
    _start_time: i64,
    _metadata: Option<serde_json::Value>,
}

/// Tracks tool calls being streamed.
#[derive(Debug, Clone)]
pub struct ToolCallState {
    part_id: String,
    tool_name: String,
    _call_id: String,
    raw_input: String,
    parsed_input: serde_json::Value,
    state: ToolState,
}

/// Tracks the current text block being streamed.
#[derive(Debug, Clone)]
struct TextState {
    _part_id: String,
    text: String,
    _start_time: i64,
    _metadata: Option<serde_json::Value>,
}

/// Full stream processor with state management, retry, doom-loop detection,
/// and abort handling — matching the TS `SessionProcessor.create()`.
pub struct StreamProcessor {
    /// Map of reasoning block id -> state.
    reasoning_map: HashMap<String, ReasoningState>,
    /// Map of tool call id -> state.
    tool_calls: HashMap<String, ToolCallState>,
    /// Current text block being streamed.
    current_text: Option<TextState>,
    /// Current snapshot hash (from start-step).
    snapshot: Option<String>,
    /// Whether processing was blocked (permission denied).
    blocked: bool,
    /// Current retry attempt counter.
    attempt: u32,
    /// Whether compaction is needed.
    needs_compaction: bool,
    /// Session ID.
    session_id: String,
    /// Assistant message ID.
    _assistant_message_id: String,
    /// Accumulated cost finalized across completed attempts.
    cost: f64,
    /// Latest token usage finalized at the end of an attempt.
    tokens: Option<MessageUsage>,
    /// Cost produced by the current attempt only. Discarded when it is retried.
    attempt_cost: f64,
    /// Usage produced by the current attempt only. Discarded when it is retried.
    attempt_tokens: Option<MessageUsage>,
    /// Finish reason from the last step.
    finish_reason: Option<String>,
    /// Error from the stream, if any.
    error: Option<MessageError>,
    /// All completed tool parts for doom-loop detection.
    completed_tool_parts: Vec<CompletedToolRecord>,
    /// Patches computed from snapshot diffs between step-start and step-finish.
    pending_patches: Vec<SnapshotPatch>,
    /// Non-terminal processor events for the caller.
    pending_events: Vec<ProcessorEvent>,
    /// Compaction policy engine (uses model-specific limits).
    compaction_engine: CompactionEngine,
    /// Resolved model limits for the active stream.
    model_limits: Option<ModelLimits>,
}

/// Minimal record for doom-loop detection.
#[derive(Debug, Clone)]
struct CompletedToolRecord {
    tool_name: String,
    input: serde_json::Value,
}

pub struct LlmProcessor;

impl StreamProcessor {
    /// Create a new stream processor for a given assistant message.
    pub fn new(session_id: String, assistant_message_id: String) -> Self {
        Self {
            reasoning_map: HashMap::new(),
            tool_calls: HashMap::new(),
            current_text: None,
            snapshot: None,
            blocked: false,
            attempt: 0,
            needs_compaction: false,
            session_id,
            _assistant_message_id: assistant_message_id,
            cost: 0.0,
            tokens: None,
            attempt_cost: 0.0,
            attempt_tokens: None,
            finish_reason: None,
            error: None,
            completed_tool_parts: Vec::new(),
            pending_patches: Vec::new(),
            pending_events: Vec::new(),
            compaction_engine: CompactionEngine::new(CompactionConfig::default()),
            model_limits: None,
        }
    }

    /// Clear every piece of state that belongs to a single streaming attempt.
    ///
    /// A retry must start from a clean slate: tool calls, patches, doom-loop
    /// records, plugin events, blocking decisions and partial usage from the
    /// failed attempt would otherwise leak into the next one (double-counted
    /// cost, stale `Running` tool parts, spurious blocks and duplicate patches).
    /// `cost` / `tokens` are intentionally not cleared here — they are only
    /// finalized when an attempt ends (see [`Self::finalize_attempt_usage`]).
    fn reset_attempt_state(&mut self) {
        self.reasoning_map.clear();
        self.tool_calls.clear();
        self.current_text = None;
        self.snapshot = None;
        self.blocked = false;
        self.needs_compaction = false;
        self.error = None;
        self.finish_reason = None;
        self.completed_tool_parts.clear();
        self.pending_patches.clear();
        self.pending_events.clear();
        self.attempt_cost = 0.0;
        self.attempt_tokens = None;
    }

    /// Finalize the usage produced by the attempt that just ended.
    ///
    /// Only the final attempt of a `process()` call contributes to the
    /// processor's cumulative cost and token snapshot; attempts that are
    /// retried are discarded by [`Self::reset_attempt_state`].
    fn finalize_attempt_usage(&mut self) {
        self.cost += self.attempt_cost;
        if let Some(tokens) = self.attempt_tokens.take() {
            self.tokens = Some(tokens);
        }
        self.attempt_cost = 0.0;
    }

    /// Get the accumulated cost.
    pub fn cost(&self) -> f64 {
        self.cost
    }

    /// Get the latest token usage.
    pub fn tokens(&self) -> Option<&MessageUsage> {
        self.tokens.as_ref()
    }

    /// Get the finish reason.
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    /// Get the error, if any.
    pub fn error(&self) -> Option<&MessageError> {
        self.error.as_ref()
    }

    /// Drain all pending patches computed from snapshot diffs.
    pub fn take_patches(&mut self) -> Vec<SnapshotPatch> {
        std::mem::take(&mut self.pending_patches)
    }

    /// Drain processor-side events (e.g. doom-loop detection).
    pub fn take_events(&mut self) -> Vec<ProcessorEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Look up a tool part by its call ID.
    pub fn part_from_tool_call(&self, tool_call_id: &str) -> Option<&ToolCallState> {
        self.tool_calls.get(tool_call_id)
    }

    fn record_tool_call_for_doom_loop(&mut self, tool_name: String, input: serde_json::Value) {
        self.completed_tool_parts.push(CompletedToolRecord {
            tool_name: tool_name.clone(),
            input: input.clone(),
        });

        if self.completed_tool_parts.len() < DOOM_LOOP_THRESHOLD {
            return;
        }

        let recent =
            &self.completed_tool_parts[self.completed_tool_parts.len() - DOOM_LOOP_THRESHOLD..];
        let all_same = recent.iter().all(|r| {
            r.tool_name == tool_name
                && serde_json::to_string(&r.input).ok() == serde_json::to_string(&input).ok()
        });

        if all_same {
            tracing::warn!(
                session_id = %self.session_id,
                tool = %tool_name,
                "Doom loop detected: last {} tool calls are identical",
                DOOM_LOOP_THRESHOLD
            );
            self.pending_events
                .push(ProcessorEvent::DoomLoopDetected { tool_name, input });
            self.blocked = true;
        }
    }

    fn should_block_for_tool_error(kind: Option<StreamToolErrorKind>, error: &str) -> bool {
        matches!(
            classify_tool_error(kind, error),
            ToolExecutionErrorKind::PermissionDenied | ToolExecutionErrorKind::QuestionRejected
        )
    }

    /// Run the full stream processing loop with retry support.
    /// Returns `ProcessResult` matching the TS "compact" | "stop" | "continue".
    pub async fn process(
        &mut self,
        stream_input: StreamInput,
        provider: Arc<dyn Provider>,
    ) -> ProcessResult {
        // A new `process()` call starts a fresh attempt chain; nothing from a
        // previous invocation may be carried into it.
        self.reset_attempt_state();
        self.model_limits = Some(resolve_model_limits(&stream_input, provider.as_ref()));

        let max_attempts = stream_input.retries.unwrap_or(DEFAULT_MAX_RETRIES);
        let mut retry_state = retry::RetryState::new(max_attempts);

        loop {
            // Bound every attempt so a stalled provider cannot block forever.
            // Provider adapters build their HTTP clients without a timeout.
            let attempt_result = match tokio::time::timeout(
                LLM_STREAM_TIMEOUT,
                self.process_stream(&stream_input, provider.clone()),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("LLM stream timed out")),
            };

            match attempt_result {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!(
                        session_id = %self.session_id,
                        error = %e,
                        "Stream processing error"
                    );

                    let error =
                        crate::message_v2::error_from_anyhow(e, &stream_input.model.provider_id);

                    let retry_msg = retry::retryable(&error);
                    if let Some(retry_message) = retry_msg {
                        if retry_state.should_retry() {
                            retry_state.increment(retry_message.clone());
                            self.attempt = retry_state.attempt;

                            let api_info = match &error {
                                MessageError::ApiError {
                                    message,
                                    is_retryable,
                                    response_headers,
                                    response_body,
                                    ..
                                } => Some(ApiErrorInfo {
                                    message: message.clone(),
                                    is_retryable: *is_retryable,
                                    response_headers: response_headers.clone(),
                                    response_body: response_body.clone(),
                                }),
                                _ => None,
                            };

                            let delay_ms = retry_state.next_delay(api_info.as_ref());

                            tracing::info!(
                                session_id = %self.session_id,
                                attempt = self.attempt,
                                max_attempts = max_attempts,
                                delay_ms = delay_ms,
                                message = %retry_message,
                                "Retrying after error"
                            );

                            // Sleep with cancellation support. Only an
                            // uninterrupted sleep earns another attempt.
                            if retry::sleep_with_cancel(delay_ms, stream_input.abort.clone())
                                .await
                                .is_ok()
                            {
                                // Retry begins: drop all state produced by the
                                // failed attempt before the next `process_stream`.
                                self.reset_attempt_state();
                                continue;
                            }

                            tracing::info!(
                                session_id = %self.session_id,
                                "Retry sleep cancelled by abort"
                            );
                        } else {
                            tracing::warn!(
                                session_id = %self.session_id,
                                attempt = self.attempt,
                                max_attempts = max_attempts,
                                "Retry limit reached; stopping"
                            );
                        }
                    }

                    self.error = Some(error);
                }
            }

            // After stream ends (success or non-retryable error):
            // Finalize this attempt's usage. Retried attempts never reach this
            // point, so their partial cost is discarded instead of accumulating.
            self.finalize_attempt_usage();

            // Flush any pending snapshot that wasn't closed by a FinishStep event.
            // This mirrors TS processor.ts lines 379-392 where, after the stream
            // loop exits (e.g. due to error or abort), any remaining snapshot is
            // diffed and emitted as a patch.
            self.flush_pending_snapshot(stream_input.work_dir.as_deref());

            // Mark any incomplete tool parts as error (abort handling)
            self.abort_incomplete_tools();

            if self.needs_compaction {
                return ProcessResult::Compact;
            }
            if self.blocked || self.error.is_some() {
                return ProcessResult::Stop;
            }
            return ProcessResult::Continue;
        }
    }

    /// Process a single stream attempt.
    async fn process_stream(
        &mut self,
        input: &StreamInput,
        provider: Arc<dyn Provider>,
    ) -> anyhow::Result<()> {
        let mut output = LlmProcessor::stream_from_input(input, provider).await?;

        while let Some(event) = output.events.recv().await {
            if input.abort.is_cancelled() {
                return Err(anyhow::anyhow!("Stream aborted"));
            }

            match event {
                StreamEvent::Start => {
                    // Session is now busy — already set by caller
                }

                // ============================================================
                // Reasoning events
                // ============================================================
                StreamEvent::ReasoningStart { id } => {
                    if self.reasoning_map.contains_key(&id) {
                        continue;
                    }
                    let now = chrono::Utc::now().timestamp_millis();
                    let part_id =
                        opencode_core::id::create(opencode_core::id::Prefix::Part, true, None);
                    self.reasoning_map.insert(
                        id,
                        ReasoningState {
                            _part_id: part_id,
                            text: String::new(),
                            _start_time: now,
                            _metadata: None,
                        },
                    );
                }

                StreamEvent::ReasoningDelta { id, text } => {
                    if let Some(state) = self.reasoning_map.get_mut(&id) {
                        state.text.push_str(&text);
                    }
                }

                StreamEvent::ReasoningEnd { id } => {
                    if let Some(mut state) = self.reasoning_map.remove(&id) {
                        state.text = state.text.trim_end().to_string();
                    }
                }

                // ============================================================
                // Tool input streaming events
                // ============================================================
                StreamEvent::ToolInputStart { id, tool_name } => {
                    let part_id = self
                        .tool_calls
                        .get(&id)
                        .map(|tc| tc.part_id.clone())
                        .unwrap_or_else(|| {
                            opencode_core::id::create(opencode_core::id::Prefix::Part, true, None)
                        });

                    self.tool_calls.insert(
                        id.clone(),
                        ToolCallState {
                            part_id,
                            tool_name,
                            _call_id: id,
                            raw_input: String::new(),
                            parsed_input: serde_json::json!({}),
                            state: ToolState::Pending {
                                input: serde_json::json!({}),
                                raw: String::new(),
                            },
                        },
                    );
                }

                StreamEvent::ToolInputDelta { id, delta } => {
                    if let Some(tc) = self.tool_calls.get_mut(&id) {
                        tc.raw_input.push_str(&delta);
                    }
                }

                StreamEvent::ToolInputEnd { id: _ } => {
                    // Input fully received; the ToolCallEnd event will finalize
                }

                // ============================================================
                // Tool call lifecycle (full call assembled)
                // ============================================================
                StreamEvent::ToolCallStart { id, name } => {
                    let part_id = self
                        .tool_calls
                        .get(&id)
                        .map(|tc| tc.part_id.clone())
                        .unwrap_or_else(|| {
                            opencode_core::id::create(opencode_core::id::Prefix::Part, true, None)
                        });

                    self.tool_calls.insert(
                        id.clone(),
                        ToolCallState {
                            part_id,
                            tool_name: name,
                            _call_id: id,
                            raw_input: String::new(),
                            parsed_input: serde_json::json!({}),
                            state: ToolState::Pending {
                                input: serde_json::json!({}),
                                raw: String::new(),
                            },
                        },
                    );
                }

                StreamEvent::ToolCallDelta { id, input } => {
                    if let Some(tc) = self.tool_calls.get_mut(&id) {
                        tc.raw_input.push_str(&input);
                    }
                }

                StreamEvent::ToolCallEnd { id, name, input } => {
                    let now = chrono::Utc::now().timestamp_millis();

                    if let Some(tc) = self.tool_calls.get_mut(&id) {
                        tc.tool_name = name.clone();
                        tc.parsed_input = input.clone();
                        tc.state = ToolState::Running {
                            input: input.clone(),
                            title: None,
                            metadata: None,
                            time: RunningTime { start: now },
                        };
                    }
                    self.record_tool_call_for_doom_loop(name, input);
                }

                // ============================================================
                // Tool result / error
                // ============================================================
                StreamEvent::ToolResult {
                    tool_call_id,
                    output,
                    input,
                    ..
                } => {
                    let now = chrono::Utc::now().timestamp_millis();
                    if let Some(tc) = self.tool_calls.get_mut(&tool_call_id) {
                        if matches!(tc.state, ToolState::Running { .. }) {
                            let start_time = match &tc.state {
                                ToolState::Running { time, .. } => time.start,
                                _ => now,
                            };
                            tc.state = ToolState::Completed {
                                input: input.unwrap_or_else(|| tc.parsed_input.clone()),
                                output: output.output,
                                title: output.title,
                                metadata: output.metadata,
                                time: CompletedTime {
                                    start: start_time,
                                    end: now,
                                    compacted: None,
                                },
                                attachments: None,
                            };
                        }
                    }
                }

                StreamEvent::ToolError {
                    tool_call_id,
                    error,
                    input,
                    kind,
                    ..
                } => {
                    let now = chrono::Utc::now().timestamp_millis();
                    if let Some(tc) = self.tool_calls.get_mut(&tool_call_id) {
                        if matches!(tc.state, ToolState::Running { .. }) {
                            let start_time = match &tc.state {
                                ToolState::Running { time, .. } => time.start,
                                _ => now,
                            };
                            tc.state = ToolState::Error {
                                input: input.unwrap_or_else(|| tc.parsed_input.clone()),
                                error: error.clone(),
                                metadata: None,
                                time: ErrorTime {
                                    start: start_time,
                                    end: now,
                                },
                            };

                            if Self::should_block_for_tool_error(kind, &error) {
                                self.blocked = true;
                            }
                        }
                    }
                }

                // ============================================================
                // Text events
                // ============================================================
                StreamEvent::TextStart => {
                    let now = chrono::Utc::now().timestamp_millis();
                    let part_id =
                        opencode_core::id::create(opencode_core::id::Prefix::Part, true, None);
                    self.current_text = Some(TextState {
                        _part_id: part_id,
                        text: String::new(),
                        _start_time: now,
                        _metadata: None,
                    });
                }

                StreamEvent::TextDelta(delta) => {
                    if let Some(ref mut text_state) = self.current_text {
                        text_state.text.push_str(&delta);
                    }
                }

                StreamEvent::TextEnd => {
                    if let Some(mut text_state) = self.current_text.take() {
                        text_state.text = text_state.text.trim_end().to_string();

                        // Plugin hook: experimental.text.complete
                        let hook_outputs = opencode_plugin::trigger_collect(
                            HookContext::new(HookEvent::TextComplete)
                                .with_session(&self.session_id)
                                .with_data(
                                    "message_id",
                                    serde_json::json!(&self._assistant_message_id),
                                )
                                .with_data("part_id", serde_json::json!(&text_state._part_id))
                                .with_data("text", serde_json::json!(&text_state.text)),
                        )
                        .await;
                        for output in hook_outputs {
                            let Some(payload) = output.payload.as_ref() else {
                                continue;
                            };
                            if let Some(next_text) = extract_text_from_hook_payload(payload) {
                                text_state.text = next_text;
                            }
                        }
                    }
                }

                // ============================================================
                // Step events
                // ============================================================
                StreamEvent::StartStep => {
                    // Track snapshot for diff computation
                    if let Some(ref dir) = input.work_dir {
                        match Snapshot::track(dir) {
                            Ok(hash) => {
                                self.snapshot = Some(hash);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    session_id = %self.session_id,
                                    error = %e,
                                    "Failed to track snapshot at step-start"
                                );
                            }
                        }
                    }
                }

                StreamEvent::FinishStep {
                    finish_reason,
                    usage,
                    provider_metadata,
                    ..
                } => {
                    let step_usage = usage_from_stream(
                        &usage,
                        provider_metadata.as_ref(),
                        &input.model.provider_id,
                    );
                    let step_cost = compute_cost(
                        &step_usage,
                        input.cost_rates.as_ref(),
                        &input.model.provider_id,
                    );

                    // Accumulate into attempt-local scratch fields. They are
                    // committed only when this attempt ends (or discarded on
                    // retry), preventing double counting across retries.
                    self.attempt_cost += step_cost;
                    let mut usage_with_cost = step_usage.clone();
                    usage_with_cost.total_cost = step_cost;
                    self.attempt_tokens = Some(usage_with_cost.clone());
                    self.finish_reason = finish_reason;

                    // Check compaction using model-specific context limits.
                    if let Some(ref limits) = self.model_limits {
                        let token_usage = token_usage_from_message_usage(&usage_with_cost);
                        if self.compaction_engine.is_overflow(&token_usage, limits) {
                            self.needs_compaction = true;
                        }
                    }

                    // Compute patch from snapshot diff (matching TS Snapshot.patch)
                    if let Some(ref snapshot_hash) = self.snapshot {
                        if let Some(ref dir) = input.work_dir {
                            match Snapshot::diff(dir, snapshot_hash) {
                                Ok(diffs) => {
                                    let files: Vec<String> =
                                        diffs.into_iter().map(|d| d.path).collect();
                                    if !files.is_empty() {
                                        self.pending_patches.push(SnapshotPatch {
                                            hash: snapshot_hash.clone(),
                                            files,
                                        });
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        session_id = %self.session_id,
                                        error = %e,
                                        "Failed to compute snapshot patch"
                                    );
                                }
                            }
                        }
                    }

                    self.snapshot = None;
                }

                // ============================================================
                // Terminal events
                // ============================================================
                StreamEvent::Finish => {}

                StreamEvent::Done => {
                    break;
                }

                StreamEvent::Error(message) => {
                    // 構造化エンベロープなら ProviderError を anyhow::Error の
                    // ルートとして保持し、error_from_anyhow の downcast が
                    // 成功するようにする（Retry-After の伝播に必須）。
                    if let Some(provider_error) = decode_stream_error(&message) {
                        return Err(anyhow::Error::new(provider_error));
                    }
                    // 復元できなかった生ペイロードはレスポンス本文を含む可能性があるため、
                    // エラー文字列へそのまま埋め込まず固定メッセージへ正規化する。
                    return Err(anyhow::anyhow!(STREAM_ERROR_DECODE_FAILED));
                }

                StreamEvent::Usage { .. } => {}
            }

            if self.needs_compaction {
                break;
            }
        }

        Ok(())
    }

    /// Flush any pending snapshot that wasn't closed by a `FinishStep` event.
    ///
    /// When the stream ends abruptly (error, abort, or missing `finish-step`),
    /// there may be a snapshot from `StartStep` that was never diffed. This
    /// mirrors the TS processor.ts post-loop logic (lines 379-392) that
    /// computes the final patch if `snapshot` is still set.
    fn flush_pending_snapshot(&mut self, work_dir: Option<&Path>) {
        let snapshot_hash = match self.snapshot.take() {
            Some(h) => h,
            None => return,
        };
        let dir = match work_dir {
            Some(d) => d,
            None => return,
        };

        match Snapshot::diff(dir, &snapshot_hash) {
            Ok(diffs) => {
                let files: Vec<String> = diffs.into_iter().map(|d| d.path).collect();
                if !files.is_empty() {
                    self.pending_patches.push(SnapshotPatch {
                        hash: snapshot_hash,
                        files,
                    });
                }
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %self.session_id,
                    error = %e,
                    "Failed to compute snapshot patch during flush"
                );
            }
        }
    }

    /// Mark any incomplete tool parts as "error" with "Tool execution aborted".
    /// Mirrors the TS abort handling at the end of the process loop.
    fn abort_incomplete_tools(&mut self) {
        let now = chrono::Utc::now().timestamp_millis();
        for tc in self.tool_calls.values_mut() {
            match &tc.state {
                ToolState::Completed { .. } | ToolState::Error { .. } => {
                    // Already terminal — skip
                }
                _ => {
                    tc.state = ToolState::Error {
                        input: tc.parsed_input.clone(),
                        error: "Tool execution aborted".to_string(),
                        metadata: None,
                        time: ErrorTime {
                            start: now,
                            end: now,
                        },
                    };
                }
            }
        }
    }
}

impl LlmProcessor {
    /// Start a stream from a `StreamInput` and provider.
    pub async fn stream(
        input: StreamInput,
        provider: Arc<dyn Provider>,
    ) -> anyhow::Result<StreamOutput> {
        Self::stream_from_input(&input, provider).await
    }

    /// Internal: build the request and start streaming.
    async fn stream_from_input(
        input: &StreamInput,
        provider: Arc<dyn Provider>,
    ) -> anyhow::Result<StreamOutput> {
        let session_id = input.session_id.clone();
        let model_id = input.model.model_id.clone();
        let provider_id = input.model.provider_id.clone();
        let agent_name = input.agent.name.clone();

        tracing::info!(
            session_id = %session_id,
            model_id = %model_id,
            provider_id = %provider_id,
            agent = %agent_name,
            "Starting LLM stream"
        );

        let mut system_prompt = build_system_prompt(input);
        let system_header_before_hooks = system_prompt.first().cloned().unwrap_or_default();

        // Plugin hook: chat.system.transform — let plugins modify the system prompt
        let system_hook_outputs = opencode_plugin::trigger_collect(
            HookContext::new(HookEvent::ChatSystemTransform)
                .with_session(&session_id)
                .with_data("model_id", serde_json::json!(&model_id))
                .with_data("provider_id", serde_json::json!(&provider_id))
                .with_data("system", serde_json::json!(&system_prompt)),
        )
        .await;
        apply_chat_system_hook_outputs(&mut system_prompt, system_hook_outputs);
        rejoin_system_prompt_if_needed(&mut system_prompt, &system_header_before_hooks);

        let mut tools = resolve_tools(input.tools.clone(), &input.agent);
        // Inject LiteLLM dummy tool if needed
        inject_litellm_dummy_tool(&mut tools, &input.messages, &input.model.provider_id);

        let mut max_tokens = if input.small {
            Some(1024u64)
        } else {
            Some(input.agent.max_tokens.unwrap_or(OUTPUT_TOKEN_MAX))
        };

        let mut temperature = if input.small {
            Some(0.5f32)
        } else {
            input.agent.temperature
        };
        let mut top_p = input.agent.top_p;
        let mut provider_options: HashMap<String, serde_json::Value> = HashMap::new();

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        // Plugin hook: chat.params — let plugins modify LLM request parameters
        let params_hook_outputs = opencode_plugin::trigger_collect(
            HookContext::new(HookEvent::ChatParams)
                .with_session(&session_id)
                .with_data("model_id", serde_json::json!(&model_id))
                .with_data("provider_id", serde_json::json!(&provider_id))
                .with_data("max_tokens", serde_json::json!(max_tokens))
                .with_data("temperature", serde_json::json!(temperature))
                .with_data("top_p", serde_json::json!(top_p))
                .with_data("agent", serde_json::json!(&agent_name))
                .with_data("options", serde_json::json!(&provider_options)),
        )
        .await;
        apply_chat_params_hook_outputs(
            &mut max_tokens,
            &mut temperature,
            &mut top_p,
            &mut provider_options,
            params_hook_outputs,
        );

        // Plugin hook: chat.headers — let plugins inject custom HTTP headers
        let header_hook_outputs = opencode_plugin::trigger_collect(
            HookContext::new(HookEvent::ChatHeaders)
                .with_session(&session_id)
                .with_data("model_id", serde_json::json!(&model_id))
                .with_data("provider_id", serde_json::json!(&provider_id))
                .with_data("headers", serde_json::json!({})),
        )
        .await;
        let plugin_headers = collect_chat_header_hook_outputs(header_hook_outputs);
        if !plugin_headers.is_empty() {
            provider_options.insert("headers".to_string(), serde_json::json!(plugin_headers));
        }

        let request = ChatRequest {
            model: model_id,
            messages: build_messages(&system_prompt, &input.messages),
            max_tokens,
            temperature,
            system: None,
            tools: if tools.is_empty() {
                None
            } else {
                Some(tools.into_values().collect())
            },
            stream: Some(true),
            top_p,
            variant: None,
            provider_options: if provider_options.is_empty() {
                None
            } else {
                Some(provider_options)
            },
        };

        let abort = input.abort.clone();

        tokio::spawn(async move {
            match provider.chat_stream(request).await {
                Ok(stream) => {
                    forward_stream_events(stream, tx, abort).await;
                }
                Err(e) => {
                    tracing::error!("Failed to start stream: {}", e);
                    // 文字列化せず構造化情報を保持したまま StreamEvent へ渡す。
                    let _ = tx.send(StreamEvent::Error(encode_stream_error(&e))).await;
                }
            }
        });

        Ok(StreamOutput { events: rx })
    }

    pub fn has_tool_calls(messages: &[Message]) -> bool {
        for msg in messages {
            if let Content::Parts(parts) = &msg.content {
                for part in parts {
                    if part.tool_use.is_some() || part.tool_result.is_some() {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// ストリームのイベントを `tx` へ転送し、キャンセル／受信側切断を即座に検知する。
///
/// `abort.cancelled()` と `tx.closed()` を `stream.next()` と競合させることで、
/// 次のイベント待ち（アイドル中）でも上流ストリームの future をドロップして終了できる。
/// `biased` によりキャンセル／切断分岐を優先しつつ、イベントの順序・Error イベント送信・
/// ログは従来のループと同じに保つ。
async fn forward_stream_events<S, E>(
    mut stream: S,
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    abort: tokio_util::sync::CancellationToken,
) where
    S: futures::Stream<Item = Result<StreamEvent, E>> + Send + Unpin,
    E: std::fmt::Display + std::any::Any,
{
    loop {
        tokio::select! {
            biased;
            _ = abort.cancelled() => {
                tracing::info!("LLM stream cancelled");
                break;
            }
            _ = tx.closed() => {
                tracing::warn!("Stream receiver dropped");
                break;
            }
            event_result = stream.next() => {
                match event_result {
                    Some(Ok(event)) => {
                        if tx.send(event).await.is_err() {
                            tracing::warn!("Stream receiver dropped");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        let provider_error =
                            (&e as &dyn std::any::Any).downcast_ref::<ProviderError>();
                        let status_code = provider_error.and_then(ProviderError::status_code);
                        // ストリーム途中の ProviderError も構造化情報を保持したまま
                        // 転送し、開始時エラーと同じ retry 経路へ載せる。
                        let payload = provider_error
                            .map(encode_stream_error)
                            .unwrap_or_else(|| e.to_string());
                        // 生ペイロードをログへ出さず、status_code と構造化マーカーの
                        // 有無だけを記録する（本文混入防止）。
                        tracing::error!(
                            status_code = ?status_code,
                            has_structured_marker = payload.starts_with(STRUCTURED_STREAM_ERROR_MARKER),
                            "Stream error"
                        );
                        let _ = tx.send(StreamEvent::Error(payload)).await;
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

fn build_system_prompt(input: &StreamInput) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();

    // Use agent prompt if available, otherwise fall back to system prompts
    if let Some(ref prompt) = input.agent.system_prompt {
        parts.push(prompt.clone());
    }

    // Any custom prompts passed into this call
    parts.extend(input.system.clone());

    // Any custom prompt from the last user message
    if let MessageInfo::User {
        system: Some(ref user_system),
        ..
    } = input.user
    {
        parts.push(user_system.clone());
    }

    // Filter empty strings and join into a single system block
    let joined: String = parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let mut system = vec![joined];

    // Rejoin logic: maintain 2-part structure for caching.
    // If plugins added extra parts and the header is unchanged,
    // collapse everything after the header into a single second part.
    let header = system.first().cloned().unwrap_or_default();
    if system.len() > 2 && system.first().map(|s| s.as_str()) == Some(header.as_str()) {
        let rest: String = system[1..].join("\n");
        system = vec![header, rest];
    }

    system
}

fn hook_payload_object(
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    payload
        .get("output")
        .and_then(|value| value.as_object())
        .or_else(|| payload.as_object())
        .or_else(|| payload.get("data").and_then(|value| value.as_object()))
}

fn parse_string_array(value: &serde_json::Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(|s| s.to_string()))
            .collect()
    })
}

fn apply_chat_system_hook_outputs(
    system_prompt: &mut Vec<String>,
    hook_outputs: Vec<opencode_plugin::HookOutput>,
) {
    for output in hook_outputs {
        let Some(payload) = output.payload.as_ref() else {
            continue;
        };
        let Some(object) = hook_payload_object(payload) else {
            continue;
        };
        let Some(system) = object.get("system").and_then(parse_string_array) else {
            continue;
        };
        *system_prompt = system;
    }
}

fn rejoin_system_prompt_if_needed(system_prompt: &mut Vec<String>, original_header: &str) {
    if system_prompt.len() > 2 && system_prompt.first().map(|s| s.as_str()) == Some(original_header)
    {
        let header = system_prompt.first().cloned().unwrap_or_default();
        let rest = system_prompt[1..].join("\n");
        *system_prompt = vec![header, rest];
    }
}

fn value_to_f32(value: &serde_json::Value) -> Option<f32> {
    value.as_f64().map(|v| v as f32)
}

fn apply_chat_params_hook_outputs(
    max_tokens: &mut Option<u64>,
    temperature: &mut Option<f32>,
    top_p: &mut Option<f32>,
    provider_options: &mut HashMap<String, serde_json::Value>,
    hook_outputs: Vec<opencode_plugin::HookOutput>,
) {
    for output in hook_outputs {
        let Some(payload) = output.payload.as_ref() else {
            continue;
        };
        let Some(object) = hook_payload_object(payload) else {
            continue;
        };

        if let Some(next_max_tokens) = object
            .get("maxTokens")
            .or_else(|| object.get("max_tokens"))
            .and_then(|value| value.as_u64())
        {
            *max_tokens = Some(next_max_tokens);
        }
        if let Some(next_temperature) = object.get("temperature").and_then(value_to_f32) {
            *temperature = Some(next_temperature);
        }
        if let Some(next_top_p) = object
            .get("topP")
            .or_else(|| object.get("top_p"))
            .and_then(value_to_f32)
        {
            *top_p = Some(next_top_p);
        }
        if let Some(top_k) = object
            .get("topK")
            .or_else(|| object.get("top_k"))
            .and_then(value_to_f32)
        {
            provider_options.insert("topK".to_string(), serde_json::json!(top_k));
        }
        if let Some(options) = object.get("options").and_then(|value| value.as_object()) {
            for (key, value) in options {
                provider_options.insert(key.clone(), value.clone());
            }
        }
        if let Some(options) = object
            .get("provider_options")
            .or_else(|| object.get("providerOptions"))
            .and_then(|value| value.as_object())
        {
            for (key, value) in options {
                provider_options.insert(key.clone(), value.clone());
            }
        }
    }
}

fn collect_chat_header_hook_outputs(
    hook_outputs: Vec<opencode_plugin::HookOutput>,
) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for output in hook_outputs {
        let Some(payload) = output.payload.as_ref() else {
            continue;
        };
        let Some(object) = hook_payload_object(payload) else {
            continue;
        };
        let Some(incoming) = object.get("headers").and_then(|value| value.as_object()) else {
            continue;
        };
        for (key, value) in incoming {
            if let Some(value_str) = value.as_str() {
                headers.insert(key.clone(), value_str.to_string());
            }
        }
    }
    headers
}

fn extract_text_from_hook_payload(payload: &serde_json::Value) -> Option<String> {
    hook_payload_object(payload)
        .and_then(|object| object.get("text"))
        .and_then(|value| value.as_str())
        .map(|text| text.to_string())
}

fn resolve_model_limits(input: &StreamInput, provider: &dyn Provider) -> ModelLimits {
    let model = provider.get_model(&input.model.model_id);
    ModelLimits {
        context: model
            .map(|info| info.context_window)
            .unwrap_or_else(|| get_model_context_limit(&input.model.model_id)),
        max_input: None,
        max_output: input
            .agent
            .max_tokens
            .or_else(|| model.map(|info| info.max_output_tokens))
            .unwrap_or(OUTPUT_TOKEN_MAX),
    }
}

fn token_usage_from_message_usage(usage: &MessageUsage) -> TokenUsage {
    TokenUsage {
        input: usage.input_tokens,
        output: usage.output_tokens + usage.reasoning_tokens,
        cache_read: usage.cache_read_tokens,
        cache_write: usage.cache_write_tokens,
        total: usage.input_tokens
            + usage.output_tokens
            + usage.reasoning_tokens
            + usage.cache_read_tokens
            + usage.cache_write_tokens,
    }
}

/// Convert `StreamUsage` from the provider into session-level `MessageUsage`.
fn usage_from_stream(
    usage: &StreamUsage,
    provider_metadata: Option<&serde_json::Value>,
    provider_id: &str,
) -> MessageUsage {
    let (meta_cache_read, meta_cache_write) =
        extract_cache_tokens_from_metadata(provider_metadata, provider_id);
    let cache_read_tokens = usage.cache_read_tokens.max(meta_cache_read);
    let cache_write_tokens = usage.cache_write_tokens.max(meta_cache_write);

    MessageUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        // Anthropic/Bedrock `input_tokens` do not include cached usage; keep cache
        // tokens explicit so total usage and compaction logic can account for them.
        cache_write_tokens,
        cache_read_tokens,
        total_cost: 0.0, // Cost is computed separately
    }
}

/// Compute cost from stream usage using model-specific pricing rates.
/// Falls back to Claude-like pricing ($3/$15 per million) if no rates provided.
fn compute_cost(usage: &MessageUsage, rates: Option<&CostRates>, provider_id: &str) -> f64 {
    let rates = rates.cloned().unwrap_or(CostRates {
        input: 3.0,
        output: 15.0,
        cache_read: 0.3,
        cache_write: 3.75,
        over_200k: None,
    });

    let total_tokens = usage.input_tokens
        + usage.output_tokens
        + usage.reasoning_tokens
        + usage.cache_read_tokens
        + usage.cache_write_tokens;
    let use_over_200k = total_tokens > 200_000;

    let (input_rate, output_rate, cache_read_rate, cache_write_rate) = if use_over_200k {
        if let Some(tier) = rates.over_200k.as_ref() {
            (tier.input, tier.output, tier.cache_read, tier.cache_write)
        } else {
            (
                rates.input,
                rates.output,
                rates.cache_read,
                rates.cache_write,
            )
        }
    } else {
        (
            rates.input,
            rates.output,
            rates.cache_read,
            rates.cache_write,
        )
    };

    // Keep input billing resilient for providers where cache tokens may be folded into input.
    let billable_input_tokens = if is_anthropic_or_bedrock_provider(provider_id) {
        usage
            .input_tokens
            .saturating_sub(usage.cache_read_tokens + usage.cache_write_tokens)
    } else {
        usage.input_tokens
    };

    // Reasoning tokens are billed at output-token rates.
    let billable_output_tokens = usage.output_tokens + usage.reasoning_tokens;

    let input_cost = (billable_input_tokens as f64) * input_rate / 1_000_000.0;
    let output_cost = (billable_output_tokens as f64) * output_rate / 1_000_000.0;
    let cache_read_cost = (usage.cache_read_tokens as f64) * cache_read_rate / 1_000_000.0;
    let cache_write_cost = (usage.cache_write_tokens as f64) * cache_write_rate / 1_000_000.0;

    input_cost + output_cost + cache_read_cost + cache_write_cost
}

fn is_anthropic_or_bedrock_provider(provider_id: &str) -> bool {
    let lower = provider_id.to_ascii_lowercase();
    lower.contains("anthropic") || lower.contains("bedrock")
}

fn metadata_u64(metadata: &serde_json::Value, pointer: &str) -> Option<u64> {
    metadata.pointer(pointer).and_then(|v| v.as_u64())
}

fn extract_cache_tokens_from_metadata(
    provider_metadata: Option<&serde_json::Value>,
    provider_id: &str,
) -> (u64, u64) {
    if !is_anthropic_or_bedrock_provider(provider_id) {
        return (0, 0);
    }

    let Some(metadata) = provider_metadata else {
        return (0, 0);
    };

    let cache_write_tokens = metadata_u64(metadata, "/anthropic/cacheCreationInputTokens")
        .or_else(|| metadata_u64(metadata, "/cacheCreationInputTokens"))
        .or_else(|| metadata_u64(metadata, "/bedrock/cacheCreationInputTokens"))
        .or_else(|| metadata_u64(metadata, "/amazon-bedrock/cacheCreationInputTokens"))
        .or_else(|| metadata_u64(metadata, "/usage/cacheCreationInputTokens"))
        .unwrap_or(0);

    let cache_read_tokens = metadata_u64(metadata, "/anthropic/cacheReadInputTokens")
        .or_else(|| metadata_u64(metadata, "/cacheReadInputTokens"))
        .or_else(|| metadata_u64(metadata, "/bedrock/cacheReadInputTokens"))
        .or_else(|| metadata_u64(metadata, "/amazon-bedrock/cacheReadInputTokens"))
        .or_else(|| metadata_u64(metadata, "/usage/cacheReadInputTokens"))
        .unwrap_or(0);

    (cache_read_tokens, cache_write_tokens)
}

fn classify_tool_error(kind: Option<StreamToolErrorKind>, error: &str) -> ToolExecutionErrorKind {
    if let Some(kind) = kind {
        return match kind {
            StreamToolErrorKind::PermissionDenied => ToolExecutionErrorKind::PermissionDenied,
            StreamToolErrorKind::QuestionRejected => ToolExecutionErrorKind::QuestionRejected,
            StreamToolErrorKind::ExecutionError => ToolExecutionErrorKind::Other,
        };
    }

    let lower = error.to_ascii_lowercase();
    if lower.starts_with("permission denied:") || lower.contains("permission denied") {
        return ToolExecutionErrorKind::PermissionDenied;
    }
    if lower.starts_with("question rejected:") || lower.contains("question rejected") {
        return ToolExecutionErrorKind::QuestionRejected;
    }
    ToolExecutionErrorKind::Other
}

fn build_messages(system: &[String], user_messages: &[Message]) -> Vec<Message> {
    let mut messages: Vec<Message> = system.iter().map(Message::system).collect();

    messages.extend(user_messages.iter().cloned());

    messages
}

fn resolve_tools(
    tools: HashMap<String, ToolDefinition>,
    _agent: &LlmAgent,
) -> HashMap<String, ToolDefinition> {
    tools
}

/// Repair a tool call by trying case-insensitive matching.
/// If the tool name doesn't match any known tool, try lowercase.
/// If still no match, return "invalid" as the tool name.
pub fn repair_tool_call(name: &str, tools: &HashMap<String, ToolDefinition>) -> String {
    // Exact match
    if tools.contains_key(name) {
        return name.to_string();
    }

    // Try lowercase
    let lower = name.to_lowercase();
    if lower != name && tools.contains_key(&lower) {
        tracing::info!(
            original = name,
            repaired = %lower,
            "Repairing tool call: case mismatch"
        );
        return lower;
    }

    // No match - return "invalid"
    tracing::warn!(tool_name = name, "Unknown tool call, mapping to 'invalid'");
    "invalid".to_string()
}

/// Inject a dummy tool for LiteLLM proxy compatibility.
/// LiteLLM and some Anthropic proxies require the tools parameter to be present
/// when message history contains tool calls, even if no tools are being used.
pub fn inject_litellm_dummy_tool(
    tools: &mut HashMap<String, ToolDefinition>,
    messages: &[Message],
    provider_id: &str,
) {
    // Check if this is a LiteLLM proxy
    let is_litellm = provider_id.to_lowercase().contains("litellm");

    if !is_litellm {
        return;
    }

    // Only inject if tools is empty and messages contain tool calls
    if !tools.is_empty() {
        return;
    }

    if !LlmProcessor::has_tool_calls(messages) {
        return;
    }

    tracing::info!("Injecting LiteLLM dummy tool for proxy compatibility");

    tools.insert(
        "_noop".to_string(),
        ToolDefinition {
            name: "_noop".to_string(),
            description: Some(
                "Placeholder for LiteLLM/Anthropic proxy compatibility - required when message history contains tool calls but no active tools are needed"
                    .to_string(),
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
    );
}

/// Resolve tools with permission filtering.
/// Removes tools that are disabled by the agent's permission configuration.
pub fn resolve_tools_with_permissions(
    mut tools: HashMap<String, ToolDefinition>,
    disabled_tools: &[String],
    user_tool_overrides: Option<&HashMap<String, bool>>,
) -> HashMap<String, ToolDefinition> {
    // Remove disabled tools
    for tool_name in disabled_tools {
        tools.remove(tool_name);
    }

    // Apply user-level tool overrides
    if let Some(overrides) = user_tool_overrides {
        for (tool_name, enabled) in overrides {
            if !enabled {
                tools.remove(tool_name);
            }
        }
    }

    tools
}

pub struct StreamResult {
    pub text: String,
    pub tool_calls: Vec<ToolCallResult>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolCallResult {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

pub async fn collect_stream(
    mut output: StreamOutput,
    abort: tokio_util::sync::CancellationToken,
) -> anyhow::Result<StreamResult> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut current_tool: Option<ToolCallResult> = None;
    let mut finish_reason = None;

    while let Some(event) = output.events.recv().await {
        if abort.is_cancelled() {
            break;
        }

        match event {
            StreamEvent::Start => {}
            StreamEvent::TextStart => {}
            StreamEvent::TextDelta(delta) => {
                text.push_str(&delta);
            }
            StreamEvent::TextEnd => {}
            StreamEvent::ReasoningStart { .. } => {}
            StreamEvent::ReasoningDelta { .. } => {}
            StreamEvent::ReasoningEnd { .. } => {}
            StreamEvent::ToolInputStart { id, tool_name } => {
                current_tool = Some(ToolCallResult {
                    id,
                    name: tool_name,
                    input: serde_json::Value::Null,
                });
            }
            StreamEvent::ToolInputDelta { delta, .. } => {
                if let Some(ref mut tool) = current_tool {
                    if tool.input.is_null() {
                        tool.input = serde_json::Value::String(delta);
                    } else if let Some(s) = tool.input.as_str() {
                        let mut existing = s.to_string();
                        existing.push_str(&delta);
                        tool.input = serde_json::Value::String(existing);
                    }
                }
            }
            StreamEvent::ToolInputEnd { .. } => {}
            StreamEvent::ToolCallStart { id, name } => {
                current_tool = Some(ToolCallResult {
                    id,
                    name,
                    input: serde_json::Value::Null,
                });
            }
            StreamEvent::ToolCallDelta { input, .. } => {
                if let Some(ref mut tool) = current_tool {
                    if tool.input.is_null() {
                        tool.input = serde_json::Value::String(input);
                    } else if let Some(s) = tool.input.as_str() {
                        let mut existing = s.to_string();
                        existing.push_str(&input);
                        tool.input = serde_json::Value::String(existing);
                    }
                }
            }
            StreamEvent::ToolCallEnd { id, name, input } => {
                tool_calls.push(ToolCallResult { id, name, input });
                current_tool = None;
            }
            StreamEvent::ToolResult { .. } => {}
            StreamEvent::ToolError { .. } => {}
            StreamEvent::StartStep => {}
            StreamEvent::FinishStep { .. } => {}
            StreamEvent::Finish => {}
            StreamEvent::Done => {
                finish_reason = Some("stop".to_string());
                break;
            }
            StreamEvent::Error(e) => {
                // 生ペイロードをログへ出さず、status_code とマーカー有無だけを記録する。
                let decoded = decode_stream_error(&e);
                tracing::error!(
                    status_code = ?decoded.as_ref().and_then(ProviderError::status_code),
                    has_structured_marker = e.starts_with(STRUCTURED_STREAM_ERROR_MARKER),
                    "Stream error"
                );
                break;
            }
            StreamEvent::Usage { .. } => {}
        }
    }

    Ok(StreamResult {
        text,
        tool_calls,
        finish_reason,
    })
}

/// Trigger the TextComplete plugin hook after stream collection.
/// Call this after `collect_stream` when you have the final text.
pub async fn notify_text_complete(session_id: &str, text: &str) {
    if !text.is_empty() {
        let _ = opencode_plugin::trigger_collect(
            HookContext::new(HookEvent::TextComplete)
                .with_session(session_id)
                .with_data("text", serde_json::json!(text)),
        )
        .await;
    }
}

pub fn to_model_messages(messages: &[MessageWithParts]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|msg| message_to_model(&msg.info, &msg.parts))
        .collect()
}

fn message_to_model(info: &MessageInfo, parts: &[Part]) -> Option<Message> {
    let role = match info {
        MessageInfo::User { .. } => opencode_provider::Role::User,
        MessageInfo::Assistant { .. } => opencode_provider::Role::Assistant,
    };

    let content = if parts.len() == 1 {
        match &parts[0] {
            Part::Text { text, .. } => Content::Text(text.clone()),
            _ => Content::Parts(parts.iter().filter_map(part_to_content_part).collect()),
        }
    } else {
        Content::Parts(parts.iter().filter_map(part_to_content_part).collect())
    };

    Some(Message {
        role,
        content,
        cache_control: None,
        provider_options: None,
    })
}

fn part_to_content_part(part: &Part) -> Option<opencode_provider::ContentPart> {
    match part {
        Part::Text { text, .. } => Some(opencode_provider::ContentPart {
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
        Part::File(file_part) => Some(opencode_provider::ContentPart {
            content_type: "file".to_string(),
            text: None,
            image_url: Some(ImageUrl {
                url: file_part.url.clone(),
            }),
            tool_use: None,
            tool_result: None,
            cache_control: None,
            filename: file_part.filename.clone(),
            media_type: Some(file_part.mime.clone()),
            provider_options: None,
        }),
        Part::Tool(tool_part) => {
            if let crate::ToolState::Completed { output, .. } = &tool_part.state {
                Some(opencode_provider::ContentPart {
                    content_type: "tool_result".to_string(),
                    text: Some(output.clone()),
                    image_url: None,
                    tool_use: None,
                    tool_result: Some(opencode_provider::ToolResult {
                        tool_use_id: tool_part.call_id.clone(),
                        content: output.clone(),
                        is_error: Some(false),
                    }),
                    cache_control: None,
                    filename: None,
                    media_type: None,
                    provider_options: None,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencode_provider::ToolDefinition;

    fn make_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: Some(format!("Test tool: {}", name)),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    #[test]
    fn test_repair_tool_call_exact_match() {
        let mut tools = HashMap::new();
        tools.insert("read_file".to_string(), make_tool("read_file"));

        assert_eq!(repair_tool_call("read_file", &tools), "read_file");
    }

    #[test]
    fn test_repair_tool_call_case_mismatch() {
        let mut tools = HashMap::new();
        tools.insert("read_file".to_string(), make_tool("read_file"));

        assert_eq!(repair_tool_call("Read_File", &tools), "read_file");
    }

    #[test]
    fn test_repair_tool_call_unknown() {
        let tools = HashMap::new();
        assert_eq!(repair_tool_call("nonexistent", &tools), "invalid");
    }

    #[test]
    fn test_inject_litellm_dummy_tool_has_tools() {
        let mut tools = HashMap::new();
        tools.insert("existing".to_string(), make_tool("existing"));

        inject_litellm_dummy_tool(&mut tools, &[], "litellm-proxy");
        assert!(!tools.contains_key("_noop")); // Not injected because tools is non-empty
    }

    #[test]
    fn test_resolve_tools_with_permissions_filters() {
        let mut tools = HashMap::new();
        tools.insert("read_file".to_string(), make_tool("read_file"));
        tools.insert("write_file".to_string(), make_tool("write_file"));
        tools.insert("execute".to_string(), make_tool("execute"));

        let disabled = vec!["execute".to_string()];
        let mut overrides = HashMap::new();
        overrides.insert("write_file".to_string(), false);

        let result = resolve_tools_with_permissions(tools, &disabled, Some(&overrides));

        assert!(result.contains_key("read_file"));
        assert!(!result.contains_key("write_file"));
        assert!(!result.contains_key("execute"));
    }

    #[test]
    fn test_has_tool_calls_true() {
        let messages = vec![Message {
            role: opencode_provider::Role::Assistant,
            content: opencode_provider::Content::Parts(vec![opencode_provider::ContentPart {
                content_type: "tool_use".to_string(),
                text: None,
                image_url: None,
                tool_use: Some(opencode_provider::ToolUse {
                    id: "call_1".to_string(),
                    name: "test".to_string(),
                    input: serde_json::json!({}),
                }),
                tool_result: None,
                cache_control: None,
                filename: None,
                media_type: None,
                provider_options: None,
            }]),
            cache_control: None,
            provider_options: None,
        }];
        assert!(LlmProcessor::has_tool_calls(&messages));
    }

    #[test]
    fn test_has_tool_calls_false() {
        let messages = vec![Message::user("hello")];
        assert!(!LlmProcessor::has_tool_calls(&messages));
    }

    #[test]
    fn test_inject_litellm_dummy_tool_empty_tools_with_tool_calls() {
        let mut tools = HashMap::new();
        let messages = vec![Message {
            role: opencode_provider::Role::Assistant,
            content: opencode_provider::Content::Parts(vec![opencode_provider::ContentPart {
                content_type: "tool_use".to_string(),
                text: None,
                image_url: None,
                tool_use: Some(opencode_provider::ToolUse {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({}),
                }),
                tool_result: None,
                cache_control: None,
                filename: None,
                media_type: None,
                provider_options: None,
            }]),
            cache_control: None,
            provider_options: None,
        }];

        inject_litellm_dummy_tool(&mut tools, &messages, "litellm-proxy");
        assert!(tools.contains_key("_noop"));
    }

    #[test]
    fn test_inject_litellm_dummy_tool_non_litellm() {
        let mut tools = HashMap::new();
        let messages = vec![Message {
            role: opencode_provider::Role::Assistant,
            content: opencode_provider::Content::Parts(vec![opencode_provider::ContentPart {
                content_type: "tool_use".to_string(),
                text: None,
                image_url: None,
                tool_use: Some(opencode_provider::ToolUse {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({}),
                }),
                tool_result: None,
                cache_control: None,
                filename: None,
                media_type: None,
                provider_options: None,
            }]),
            cache_control: None,
            provider_options: None,
        }];

        inject_litellm_dummy_tool(&mut tools, &messages, "anthropic");
        assert!(!tools.contains_key("_noop"));
    }

    #[test]
    fn test_overflow_detection_uses_model_limits() {
        let usage = MessageUsage {
            input_tokens: 124_000,
            output_tokens: 2_000,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_cost: 0.0,
        };
        let limits = ModelLimits {
            context: 128_000,
            max_input: None,
            max_output: 4_096,
        };
        let token_usage = token_usage_from_message_usage(&usage);
        let engine = CompactionEngine::new(CompactionConfig::default());
        assert!(engine.is_overflow(&token_usage, &limits));
    }

    #[test]
    fn test_cost_includes_reasoning_tokens_at_output_rate() {
        let usage = MessageUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            reasoning_tokens: 200,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_cost: 0.0,
        };
        let rates = CostRates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
            over_200k: None,
        };

        let cost = compute_cost(&usage, Some(&rates), "openai");
        let base_without_reasoning = ((usage.input_tokens as f64) * rates.input
            + (usage.output_tokens as f64) * rates.output)
            / 1_000_000.0;
        assert!(cost > base_without_reasoning);
    }

    #[test]
    fn test_anthropic_cache_tokens_extracted_from_metadata() {
        let metadata = serde_json::json!({
            "anthropic": {
                "cacheCreationInputTokens": 500,
                "cacheReadInputTokens": 300
            }
        });
        let usage = StreamUsage {
            prompt_tokens: 1_000,
            completion_tokens: 500,
            ..Default::default()
        };

        let normalized = usage_from_stream(&usage, Some(&metadata), "anthropic");
        assert_eq!(normalized.cache_write_tokens, 500);
        assert_eq!(normalized.cache_read_tokens, 300);
    }

    #[test]
    fn test_doom_loop_emits_event_instead_of_only_blocking() {
        let mut processor = StreamProcessor::new("session-1".into(), "assistant-1".into());
        let input = serde_json::json!({"path": "src/main.rs"});

        for _ in 0..DOOM_LOOP_THRESHOLD {
            processor.record_tool_call_for_doom_loop("read".to_string(), input.clone());
        }

        let events = processor.take_events();
        assert!(events.iter().any(|e| matches!(
            e,
            ProcessorEvent::DoomLoopDetected { tool_name, .. } if tool_name == "read"
        )));
        assert!(processor.blocked);
    }

    #[test]
    fn test_typed_tool_error_classification() {
        assert!(StreamProcessor::should_block_for_tool_error(
            Some(StreamToolErrorKind::PermissionDenied),
            "anything"
        ));
        assert!(StreamProcessor::should_block_for_tool_error(
            Some(StreamToolErrorKind::QuestionRejected),
            "anything"
        ));
        assert!(!StreamProcessor::should_block_for_tool_error(
            Some(StreamToolErrorKind::ExecutionError),
            "anything"
        ));
    }

    /// アイドル中（ストリームが何も発行しない状態）でも abort で即座に終了すること。
    #[tokio::test]
    async fn test_forward_stream_exits_on_abort_while_idle() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let abort = tokio_util::sync::CancellationToken::new();

        let mut handle = tokio::spawn(forward_stream_events(
            futures::stream::pending::<Result<StreamEvent, String>>(),
            tx,
            abort.clone(),
        ));

        // キャンセル前はアイドルのまま終了しない。
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut handle)
                .await
                .is_err(),
            "task should stay alive while the stream is idle"
        );

        abort.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("task should exit promptly after abort")
            .expect("task panicked");
    }

    /// 受信側がドロップされたらアイドル中でも即座に終了すること。
    #[tokio::test]
    async fn test_forward_stream_exits_on_receiver_drop_while_idle() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let abort = tokio_util::sync::CancellationToken::new();

        let mut handle = tokio::spawn(forward_stream_events(
            futures::stream::pending::<Result<StreamEvent, String>>(),
            tx,
            abort,
        ));

        // 受信側が生きている間はアイドルのまま終了しない。
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut handle)
                .await
                .is_err(),
            "task should stay alive while the receiver is open"
        );

        drop(rx);
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("task should exit promptly after receiver drop")
            .expect("task panicked");
    }

    /// 従来どおりイベント順序と Error イベントの転送が維持されること。
    #[tokio::test]
    async fn test_forward_stream_preserves_event_order_and_error_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let abort = tokio_util::sync::CancellationToken::new();

        let stream = Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::Start),
            Ok(StreamEvent::TextDelta("hi".to_string())),
            Err("boom".to_string()),
            // Error 後に到達しても転送されない（従来どおり break するため）。
            Ok(StreamEvent::Done),
        ]));

        tokio::spawn(forward_stream_events(stream, tx, abort))
            .await
            .expect("task panicked");

        let mut received = Vec::new();
        while let Some(event) = rx.recv().await {
            received.push(event);
        }

        assert_eq!(received.len(), 3);
        assert!(matches!(received[0], StreamEvent::Start));
        match &received[1] {
            StreamEvent::TextDelta(text) => assert_eq!(text, "hi"),
            other => panic!("unexpected event: {:?}", other),
        }
        match &received[2] {
            StreamEvent::Error(message) => assert_eq!(message, "boom"),
            other => panic!("unexpected event: {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // R4 残存 high: ストリーミング経路の Retry-After 伝播回帰テスト
    // -----------------------------------------------------------------------

    struct RateLimitProvider {
        error: std::sync::Mutex<Option<ProviderError>>,
    }

    #[async_trait::async_trait]
    impl Provider for RateLimitProvider {
        fn id(&self) -> &str {
            "openai"
        }

        fn name(&self) -> &str {
            "Mock"
        }

        fn models(&self) -> Vec<opencode_provider::ModelInfo> {
            vec![]
        }

        fn get_model(&self, _id: &str) -> Option<&opencode_provider::ModelInfo> {
            None
        }

        async fn chat(
            &self,
            _request: ChatRequest,
        ) -> Result<opencode_provider::ChatResponse, ProviderError> {
            Err(self
                .error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| ProviderError::ApiError("no error configured".to_string())))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<opencode_provider::StreamResult, ProviderError> {
            Err(self
                .error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| ProviderError::ApiError("no error configured".to_string())))
        }
    }

    fn make_stream_input() -> StreamInput {
        StreamInput {
            user: MessageInfo::User {
                id: "msg_user".to_string(),
                session_id: "ses_test".to_string(),
                time: crate::message_v2::UserTime { created: 0 },
                agent: "test".to_string(),
                model: crate::message_v2::ModelRef {
                    provider_id: "openai".to_string(),
                    model_id: "gpt-test".to_string(),
                },
                format: None,
                summary: None,
                system: None,
                tools: None,
                variant: None,
            },
            session_id: "ses_test".to_string(),
            model: LlmModelRef {
                model_id: "gpt-test".to_string(),
                provider_id: "openai".to_string(),
            },
            agent: LlmAgent {
                name: "test".to_string(),
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_tokens: None,
            },
            system: vec![],
            abort: tokio_util::sync::CancellationToken::new(),
            messages: vec![Message::user("hello")],
            small: false,
            tools: HashMap::new(),
            retries: Some(1),
            tool_choice: None,
            cost_rates: None,
            work_dir: None,
        }
    }

    /// chat_stream が返した 429 + Retry-After 付き ProviderError が
    /// StreamEvent::Error を経て error_from_anyhow の response_headers に届き、
    /// retry::delay が Retry-After を反映すること。本文は漏洩しないこと。
    #[tokio::test]
    async fn test_stream_start_error_propagates_retry_after_to_session_retry() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "2".to_string());
        headers.insert("x-request-id".to_string(), "req-1".to_string());
        // 500 バイト preview を超える位置にのみ機微文字列を置く全文をわざと渡す。
        let full_body = format!(
            "{}{}",
            "rate limited; retryable payload padding. ".repeat(20),
            "SECRET-BODY-AFTER-PREVIEW"
        );
        let provider = Arc::new(RateLimitProvider {
            error: std::sync::Mutex::new(Some(ProviderError::api_error_with_headers(
                429,
                "rate limited",
                Some(headers.clone()),
                Some(full_body.clone()),
            ))),
        });
        let input = make_stream_input();

        let mut output = LlmProcessor::stream_from_input(&input, provider)
            .await
            .expect("stream setup must succeed");
        let payload = match output
            .events
            .recv()
            .await
            .expect("error event must be emitted")
        {
            StreamEvent::Error(payload) => payload,
            other => panic!("unexpected event: {:?}", other),
        };

        // 500 バイトを超える本文（機微文字列）はストリームペイロードへ出ないこと。
        assert!(
            !payload.contains("SECRET-BODY-AFTER-PREVIEW"),
            "full response body must not leak into StreamEvent payload"
        );

        let decoded = decode_stream_error(&payload).expect("structured envelope must decode");
        let message_error =
            crate::message_v2::error_from_anyhow(anyhow::Error::new(decoded), "openai");
        match &message_error {
            MessageError::ApiError {
                status_code,
                is_retryable,
                response_headers,
                response_body,
                ..
            } => {
                assert_eq!(*status_code, Some(429));
                assert!(*is_retryable);
                assert_eq!(response_headers.as_ref(), Some(&headers));
                let body = response_body
                    .as_deref()
                    .expect("bounded body preview must propagate for retry detection");
                assert!(
                    body.len() <= ERROR_BODY_PREVIEW_MAX_BYTES + 3,
                    "only a bounded preview may propagate, got {} bytes",
                    body.len()
                );
                assert!(
                    !body.contains("SECRET-BODY-AFTER-PREVIEW"),
                    "full body must not propagate through the streaming envelope"
                );
            }
            other => panic!("expected ApiError, got {other:?}"),
        }

        let api_info = crate::retry::ApiErrorInfo {
            message: "rate limited".to_string(),
            is_retryable: true,
            response_headers: Some(headers),
            response_body: None,
        };
        assert_eq!(crate::retry::delay(1, Some(&api_info)), 2_000);
    }

    /// process_stream が ProviderError を anyhow::Error として保持し、
    /// error_from_anyhow の downcast が成功すること。
    #[tokio::test]
    async fn test_process_stream_keeps_provider_error_for_error_from_anyhow() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "3".to_string());
        let provider = Arc::new(RateLimitProvider {
            error: std::sync::Mutex::new(Some(ProviderError::api_error_with_headers(
                429,
                "rate limited",
                Some(headers.clone()),
                None,
            ))),
        });
        let input = make_stream_input();
        let mut processor =
            StreamProcessor::new("ses_test".to_string(), "msg_assistant".to_string());

        let error = processor
            .process_stream(&input, provider)
            .await
            .expect_err("stream must fail");
        assert!(
            error.downcast_ref::<ProviderError>().is_some(),
            "ProviderError must remain the root of the anyhow error"
        );
        assert_eq!(error.to_string(), "API error (status 429): rate limited");

        let message_error = crate::message_v2::error_from_anyhow(error, "openai");
        match message_error {
            MessageError::ApiError {
                response_headers, ..
            } => assert_eq!(response_headers.as_ref(), Some(&headers)),
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    /// ストリーム途中の ProviderError も同じ経路で Retry-After を保持すること。
    #[tokio::test]
    async fn test_forward_stream_preserves_structured_provider_error() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "5".to_string());
        let provider_error =
            ProviderError::api_error_with_headers(429, "rate limited", Some(headers.clone()), None);
        let stream = Box::pin(futures::stream::iter(vec![
            Err::<StreamEvent, ProviderError>(provider_error),
        ]));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let abort = tokio_util::sync::CancellationToken::new();

        tokio::spawn(forward_stream_events(stream, tx, abort))
            .await
            .expect("task panicked");
        let payload = match rx.recv().await.expect("error event must be emitted") {
            StreamEvent::Error(payload) => payload,
            other => panic!("unexpected event: {:?}", other),
        };

        let decoded = decode_stream_error(&payload).expect("structured envelope must decode");
        let message_error =
            crate::message_v2::error_from_anyhow(anyhow::Error::new(decoded), "openai");
        match message_error {
            MessageError::ApiError {
                response_headers, ..
            } => assert_eq!(response_headers.as_ref(), Some(&headers)),
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    /// 通常の文字列エラーは従来どおりプレーンな文字列として届くこと。
    #[test]
    fn test_decode_stream_error_ignores_plain_strings() {
        assert!(decode_stream_error("boom").is_none());
        assert!(decode_stream_error("Stream error: rate limited").is_none());
    }

    // -----------------------------------------------------------------------
    // R4 指摘: エンベロープの厳密性 / 情報漏洩 / テスト不足
    // -----------------------------------------------------------------------

    /// マーカー付きでも不正 JSON・バージョン違い・未知 kind・余分フィールドは
    /// すべて拒否されること。
    #[test]
    fn test_decode_stream_error_rejects_malformed_envelopes() {
        let valid = serde_json::json!({
            "_version": STRUCTURED_STREAM_ERROR_VERSION,
            "error": { "kind": "rate_limit" }
        });

        // マーカー始まりの不正 JSON
        let bad_json = format!("{STRUCTURED_STREAM_ERROR_MARKER}{{not json");
        assert!(decode_stream_error(&bad_json).is_none());

        // _version 違い
        let mut wrong_version = valid.clone();
        wrong_version["_version"] = serde_json::json!(STRUCTURED_STREAM_ERROR_VERSION + 1);
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{wrong_version}");
        assert!(decode_stream_error(&payload).is_none());

        // 未知 kind
        let mut unknown_kind = valid.clone();
        unknown_kind["error"] = serde_json::json!({ "kind": "totally_unknown" });
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{unknown_kind}");
        assert!(decode_stream_error(&payload).is_none());

        // 余分フィールド（トップレベル）
        let mut extra_top = valid.clone();
        extra_top["extra"] = serde_json::json!("x");
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{extra_top}");
        assert!(decode_stream_error(&payload).is_none());

        // 余分フィールド（error 内）
        let mut extra_inner = valid.clone();
        extra_inner["error"] = serde_json::json!({ "kind": "rate_limit", "extra": "x" });
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{extra_inner}");
        assert!(decode_stream_error(&payload).is_none());

        // 対照: 正しいエンベロープは復元できること
        let valid_payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{valid}");
        assert!(matches!(
            decode_stream_error(&valid_payload),
            Some(ProviderError::RateLimit)
        ));
    }

    /// カスタム provider が混入した機微ヘッダが allowlist で除去されること。
    #[test]
    fn test_decode_stream_error_reapplies_header_allowlist() {
        let payload = format!(
            "{STRUCTURED_STREAM_ERROR_MARKER}{}",
            serde_json::json!({
                "_version": STRUCTURED_STREAM_ERROR_VERSION,
                "error": {
                    "kind": "api_error_with_headers",
                    "status_code": 429,
                    "message": "rate limited",
                    "response_headers": {
                        "authorization": "Bearer should-be-dropped",
                        "set-cookie": "session=secret",
                        "retry-after": "2",
                        "retry-after-ms": "1500",
                        "x-request-id": "req-1",
                        "x-ratelimit-remaining": "0",
                        "x-custom-secret": "drop-me"
                    },
                    "response_body": ""
                }
            })
        );
        let decoded = decode_stream_error(&payload).expect("envelope must decode");
        let headers = decoded.response_headers().expect("headers must survive");
        assert_eq!(headers.len(), 4);
        assert_eq!(headers.get("retry-after").map(String::as_str), Some("2"));
        assert_eq!(
            headers.get("retry-after-ms").map(String::as_str),
            Some("1500")
        );
        assert_eq!(
            headers.get("x-request-id").map(String::as_str),
            Some("req-1")
        );
        assert_eq!(
            headers.get("x-ratelimit-remaining").map(String::as_str),
            Some("0")
        );
        for dropped in ["authorization", "set-cookie", "x-custom-secret"] {
            assert!(
                !headers.contains_key(dropped),
                "sensitive header must be dropped: {dropped}"
            );
        }
    }

    /// 構造化エラーの message に credential クエリ / Bearer 値が含まれても、
    /// `ProviderError` へ再構成する時点で伏せられること。
    /// ヘッダ allowlist は既存テストで担保されており、ここでは message を検証する。
    #[test]
    fn test_decode_stream_error_redacts_credential_values_in_message() {
        let cases: [(&str, &str, &[&str]); 3] = [
            (
                "network_error",
                "connect failed: https://api.example.com/v1?key=sk-network-secret&model=gpt-4",
                &["sk-network-secret"],
            ),
            (
                "auth_error",
                "unauthorized: Authorization: Bearer sk-auth-secret-1234",
                &["sk-auth-secret-1234"],
            ),
            (
                "other",
                "failed with api_key=sk-other-secret and token=sk-token-secret",
                &["sk-other-secret", "sk-token-secret"],
            ),
        ];

        for (kind, message, secrets) in cases {
            let payload = format!(
                "{STRUCTURED_STREAM_ERROR_MARKER}{}",
                serde_json::json!({
                    "_version": STRUCTURED_STREAM_ERROR_VERSION,
                    "error": { "kind": kind, "message": message }
                })
            );
            let decoded = decode_stream_error(&payload).expect("envelope must decode");
            let text = decoded.to_string();

            assert!(
                text.contains("<redacted>"),
                "no redaction for {kind}: {text}"
            );
            for &secret in secrets {
                assert!(
                    !text.contains(secret),
                    "credential leaked into decoded {kind} error: {text}"
                );
            }
            // 障害追跡に必要なホスト名とパラメータ名は残ること。
            if kind == "network_error" {
                assert!(
                    text.contains("api.example.com"),
                    "host must survive: {text}"
                );
                assert!(text.contains("key="), "parameter name must survive: {text}");
                assert!(
                    text.contains("model=gpt-4"),
                    "safe query must survive: {text}"
                );
            }
        }
    }

    /// 本文 preview が streaming 経路でも free-usage-limit 検出に使われ、
    /// 500 バイトを超える本文は漏洩しないこと。
    #[test]
    fn test_streaming_body_preview_enables_free_usage_limit_detection() {
        let full_body = format!(
            "{}{}{}{}",
            "p".repeat(430),
            "FreeUsageLimitError",
            "q".repeat(200),
            "LONGTAILSECRET".repeat(50)
        );
        let provider_error = ProviderError::api_error_with_headers(
            429,
            "rate limited",
            None,
            Some(full_body.clone()),
        );
        let payload = encode_stream_error(&provider_error);
        assert!(
            !payload.contains("LONGTAILSECRET"),
            "full body must not enter the envelope"
        );

        let decoded = decode_stream_error(&payload).expect("envelope must decode");
        let message_error =
            crate::message_v2::error_from_anyhow(anyhow::Error::new(decoded), "openai");
        assert!(
            crate::retry::retryable(&message_error).is_none(),
            "free-usage-limit marker in the preview must stop retries on the streaming path"
        );

        match &message_error {
            MessageError::ApiError { response_body, .. } => {
                let body = response_body.as_deref().expect("preview must propagate");
                assert!(body.contains("FreeUsageLimitError"));
                assert!(!body.contains("LONGTAILSECRET"));
                assert!(body.len() <= ERROR_BODY_PREVIEW_MAX_BYTES + 3);
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    /// デコードできない生ペイロードはエラー文字列へ埋め込まれないこと。
    struct PayloadErrorProvider {
        payload: String,
    }

    #[async_trait::async_trait]
    impl Provider for PayloadErrorProvider {
        fn id(&self) -> &str {
            "openai"
        }

        fn name(&self) -> &str {
            "Mock"
        }

        fn models(&self) -> Vec<opencode_provider::ModelInfo> {
            vec![]
        }

        fn get_model(&self, _id: &str) -> Option<&opencode_provider::ModelInfo> {
            None
        }

        async fn chat(
            &self,
            _request: ChatRequest,
        ) -> Result<opencode_provider::ChatResponse, ProviderError> {
            Err(ProviderError::ApiError("unused".to_string()))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<opencode_provider::StreamResult, ProviderError> {
            let payload = self.payload.clone();
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::Error(payload)),
                Ok(StreamEvent::Done),
            ])))
        }
    }

    #[tokio::test]
    async fn test_process_stream_normalizes_undecodable_payload() {
        let secret = "RAW-RESPONSE-BODY-SECRET";
        let provider = Arc::new(PayloadErrorProvider {
            payload: format!("Stream error: {secret}"),
        });
        let input = make_stream_input();
        let mut processor =
            StreamProcessor::new("ses_test".to_string(), "msg_assistant".to_string());

        let error = processor
            .process_stream(&input, provider)
            .await
            .expect_err("stream must fail");
        let rendered = error.to_string();
        assert_eq!(rendered, STREAM_ERROR_DECODE_FAILED);
        assert!(
            !rendered.contains(secret),
            "raw payload must not leak into the normalized error"
        );
    }

    // -----------------------------------------------------------------------
    // R2: source 保持型エラーの retryable 再符号化往復
    // -----------------------------------------------------------------------

    /// `ApiErrorWithSource` の source に対する transient 判定が、
    /// ストリーム再符号化を往復しても保持されること。
    #[tokio::test]
    async fn test_stream_error_round_trip_preserves_api_source_retryable() {
        // loopback の閉じたポートへの connect 失敗のみを使い、外部ホストへ送信しない。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        let transient_source = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("closed loopback port must fail to connect");

        // 非一時 source の本文にはトークンとローカルパスを含め、漏洩も検証する。
        let source_secret = "sk-source-only-1234";
        let source_path = "/home/koppe/.secret/source-only";
        let cases = [
            (
                ProviderError::api_error_with_source(
                    "transient upstream failure",
                    transient_source,
                ),
                true,
            ),
            (
                ProviderError::api_error_with_source(
                    "permanent upstream failure",
                    std::io::Error::other(format!("key={source_secret} at {source_path}")),
                ),
                false,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.is_retryable().is_some(), expected);
            let payload = encode_stream_error(&error);
            assert!(
                payload.contains(&format!("\"retryable\":{expected}")),
                "envelope must carry the source classification"
            );
            // 失敗時もペイロード本文をアサーションへ埋め込まない（機微値の二次漏洩防止）。
            assert!(
                !payload.contains(source_secret) && !payload.contains(source_path),
                "source body must never enter the envelope"
            );

            let decoded = decode_stream_error(&payload).expect("envelope must decode");
            assert!(
                matches!(decoded, ProviderError::ApiErrorWithSource { .. }),
                "structured kind must survive round trip: {decoded:?}"
            );
            assert_eq!(
                decoded.is_retryable().is_some(),
                expected,
                "retryable must survive encode/decode round trip"
            );
        }
    }

    /// `NetworkErrorWithSource` も structured kind として往復し、判定が保たれること。
    #[test]
    fn test_stream_error_round_trip_preserves_network_source_retryable() {
        let error = ProviderError::network_error_with_source(
            "downstream network failure",
            std::io::Error::other("connection reset"),
        );
        assert!(error.is_retryable().is_some());

        let payload = encode_stream_error(&error);
        assert!(payload.contains("\"retryable\":true"));
        let decoded = decode_stream_error(&payload).expect("envelope must decode");
        assert!(
            matches!(decoded, ProviderError::NetworkErrorWithSource { .. }),
            "structured kind must survive round trip: {decoded:?}"
        );
        assert_eq!(decoded.is_retryable(), error.is_retryable());
    }

    /// 新しい kind でも retryable の型・必須性・余分フィールドが厳密に検証されること。
    #[test]
    fn test_decode_stream_error_validates_source_kind_payload() {
        let base = serde_json::json!({
            "_version": STRUCTURED_STREAM_ERROR_VERSION,
            "error": {
                "kind": "api_error_with_source",
                "message": "request failed",
                "retryable": true
            }
        });
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{base}");
        assert!(decode_stream_error(&payload).is_some());

        let mut missing = base.clone();
        missing["error"]
            .as_object_mut()
            .expect("object")
            .remove("retryable");
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{missing}");
        assert!(decode_stream_error(&payload).is_none());

        let mut extra = base.clone();
        extra["error"]["source_debug"] = serde_json::json!("sk-leaked");
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{extra}");
        assert!(decode_stream_error(&payload).is_none());

        let mut wrong_type = base;
        wrong_type["error"]["retryable"] = serde_json::json!("true");
        let payload = format!("{STRUCTURED_STREAM_ERROR_MARKER}{wrong_type}");
        assert!(decode_stream_error(&payload).is_none());
    }

    // -----------------------------------------------------------------------
    // R2: retry 時の attempt 単位状態リセット / cost・tokens の確定
    // -----------------------------------------------------------------------

    /// attempt ごとに異なるストリームを再生する provider。
    struct ScriptedProvider {
        scripts: std::sync::Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
    }

    #[async_trait::async_trait]
    impl Provider for ScriptedProvider {
        fn id(&self) -> &str {
            "openai"
        }

        fn name(&self) -> &str {
            "Scripted"
        }

        fn models(&self) -> Vec<opencode_provider::ModelInfo> {
            vec![]
        }

        fn get_model(&self, _id: &str) -> Option<&opencode_provider::ModelInfo> {
            None
        }

        async fn chat(
            &self,
            _request: ChatRequest,
        ) -> Result<opencode_provider::ChatResponse, ProviderError> {
            Err(ProviderError::InvalidRequest("unused".to_string()))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<opencode_provider::StreamResult, ProviderError> {
            let events = self
                .scripts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(
                events.into_iter().map(Ok::<StreamEvent, ProviderError>),
            )))
        }
    }

    fn retryable_stream_error() -> String {
        let mut headers = HashMap::new();
        headers.insert("retry-after-ms".to_string(), "1".to_string());
        encode_stream_error(&ProviderError::api_error_with_headers(
            429,
            "rate limited",
            Some(headers),
            None,
        ))
    }

    /// 失敗 attempt のツール呼び出し・blocking・cost が次の attempt へ
    /// 持ち越されず、cost / tokens が最終 attempt のみで確定すること。
    #[tokio::test]
    async fn test_retry_discards_failed_attempt_state_and_finalizes_usage_once() {
        let first_attempt = vec![
            StreamEvent::StartStep,
            StreamEvent::ToolCallStart {
                id: "call-1".to_string(),
                name: "read".to_string(),
            },
            StreamEvent::ToolCallEnd {
                id: "call-1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({}),
            },
            StreamEvent::ToolError {
                tool_call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                input: None,
                error: "permission denied: transient".to_string(),
                kind: Some(StreamToolErrorKind::PermissionDenied),
            },
            StreamEvent::FinishStep {
                finish_reason: Some("tool-error".to_string()),
                usage: StreamUsage {
                    prompt_tokens: 1_000,
                    completion_tokens: 100,
                    ..Default::default()
                },
                provider_metadata: None,
            },
            StreamEvent::Error(retryable_stream_error()),
        ];
        let second_usage = StreamUsage {
            prompt_tokens: 2_000,
            completion_tokens: 200,
            ..Default::default()
        };
        let second_attempt = vec![
            StreamEvent::FinishStep {
                finish_reason: Some("stop".to_string()),
                usage: second_usage.clone(),
                provider_metadata: None,
            },
            StreamEvent::Done,
        ];

        let provider = Arc::new(ScriptedProvider {
            scripts: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
                first_attempt,
                second_attempt,
            ])),
        });
        let mut input = make_stream_input();
        input.retries = Some(1);
        let mut processor =
            StreamProcessor::new("ses_test".to_string(), "msg_assistant".to_string());

        assert_eq!(
            processor.process(input, provider).await,
            ProcessResult::Continue
        );

        let expected_usage = usage_from_stream(&second_usage, None, "openai");
        let expected_cost = compute_cost(&expected_usage, None, "openai");
        assert!(
            (processor.cost() - expected_cost).abs() < 1e-9,
            "only the final attempt may contribute cost"
        );
        assert_eq!(processor.tokens().map(|u| u.input_tokens), Some(2_000));
        assert_eq!(processor.finish_reason(), Some("stop"));
        assert!(processor.error().is_none());
        assert!(
            !processor.blocked,
            "blocking from a retried attempt must be cleared"
        );
        assert!(processor.tool_calls.is_empty());
        assert!(processor.completed_tool_parts.is_empty());
        assert!(processor.pending_patches.is_empty());
        assert!(processor.pending_events.is_empty());
    }

    /// ヘルパ単体でも attempt 一時状態が消え、確定済み cost は保持されること。
    #[test]
    fn test_reset_attempt_state_preserves_finalized_cost() {
        let mut processor =
            StreamProcessor::new("ses_test".to_string(), "msg_assistant".to_string());
        processor.cost = 0.25;
        processor.attempt_cost = 0.75;
        processor.attempt_tokens = Some(MessageUsage {
            input_tokens: 10,
            ..Default::default()
        });
        processor.blocked = true;
        processor.tool_calls.insert(
            "call-1".to_string(),
            ToolCallState {
                part_id: "prt-1".to_string(),
                tool_name: "read".to_string(),
                _call_id: "call-1".to_string(),
                raw_input: String::new(),
                parsed_input: serde_json::json!({}),
                state: ToolState::Pending {
                    input: serde_json::json!({}),
                    raw: String::new(),
                },
            },
        );
        processor.pending_patches.push(SnapshotPatch {
            hash: "h".to_string(),
            files: vec![],
        });

        processor.reset_attempt_state();

        assert_eq!(processor.cost, 0.25);
        assert_eq!(processor.attempt_cost, 0.0);
        assert!(processor.attempt_tokens.is_none());
        assert!(!processor.blocked);
        assert!(processor.tool_calls.is_empty());
        assert!(processor.pending_patches.is_empty());
    }
}
