use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const DEEPINFRA_API_URL: &str = "https://api.deepinfra.com/v1/openai/chat/completions";

#[derive(Debug)]
pub struct DeepInfraProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl DeepInfraProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "meta-llama/Meta-Llama-3-70B-Instruct".to_string(),
                    name: "Llama 3 70B (DeepInfra)".to_string(),
                    provider: "deepinfra".to_string(),
                    context_window: 8192,
                    max_output_tokens: 4096,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.59,
                    cost_per_million_output: 0.79,
                },
                ModelInfo {
                    id: "meta-llama/Llama-3.3-70B-Instruct".to_string(),
                    name: "Llama 3.3 70B (DeepInfra)".to_string(),
                    provider: "deepinfra".to_string(),
                    context_window: 128000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.35,
                    cost_per_million_output: 0.40,
                },
                ModelInfo {
                    id: "mistralai/Mistral-Small-24B-Instruct-2501".to_string(),
                    name: "Mistral Small 24B (DeepInfra)".to_string(),
                    provider: "deepinfra".to_string(),
                    context_window: 32768,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.10,
                    cost_per_million_output: 0.10,
                },
                ModelInfo {
                    id: "Qwen/Qwen2.5-72B-Instruct".to_string(),
                    name: "Qwen 2.5 72B (DeepInfra)".to_string(),
                    provider: "deepinfra".to_string(),
                    context_window: 32768,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.35,
                    cost_per_million_output: 0.40,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for DeepInfraProvider {
    fn id(&self) -> &str {
        "deepinfra"
    }

    fn name(&self) -> &str {
        "DeepInfra"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }

        let response = self
            .client
            .post(DEEPINFRA_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(error_text));
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        Ok(chat_response)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "stream": true,
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }

        let response = self
            .client
            .post(DEEPINFRA_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(error_text));
        }

        let stream = response
            .bytes_stream()
            .then(|result| async move {
                match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let mut events: Vec<Result<StreamEvent, ProviderError>> = Vec::new();

                        for line in text.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    events.push(Ok(StreamEvent::Done));
                                    continue;
                                }

                                if let Some(event) = crate::stream::parse_openai_sse(data) {
                                    events.push(Ok(event));
                                }
                            }
                        }

                        events
                    }
                    Err(e) => vec![Err(ProviderError::StreamError(e.to_string()))],
                }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 外部 HTTP を呼ばず、ダミーキーだけで検証できる純粋ロジックに限定する。
    fn provider() -> DeepInfraProvider {
        DeepInfraProvider::new("dummy-deepinfra-key-for-tests")
    }

    #[test]
    fn new_accepts_dummy_key_and_builds_identical_catalog() {
        let a = DeepInfraProvider::new("dummy-deepinfra-key-a");
        let b = DeepInfraProvider::new("dummy-deepinfra-key-b");

        assert_eq!(a.id(), b.id());
        assert_eq!(a.name(), b.name());

        let models_a = a.models();
        let models_b = b.models();
        let ids_a: Vec<&str> = models_a.iter().map(|m| m.id.as_str()).collect();
        let ids_b: Vec<&str> = models_b.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids_a, ids_b);
        assert_eq!(ids_a.len(), 4);
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "deepinfra");
        assert_eq!(provider.name(), "DeepInfra");
    }

    #[test]
    fn models_use_deepinfra_specific_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 4);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "meta-llama/Meta-Llama-3-70B-Instruct",
                "meta-llama/Llama-3.3-70B-Instruct",
                "mistralai/Mistral-Small-24B-Instruct-2501",
                "Qwen/Qwen2.5-72B-Instruct",
            ]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "deepinfra" && !m.name.is_empty()));
        // DeepInfra のモデル名は "(DeepInfra)" サフィックスを持つ。
        assert!(models.iter().all(|m| m.name.ends_with("(DeepInfra)")));
    }

    #[test]
    fn get_model_returns_known_deepinfra_fields() {
        let provider = provider();
        let model = provider
            .get_model("meta-llama/Meta-Llama-3-70B-Instruct")
            .expect("known model should be present");

        assert_eq!(model.id, "meta-llama/Meta-Llama-3-70B-Instruct");
        assert_eq!(model.name, "Llama 3 70B (DeepInfra)");
        assert_eq!(model.provider, "deepinfra");
        assert_eq!(model.context_window, 8192);
        assert_eq!(model.max_output_tokens, 4096);
        assert_eq!(model.cost_per_million_input, 0.59);
        assert_eq!(model.cost_per_million_output, 0.79);
    }

    #[test]
    fn get_model_returns_deepinfra_specific_costs_and_limits() {
        let provider = provider();

        let llama33 = provider
            .get_model("meta-llama/Llama-3.3-70B-Instruct")
            .expect("llama 3.3 should be present");
        assert_eq!(llama33.name, "Llama 3.3 70B (DeepInfra)");
        assert_eq!(llama33.context_window, 128_000);
        assert_eq!(llama33.max_output_tokens, 8192);
        assert_eq!(llama33.cost_per_million_input, 0.35);
        assert_eq!(llama33.cost_per_million_output, 0.40);

        let mistral = provider
            .get_model("mistralai/Mistral-Small-24B-Instruct-2501")
            .expect("mistral small should be present");
        assert_eq!(mistral.name, "Mistral Small 24B (DeepInfra)");
        assert_eq!(mistral.context_window, 32_768);
        assert_eq!(mistral.max_output_tokens, 8192);
        // DeepInfra の Mistral Small 24B は入出力コストが同額。
        assert_eq!(mistral.cost_per_million_input, 0.10);
        assert_eq!(mistral.cost_per_million_output, 0.10);

        let qwen = provider
            .get_model("Qwen/Qwen2.5-72B-Instruct")
            .expect("qwen should be present");
        assert_eq!(qwen.name, "Qwen 2.5 72B (DeepInfra)");
        assert_eq!(qwen.context_window, 32_768);
        assert_eq!(qwen.max_output_tokens, 8192);
        assert_eq!(qwen.cost_per_million_input, 0.35);
        assert_eq!(qwen.cost_per_million_output, 0.40);
    }

    #[test]
    fn every_deepinfra_model_supports_tools_without_vision() {
        let provider = provider();

        // DeepInfra の全モデルは tools 対応・vision 非対応（兄弟プロバイダとの差別点）。
        for model in provider.models() {
            assert!(
                !model.supports_vision,
                "{} should not support vision",
                model.id
            );
            assert!(model.supports_tools, "{} should support tools", model.id);
        }
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider
            .get_model("meta-llama/Meta-Llama-3-70B-Instruct-unknown")
            .is_none());
        assert!(provider.get_model("meta-llama/").is_none());
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
