use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const MISTRAL_API_URL: &str = "https://api.mistral.ai/v1/chat/completions";

#[derive(Debug)]
pub struct MistralProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl MistralProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "mistral-large-latest".to_string(),
                    name: "Mistral Large".to_string(),
                    provider: "mistral".to_string(),
                    context_window: 128000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 2.0,
                    cost_per_million_output: 6.0,
                },
                ModelInfo {
                    id: "mistral-medium-latest".to_string(),
                    name: "Mistral Medium".to_string(),
                    provider: "mistral".to_string(),
                    context_window: 32000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 2.7,
                    cost_per_million_output: 8.1,
                },
                ModelInfo {
                    id: "mistral-small-latest".to_string(),
                    name: "Mistral Small".to_string(),
                    provider: "mistral".to_string(),
                    context_window: 32000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.2,
                    cost_per_million_output: 0.6,
                },
                ModelInfo {
                    id: "codestral-latest".to_string(),
                    name: "Codestral".to_string(),
                    provider: "mistral".to_string(),
                    context_window: 32000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.3,
                    cost_per_million_output: 0.9,
                },
                ModelInfo {
                    id: "pixtral-12b-2409".to_string(),
                    name: "Pixtral 12B".to_string(),
                    provider: "mistral".to_string(),
                    context_window: 128000,
                    max_output_tokens: 8192,
                    supports_vision: true,
                    supports_tools: false,
                    cost_per_million_input: 0.15,
                    cost_per_million_output: 0.15,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for MistralProvider {
    fn id(&self) -> &str {
        "mistral"
    }
    fn name(&self) -> &str {
        "Mistral AI"
    }
    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }
    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let response = self
            .client
            .post(MISTRAL_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
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

        response
            .json()
            .await
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let mut stream_request = request;
        stream_request.stream = Some(true);

        let response = self
            .client
            .post(MISTRAL_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&stream_request)
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

        // bytes_stream のチャンク境界をまたぐ SSE 行を SseLineBuffer で組み立てる。
        // 完全な行が確定してからデコードするため、UTF-8 文字の途中でチャンクが
        // 切れても U+FFFD に置換されずに復元される。
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
                            push_openai_sse_lines(buffer.push(bytes.as_ref()), &mut pending);
                        }
                        Some(Err(err)) => {
                            return Err(ProviderError::StreamError(err.to_string()));
                        }
                        None => {
                            exhausted = true;
                            // 終端で改行のない末尾行を処理する。
                            push_openai_sse_lines(buffer.flush(), &mut pending);
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// SseLineBuffer が返した完全な SSE 行のうち `data: ` 行だけを OpenAI 互換パーサへ渡す。
///
/// `data: ` で始まらない行（イベント名や空行）は従来どおり無視する。
fn push_openai_sse_lines(lines: Vec<String>, pending: &mut VecDeque<StreamEvent>) {
    for line in lines {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Some(event) = crate::stream::parse_openai_sse(data) {
                pending.push_back(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 外部 HTTP を呼ばず、ダミーキーだけで検証できる純粋ロジックに限定する。
    fn provider() -> MistralProvider {
        MistralProvider::new("dummy-mistral-key-for-tests")
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "mistral");
        assert_eq!(provider.name(), "Mistral AI");
    }

    #[test]
    fn models_are_prepopulated_with_expected_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 5);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "mistral-large-latest",
                "mistral-medium-latest",
                "mistral-small-latest",
                "codestral-latest",
                "pixtral-12b-2409",
            ]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "mistral" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("mistral-large-latest")
            .expect("known model should be present");

        assert_eq!(model.id, "mistral-large-latest");
        assert_eq!(model.name, "Mistral Large");
        assert_eq!(model.provider, "mistral");
        assert_eq!(model.context_window, 128_000);
        assert_eq!(model.max_output_tokens, 8192);
        assert_eq!(model.cost_per_million_input, 2.0);
        assert_eq!(model.cost_per_million_output, 6.0);
    }

    #[test]
    fn get_model_reflects_capability_flags() {
        let provider = provider();

        let pixtral = provider
            .get_model("pixtral-12b-2409")
            .expect("pixtral should be present");
        assert!(pixtral.supports_vision);
        assert!(!pixtral.supports_tools);

        let large = provider
            .get_model("mistral-large-latest")
            .expect("large should be present");
        assert!(!large.supports_vision);
        assert!(large.supports_tools);
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider.get_model("not-a-model").is_none());
        assert!(provider.get_model("").is_none());
    }

    #[test]
    fn models_returns_owned_snapshot() {
        let provider = provider();
        let mut models = provider.models();
        models.clear();

        assert!(models.is_empty());
        assert_eq!(provider.models().len(), 5);
    }

    #[test]
    fn sse_line_buffer_reassembles_split_openai_event() {
        use crate::stream::SseLineBuffer;

        let mut buffer = SseLineBuffer::new();
        let mut pending = VecDeque::new();
        let text = "こんにちは";
        let line = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n");
        let bytes = line.as_bytes();
        // "こ" の先頭バイト 0xE3 の直後で分割し、UTF-8 途中分割を再現する。
        let split = bytes
            .iter()
            .position(|byte| *byte == 0xE3)
            .expect("UTF-8 先頭バイト")
            + 1;

        push_openai_sse_lines(buffer.push(&bytes[..split]), &mut pending);
        assert!(pending.is_empty(), "完全な行が揃うまでイベントを出さない");

        push_openai_sse_lines(buffer.push(&bytes[split..]), &mut pending);
        assert!(
            matches!(pending.pop_front(), Some(StreamEvent::TextDelta(delta)) if delta == text),
            "分割されたマルチバイト文字は U+FFFD に置換されず復元される必要がある"
        );
        assert!(pending.is_empty());

        push_openai_sse_lines(buffer.push(b"data: [DONE]\n"), &mut pending);
        assert!(matches!(pending.pop_front(), Some(StreamEvent::Done)));
        assert!(pending.is_empty());
    }
}
