use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

#[derive(Debug, Clone)]
pub struct AzureConfig {
    pub api_key: String,
    pub endpoint: String,
    pub deployment_name: Option<String>,
    pub api_version: Option<String>,
}

#[derive(Debug)]
pub struct AzureProvider {
    client: Client,
    config: AzureConfig,
    models: Vec<ModelInfo>,
}

impl AzureProvider {
    pub fn new(api_key: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self::with_config(AzureConfig {
            api_key: api_key.into(),
            endpoint: endpoint.into(),
            deployment_name: None,
            api_version: None,
        })
    }

    pub fn with_config(config: AzureConfig) -> Self {
        let models = vec![
            ModelInfo {
                id: "gpt-4o".to_string(),
                name: "GPT-4o (Azure)".to_string(),
                provider: "azure".to_string(),
                context_window: 128000,
                max_output_tokens: 16384,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 2.5,
                cost_per_million_output: 10.0,
            },
            ModelInfo {
                id: "gpt-4o-mini".to_string(),
                name: "GPT-4o Mini (Azure)".to_string(),
                provider: "azure".to_string(),
                context_window: 128000,
                max_output_tokens: 16384,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.15,
                cost_per_million_output: 0.6,
            },
            ModelInfo {
                id: "gpt-4-turbo".to_string(),
                name: "GPT-4 Turbo (Azure)".to_string(),
                provider: "azure".to_string(),
                context_window: 128000,
                max_output_tokens: 4096,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 10.0,
                cost_per_million_output: 30.0,
            },
            ModelInfo {
                id: "gpt-35-turbo".to_string(),
                name: "GPT-3.5 Turbo (Azure)".to_string(),
                provider: "azure".to_string(),
                context_window: 16384,
                max_output_tokens: 4096,
                supports_vision: false,
                supports_tools: true,
                cost_per_million_input: 0.5,
                cost_per_million_output: 1.5,
            },
        ];

        Self {
            client: Client::new(),
            config,
            models,
        }
    }

    fn build_url(&self, model: &str, _stream: bool) -> String {
        let deployment = self.config.deployment_name.as_deref().unwrap_or(model);
        let api_version = self
            .config
            .api_version
            .as_deref()
            .unwrap_or("2024-02-15-preview");

        let endpoint = self.config.endpoint.trim_end_matches('/');

        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            endpoint, deployment, api_version
        )
    }

    fn build_request_body(request: &ChatRequest) -> Result<Value, ProviderError> {
        let mut value = serde_json::to_value(request)
            .map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
        let effort = openai_reasoning_effort(&request.model, request.variant.as_deref());
        if let Some(effort) = effort {
            if let Value::Object(obj) = &mut value {
                obj.insert(
                    "reasoning_effort".to_string(),
                    Value::String(effort.to_string()),
                );
            }
        }
        Ok(value)
    }
}

#[async_trait]
impl Provider for AzureProvider {
    fn id(&self) -> &str {
        "azure"
    }

    fn name(&self) -> &str {
        "Azure OpenAI"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let url = self.build_url(&request.model, false);
        let request_body = Self::build_request_body(&request)?;

        let response = self
            .client
            .post(&url)
            .header("api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&request_body)
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
        let url = self.build_url(&request.model, true);

        let mut stream_request = request;
        stream_request.stream = Some(true);
        let request_body = Self::build_request_body(&stream_request)?;

        let response = self
            .client
            .post(&url)
            .header("api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError(format!("{}: {}", status, body)));
        }

        let stream = response
            .bytes_stream()
            .map(move |chunk_result| match chunk_result {
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

fn openai_reasoning_effort(model_id: &str, variant: Option<&str>) -> Option<&'static str> {
    let variant = variant?.trim().to_ascii_lowercase();
    let model = model_id.to_ascii_lowercase();
    let supports_effort = model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.contains("gpt-5")
        || model.contains("codex");
    if !supports_effort {
        return None;
    }

    match variant.as_str() {
        "none" => Some("none"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "max" | "xhigh" => Some("high"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 外部 HTTP を呼ばず、ダミー値だけで検証できる純粋ロジックに限定する。
    // 実 API キー・実 endpoint は一切使用しない。
    fn provider() -> AzureProvider {
        AzureProvider::new(
            "dummy-azure-key-for-tests",
            "https://dummy-endpoint.invalid",
        )
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "azure");
        assert_eq!(provider.name(), "Azure OpenAI");
    }

    #[test]
    fn new_stores_endpoint_and_dummy_key_with_default_config() {
        let provider = provider();

        assert_eq!(provider.config.api_key, "dummy-azure-key-for-tests");
        assert_eq!(provider.config.endpoint, "https://dummy-endpoint.invalid");
        assert!(provider.config.deployment_name.is_none());
        assert!(provider.config.api_version.is_none());
    }

    #[test]
    fn models_are_prepopulated_with_expected_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 4);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-35-turbo"]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "azure" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("gpt-4o")
            .expect("gpt-4o should be present");

        assert_eq!(model.id, "gpt-4o");
        assert_eq!(model.name, "GPT-4o (Azure)");
        assert_eq!(model.provider, "azure");
        assert_eq!(model.context_window, 128000);
        assert_eq!(model.max_output_tokens, 16384);
        assert_eq!(model.cost_per_million_input, 2.5);
        assert_eq!(model.cost_per_million_output, 10.0);
    }

    #[test]
    fn get_model_distinguishes_capability_flags() {
        let provider = provider();

        let gpt_4o = provider
            .get_model("gpt-4o")
            .expect("gpt-4o should be present");
        assert!(gpt_4o.supports_vision);
        assert!(gpt_4o.supports_tools);

        let gpt_35 = provider
            .get_model("gpt-35-turbo")
            .expect("gpt-35-turbo should be present");
        assert!(!gpt_35.supports_vision);
        assert!(gpt_35.supports_tools);
        assert_eq!(gpt_35.name, "GPT-3.5 Turbo (Azure)");
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider.get_model("gpt-99").is_none());
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

    #[test]
    fn build_url_uses_model_as_deployment_and_default_api_version() {
        let provider = provider();

        assert_eq!(
            provider.build_url("gpt-4o", false),
            "https://dummy-endpoint.invalid/openai/deployments/gpt-4o/chat/completions\
             ?api-version=2024-02-15-preview"
        );
    }

    #[test]
    fn build_url_trims_trailing_slash_from_endpoint() {
        let provider = AzureProvider::new(
            "dummy-azure-key-for-tests",
            "https://dummy-endpoint.invalid/",
        );

        assert_eq!(
            provider.build_url("gpt-4o", true),
            "https://dummy-endpoint.invalid/openai/deployments/gpt-4o/chat/completions\
             ?api-version=2024-02-15-preview"
        );
    }

    #[test]
    fn build_url_honors_explicit_deployment_and_api_version() {
        let provider = AzureProvider::with_config(AzureConfig {
            api_key: "dummy-azure-key-for-tests".to_string(),
            endpoint: "https://dummy-endpoint.invalid".to_string(),
            deployment_name: Some("my-deployment".to_string()),
            api_version: Some("2024-10-01-preview".to_string()),
        });

        assert_eq!(
            provider.build_url("gpt-4o", false),
            "https://dummy-endpoint.invalid/openai/deployments/my-deployment/chat/completions\
             ?api-version=2024-10-01-preview"
        );
    }
}
