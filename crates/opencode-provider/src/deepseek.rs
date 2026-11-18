use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const DEEPSEEK_API_URL: &str = "https://api.deepseek.com/chat/completions";

#[derive(Debug)]
pub struct DeepSeekProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl DeepSeekProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "deepseek-chat".to_string(),
                    name: "DeepSeek Chat".to_string(),
                    provider: "deepseek".to_string(),
                    context_window: 64000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.27,
                    cost_per_million_output: 1.1,
                },
                ModelInfo {
                    id: "deepseek-reasoner".to_string(),
                    name: "DeepSeek Reasoner".to_string(),
                    provider: "deepseek".to_string(),
                    context_window: 64000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 0.55,
                    cost_per_million_output: 2.19,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    fn id(&self) -> &str {
        "deepseek"
    }
    fn name(&self) -> &str {
        "DeepSeek"
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
            .post(DEEPSEEK_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::ApiError(e.to_string()))
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let mut stream_request = request;
        stream_request.stream = Some(true);

        let response = self
            .client
            .post(DEEPSEEK_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&stream_request)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
        }

        let stream = response
            .bytes_stream()
            .map(|chunk_result| match chunk_result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if let Some(event) = crate::stream::parse_openai_sse(data) {
                                return Ok(event);
                            }
                        }
                    }
                    Ok(StreamEvent::TextDelta(String::new()))
                }
                Err(e) => Err(ProviderError::StreamError(e.to_string())),
            });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 外部 HTTP を呼ばず、ダミーキーだけで検証できる純粋ロジックに限定する。
    fn provider() -> DeepSeekProvider {
        DeepSeekProvider::new("dummy-deepseek-key-for-tests")
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "deepseek");
        assert_eq!(provider.name(), "DeepSeek");
    }

    #[test]
    fn models_are_prepopulated_with_expected_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 2);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["deepseek-chat", "deepseek-reasoner"]);
        assert!(models
            .iter()
            .all(|m| m.provider == "deepseek" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("deepseek-chat")
            .expect("known model should be present");

        assert_eq!(model.id, "deepseek-chat");
        assert_eq!(model.name, "DeepSeek Chat");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.context_window, 64_000);
        assert_eq!(model.max_output_tokens, 8192);
        assert_eq!(model.cost_per_million_input, 0.27);
        assert_eq!(model.cost_per_million_output, 1.1);
    }

    #[test]
    fn get_model_distinguishes_tools_capability() {
        let provider = provider();

        let chat = provider
            .get_model("deepseek-chat")
            .expect("chat should be present");
        assert!(!chat.supports_vision);
        assert!(chat.supports_tools);

        let reasoner = provider
            .get_model("deepseek-reasoner")
            .expect("reasoner should be present");
        assert!(!reasoner.supports_vision);
        assert!(!reasoner.supports_tools);
        assert_eq!(reasoner.name, "DeepSeek Reasoner");
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider.get_model("deepseek-v9").is_none());
        assert!(provider.get_model("").is_none());
    }

    #[test]
    fn models_returns_owned_snapshot() {
        let provider = provider();
        let mut models = provider.models();
        models.clear();

        assert!(models.is_empty());
        assert_eq!(provider.models().len(), 2);
    }
}
