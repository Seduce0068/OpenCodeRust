use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const COHERE_API_URL: &str = "https://api.cohere.ai/v2/chat";

#[derive(Debug)]
pub struct CohereProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl CohereProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "command-r-plus-08-2024".to_string(),
                    name: "Command R+".to_string(),
                    provider: "cohere".to_string(),
                    context_window: 128000,
                    max_output_tokens: 4096,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 2.5,
                    cost_per_million_output: 10.0,
                },
                ModelInfo {
                    id: "command-r-08-2024".to_string(),
                    name: "Command R".to_string(),
                    provider: "cohere".to_string(),
                    context_window: 128000,
                    max_output_tokens: 4096,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.15,
                    cost_per_million_output: 0.6,
                },
                ModelInfo {
                    id: "command".to_string(),
                    name: "Command".to_string(),
                    provider: "cohere".to_string(),
                    context_window: 4096,
                    max_output_tokens: 4096,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 1.0,
                    cost_per_million_output: 2.0,
                },
                ModelInfo {
                    id: "command-light".to_string(),
                    name: "Command Light".to_string(),
                    provider: "cohere".to_string(),
                    context_window: 4096,
                    max_output_tokens: 4096,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 0.3,
                    cost_per_million_output: 0.6,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for CohereProvider {
    fn id(&self) -> &str {
        "cohere"
    }
    fn name(&self) -> &str {
        "Cohere"
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
            .post(COHERE_API_URL)
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
            .post(COHERE_API_URL)
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
    fn provider() -> CohereProvider {
        CohereProvider::new("dummy-cohere-key-for-tests")
    }

    #[test]
    fn new_accepts_dummy_key_and_exposes_stable_identity() {
        let from_str = provider();
        let from_owned = CohereProvider::new(String::from("another-dummy-cohere-key"));

        assert_eq!(from_str.id(), "cohere");
        assert_eq!(from_str.name(), "Cohere");
        // new は impl Into<String> を受け取るので String でも同一の内容になる。
        assert_eq!(from_owned.id(), from_str.id());
        assert_eq!(from_owned.name(), from_str.name());
        assert_eq!(from_owned.models().len(), from_str.models().len());
    }

    #[test]
    fn models_include_dated_command_r_and_legacy_command_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 4);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "command-r-plus-08-2024",
                "command-r-08-2024",
                "command",
                "command-light",
            ]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "cohere" && !m.name.is_empty()));
    }

    #[test]
    fn command_r_plus_has_premium_cohere_pricing_and_capabilities() {
        let provider = provider();
        let model = provider
            .get_model("command-r-plus-08-2024")
            .expect("dated Command R+ should be present");

        assert_eq!(model.id, "command-r-plus-08-2024");
        assert_eq!(model.name, "Command R+");
        assert_eq!(model.provider, "cohere");
        assert_eq!(model.context_window, 128_000);
        assert_eq!(model.max_output_tokens, 4096);
        assert!(!model.supports_vision);
        assert!(model.supports_tools);
        assert_eq!(model.cost_per_million_input, 2.5);
        assert_eq!(model.cost_per_million_output, 10.0);
    }

    #[test]
    fn dated_command_r_models_support_tools_but_legacy_command_models_do_not() {
        let provider = provider();

        let command_r = provider
            .get_model("command-r-08-2024")
            .expect("dated Command R should be present");
        assert_eq!(command_r.name, "Command R");
        assert_eq!(command_r.context_window, 128_000);
        assert!(!command_r.supports_vision);
        assert!(command_r.supports_tools);
        assert_eq!(command_r.cost_per_million_input, 0.15);
        assert_eq!(command_r.cost_per_million_output, 0.6);

        for legacy_id in ["command", "command-light"] {
            let legacy = provider
                .get_model(legacy_id)
                .expect("legacy Command model should be present");
            assert_eq!(legacy.provider, "cohere");
            assert_eq!(legacy.context_window, 4096);
            assert_eq!(legacy.max_output_tokens, 4096);
            assert!(!legacy.supports_vision);
            assert!(!legacy.supports_tools);
        }
    }

    #[test]
    fn legacy_command_light_is_cheaper_than_command() {
        let provider = provider();
        let command = provider
            .get_model("command")
            .expect("command should be present");
        let light = provider
            .get_model("command-light")
            .expect("command-light should be present");

        assert_eq!(command.cost_per_million_input, 1.0);
        assert_eq!(command.cost_per_million_output, 2.0);
        assert_eq!(light.cost_per_million_input, 0.3);
        assert_eq!(light.cost_per_million_output, 0.6);
        assert!(light.cost_per_million_input < command.cost_per_million_input);
        assert!(light.cost_per_million_output < command.cost_per_million_output);
    }

    #[test]
    fn all_cohere_models_are_text_only() {
        let provider = provider();
        assert!(provider
            .models()
            .iter()
            .all(|m| !m.supports_vision && m.provider == "cohere"));
    }

    #[test]
    fn get_model_rejects_undated_and_unknown_ids() {
        let provider = provider();
        // ID に日付サフィックスが付くため、サフィックスなしは登録済みモデルではない。
        assert!(provider.get_model("command-r-plus").is_none());
        assert!(provider.get_model("command-r").is_none());
        assert!(provider
            .get_model("command-r-plus-08-2024-unknown")
            .is_none());
        assert!(provider.get_model("").is_none());
    }

    #[test]
    fn models_returns_detached_owned_snapshot() {
        let provider = provider();
        let mut models = provider.models();
        models.clear();

        assert!(models.is_empty());
        assert_eq!(provider.models().len(), 4);
    }
}
