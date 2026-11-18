use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteRTConfig {
    pub id: String,
    pub name: String,
    pub model_path: String,
    pub backend: String, // "npu", "gpu", "cpu"
}

pub struct LiteRTProvider {
    config: LiteRTConfig,
    models: HashMap<String, ModelInfo>,
}

impl LiteRTProvider {
    pub fn new(config: LiteRTConfig) -> Self {
        let mut models = HashMap::new();
        let model_id = "gemma-4-e2b".to_string();
        models.insert(
            model_id.clone(),
            ModelInfo {
                id: model_id,
                name: "Gemma 4 E2B".to_string(),
                provider: config.id.clone(),
                context_window: 262144, // 256k
                max_output_tokens: 4096,
                supports_vision: false,
                supports_tools: true,
                cost_per_million_input: 0.0,
                cost_per_million_output: 0.0,
            },
        );
        Self { config, models }
    }
}

#[async_trait]
impl Provider for LiteRTProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.values().cloned().collect()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.get(id)
    }

    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        // TODO: Implement the LiteRT-LM C++ FFI call. Until then the provider is
        // intentionally not registered by bootstrap; keep the failure explicit.
        Err(ProviderError::ApiError(
            "LiteRT provider is not implemented: the native LiteRT-LM FFI is not linked. \
             LITERT_MODEL_PATH is configuration-only and no model can be served."
                .to_string(),
        ))
    }

    async fn chat_stream(&self, _request: ChatRequest) -> Result<StreamResult, ProviderError> {
        Err(ProviderError::ApiError(
            "LiteRT provider is not implemented: streaming is unavailable until the native \
             LiteRT-LM FFI is linked."
                .to_string(),
        ))
    }
}
