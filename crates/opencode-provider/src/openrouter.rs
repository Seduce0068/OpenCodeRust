use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const OPENROUTER_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    pub api_key: String,
    pub base_url: Option<String>,
    pub site_url: Option<String>,
    pub site_name: Option<String>,
}

#[derive(Debug)]
pub struct OpenRouterProvider {
    client: Client,
    config: OpenRouterConfig,
    models: Vec<ModelInfo>,
}

impl OpenRouterProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_config(OpenRouterConfig {
            api_key: api_key.into(),
            base_url: None,
            site_url: None,
            site_name: None,
        })
    }

    pub fn with_config(config: OpenRouterConfig) -> Self {
        let models = vec![
            ModelInfo {
                id: "anthropic/claude-sonnet-4".to_string(),
                name: "Claude Sonnet 4 (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 200000,
                max_output_tokens: 16000,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 3.0,
                cost_per_million_output: 15.0,
            },
            ModelInfo {
                id: "anthropic/claude-3.5-sonnet".to_string(),
                name: "Claude 3.5 Sonnet (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 3.0,
                cost_per_million_output: 15.0,
            },
            ModelInfo {
                id: "openai/gpt-4o".to_string(),
                name: "GPT-4o (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 128000,
                max_output_tokens: 16384,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 2.5,
                cost_per_million_output: 10.0,
            },
            ModelInfo {
                id: "openai/gpt-4o-mini".to_string(),
                name: "GPT-4o Mini (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 128000,
                max_output_tokens: 16384,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.15,
                cost_per_million_output: 0.6,
            },
            ModelInfo {
                id: "google/gemini-2.5-pro-preview".to_string(),
                name: "Gemini 2.5 Pro (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 1000000,
                max_output_tokens: 65536,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 1.25,
                cost_per_million_output: 10.0,
            },
            ModelInfo {
                id: "google/gemini-2.0-flash-001".to_string(),
                name: "Gemini 2.0 Flash (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 1000000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.1,
                cost_per_million_output: 0.4,
            },
            ModelInfo {
                id: "deepseek/deepseek-chat".to_string(),
                name: "DeepSeek Chat (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 64000,
                max_output_tokens: 8192,
                supports_vision: false,
                supports_tools: true,
                cost_per_million_input: 0.14,
                cost_per_million_output: 0.28,
            },
            ModelInfo {
                id: "meta-llama/llama-3.3-70b-instruct".to_string(),
                name: "Llama 3.3 70B (OpenRouter)".to_string(),
                provider: "openrouter".to_string(),
                context_window: 131072,
                max_output_tokens: 8192,
                supports_vision: false,
                supports_tools: true,
                cost_per_million_input: 0.35,
                cost_per_million_output: 0.4,
            },
        ];

        Self {
            client: Client::new(),
            config,
            models,
        }
    }

    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", self.config.api_key).parse().unwrap(),
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());

        let site_url = self
            .config
            .site_url
            .as_deref()
            .unwrap_or("https://opencode.ai/");
        headers.insert("HTTP-Referer", site_url.parse().unwrap());

        let site_name = self.config.site_name.as_deref().unwrap_or("opencode");
        headers.insert("X-Title", site_name.parse().unwrap());

        headers
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> &str {
        "openrouter"
    }

    fn name(&self) -> &str {
        "OpenRouter"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let url = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(OPENROUTER_API_URL);

        let response = self
            .client
            .post(url)
            .headers(self.build_headers())
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
        let url = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(OPENROUTER_API_URL);

        let mut stream_request = request;
        stream_request.stream = Some(true);

        let response = self
            .client
            .post(url)
            .headers(self.build_headers())
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

    // 外部 HTTP を呼ばず、ダミー値だけで検証できる純粋ロジックに限定する。
    // 実 API キー・実サイト URL は一切使用しない。
    fn provider() -> OpenRouterProvider {
        OpenRouterProvider::new("dummy-openrouter-key-for-tests")
    }

    fn configured_provider() -> OpenRouterProvider {
        OpenRouterProvider::with_config(OpenRouterConfig {
            api_key: "dummy-openrouter-key-for-tests".to_string(),
            base_url: Some("https://dummy-openrouter.invalid/v1/chat/completions".to_string()),
            site_url: Some("https://dummy-site.invalid/".to_string()),
            site_name: Some("dummy-site-name".to_string()),
        })
    }

    fn header_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
        headers
            .get(name)
            .expect("expected header should be present")
            .to_str()
            .expect("header value should be UTF-8")
            .to_string()
    }

    #[test]
    fn id_and_name_identify_openrouter() {
        let provider = provider();
        assert_eq!(provider.id(), "openrouter");
        assert_eq!(provider.name(), "OpenRouter");
    }

    #[test]
    fn new_stores_dummy_key_and_default_openrouter_config() {
        let provider = provider();

        assert_eq!(provider.config.api_key, "dummy-openrouter-key-for-tests");
        assert!(provider.config.base_url.is_none());
        assert!(provider.config.site_url.is_none());
        assert!(provider.config.site_name.is_none());
    }

    #[test]
    fn with_config_keeps_openrouter_specific_options() {
        let provider = configured_provider();

        assert_eq!(
            provider.config.base_url.as_deref(),
            Some("https://dummy-openrouter.invalid/v1/chat/completions")
        );
        assert_eq!(
            provider.config.site_url.as_deref(),
            Some("https://dummy-site.invalid/")
        );
        assert_eq!(
            provider.config.site_name.as_deref(),
            Some("dummy-site-name")
        );
    }

    #[test]
    fn models_expose_namespaced_openrouter_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 8);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "anthropic/claude-sonnet-4",
                "anthropic/claude-3.5-sonnet",
                "openai/gpt-4o",
                "openai/gpt-4o-mini",
                "google/gemini-2.5-pro-preview",
                "google/gemini-2.0-flash-001",
                "deepseek/deepseek-chat",
                "meta-llama/llama-3.3-70b-instruct",
            ]
        );
        assert!(models.iter().all(|m| {
            m.provider == "openrouter" && m.id.contains('/') && m.name.ends_with(" (OpenRouter)")
        }));
    }

    #[test]
    fn get_model_returns_anthropic_premium_pricing_and_limits() {
        let provider = provider();
        let model = provider
            .get_model("anthropic/claude-sonnet-4")
            .expect("claude-sonnet-4 should be present");

        assert_eq!(model.id, "anthropic/claude-sonnet-4");
        assert_eq!(model.name, "Claude Sonnet 4 (OpenRouter)");
        assert_eq!(model.provider, "openrouter");
        assert_eq!(model.context_window, 200_000);
        assert_eq!(model.max_output_tokens, 16_000);
        assert!(model.supports_vision);
        assert!(model.supports_tools);
        assert_eq!(model.cost_per_million_input, 3.0);
        assert_eq!(model.cost_per_million_output, 15.0);
    }

    #[test]
    fn get_model_distinguishes_vision_capability_by_model_family() {
        let provider = provider();

        let gpt_4o = provider
            .get_model("openai/gpt-4o")
            .expect("gpt-4o should be present");
        assert!(gpt_4o.supports_vision);
        assert!(gpt_4o.supports_tools);

        let deepseek = provider
            .get_model("deepseek/deepseek-chat")
            .expect("deepseek-chat should be present");
        assert!(!deepseek.supports_vision);
        assert!(deepseek.supports_tools);

        let llama = provider
            .get_model("meta-llama/llama-3.3-70b-instruct")
            .expect("llama should be present");
        assert!(!llama.supports_vision);
        assert!(llama.supports_tools);
    }

    #[test]
    fn get_model_reflects_low_cost_openrouter_models() {
        let provider = provider();

        let mini = provider
            .get_model("openai/gpt-4o-mini")
            .expect("gpt-4o-mini should be present");
        assert_eq!(mini.cost_per_million_input, 0.15);
        assert_eq!(mini.cost_per_million_output, 0.6);

        let flash = provider
            .get_model("google/gemini-2.0-flash-001")
            .expect("gemini-2.0-flash-001 should be present");
        assert_eq!(flash.cost_per_million_input, 0.1);
        assert_eq!(flash.cost_per_million_output, 0.4);
        assert_eq!(flash.context_window, 1_000_000);
        assert_eq!(flash.max_output_tokens, 8192);
    }

    #[test]
    fn get_model_returns_none_for_unknown_namespaced_id() {
        let provider = provider();
        assert!(provider.get_model("openai/gpt-4o-unknown").is_none());
        assert!(provider.get_model("gpt-4o").is_none());
        assert!(provider.get_model("").is_none());
    }

    #[test]
    fn models_returns_owned_snapshot_of_full_catalog() {
        let provider = provider();
        let mut models = provider.models();
        models.clear();

        assert!(models.is_empty());
        assert_eq!(provider.models().len(), 8);
    }

    #[test]
    fn build_headers_are_openrouter_specific_with_dummy_values() {
        let default_headers = provider().build_headers();
        assert_eq!(
            header_value(&default_headers, "authorization"),
            "Bearer dummy-openrouter-key-for-tests"
        );
        assert_eq!(
            header_value(&default_headers, "content-type"),
            "application/json"
        );
        assert_eq!(
            header_value(&default_headers, "http-referer"),
            "https://opencode.ai/"
        );
        assert_eq!(header_value(&default_headers, "x-title"), "opencode");

        let custom_headers = configured_provider().build_headers();
        assert_eq!(
            header_value(&custom_headers, "http-referer"),
            "https://dummy-site.invalid/"
        );
        assert_eq!(header_value(&custom_headers, "x-title"), "dummy-site-name");
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
