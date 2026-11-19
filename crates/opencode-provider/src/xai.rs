use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const XAI_API_URL: &str = "https://api.x.ai/v1/chat/completions";

#[derive(Debug)]
pub struct XaiProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl XaiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "grok-2-latest".to_string(),
                    name: "Grok 2".to_string(),
                    provider: "xai".to_string(),
                    context_window: 131072,
                    max_output_tokens: 8192,
                    supports_vision: true,
                    supports_tools: true,
                    cost_per_million_input: 2.0,
                    cost_per_million_output: 10.0,
                },
                ModelInfo {
                    id: "grok-2-1212".to_string(),
                    name: "Grok 2 (1212)".to_string(),
                    provider: "xai".to_string(),
                    context_window: 131072,
                    max_output_tokens: 8192,
                    supports_vision: true,
                    supports_tools: true,
                    cost_per_million_input: 2.0,
                    cost_per_million_output: 10.0,
                },
                ModelInfo {
                    id: "grok-beta".to_string(),
                    name: "Grok Beta".to_string(),
                    provider: "xai".to_string(),
                    context_window: 131072,
                    max_output_tokens: 8192,
                    supports_vision: true,
                    supports_tools: true,
                    cost_per_million_input: 5.0,
                    cost_per_million_output: 15.0,
                },
                ModelInfo {
                    id: "grok-vision-beta".to_string(),
                    name: "Grok Vision Beta".to_string(),
                    provider: "xai".to_string(),
                    context_window: 8192,
                    max_output_tokens: 4096,
                    supports_vision: true,
                    supports_tools: false,
                    cost_per_million_input: 5.0,
                    cost_per_million_output: 15.0,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for XaiProvider {
    fn id(&self) -> &str {
        "xai"
    }
    fn name(&self) -> &str {
        "X.AI (Grok)"
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
            .post(XAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
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
            .post(XAI_API_URL)
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
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
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
    fn provider() -> XaiProvider {
        XaiProvider::new("dummy-xai-key-for-tests")
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "xai");
        assert_eq!(provider.name(), "X.AI (Grok)");
    }

    #[test]
    fn models_are_prepopulated_with_expected_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 4);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "grok-2-latest",
                "grok-2-1212",
                "grok-beta",
                "grok-vision-beta",
            ]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "xai" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("grok-2-latest")
            .expect("known model should be present");

        assert_eq!(model.id, "grok-2-latest");
        assert_eq!(model.name, "Grok 2");
        assert_eq!(model.provider, "xai");
        assert_eq!(model.context_window, 131_072);
        assert_eq!(model.max_output_tokens, 8192);
        assert!(model.supports_vision);
        assert!(model.supports_tools);
        assert_eq!(model.cost_per_million_input, 2.0);
        assert_eq!(model.cost_per_million_output, 10.0);
    }

    #[test]
    fn get_model_distinguishes_grok_2_variants_and_costs() {
        let provider = provider();

        let latest = provider
            .get_model("grok-2-latest")
            .expect("latest should be present");
        let variant = provider
            .get_model("grok-2-1212")
            .expect("1212 variant should be present");
        assert_ne!(latest.id, variant.id);
        assert_eq!(variant.name, "Grok 2 (1212)");
        assert_eq!(variant.context_window, latest.context_window);
        assert_eq!(variant.max_output_tokens, latest.max_output_tokens);
        assert_eq!(
            variant.cost_per_million_input,
            latest.cost_per_million_input
        );
        assert_eq!(
            variant.cost_per_million_output,
            latest.cost_per_million_output
        );

        let beta = provider
            .get_model("grok-beta")
            .expect("beta should be present");
        assert_eq!(beta.name, "Grok Beta");
        assert!(beta.cost_per_million_input > latest.cost_per_million_input);
        assert!(beta.cost_per_million_output > latest.cost_per_million_output);
    }

    #[test]
    fn get_model_reflects_capability_flags() {
        let provider = provider();

        // xai のモデルはすべて vision 対応だが、tools は grok-vision-beta のみ非対応。
        assert!(provider
            .models()
            .iter()
            .filter(|m| m.id != "grok-vision-beta")
            .all(|m| m.supports_vision && m.supports_tools));

        let vision = provider
            .get_model("grok-vision-beta")
            .expect("vision beta should be present");
        assert!(vision.supports_vision);
        assert!(!vision.supports_tools);
        assert_eq!(vision.context_window, 8192);
        assert_eq!(vision.max_output_tokens, 4096);
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider.get_model("grok-v9").is_none());
        assert!(provider.get_model("").is_none());
    }

    #[test]
    fn models_returns_owned_snapshot() {
        let provider = provider();
        let mut models = provider.models();
        models.clear();

        assert!(models.is_empty());
        assert_eq!(provider.models().len(), 4);
    }
}
