use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const TOGETHER_API_URL: &str = "https://api.together.xyz/v1/chat/completions";

#[derive(Debug)]
pub struct TogetherProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl TogetherProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "meta-llama/Llama-3.3-70B-Instruct-Turbo".to_string(),
                    name: "Llama 3.3 70B (Together)".to_string(),
                    provider: "together".to_string(),
                    context_window: 131072,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.88,
                    cost_per_million_output: 0.88,
                },
                ModelInfo {
                    id: "meta-llama/Llama-3.2-90B-Vision-Instruct-Turbo".to_string(),
                    name: "Llama 3.2 90B Vision (Together)".to_string(),
                    provider: "together".to_string(),
                    context_window: 131072,
                    max_output_tokens: 8192,
                    supports_vision: true,
                    supports_tools: true,
                    cost_per_million_input: 0.88,
                    cost_per_million_output: 0.88,
                },
                ModelInfo {
                    id: "mistralai/Mixtral-8x7B-Instruct-v0.1".to_string(),
                    name: "Mixtral 8x7B (Together)".to_string(),
                    provider: "together".to_string(),
                    context_window: 32768,
                    max_output_tokens: 4096,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 0.6,
                    cost_per_million_output: 0.6,
                },
                ModelInfo {
                    id: "Qwen/Qwen2.5-72B-Instruct-Turbo".to_string(),
                    name: "Qwen 2.5 72B (Together)".to_string(),
                    provider: "together".to_string(),
                    context_window: 32768,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.88,
                    cost_per_million_output: 0.88,
                },
                ModelInfo {
                    id: "deepseek-ai/DeepSeek-V3".to_string(),
                    name: "DeepSeek V3 (Together)".to_string(),
                    provider: "together".to_string(),
                    context_window: 131072,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 1.25,
                    cost_per_million_output: 1.25,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for TogetherProvider {
    fn id(&self) -> &str {
        "together"
    }
    fn name(&self) -> &str {
        "Together AI"
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
            .post(TOGETHER_API_URL)
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
            .post(TOGETHER_API_URL)
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

        // チャンク境界で SSE 行や UTF-8 文字が分断されても取りこぼさないよう、
        // 生バイトを SseLineBuffer に保持し、行が確定してからパースする。
        let stream = stream::try_unfold(
            (
                response.bytes_stream(),
                crate::stream::SseLineBuffer::new(),
                VecDeque::<StreamEvent>::new(),
                false,
            ),
            |(mut chunks, mut line_buffer, mut pending, mut exhausted)| async move {
                loop {
                    if let Some(event) = pending.pop_front() {
                        return Ok(Some((event, (chunks, line_buffer, pending, exhausted))));
                    }

                    if exhausted {
                        return Ok(None);
                    }

                    match chunks.next().await {
                        Some(Ok(bytes)) => {
                            push_openai_events(line_buffer.push(bytes.as_ref()), &mut pending);
                        }
                        Some(Err(e)) => {
                            return Err(ProviderError::StreamError(e.to_string()));
                        }
                        None => {
                            exhausted = true;
                            // 改行の無い末尾行も処理する。
                            push_openai_events(line_buffer.flush(), &mut pending);
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// 確定した SSE 行を OpenAI 互換パーサへ渡し、生成されたイベントを `pending` に積む。
fn push_openai_events(lines: Vec<String>, pending: &mut VecDeque<StreamEvent>) {
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
    fn provider() -> TogetherProvider {
        TogetherProvider::new("dummy-together-key-for-tests")
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "together");
        assert_eq!(provider.name(), "Together AI");
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
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                "meta-llama/Llama-3.2-90B-Vision-Instruct-Turbo",
                "mistralai/Mixtral-8x7B-Instruct-v0.1",
                "Qwen/Qwen2.5-72B-Instruct-Turbo",
                "deepseek-ai/DeepSeek-V3",
            ]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "together" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("meta-llama/Llama-3.3-70B-Instruct-Turbo")
            .expect("known model should be present");

        assert_eq!(model.id, "meta-llama/Llama-3.3-70B-Instruct-Turbo");
        assert_eq!(model.name, "Llama 3.3 70B (Together)");
        assert_eq!(model.provider, "together");
        assert_eq!(model.context_window, 131_072);
        assert_eq!(model.max_output_tokens, 8192);
        assert!(!model.supports_vision);
        assert!(model.supports_tools);
        assert_eq!(model.cost_per_million_input, 0.88);
        assert_eq!(model.cost_per_million_output, 0.88);
    }

    #[test]
    fn get_model_reflects_together_capability_flags() {
        let provider = provider();

        // Together のカタログで唯一 vision 対応のモデル。
        let vision = provider
            .get_model("meta-llama/Llama-3.2-90B-Vision-Instruct-Turbo")
            .expect("vision model should be present");
        assert!(vision.supports_vision);
        assert!(vision.supports_tools);
        assert_eq!(vision.name, "Llama 3.2 90B Vision (Together)");

        let all_models = provider.models();
        let vision_models: Vec<&ModelInfo> =
            all_models.iter().filter(|m| m.supports_vision).collect();
        assert_eq!(vision_models.len(), 1);
        assert_eq!(
            vision_models[0].id,
            "meta-llama/Llama-3.2-90B-Vision-Instruct-Turbo"
        );

        // Together のカタログで唯一 tools 非対応のモデル。
        let mixtral = provider
            .get_model("mistralai/Mixtral-8x7B-Instruct-v0.1")
            .expect("mixtral should be present");
        assert!(!mixtral.supports_vision);
        assert!(!mixtral.supports_tools);
        assert_eq!(mixtral.context_window, 32_768);
        assert_eq!(mixtral.max_output_tokens, 4096);
        assert_eq!(mixtral.cost_per_million_input, 0.6);

        // 最も高価なモデルは DeepSeek V3。
        let deepseek = provider
            .get_model("deepseek-ai/DeepSeek-V3")
            .expect("deepseek should be present");
        assert_eq!(deepseek.cost_per_million_input, 1.25);
        assert_eq!(deepseek.cost_per_million_output, 1.25);
        assert!(deepseek.supports_tools);
        assert!(!deepseek.supports_vision);
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider
            .get_model("meta-llama/Llama-3.3-70B-Instruct-Turbo-unknown")
            .is_none());
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
}
