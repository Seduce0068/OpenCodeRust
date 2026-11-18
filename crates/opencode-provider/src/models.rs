use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub const MODELS_DEV_URL: &str = "https://models.dev";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub context_over_200k: Option<Box<ModelCost>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLimit {
    pub context: u64,
    #[serde(default)]
    pub input: Option<u64>,
    pub output: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelModalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProvider {
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelInterleaved {
    Bool(bool),
    Field { field: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub interleaved: Option<ModelInterleaved>,
    #[serde(default)]
    pub cost: Option<ModelCost>,
    pub limit: ModelLimit,
    #[serde(default)]
    pub modalities: Option<ModelModalities>,
    #[serde(default)]
    pub experimental: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub options: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub provider: Option<ModelProvider>,
    #[serde(default)]
    pub variants: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default)]
    pub api: Option<String>,
    pub name: String,
    pub env: Vec<String>,
    pub id: String,
    #[serde(default)]
    pub npm: Option<String>,
    pub models: HashMap<String, ModelInfo>,
}

pub type ModelsData = HashMap<String, ProviderInfo>;

pub struct ModelsRegistry {
    data: Arc<RwLock<Option<ModelsData>>>,
    cache_path: PathBuf,
}

impl ModelsRegistry {
    pub fn new(cache_path: PathBuf) -> Self {
        Self {
            data: Arc::new(RwLock::new(None)),
            cache_path,
        }
    }

    pub async fn get(&self) -> ModelsData {
        let data = self.data.read().await;
        if let Some(ref d) = *data {
            return d.clone();
        }
        drop(data);

        self.load().await
    }

    async fn load(&self) -> ModelsData {
        if let Ok(content) = tokio::fs::read_to_string(&self.cache_path).await {
            if let Ok(parsed) = serde_json::from_str::<ModelsData>(&content) {
                let mut data = self.data.write().await;
                *data = Some(parsed.clone());
                return parsed;
            }
        }

        self.fetch().await
    }

    async fn fetch(&self) -> ModelsData {
        let url = format!("{}/api.json", MODELS_DEV_URL);

        match reqwest::Client::new()
            .get(&url)
            .header("User-Agent", "opencode-rust")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                if let Ok(text) = response.text().await {
                    if let Ok(parsed) = serde_json::from_str::<ModelsData>(&text) {
                        let _ = tokio::fs::write(&self.cache_path, &text).await;
                        let mut data = self.data.write().await;
                        *data = Some(parsed.clone());
                        return parsed;
                    }
                }
            }
            _ => {}
        }

        HashMap::new()
    }

    pub async fn refresh(&self) {
        self.fetch().await;
    }

    pub async fn get_provider(&self, provider_id: &str) -> Option<ProviderInfo> {
        let data = self.get().await;
        data.get(provider_id).cloned()
    }

    pub async fn get_model(&self, provider_id: &str, model_id: &str) -> Option<ModelInfo> {
        let data = self.get().await;
        data.get(provider_id)
            .and_then(|p| p.models.get(model_id).cloned())
    }

    pub async fn list_models_for_provider(&self, provider_id: &str) -> Vec<ModelInfo> {
        let data = self.get().await;
        data.get(provider_id)
            .map(|p| p.models.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Apply custom loaders and filtering to the loaded models data
    pub async fn get_with_customization(&self, enable_experimental: bool) -> ModelsData {
        let mut data = self.get().await;
        crate::bootstrap::apply_custom_loaders(&mut data);
        crate::bootstrap::filter_models_by_status(&mut data, enable_experimental);
        data
    }
}

impl Default for ModelsRegistry {
    fn default() -> Self {
        let cache_path = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("opencode")
            .join("models.json");
        Self::new(cache_path)
    }
}

pub fn default_model_limits() -> (u64, u64) {
    (4096, 128000)
}

pub fn get_model_context_limit(model_id: &str) -> u64 {
    let lower = model_id.to_lowercase();

    if lower.contains("gpt-4") || lower.contains("gpt-4") {
        if lower.contains("32k") {
            return 32768;
        }
        if lower.contains("128k") || lower.contains("turbo") {
            return 128000;
        }
        return 8192;
    }

    if lower.contains("claude-3") || lower.contains("claude-3") {
        return 200000;
    }

    if lower.contains("claude-2") {
        return 100000;
    }

    if lower.contains("gemini") {
        if lower.contains("pro") || lower.contains("ultra") {
            return 1000000;
        }
        return 32000;
    }

    if lower.contains("llama") {
        return 128000;
    }

    128000
}

pub fn supports_vision(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();

    lower.contains("vision")
        || lower.contains("gpt-4")
        || lower.contains("claude-3")
        || lower.contains("gemini")
        || lower.contains("qwen-vl")
}

pub fn supports_function_calling(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();

    !lower.contains("embedding") && !lower.contains("whisper") && !lower.contains("tts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_limits_are_stable() {
        assert_eq!(default_model_limits(), (4096, 128000));
    }

    #[test]
    fn context_limit_gpt4_variants() {
        assert_eq!(get_model_context_limit("gpt-4-32k"), 32768);
        assert_eq!(get_model_context_limit("gpt-4-turbo"), 128000);
        assert_eq!(get_model_context_limit("gpt-4"), 8192);
        assert_eq!(get_model_context_limit("GPT-4-32K"), 32768);
    }

    #[test]
    fn context_limit_other_families() {
        assert_eq!(get_model_context_limit("claude-3-opus"), 200000);
        assert_eq!(get_model_context_limit("claude-2"), 100000);
        assert_eq!(get_model_context_limit("gemini-pro"), 1_000_000);
        assert_eq!(get_model_context_limit("gemini"), 32000);
        assert_eq!(get_model_context_limit("llama-3"), 128000);
    }

    #[test]
    fn context_limit_unknown_defaults() {
        assert_eq!(get_model_context_limit("unknown-model"), 128000);
    }

    #[test]
    fn vision_support_by_model_name() {
        assert!(supports_vision("gpt-4"));
        assert!(supports_vision("claude-3-opus"));
        assert!(supports_vision("gemini-pro"));
        assert!(supports_vision("qwen-vl"));
        assert!(supports_vision("my-vision-model"));
        assert!(!supports_vision("llama-3"));
    }

    #[test]
    fn function_calling_excludes_audio_and_embedding() {
        assert!(supports_function_calling("gpt-4"));
        assert!(!supports_function_calling("text-embedding-ada"));
        assert!(!supports_function_calling("whisper-1"));
        assert!(!supports_function_calling("TTS-1"));
    }

    #[tokio::test]
    async fn registry_loads_from_local_cache() {
        let dir =
            std::env::temp_dir().join(format!("opencode-models-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("models.json");
        let json = serde_json::json!({
            "anthropic": {
                "name": "Anthropic",
                "env": ["ANTHROPIC_API_KEY"],
                "id": "anthropic",
                "models": {
                    "claude-3": {
                        "id": "claude-3",
                        "name": "Claude 3",
                        "limit": { "context": 200000, "output": 4096 }
                    }
                }
            }
        });
        std::fs::write(&cache, json.to_string()).unwrap();
        let registry = ModelsRegistry::new(cache);

        let provider = registry.get_provider("anthropic").await.unwrap();
        assert_eq!(provider.name, "Anthropic");
        assert_eq!(provider.models.len(), 1);
        let model = registry.get_model("anthropic", "claude-3").await.unwrap();
        assert_eq!(model.limit.context, 200000);
        assert_eq!(
            registry.list_models_for_provider("anthropic").await.len(),
            1
        );
        assert!(registry.get_provider("missing").await.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn model_info_deserialization_applies_defaults() {
        let json = r#"{
            "id": "dummy-model",
            "name": "Dummy Model",
            "limit": { "context": 1000, "output": 100 }
        }"#;
        let model: ModelInfo = serde_json::from_str(json).expect("dummy model should parse");

        assert_eq!(model.id, "dummy-model");
        assert_eq!(model.limit.context, 1000);
        assert_eq!(model.limit.output, 100);
        assert!(model.limit.input.is_none());
        assert!(!model.attachment);
        assert!(!model.reasoning);
        assert!(!model.temperature);
        assert!(!model.tool_call);
        assert!(model.interleaved.is_none());
        assert!(model.cost.is_none());
        assert!(model.modalities.is_none());
        assert!(model.experimental.is_none());
        assert!(model.status.is_none());
        assert!(model.options.is_empty());
        assert!(model.headers.is_none());
        assert!(model.provider.is_none());
        assert!(model.variants.is_none());
    }

    #[test]
    fn model_interleaved_accepts_bool_and_field() {
        let flag: ModelInterleaved =
            serde_json::from_str("true").expect("boolean interleaved should parse");
        assert!(matches!(flag, ModelInterleaved::Bool(true)));

        let field: ModelInterleaved = serde_json::from_str(r#"{"field":"reasoning"}"#)
            .expect("field interleaved should parse");
        match field {
            ModelInterleaved::Field { field } => assert_eq!(field, "reasoning"),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn model_cost_keeps_optional_cache_and_over_200k() {
        let json = r#"{
            "input": 1.0,
            "output": 2.0,
            "cache_read": 0.1,
            "cache_write": 0.2,
            "context_over_200k": { "input": 3.0, "output": 4.0 }
        }"#;
        let cost: ModelCost = serde_json::from_str(json).expect("dummy cost should parse");

        assert_eq!(cost.input, 1.0);
        assert_eq!(cost.output, 2.0);
        assert_eq!(cost.cache_read, Some(0.1));
        assert_eq!(cost.cache_write, Some(0.2));
        let over = cost
            .context_over_200k
            .expect("over-200k cost should be preserved");
        assert_eq!(over.input, 3.0);
        assert_eq!(over.output, 4.0);
        assert!(over.cache_read.is_none());
        assert!(over.cache_write.is_none());
        assert!(over.context_over_200k.is_none());
    }
}
