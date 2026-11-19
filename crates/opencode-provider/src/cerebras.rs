use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const CEREBRAS_API_URL: &str = "https://api.cerebras.ai/v1/chat/completions";

#[derive(Debug)]
pub struct CerebrasProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl CerebrasProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            models: vec![
                ModelInfo {
                    id: "llama-3.3-70b".to_string(),
                    name: "Llama 3.3 70B (Cerebras)".to_string(),
                    provider: "cerebras".to_string(),
                    context_window: 128000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.6,
                    cost_per_million_output: 0.6,
                },
                ModelInfo {
                    id: "llama-3.1-8b".to_string(),
                    name: "Llama 3.1 8B (Cerebras)".to_string(),
                    provider: "cerebras".to_string(),
                    context_window: 128000,
                    max_output_tokens: 8192,
                    supports_vision: false,
                    supports_tools: true,
                    cost_per_million_input: 0.1,
                    cost_per_million_output: 0.1,
                },
            ],
        }
    }
}

#[async_trait]
impl Provider for CerebrasProvider {
    fn id(&self) -> &str {
        "cerebras"
    }

    fn name(&self) -> &str {
        "Cerebras"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let url = CEREBRAS_API_URL;

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
            .post(url)
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
        let url = CEREBRAS_API_URL;

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
            .post(url)
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
    fn provider() -> CerebrasProvider {
        CerebrasProvider::new("dummy-cerebras-key-for-tests")
    }

    #[test]
    fn id_and_name_are_stable() {
        let provider = provider();
        assert_eq!(provider.id(), "cerebras");
        assert_eq!(provider.name(), "Cerebras");
    }

    #[test]
    fn models_are_prepopulated_with_expected_ids() {
        let provider = provider();
        let models = provider.models();

        assert_eq!(models.len(), 2);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["llama-3.3-70b", "llama-3.1-8b"]);
        assert!(models
            .iter()
            .all(|m| m.provider == "cerebras" && !m.name.is_empty()));
    }

    #[test]
    fn get_model_returns_known_model_fields() {
        let provider = provider();
        let model = provider
            .get_model("llama-3.3-70b")
            .expect("known model should be present");

        assert_eq!(model.id, "llama-3.3-70b");
        assert_eq!(model.name, "Llama 3.3 70B (Cerebras)");
        assert_eq!(model.provider, "cerebras");
        assert_eq!(model.context_window, 128_000);
        assert_eq!(model.max_output_tokens, 8192);
        assert!(!model.supports_vision);
        assert!(model.supports_tools);
        assert_eq!(model.cost_per_million_input, 0.6);
        assert_eq!(model.cost_per_million_output, 0.6);
    }

    #[test]
    fn get_model_reflects_second_model_and_capabilities() {
        let provider = provider();
        let model = provider
            .get_model("llama-3.1-8b")
            .expect("8b model should be present");

        assert_eq!(model.name, "Llama 3.1 8B (Cerebras)");
        assert_eq!(model.cost_per_million_input, 0.1);
        assert_eq!(model.cost_per_million_output, 0.1);
        // Cerebras の Llama モデルは vision 非対応・tools 対応。
        assert!(!model.supports_vision);
        assert!(model.supports_tools);
    }

    #[test]
    fn get_model_returns_none_for_unknown_id() {
        let provider = provider();
        assert!(provider.get_model("llama-3.3-70b-unknown").is_none());
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
