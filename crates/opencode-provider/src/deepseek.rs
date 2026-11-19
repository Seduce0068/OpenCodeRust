use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::{HashMap, VecDeque};

use crate::{
    ChatRequest, ChatResponse, ModelInfo, Provider, ProviderError, StreamEvent, StreamResult,
};

const DEEPSEEK_API_URL: &str = "https://api.deepseek.com/chat/completions";

/// 構造化エラーへ渡すレスポンスヘッダから機微情報を除外する。
///
/// 実装は [`crate::provider::sanitize_response_headers`] へ一本化した薄いラッパ。
/// allowlist + 機微ヘッダ除外リスト和集合による 2 段防御はそちらを参照。
fn response_error_headers(response: &reqwest::Response) -> HashMap<String, String> {
    crate::provider::sanitize_response_headers(response.headers())
}

#[derive(Debug)]
pub struct DeepSeekProvider {
    client: Client,
    api_key: String,
    models: Vec<ModelInfo>,
}

impl DeepSeekProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
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
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let response_headers = response_error_headers(&response);
            let body = response.text().await.unwrap_or_default();
            let body_preview = crate::openai::error_body_preview(&body, 500);
            let message = format!("{}: {}", status, body_preview);
            return Err(ProviderError::api_error_with_headers(
                status.as_u16(),
                message,
                Some(response_headers),
                Some(body_preview),
            ));
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
            .post(DEEPSEEK_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&stream_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let response_headers = response_error_headers(&response);
            let body = response.text().await.unwrap_or_default();
            let body_preview = crate::openai::error_body_preview(&body, 500);
            let message = format!("{}: {}", status, body_preview);
            return Err(ProviderError::api_error_with_headers(
                status.as_u16(),
                message,
                Some(response_headers),
                Some(body_preview),
            ));
        }

        // bytes_stream() のチャンクは SSE 行の途中（JSON や UTF-8 文字の途中を含む）
        // で終わり得るため、SseLineBuffer に生バイトのまま積み、改行が確定した
        // 完全な行だけをデコードしてパーサへ渡す。イベントは行の順番どおりに
        // pending へ積み、1 つずつ放出する。
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
                            for line in buffer.push(bytes.as_ref()) {
                                push_deepseek_sse_line(&line, &mut pending);
                            }
                        }
                        Some(Err(err)) => return Err(ProviderError::StreamError(err.to_string())),
                        None => {
                            exhausted = true;
                            for line in buffer.flush() {
                                push_deepseek_sse_line(&line, &mut pending);
                            }
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// 完全な SSE 行をパースし、生成されたイベントを `pending` へ積む。
///
/// `data: ` で始まらない行（イベント名や空行）は従来どおり無視する。
/// 1 行から複数のイベントが得られる場合は順番を保ったまま積む。
fn push_deepseek_sse_line(line: &str, pending: &mut VecDeque<StreamEvent>) {
    if let Some(data) = line.strip_prefix("data: ") {
        if let Some(event) = crate::stream::parse_openai_sse(data) {
            pending.push_back(event);
        }
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

    #[test]
    fn sse_buffer_reassembles_split_line_and_multibyte_text() {
        let mut buffer = crate::stream::SseLineBuffer::new();
        let mut pending = VecDeque::new();

        let text = "こんにちは";
        let text_bytes = text.as_bytes();
        // "こ" の UTF-8 先頭 1 バイト目 (0xE3) の直後でチャンクを切る。
        assert_eq!(text_bytes[0], 0xE3);

        let mut first = b"event: message\ndata: {\"choices\":[{\"delta\":{\"content\":\"".to_vec();
        first.extend_from_slice(&text_bytes[..1]);
        for line in buffer.push(&first) {
            push_deepseek_sse_line(&line, &mut pending);
        }
        assert!(pending.is_empty(), "行が確定する前はイベントを出さない");

        let mut second = text_bytes[1..].to_vec();
        second.extend_from_slice(b"\"}}]}\n\ndata: [DONE]\n\n");
        for line in buffer.push(&second) {
            push_deepseek_sse_line(&line, &mut pending);
        }

        let events: Vec<StreamEvent> = pending.into_iter().collect();
        assert_eq!(events.len(), 2, "イベントは行の順番どおり: {events:?}");
        assert!(matches!(&events[0], StreamEvent::TextDelta(delta) if delta == text));
        assert!(matches!(&events[1], StreamEvent::Done));
    }
}
