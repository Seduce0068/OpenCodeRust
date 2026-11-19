use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, Choice, Message, ModelInfo, Provider, ProviderError, StreamEvent,
    StreamResult, Usage,
};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: Option<String>,
}

#[derive(Debug)]
pub struct AnthropicProvider {
    client: Client,
    config: AnthropicConfig,
    models: Vec<ModelInfo>,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_config(AnthropicConfig {
            api_key: api_key.into(),
            base_url: None,
        })
    }

    pub fn with_config(config: AnthropicConfig) -> Self {
        let models = vec![
            ModelInfo {
                id: "claude-opus-4-6".to_string(),
                name: "Claude Opus 4.6".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200_000,
                max_output_tokens: 128_000,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 5.0,
                cost_per_million_output: 25.0,
            },
            ModelInfo {
                id: "claude-sonnet-4-20250514".to_string(),
                name: "Claude Sonnet 4".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200000,
                max_output_tokens: 16000,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 3.0,
                cost_per_million_output: 15.0,
            },
            ModelInfo {
                id: "claude-3-5-sonnet-20241022".to_string(),
                name: "Claude 3.5 Sonnet".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 3.0,
                cost_per_million_output: 15.0,
            },
            ModelInfo {
                id: "claude-3-5-haiku-20241022".to_string(),
                name: "Claude 3.5 Haiku".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 1.0,
                cost_per_million_output: 5.0,
            },
            ModelInfo {
                id: "claude-3-opus-20240229".to_string(),
                name: "Claude 3 Opus".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200000,
                max_output_tokens: 4096,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 15.0,
                cost_per_million_output: 75.0,
            },
        ];

        Self {
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
            config,
            models,
        }
    }

    fn convert_request(&self, request: ChatRequest) -> AnthropicRequest {
        let max_tokens = request.max_tokens.unwrap_or(4096);
        let mut messages = Vec::new();
        let mut system = None;

        for msg in request.messages {
            match msg.role {
                crate::Role::System => {
                    if let crate::Content::Text(text) = msg.content {
                        system = Some(text);
                    }
                }
                _ => {
                    messages.push(AnthropicMessage {
                        role: match msg.role {
                            crate::Role::User => "user".to_string(),
                            crate::Role::Assistant => "assistant".to_string(),
                            _ => "user".to_string(),
                        },
                        content: match msg.content {
                            crate::Content::Text(text) => vec![AnthropicContent::Text { text }],
                            crate::Content::Parts(parts) => parts
                                .into_iter()
                                .filter_map(|p| p.text.map(|text| AnthropicContent::Text { text }))
                                .collect(),
                        },
                    });
                }
            }
        }

        AnthropicRequest {
            model: request.model,
            max_tokens,
            messages,
            system,
            stream: request.stream,
            thinking: anthropic_thinking_config(request.variant.as_deref(), max_tokens),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn name(&self) -> &str {
        "Anthropic"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let anthropic_request = self.convert_request(request);

        let url = self.config.base_url.as_deref().unwrap_or(ANTHROPIC_API_URL);

        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14")
            .header("content-type", "application/json")
            .json(&anthropic_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::api_error_from_status(
                status.as_u16(),
                format!(
                    "{}: {}",
                    status,
                    crate::openai::error_body_preview(&body, 500)
                ),
            ));
        }

        let anthropic_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        Ok(convert_response(anthropic_response))
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let mut anthropic_request = self.convert_request(request);
        anthropic_request.stream = Some(true);

        let url = self.config.base_url.as_deref().unwrap_or(ANTHROPIC_API_URL);

        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&anthropic_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::api_error_from_status(
                status.as_u16(),
                format!(
                    "{}: {}",
                    status,
                    crate::openai::error_body_preview(&body, 500)
                ),
            ));
        }

        // ストリーム全体で 1 つのパーサを共有し、content_block_start で得た tool_use id を
        // 後続の content_block_delta (partial_json) へ対応付ける。
        // try_unfold の状態としてパーサを保持し、各行を push するたびに状態を更新する。
        let parser = crate::stream::AnthropicSseParser::new();
        let stream = stream::try_unfold(
            (
                response.bytes_stream(),
                Vec::<u8>::new(),
                parser,
                VecDeque::<StreamEvent>::new(),
                false,
            ),
            |(mut chunks, mut buffer, mut parser, mut pending, mut exhausted)| async move {
                loop {
                    if let Some(event) = pending.pop_front() {
                        return Ok(Some((event, (chunks, buffer, parser, pending, exhausted))));
                    }

                    if exhausted {
                        return Ok(None);
                    }

                    match chunks.next().await {
                        Some(Ok(bytes)) => {
                            // 生バイトのままバッファへ積み、行が確定してからデコードする。
                            buffer.extend_from_slice(bytes.as_ref());
                            pending.extend(drain_anthropic_sse_events(
                                &mut buffer,
                                &mut parser,
                                false,
                            ));
                        }
                        Some(Err(err)) => {
                            return Err(ProviderError::StreamError(err.to_string()));
                        }
                        None => {
                            exhausted = true;
                            pending.extend(drain_anthropic_sse_events(
                                &mut buffer,
                                &mut parser,
                                true,
                            ));
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// 完全な SSE 行 1 行をパーサへ渡し、生成されたイベントを収集する。
///
/// UTF-8 変換は行が確定したここで初めて行う。チャンク単位で
/// `String::from_utf8_lossy` を呼ぶとマルチバイト文字がチャンク境界で分断された
/// 際に U+FFFD へ置換されるため、生バイトを保持してからデコードする。
/// `str::lines()` と同じく CRLF の `\r` を取り除くため、wire format の違いで
/// JSON の末尾に `\r` が残ることを防ぐ。`data: ` で始まらない行（イベント名や
/// 空行）は従来どおり無視する。
fn handle_anthropic_sse_line(
    line: &[u8],
    parser: &mut crate::stream::AnthropicSseParser,
    events: &mut Vec<StreamEvent>,
) {
    let decoded = String::from_utf8_lossy(line);
    let decoded = decoded.as_ref();
    let line = decoded.strip_suffix('\r').unwrap_or(decoded);
    if let Some(data) = line.strip_prefix("data: ") {
        if let Some(event) = parser.push(data) {
            events.push(event);
        }
    }
}

/// 行バッファから改行が揃った `data: ...` 行だけを取り出してパーサへ渡す。
///
/// `bytes_stream()` の 1 チャンクは SSE 行の途中（JSON の途中や UTF-8 文字の
/// 途中を含む）で終わる可能性があるため、改行が見つかるまでは生バイトの
/// `buffer` に保持する。UTF-8 デコードは完全な行単位で行う。
/// `flush_remainder` はストリーム終端で末尾の改行なし行を処理するために使う。
fn drain_anthropic_sse_events(
    buffer: &mut Vec<u8>,
    parser: &mut crate::stream::AnthropicSseParser,
    flush_remainder: bool,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    while let Some(newline_idx) = buffer.iter().position(|&byte| byte == b'\n') {
        let line = buffer[..newline_idx].to_vec();
        buffer.drain(..=newline_idx);
        handle_anthropic_sse_line(&line, parser, &mut events);
    }

    if flush_remainder && !buffer.is_empty() {
        let tail = std::mem::take(buffer);
        handle_anthropic_sse_line(&tail, parser, &mut events);
    }

    events
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u64,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContent>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicThinking {
    #[serde(rename = "enabled")]
    Enabled {
        #[serde(rename = "budget_tokens")]
        budget_tokens: u64,
    },
}

fn anthropic_thinking_config(variant: Option<&str>, max_tokens: u64) -> Option<AnthropicThinking> {
    let variant = variant?.trim().to_ascii_lowercase();
    let target = match variant.as_str() {
        "low" => 4_000,
        "medium" => 8_000,
        "high" => 16_000,
        "max" | "xhigh" => 31_999,
        _ => return None,
    };

    let ceiling = max_tokens.saturating_sub(1);
    let budget_tokens = target.min(ceiling);
    if budget_tokens == 0 {
        return None;
    }
    Some(AnthropicThinking::Enabled { budget_tokens })
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    model: String,
    content: Vec<AnthropicResponseContent>,
    usage: AnthropicResponseUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponseContent {
    #[serde(rename = "type")]
    _content_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponseUsage {
    input_tokens: u64,
    output_tokens: u64,
}

fn convert_response(response: AnthropicResponse) -> ChatResponse {
    let content = response
        .content
        .iter()
        .filter_map(|c| c.text.clone())
        .collect::<Vec<_>>()
        .join("");

    ChatResponse {
        id: response.id,
        model: response.model,
        choices: vec![Choice {
            index: 0,
            message: Message::assistant(&content),
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: response.usage.input_tokens,
            completion_tokens: response.usage.output_tokens,
            total_tokens: response.usage.input_tokens + response.usage.output_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Content;

    fn thinking_budget(variant: Option<&str>, max_tokens: u64) -> Option<u64> {
        anthropic_thinking_config(variant, max_tokens)
            .map(|AnthropicThinking::Enabled { budget_tokens }| budget_tokens)
    }

    #[test]
    fn thinking_config_budgets_by_variant() {
        assert_eq!(thinking_budget(Some("low"), 32000), Some(4_000));
        assert_eq!(thinking_budget(Some("medium"), 32000), Some(8_000));
        assert_eq!(thinking_budget(Some("high"), 32000), Some(16_000));
        assert_eq!(thinking_budget(Some("xhigh"), 32000), Some(31_999));
    }

    #[test]
    fn thinking_config_clamps_to_max_tokens() {
        assert_eq!(thinking_budget(Some("high"), 5000), Some(4999));
        assert_eq!(thinking_budget(Some("low"), 1), None);
    }

    #[test]
    fn thinking_config_rejects_unknown_variant() {
        assert_eq!(thinking_budget(Some("invalid"), 32000), None);
        assert_eq!(thinking_budget(None, 32000), None);
    }

    #[test]
    fn convert_response_joins_text_and_maps_usage() {
        let response = AnthropicResponse {
            id: "msg_1".to_owned(),
            model: "claude-3".to_owned(),
            content: vec![
                AnthropicResponseContent {
                    _content_type: "text".to_owned(),
                    text: Some("hello ".to_owned()),
                },
                AnthropicResponseContent {
                    _content_type: "text".to_owned(),
                    text: Some("world".to_owned()),
                },
            ],
            usage: AnthropicResponseUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };

        let result = convert_response(response);
        assert_eq!(result.id, "msg_1");
        assert_eq!(result.model, "claude-3");
        assert_eq!(result.choices.len(), 1);
        assert_eq!(result.choices[0].finish_reason.as_deref(), Some("stop"));
        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn convert_response_ignores_non_text_content() {
        let response = AnthropicResponse {
            id: "msg_2".to_owned(),
            model: "claude-3".to_owned(),
            content: vec![AnthropicResponseContent {
                _content_type: "tool_use".to_owned(),
                text: None,
            }],
            usage: AnthropicResponseUsage {
                input_tokens: 1,
                output_tokens: 1,
            },
        };
        let result = convert_response(response);
        assert!(matches!(
            result.choices[0].message.content,
            Content::Text(ref text) if text.is_empty()
        ));
    }

    #[test]
    fn sse_buffer_reassembles_split_data_line() {
        let mut buffer = Vec::new();
        let mut parser = crate::stream::AnthropicSseParser::new();

        buffer.extend_from_slice(b"event: message_start\n");
        assert!(drain_anthropic_sse_events(&mut buffer, &mut parser, false).is_empty());

        buffer.extend_from_slice(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n",
        );
        let usage_events = drain_anthropic_sse_events(&mut buffer, &mut parser, false);
        assert!(matches!(
            usage_events.as_slice(),
            [StreamEvent::Usage {
                prompt_tokens: 7,
                completion_tokens: 0
            }]
        ));

        // データ行の途中でチャンクが終わってもイベントを出さず、バッファに残す。
        buffer.extend_from_slice(b"event: content_block_delta\n");
        buffer.extend_from_slice(
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel",
        );
        assert!(drain_anthropic_sse_events(&mut buffer, &mut parser, false).is_empty());
        assert!(buffer.ends_with(b"\"text\":\"hel"));

        buffer.extend_from_slice(
            b"lo\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let events = drain_anthropic_sse_events(&mut buffer, &mut parser, false);
        assert_eq!(events.len(), 2, "イベントは行の順番どおりに出る");
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "hello"));
        assert!(matches!(&events[1], StreamEvent::Done));
        assert!(buffer.is_empty());
    }

    #[test]
    fn sse_buffer_flushes_trailing_line_without_newline() {
        let mut buffer = Vec::new();
        let mut parser = crate::stream::AnthropicSseParser::new();

        buffer.extend_from_slice(b"data: {\"type\":\"message_stop\"}");
        assert!(drain_anthropic_sse_events(&mut buffer, &mut parser, false).is_empty());
        let events = drain_anthropic_sse_events(&mut buffer, &mut parser, true);
        assert!(matches!(events.as_slice(), [StreamEvent::Done]));
        assert!(buffer.is_empty());
    }

    #[test]
    fn sse_buffer_tolerates_crlf_and_ignores_non_data_lines() {
        let mut buffer = Vec::new();
        let mut parser = crate::stream::AnthropicSseParser::new();

        buffer.extend_from_slice(b"event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n");
        let events = drain_anthropic_sse_events(&mut buffer, &mut parser, true);
        assert!(matches!(events.as_slice(), [StreamEvent::Done]));
    }

    #[test]
    fn sse_buffer_preserves_multibyte_text_split_mid_character() {
        let mut buffer = Vec::new();
        let mut parser = crate::stream::AnthropicSseParser::new();

        let text = "こんにちは";
        let prefix = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"";
        let text_bytes = text.as_bytes();
        // "こ" の UTF-8 は E3 81 93。先頭 1 バイト目の直後で分割する。
        assert_eq!(&text_bytes[..3], &[0xE3u8, 0x81, 0x93]);

        buffer.extend_from_slice(prefix.as_bytes());
        buffer.extend_from_slice(&text_bytes[..1]);
        assert!(drain_anthropic_sse_events(&mut buffer, &mut parser, false).is_empty());
        assert!(buffer.ends_with(&[0xE3u8]));

        buffer.extend_from_slice(&text_bytes[1..]);
        buffer.extend_from_slice(b"\"}}\n\n");
        let events = drain_anthropic_sse_events(&mut buffer, &mut parser, false);
        assert_eq!(events.len(), 1, "イベント数が想定と異なる: {events:?}");
        assert!(
            matches!(events.first(), Some(StreamEvent::TextDelta(delta)) if delta.as_str() == text),
            "分割されたマルチバイト文字は U+FFFD に置換されず復元される必要がある: {events:?}"
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn rate_limited_status_response_is_retryable() {
        use crate::retry::IsRetryable;

        // アダプタと同一の構築経路。平坦化された ApiError ではリトライ判定を失う。
        let status = reqwest::StatusCode::TOO_MANY_REQUESTS;
        let body = "rate limited";
        let error = ProviderError::api_error_from_status(
            status.as_u16(),
            format!(
                "{}: {}",
                status,
                crate::openai::error_body_preview(body, 500)
            ),
        );
        assert_eq!(error.status_code(), Some(429));
        assert!(
            error.is_retryable().is_some(),
            "429 レスポンスはリトライ可能でなければならない: {error:?}"
        );

        // 本文全体をエラーへ埋め込まないことの回帰防止。
        let long_body = "x".repeat(1000);
        let long_error = ProviderError::api_error_from_status(
            status.as_u16(),
            format!(
                "{}: {}",
                status,
                crate::openai::error_body_preview(&long_body, 500)
            ),
        );
        let message = long_error.to_string();
        assert!(message.contains("..."), "長い本文は省略されるべき");
        assert!(
            !message.contains(long_body.as_str()),
            "本文全体をエラーへ露出してはならない"
        );
    }
}
