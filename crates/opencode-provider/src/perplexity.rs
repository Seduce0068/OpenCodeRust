use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const PERPLEXITY_API_URL: &str = "https://api.perplexity.ai/chat/completions";

#[derive(Debug)]
pub struct PerplexityProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl PerplexityProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "sonar-pro".to_string(),
                    name: "Sonar Pro".to_string(),
                    provider: "perplexity".to_string(),
                    context_window: 200000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 3.0,
                    cost_per_million_output: 15.0,
                },
                ModelInfo {
                    id: "sonar".to_string(),
                    name: "Sonar".to_string(),
                    provider: "perplexity".to_string(),
                    context_window: 127000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 1.0,
                    cost_per_million_output: 1.0,
                },
                ModelInfo {
                    id: "sonar-reasoning-pro".to_string(),
                    name: "Sonar Reasoning Pro".to_string(),
                    provider: "perplexity".to_string(),
                    context_window: 127000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 2.0,
                    cost_per_million_output: 8.0,
                },
                ModelInfo {
                    id: "sonar-reasoning".to_string(),
                    name: "Sonar Reasoning".to_string(),
                    provider: "perplexity".to_string(),
                    context_window: 127000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: false,
                    cost_per_million_input: 1.0,
                    cost_per_million_output: 5.0,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for PerplexityProvider {
    fn id(&self) -> &str {
        "perplexity"
    }
    fn name(&self) -> &str {
        "Perplexity"
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
            .post(PERPLEXITY_API_URL)
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
            .post(PERPLEXITY_API_URL)
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
    fn provider() -> PerplexityProvider {
        PerplexityProvider::new("dummy-perplexity-key-for-tests")
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "perplexity");
        assert_eq!(provider.name(), "Perplexity");
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
                "sonar-pro",
                "sonar",
                "sonar-reasoning-pro",
                "sonar-reasoning",
            ]
        );
        assert!(models
            .iter()
            .all(|m| m.provider == "perplexity" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("sonar-pro")
            .expect("known model should be present");

        assert_eq!(model.id, "sonar-pro");
        assert_eq!(model.name, "Sonar Pro");
        assert_eq!(model.provider, "perplexity");
        assert_eq!(model.context_window, 200_000);
        assert_eq!(model.max_output_tokens, 8192);
        assert!(!model.supports_vision);
        assert!(!model.supports_tools);
        assert_eq!(model.cost_per_million_input, 3.0);
        assert_eq!(model.cost_per_million_output, 15.0);
    }

    #[test]
    fn all_models_have_no_vision_or_tools_support() {
        // Perplexity 固有の特徴: 全モデルが vision / tools 非対応。
        let provider = provider();
        assert!(provider
            .models()
            .iter()
            .all(|m| !m.supports_vision && !m.supports_tools));
    }

    #[test]
    fn get_model_distinguishes_context_window_and_costs() {
        let provider = provider();

        let pro = provider
            .get_model("sonar-pro")
            .expect("sonar-pro should be present");
        let sonar = provider
            .get_model("sonar")
            .expect("sonar should be present");
        let reasoning_pro = provider
            .get_model("sonar-reasoning-pro")
            .expect("sonar-reasoning-pro should be present");
        let reasoning = provider
            .get_model("sonar-reasoning")
            .expect("sonar-reasoning should be present");

        // sonar-pro のみが広いコンテキストウィンドウを持つ。
        assert_eq!(pro.context_window, 200_000);
        assert_eq!(sonar.context_window, 127_000);
        assert_eq!(reasoning_pro.context_window, 127_000);
        assert_eq!(reasoning.context_window, 127_000);

        // 全モデルで max_output_tokens は共通。
        assert_eq!(pro.max_output_tokens, 8192);
        assert_eq!(sonar.max_output_tokens, 8192);
        assert_eq!(reasoning_pro.max_output_tokens, 8192);
        assert_eq!(reasoning.max_output_tokens, 8192);

        // コスト体系がモデルごとに異なる。
        assert_eq!(sonar.cost_per_million_input, 1.0);
        assert_eq!(sonar.cost_per_million_output, 1.0);
        assert_eq!(reasoning_pro.cost_per_million_input, 2.0);
        assert_eq!(reasoning_pro.cost_per_million_output, 8.0);
        assert_eq!(reasoning.cost_per_million_input, 1.0);
        assert_eq!(reasoning.cost_per_million_output, 5.0);
        assert_eq!(reasoning_pro.name, "Sonar Reasoning Pro");
        assert_eq!(reasoning.name, "Sonar Reasoning");
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider.get_model("sonar-v9").is_none());
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
