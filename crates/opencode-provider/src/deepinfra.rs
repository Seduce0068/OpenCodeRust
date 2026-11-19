use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

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
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
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

    #[test]
    fn sse_line_buffer_reassembles_split_multibyte_and_multiple_lines() {
        let mut buffer = crate::stream::SseLineBuffer::new();

        let first = serde_json::json!({
            "choices": [{ "delta": { "content": "こんにちは" } }]
        })
        .to_string();
        let second = serde_json::json!({
            "choices": [{ "delta": { "content": "world" } }]
        })
        .to_string();
        let payload = format!("data: {first}\n\ndata: {second}\n\n");

        // "こ" (E3 81 93) の 1 バイト目直後でチャンクを分割する。
        let split = payload.find("こんにちは").expect("text should be present") + 1;
        let bytes = payload.as_bytes();
        assert!(buffer.push(&bytes[..split]).is_empty());
        let lines = buffer.push(&bytes[split..]);

        let mut pending = VecDeque::new();
        push_openai_sse_lines(lines, &mut pending);
        let events: Vec<StreamEvent> = pending.into_iter().collect();

        assert_eq!(events.len(), 2, "1 チャンクの複数行を全て順序どおり返す");
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "こんにちは"));
        assert!(matches!(&events[1], StreamEvent::TextDelta(t) if t == "world"));
    }

    #[test]
    fn sse_buffer_flushes_trailing_data_line_without_newline() {
        let mut buffer = crate::stream::SseLineBuffer::new();
        let mut pending = VecDeque::new();

        push_openai_sse_lines(
            buffer.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n"),
            &mut pending,
        );
        // 改行のない終端行は push では確定しない。
        assert!(buffer.push(b"data: [DONE]").is_empty());
        assert_eq!(pending.len(), 1);

        push_openai_sse_lines(buffer.flush(), &mut pending);

        assert_eq!(pending.len(), 2);
        assert!(matches!(
            pending.pop_front(),
            Some(StreamEvent::TextDelta(ref text)) if text == "hi"
        ));
        assert!(matches!(pending.pop_front(), Some(StreamEvent::Done)));
    }

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
