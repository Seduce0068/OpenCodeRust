use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, Choice, Content, Message, ModelInfo, Provider, ProviderError, Role,
    StreamEvent, StreamResult, Usage,
};

const GITLAB_API_URL: &str = "https://gitlab.com/api/v4/ai/chat/completions";

#[derive(Debug, Clone)]
pub struct GitLabConfig {
    pub api_key: String,
    pub instance_url: Option<String>,
}

#[derive(Debug)]
pub struct GitLabProvider {
    client: Client,
    config: GitLabConfig,
    models: Vec<ModelInfo>,
}

impl GitLabProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_config(GitLabConfig {
            api_key: api_key.into(),
            instance_url: None,
        })
    }

    pub fn with_config(config: GitLabConfig) -> Self {
        let models = vec![
            ModelInfo {
                id: "claude-3-5-sonnet-20241022".to_string(),
                name: "Claude 3.5 Sonnet (GitLab Duo)".to_string(),
                provider: "gitlab".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.0,
                cost_per_million_output: 0.0,
            },
            ModelInfo {
                id: "claude-3-5-haiku-20241022".to_string(),
                name: "Claude 3.5 Haiku (GitLab Duo)".to_string(),
                provider: "gitlab".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.0,
                cost_per_million_output: 0.0,
            },
            ModelInfo {
                id: "code-suggestions".to_string(),
                name: "Code Suggestions (GitLab Duo)".to_string(),
                provider: "gitlab".to_string(),
                context_window: 8192,
                max_output_tokens: 2048,
                supports_vision: false,
                supports_tools: false,
                cost_per_million_input: 0.0,
                cost_per_million_output: 0.0,
            },
        ];

        Self {
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
            config,
            models,
        }
    }

    fn get_api_url(&self) -> String {
        self.config
            .instance_url
            .as_deref()
            .map(|base| format!("{}/api/v4/ai/chat/completions", base.trim_end_matches('/')))
            .unwrap_or_else(|| GITLAB_API_URL.to_string())
    }

    fn convert_request(&self, request: ChatRequest) -> GitLabRequest {
        let messages: Vec<GitLabMessage> = request
            .messages
            .into_iter()
            .map(|msg| GitLabMessage {
                role: match msg.role {
                    Role::System => "system".to_string(),
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                    Role::Tool => "user".to_string(),
                },
                content: match msg.content {
                    Content::Text(t) => GitLabContent::Text(t),
                    Content::Parts(parts) => {
                        let text = parts
                            .iter()
                            .filter_map(|p| p.text.as_ref())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");
                        GitLabContent::Text(text)
                    }
                },
            })
            .collect();

        GitLabRequest {
            model: request.model,
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stream: false,
        }
    }
}

#[async_trait]
impl Provider for GitLabProvider {
    fn id(&self) -> &str {
        "gitlab"
    }

    fn name(&self) -> &str {
        "GitLab Duo"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let url = self.get_api_url();
        let gitlab_request = self.convert_request(request);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("PRIVATE-TOKEN", &self.config.api_key)
            .json(&gitlab_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::api_error_with_status(
                format!("{}: {}", status, body),
                status.as_u16(),
            ));
        }

        let gitlab_response: GitLabResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        Ok(convert_gitlab_response(gitlab_response))
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let url = self.get_api_url();
        let mut gitlab_request = self.convert_request(request);
        gitlab_request.stream = true;

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("PRIVATE-TOKEN", &self.config.api_key)
            .header("Accept", "text/event-stream")
            .json(&gitlab_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::api_error_with_status(
                format!("{}: {}", status, body),
                status.as_u16(),
            ));
        }

        // bytes_stream() のチャンクは SSE 行の途中で終わり得るため、
        // SseLineBuffer に生バイトのまま積み、改行が確定した完全な行だけを
        // デコードしてパーサへ渡す。イベントは行の順番どおりに放出する。
        let stream = stream::try_unfold(
            (
                response.bytes_stream(),
                crate::stream::SseLineBuffer::new(),
                VecDeque::<StreamEvent>::new(),
                false,
            ),
            |(mut chunks, mut buffer, mut pending, mut exhausted)| async move {
                loop {
                    if let Some(event) = pending.pop_front() {
                        return Ok(Some((event, (chunks, buffer, pending, exhausted))));
                    }

                    if exhausted {
                        return Ok(None);
                    }

                    match chunks.next().await {
                        Some(Ok(bytes)) => {
                            for line in buffer.push(bytes.as_ref()) {
                                push_gitlab_sse_line(&line, &mut pending);
                            }
                        }
                        Some(Err(err)) => return Err(ProviderError::StreamError(err.to_string())),
                        None => {
                            exhausted = true;
                            for line in buffer.flush() {
                                push_gitlab_sse_line(&line, &mut pending);
                            }
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// 完全な SSE 行をパースし、生成されたイベントを `pending` へ積む。
///
/// `[DONE]` と `data: ` で始まらない行（イベント名や空行）の扱いは従来どおり。
fn push_gitlab_sse_line(line: &str, pending: &mut VecDeque<StreamEvent>) {
    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            pending.push_back(StreamEvent::Done);
        } else if let Some(event) = parse_gitlab_sse(data) {
            pending.push_back(event);
        }
    }
}

#[derive(Debug, Serialize)]
struct GitLabRequest {
    model: String,
    messages: Vec<GitLabMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct GitLabMessage {
    role: String,
    content: GitLabContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum GitLabContent {
    Text(String),
}

#[derive(Debug, Deserialize)]
struct GitLabResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<GitLabChoice>,
    usage: Option<GitLabUsage>,
}

#[derive(Debug, Deserialize)]
struct GitLabChoice {
    _index: u32,
    message: GitLabResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabResponseMessage {
    _role: String,
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct GitLabStreamResponse {
    choices: Vec<GitLabStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct GitLabStreamChoice {
    delta: GitLabDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabDelta {
    content: Option<String>,
}

fn convert_gitlab_response(response: GitLabResponse) -> ChatResponse {
    let content = response
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    let usage = response.usage.map(|u| Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    });

    ChatResponse {
        id: response
            .id
            .unwrap_or_else(|| format!("gitlab_{}", uuid::Uuid::new_v4())),
        model: response.model.unwrap_or_else(|| "gitlab".to_string()),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: Role::Assistant,
                content: Content::Text(content),
                cache_control: None,
                provider_options: None,
            },
            finish_reason: response
                .choices
                .first()
                .and_then(|c| c.finish_reason.clone()),
        }],
        usage,
    }
}

fn parse_gitlab_sse(data: &str) -> Option<StreamEvent> {
    if data.is_empty() {
        return None;
    }

    let response: GitLabStreamResponse = serde_json::from_str(data).ok()?;

    let choice = response.choices.first()?;

    if let Some(content) = &choice.delta.content {
        if !content.is_empty() {
            return Some(StreamEvent::TextDelta(content.clone()));
        }
    }

    if let Some(reason) = &choice.finish_reason {
        if reason == "tool_calls" {
            return Some(StreamEvent::Done);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_text_delta() {
        let event =
            parse_gitlab_sse(r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#)
                .unwrap();
        assert!(matches!(event, StreamEvent::TextDelta(t) if t == "hello"));
    }

    #[test]
    fn parse_sse_empty_and_tool_calls() {
        assert!(parse_gitlab_sse("").is_none());
        assert!(parse_gitlab_sse("not json").is_none());

        let done =
            parse_gitlab_sse(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#).unwrap();
        assert!(matches!(done, StreamEvent::Done));
    }

    #[test]
    fn sse_buffer_reassembles_split_line_and_multibyte_text() {
        let mut buffer = crate::stream::SseLineBuffer::new();
        let mut pending = VecDeque::new();

        let text = "こんにちは";
        let text_bytes = text.as_bytes();
        assert_eq!(text_bytes[0], 0xE3);

        let mut first = b"event: message\ndata: {\"choices\":[{\"delta\":{\"content\":\"".to_vec();
        first.extend_from_slice(&text_bytes[..1]);
        for line in buffer.push(&first) {
            push_gitlab_sse_line(&line, &mut pending);
        }
        assert!(pending.is_empty(), "行が確定する前はイベントを出さない");

        let mut second = text_bytes[1..].to_vec();
        second.extend_from_slice(b"\"}}]}\n\ndata: [DONE]\n\n");
        for line in buffer.push(&second) {
            push_gitlab_sse_line(&line, &mut pending);
        }

        let events: Vec<StreamEvent> = pending.into_iter().collect();
        assert_eq!(events.len(), 2, "イベントは行の順番どおり: {events:?}");
        assert!(matches!(&events[0], StreamEvent::TextDelta(delta) if delta == text));
        assert!(matches!(&events[1], StreamEvent::Done));
    }
}
