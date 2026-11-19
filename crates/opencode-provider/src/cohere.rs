use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use std::collections::VecDeque;

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
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
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
            .map_err(|e| ProviderError::network_error_with_source(e.to_string(), e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::api_error_from_status(
                status.as_u16(),
                format!(
                    "{}: {}",
                    status,
                    crate::openai::error_body_preview(&body, 500)
                ),
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
            .post(COHERE_API_URL)
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
            return Err(ProviderError::api_error_from_status(
                status.as_u16(),
                format!(
                    "{}: {}",
                    status,
                    crate::openai::error_body_preview(&body, 500)
                ),
            ));
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

    /// 回帰テスト: 500 応答を平坦化せず構造化ステータスとして保持し、
    /// リトライ可と判定されることを固定する。
    #[test]
    fn http_500_response_is_retryable_and_body_is_previewed() {
        use crate::retry::IsRetryable;

        // chat / chat_stream と同一の構築経由（共通ヘルパ + 本文プレビュー）。
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        let body = "Internal Server Error";
        let error = ProviderError::api_error_from_status(
            status.as_u16(),
            format!(
                "{}: {}",
                status,
                crate::openai::error_body_preview(body, 500)
            ),
        );
        assert_eq!(error.status_code(), Some(500));
        assert!(
            error.is_retryable().is_some(),
            "500 レスポンスはリトライ可能でなければならない: {error:?}"
        );

        // 本文全体をエラーへ埋め込まないことの回帰防止。
        let long_body = "x".repeat(1000);
        let long_error = ProviderError::api_error_from_status(
            status.as_u16(),
            format!(
                "{}: {}",
                status,
                crate::openai::error_body_preview(&long_body, 500)
            ),
        );
        let message = long_error.to_string();
        assert!(message.contains("..."), "長い本文は省略されるべき");
        assert!(
            !message.contains(long_body.as_str()),
            "本文全体をエラーへ露出してはならない"
        );
    }
}
