use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, Choice, Content, Message, ModelInfo, Provider, ProviderError, Role,
    StreamEvent, StreamResult, Usage,
};

const GOOGLE_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

#[derive(Debug, Clone)]
pub struct GoogleConfig {
    pub api_key: String,
    pub base_url: Option<String>,
}

#[derive(Debug)]
pub struct GoogleProvider {
    client: Client,
    config: GoogleConfig,
    models: Vec<ModelInfo>,
}

impl GoogleProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_config(GoogleConfig {
            api_key: api_key.into(),
            base_url: None,
        })
    }

    pub fn with_config(config: GoogleConfig) -> Self {
        let models = vec![
            ModelInfo {
                id: "gemini-2.5-pro-preview-06-05".to_string(),
                name: "Gemini 2.5 Pro".to_string(),
                provider: "google".to_string(),
                context_window: 1000000,
                max_output_tokens: 65536,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 1.25,
                cost_per_million_output: 10.0,
            },
            ModelInfo {
                id: "gemini-2.0-flash".to_string(),
                name: "Gemini 2.0 Flash".to_string(),
                provider: "google".to_string(),
                context_window: 1000000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.1,
                cost_per_million_output: 0.4,
            },
            ModelInfo {
                id: "gemini-2.0-flash-lite".to_string(),
                name: "Gemini 2.0 Flash Lite".to_string(),
                provider: "google".to_string(),
                context_window: 1000000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.075,
                cost_per_million_output: 0.3,
            },
            ModelInfo {
                id: "gemini-1.5-pro".to_string(),
                name: "Gemini 1.5 Pro".to_string(),
                provider: "google".to_string(),
                context_window: 2000000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 1.25,
                cost_per_million_output: 5.0,
            },
            ModelInfo {
                id: "gemini-1.5-flash".to_string(),
                name: "Gemini 1.5 Flash".to_string(),
                provider: "google".to_string(),
                context_window: 1000000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.075,
                cost_per_million_output: 0.3,
            },
        ];

        Self {
            client: Client::new(),
            config,
            models,
        }
    }

    fn convert_request(&self, request: ChatRequest) -> GoogleRequest {
        let mut contents = Vec::new();
        let mut system_instruction = None;

        for msg in request.messages {
            match msg.role {
                Role::System => {
                    if let Content::Text(text) = msg.content {
                        system_instruction = Some(GoogleContent {
                            parts: vec![GooglePart::text(&text)],
                            role: "user".to_string(),
                        });
                    }
                }
                Role::User => {
                    let text_content = match &msg.content {
                        Content::Text(t) => t.clone(),
                        Content::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| p.text.clone())
                            .collect::<Vec<_>>()
                            .join(" "),
                    };
                    contents.push(GoogleContent {
                        parts: vec![GooglePart::text(&text_content)],
                        role: "user".to_string(),
                    });
                }
                Role::Assistant => {
                    let text_content = match &msg.content {
                        Content::Text(t) => t.clone(),
                        Content::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| p.text.clone())
                            .collect::<Vec<_>>()
                            .join(" "),
                    };
                    contents.push(GoogleContent {
                        parts: vec![GooglePart::text(&text_content)],
                        role: "model".to_string(),
                    });
                }
                Role::Tool => {}
            }
        }

        GoogleRequest {
            contents,
            system_instruction,
            generation_config: Some(GenerationConfig {
                max_output_tokens: request.max_tokens,
                temperature: request.temperature,
            }),
        }
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    fn id(&self) -> &str {
        "google"
    }

    fn name(&self) -> &str {
        "Google AI"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let base_url = self.config.base_url.as_deref().unwrap_or(GOOGLE_API_URL);
        let url = format!(
            "{}/{}:generateContent?key={}",
            base_url, request.model, self.config.api_key
        );

        let google_request = self.convert_request(request);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&google_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
        }

        let google_response: GoogleResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        Ok(convert_google_response(google_response))
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let base_url = self.config.base_url.as_deref().unwrap_or(GOOGLE_API_URL);
        let url = format!(
            "{}/{}:streamGenerateContent?key={}&alt=sse",
            base_url, request.model, self.config.api_key
        );

        let google_request = self.convert_request(request);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&google_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
        }

        // bytes_stream() のチャンクは SSE 行の途中で終わり得るため、
        // SseLineBuffer に生バイトのまま積み、改行が確定した完全な行だけを
        // デコードしてパーサへ渡す。イベントは行の順番どおりに放出する。
        // parse_google_sse の usage_metadata / finish_reason の扱いは変更しない。
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
                                push_google_sse_line(&line, &mut pending);
                            }
                        }
                        Some(Err(err)) => return Err(ProviderError::StreamError(err.to_string())),
                        None => {
                            exhausted = true;
                            for line in buffer.flush() {
                                push_google_sse_line(&line, &mut pending);
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
/// `data: ` で始まらない行（イベント名や空行）は従来どおり無視する。
/// `usage_metadata` / `finish_reason` は `parse_google_sse` が従来どおり扱う。
fn push_google_sse_line(line: &str, pending: &mut VecDeque<StreamEvent>) {
    if let Some(data) = line.strip_prefix("data: ") {
        if let Some(event) = parse_google_sse(data) {
            pending.push_back(event);
        }
    }
}

#[derive(Debug, Serialize)]
struct GoogleRequest {
    contents: Vec<GoogleContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GoogleContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleContent {
    parts: Vec<GooglePart>,
    role: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GooglePart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

impl GooglePart {
    fn text(t: &str) -> Self {
        Self {
            text: Some(t.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct GoogleResponse {
    candidates: Vec<GoogleCandidate>,
    usage_metadata: Option<GoogleUsage>,
}

#[derive(Debug, Deserialize)]
struct GoogleCandidate {
    content: GoogleContent,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUsage {
    prompt_token_count: u64,
    candidates_token_count: u64,
    total_token_count: u64,
}

fn convert_google_response(response: GoogleResponse) -> ChatResponse {
    let content = response
        .candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .and_then(|p| p.text.clone())
        .unwrap_or_default();

    let usage = response.usage_metadata.map(|u| Usage {
        prompt_tokens: u.prompt_token_count,
        completion_tokens: u.candidates_token_count,
        total_tokens: u.total_token_count,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    });

    ChatResponse {
        id: format!("google_{}", uuid::Uuid::new_v4()),
        model: "google".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message::assistant(&content),
            finish_reason: response
                .candidates
                .first()
                .and_then(|c| c.finish_reason.clone()),
        }],
        usage,
    }
}

fn parse_google_sse(data: &str) -> Option<StreamEvent> {
    if data.is_empty() {
        return None;
    }

    let response: GoogleResponse = serde_json::from_str(data).ok()?;

    let text = response
        .candidates
        .first()?
        .content
        .parts
        .first()?
        .text
        .clone()?;

    Some(StreamEvent::TextDelta(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentPart;

    fn dummy_config() -> GoogleConfig {
        GoogleConfig {
            api_key: "dummy-key-for-tests".to_string(),
            base_url: None,
        }
    }

    #[test]
    fn new_exposes_google_identity_and_bundled_models() {
        let provider = GoogleProvider::new("dummy-key");
        assert_eq!(provider.id(), "google");
        assert_eq!(provider.name(), "Google AI");

        let models = provider.models();
        assert_eq!(models.len(), 5);
        assert!(models.iter().all(|m| m.provider == "google"));
        assert!(models
            .iter()
            .any(|m| m.id == "gemini-2.5-pro-preview-06-05"));
        assert!(models.iter().any(|m| m.id == "gemini-2.0-flash-lite"));
    }

    #[test]
    fn with_config_keeps_config_and_model_catalog() {
        let base_url = "http://127.0.0.1:9/v1beta/models";
        let provider = GoogleProvider::with_config(GoogleConfig {
            api_key: "dummy-key".to_string(),
            base_url: Some(base_url.to_string()),
        });
        assert_eq!(provider.config.api_key, "dummy-key");
        assert_eq!(provider.config.base_url.as_deref(), Some(base_url));
        assert_eq!(provider.models().len(), 5);

        let default_provider = GoogleProvider::with_config(dummy_config());
        assert_eq!(default_provider.config.base_url, None);
        assert_eq!(default_provider.models().len(), provider.models().len());
    }

    #[test]
    fn get_model_finds_known_model_and_rejects_unknown() {
        let provider = GoogleProvider::new("dummy-key");
        let model = provider
            .get_model("gemini-1.5-pro")
            .expect("bundled model must be present");
        assert_eq!(model.name, "Gemini 1.5 Pro");
        assert_eq!(model.context_window, 2_000_000);
        assert!(provider.get_model("does-not-exist").is_none());
    }

    #[test]
    fn convert_request_maps_roles_and_generation_config() {
        let provider = GoogleProvider::new("dummy-key");
        let request = ChatRequest::new(
            "gemini-2.0-flash",
            vec![
                Message::system("system prompt"),
                Message::user("hello"),
                Message::assistant("previous answer"),
            ],
        )
        .with_max_tokens(128)
        .with_temperature(0.5);

        let converted = provider.convert_request(request);

        let system = converted.system_instruction.expect("system instruction");
        assert_eq!(system.role, "user");
        assert_eq!(system.parts.len(), 1);
        assert_eq!(system.parts[0].text.as_deref(), Some("system prompt"));

        assert_eq!(converted.contents.len(), 2);
        assert_eq!(converted.contents[0].role, "user");
        assert_eq!(
            converted.contents[0].parts[0].text.as_deref(),
            Some("hello")
        );
        assert_eq!(converted.contents[1].role, "model");
        assert_eq!(
            converted.contents[1].parts[0].text.as_deref(),
            Some("previous answer")
        );

        let config = converted.generation_config.expect("generation config");
        assert_eq!(config.max_output_tokens, Some(128));
        assert_eq!(config.temperature, Some(0.5));
    }

    #[test]
    fn convert_request_joins_text_parts_and_skips_tool_messages() {
        let provider = GoogleProvider::new("dummy-key");
        let user_parts = Message {
            role: Role::User,
            content: Content::Parts(vec![
                ContentPart {
                    text: Some("first".to_string()),
                    ..Default::default()
                },
                ContentPart {
                    text: None,
                    ..Default::default()
                },
                ContentPart {
                    text: Some("second".to_string()),
                    ..Default::default()
                },
            ]),
            cache_control: None,
            provider_options: None,
        };
        let tool_message = Message {
            role: Role::Tool,
            content: Content::Text("tool output".to_string()),
            cache_control: None,
            provider_options: None,
        };
        let request = ChatRequest::new("gemini-2.0-flash", vec![user_parts, tool_message]);

        let converted = provider.convert_request(request);

        assert!(converted.system_instruction.is_none());
        assert_eq!(converted.contents.len(), 1);
        assert_eq!(converted.contents[0].role, "user");
        assert_eq!(
            converted.contents[0].parts[0].text.as_deref(),
            Some("first second")
        );
        let config = converted.generation_config.expect("generation config");
        assert_eq!(config.max_output_tokens, None);
        assert_eq!(config.temperature, None);
    }

    #[test]
    fn convert_request_ignores_non_text_system_content() {
        let provider = GoogleProvider::new("dummy-key");
        let system_parts = Message {
            role: Role::System,
            content: Content::Parts(vec![ContentPart {
                text: Some("ignored".to_string()),
                ..Default::default()
            }]),
            cache_control: None,
            provider_options: None,
        };
        let converted =
            provider.convert_request(ChatRequest::new("gemini-2.0-flash", vec![system_parts]));
        assert!(converted.system_instruction.is_none());
        assert!(converted.contents.is_empty());
    }

    #[test]
    fn convert_google_response_maps_text_usage_and_finish_reason() {
        let response = GoogleResponse {
            candidates: vec![GoogleCandidate {
                content: GoogleContent {
                    parts: vec![GooglePart::text("answer")],
                    role: "model".to_string(),
                },
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(GoogleUsage {
                prompt_token_count: 10,
                candidates_token_count: 4,
                total_token_count: 14,
            }),
        };

        let converted = convert_google_response(response);

        assert!(converted.id.starts_with("google_"));
        assert_eq!(converted.model, "google");
        assert_eq!(converted.choices.len(), 1);
        assert_eq!(converted.choices[0].finish_reason.as_deref(), Some("STOP"));
        assert!(matches!(
            &converted.choices[0].message.content,
            Content::Text(text) if text == "answer"
        ));
        let usage = converted.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 4);
        assert_eq!(usage.total_tokens, 14);
        assert_eq!(usage.cache_read_input_tokens, None);
        assert_eq!(usage.cache_creation_input_tokens, None);
    }

    #[test]
    fn parse_google_sse_handles_text_and_invalid_payloads() {
        let event = parse_google_sse(
            r#"{"candidates":[{"content":{"parts":[{"text":"chunk"}],"role":"model"}}]}"#,
        )
        .expect("valid SSE payload");
        assert!(matches!(event, StreamEvent::TextDelta(text) if text == "chunk"));
        assert!(parse_google_sse("").is_none());
        assert!(parse_google_sse("not json").is_none());
        assert!(
            parse_google_sse(r#"{"candidates":[{"content":{"parts":[],"role":"model"}}]}"#)
                .is_none()
        );
    }

    #[test]
    fn sse_buffer_reassembles_split_line_and_keeps_google_parse_semantics() {
        let mut buffer = crate::stream::SseLineBuffer::new();
        let mut pending = VecDeque::new();

        let text = "こんにちは";
        let text_bytes = text.as_bytes();
        assert_eq!(text_bytes[0], 0xE3);

        let mut first = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"".to_vec();
        first.extend_from_slice(&text_bytes[..1]);
        for line in buffer.push(&first) {
            push_google_sse_line(&line, &mut pending);
        }
        assert!(pending.is_empty(), "行が確定する前はイベントを出さない");

        let mut second = text_bytes[1..].to_vec();
        // finish_reason / usage_metadata を含めても parse_google_sse の挙動は不変。
        second.extend_from_slice(
            b"\"}],\"role\":\"model\"},\"finish_reason\":\"STOP\"}],\"usage_metadata\":{\"prompt_token_count\":1,\"candidates_token_count\":2,\"total_token_count\":3}}\n\n",
        );
        for line in buffer.push(&second) {
            push_google_sse_line(&line, &mut pending);
        }

        let events: Vec<StreamEvent> = pending.into_iter().collect();
        assert_eq!(
            events.len(),
            1,
            "usage_metadata/finish_reason は追加イベントを出さない: {events:?}"
        );
        assert!(matches!(&events[0], StreamEvent::TextDelta(delta) if delta == text));
    }
}
