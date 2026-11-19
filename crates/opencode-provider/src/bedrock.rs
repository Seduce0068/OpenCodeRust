use async_trait::async_trait;
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

use crate::{
    ChatRequest, ChatResponse, Choice, Content, Message, ModelInfo, Provider, ProviderError, Role,
    StreamEvent, StreamResult, Usage,
};

/// 構造化エラーへ渡すレスポンスヘッダから機微情報を除外する。
///
/// 実装は [`crate::provider::sanitize_response_headers`] へ一本化した薄いラッパ。
/// allowlist + 機微ヘッダ除外リスト和集合による 2 段防御はそちらを参照。
fn response_error_headers(response: &reqwest::Response) -> HashMap<String, String> {
    crate::provider::sanitize_response_headers(response.headers())
}

const BEDROCK_RUNTIME_URL: &str = "https://bedrock-runtime.{region}.amazonaws.com";

#[derive(Debug, Clone)]
pub struct BedrockConfig {
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub endpoint_url: Option<String>,
}

#[derive(Debug)]
pub struct BedrockProvider {
    client: Client,
    config: BedrockConfig,
    models: Vec<ModelInfo>,
}

impl BedrockProvider {
    pub fn new(
        region: impl Into<String>,
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
    ) -> Self {
        Self::with_config(BedrockConfig {
            region: region.into(),
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token: None,
            endpoint_url: None,
        })
    }

    pub fn with_config(config: BedrockConfig) -> Self {
        let models = vec![
            ModelInfo {
                id: "anthropic.claude-sonnet-4-20250514-v1:0".to_string(),
                name: "Claude Sonnet 4 (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 200000,
                max_output_tokens: 16000,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 3.0,
                cost_per_million_output: 15.0,
            },
            ModelInfo {
                id: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
                name: "Claude 3.5 Sonnet v2 (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 3.0,
                cost_per_million_output: 15.0,
            },
            ModelInfo {
                id: "anthropic.claude-3-5-haiku-20241022-v1:0".to_string(),
                name: "Claude 3.5 Haiku (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 1.0,
                cost_per_million_output: 5.0,
            },
            ModelInfo {
                id: "anthropic.claude-3-opus-20240229-v1:0".to_string(),
                name: "Claude 3 Opus (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 200000,
                max_output_tokens: 4096,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 15.0,
                cost_per_million_output: 75.0,
            },
            ModelInfo {
                id: "amazon.nova-pro-v1:0".to_string(),
                name: "Amazon Nova Pro (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 300000,
                max_output_tokens: 4096,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.8,
                cost_per_million_output: 3.2,
            },
            ModelInfo {
                id: "amazon.nova-lite-v1:0".to_string(),
                name: "Amazon Nova Lite (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 300000,
                max_output_tokens: 4096,
                supports_vision: true,
                supports_tools: true,
                cost_per_million_input: 0.06,
                cost_per_million_output: 0.24,
            },
            ModelInfo {
                id: "amazon.nova-micro-v1:0".to_string(),
                name: "Amazon Nova Micro (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 128000,
                max_output_tokens: 4096,
                supports_vision: false,
                supports_tools: true,
                cost_per_million_input: 0.035,
                cost_per_million_output: 0.14,
            },
            ModelInfo {
                id: "meta.llama3-3-70b-instruct-v1:0".to_string(),
                name: "Llama 3.3 70B (Bedrock)".to_string(),
                provider: "amazon-bedrock".to_string(),
                context_window: 128000,
                max_output_tokens: 8192,
                supports_vision: false,
                supports_tools: true,
                cost_per_million_input: 0.72,
                cost_per_million_output: 0.72,
            },
        ];

        Self {
            client: crate::provider::default_http_client().unwrap_or_else(|_| Client::new()),
            config,
            models,
        }
    }

    fn get_endpoint(&self) -> String {
        if let Some(ref url) = self.config.endpoint_url {
            return url.clone();
        }
        BEDROCK_RUNTIME_URL.replace("{region}", &self.config.region)
    }

    fn convert_request(&self, request: ChatRequest) -> BedrockConverseRequest {
        let mut messages = Vec::new();
        let mut system = Vec::new();

        for msg in request.messages {
            match msg.role {
                Role::System => {
                    if let Content::Text(text) = msg.content {
                        system.push(BedrockSystemContent { text });
                    }
                }
                Role::User => {
                    let content = match msg.content {
                        Content::Text(t) => vec![BedrockContentBlock::text(&t)],
                        Content::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| p.text.as_ref().map(|t| BedrockContentBlock::text(t)))
                            .collect(),
                    };
                    messages.push(BedrockMessage {
                        role: "user".to_string(),
                        content,
                    });
                }
                Role::Assistant => {
                    let content = match msg.content {
                        Content::Text(t) => vec![BedrockContentBlock::text(&t)],
                        Content::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| p.text.as_ref().map(|t| BedrockContentBlock::text(t)))
                            .collect(),
                    };
                    messages.push(BedrockMessage {
                        role: "assistant".to_string(),
                        content,
                    });
                }
                Role::Tool => {}
            }
        }

        BedrockConverseRequest {
            messages,
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            inference_config: Some(BedrockInferenceConfig {
                max_tokens: request.max_tokens,
                temperature: request.temperature,
            }),
        }
    }

    async fn sign_request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<reqwest::header::HeaderMap, ProviderError> {
        let mut headers = reqwest::header::HeaderMap::new();

        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let service = "bedrock";
        let region = &self.config.region;

        let body_hash = hex::encode(sha256(body));

        let header_value = |name: &str, value: &str| {
            value
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|e| ProviderError::AuthError(format!("invalid {} header: {}", name, e)))
        };
        headers.insert(
            "Content-Type",
            header_value("Content-Type", "application/json")?,
        );
        headers.insert("X-Amz-Date", header_value("X-Amz-Date", &amz_date)?);
        headers.insert(
            "Host",
            header_value("Host", &format!("bedrock-runtime.{}.amazonaws.com", region))?,
        );

        if let Some(ref token) = self.config.session_token {
            headers.insert(
                "X-Amz-Security-Token",
                header_value("X-Amz-Security-Token", token)?,
            );
        }

        let canonical_request =
            format!(
            "{}\n{}\n\nhost:bedrock-runtime.{}.amazonaws.com\nx-amz-date:{}\n\nhost;x-amz-date\n{}",
            method, path, region,
            headers.get("X-Amz-Date").unwrap().to_str().unwrap(),
            body_hash
        );

        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            headers.get("X-Amz-Date").unwrap().to_str().unwrap(),
            credential_scope,
            hex::encode(sha256(canonical_request.as_bytes()))
        );

        let signing_key =
            Self::get_signature_key(&self.config.secret_access_key, &date_stamp, region, service);

        let signature = hex::encode(Self::hmac_sha256(&signing_key, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders=host;x-amz-date, Signature={}",
            self.config.access_key_id, credential_scope, signature
        );

        headers.insert(
            "Authorization",
            header_value("Authorization", &authorization)?,
        );

        Ok(headers)
    }

    fn get_signature_key(key: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
        let k_date = Self::hmac_sha256(format!("AWS4{}", key).as_bytes(), date.as_bytes());
        let k_region = Self::hmac_sha256(&k_date, region.as_bytes());
        let k_service = Self::hmac_sha256(&k_region, service.as_bytes());
        Self::hmac_sha256(&k_service, b"aws4_request")
    }

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }
}

fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

#[async_trait]
impl Provider for BedrockProvider {
    fn id(&self) -> &str {
        "amazon-bedrock"
    }

    fn name(&self) -> &str {
        "Amazon Bedrock"
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let endpoint = self.get_endpoint();
        let model_id = crate::bootstrap::AmazonBedrockLoader::prefix_model_id(
            &request.model,
            &self.config.region,
        );
        let model_id_encoded = urlencoding::encode(&model_id);

        let bedrock_request = self.convert_request(request);
        let body = serde_json::to_vec(&bedrock_request)
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        let headers = self
            .sign_request(
                "POST",
                &format!("/model/{}/converse", model_id_encoded),
                &body,
            )
            .await?;

        let url = format!("{}/model/{}/converse", endpoint, model_id_encoded);

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .body(body)
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

        let bedrock_response: BedrockConverseResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        Ok(convert_bedrock_response(bedrock_response))
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<StreamResult, ProviderError> {
        let endpoint = self.get_endpoint();
        let model_id = crate::bootstrap::AmazonBedrockLoader::prefix_model_id(
            &request.model,
            &self.config.region,
        );
        let model_id_encoded = urlencoding::encode(&model_id);

        let bedrock_request = self.convert_request(request);
        let body = serde_json::to_vec(&bedrock_request)
            .map_err(|e| ProviderError::api_error_with_source(e.to_string(), e))?;

        let headers = self
            .sign_request(
                "POST",
                &format!("/model/{}/converse-stream", model_id_encoded),
                &body,
            )
            .await?;

        let url = format!("{}/model/{}/converse-stream", endpoint, model_id_encoded);

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .header("Accept", "application/vnd.amazon.eventstream")
            .body(body)
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

        // 生バイトを保持し、完全な JSON オブジェクトが揃ってからデコードする。
        // AWS eventstream は改行区切りではないため SseLineBuffer ではフレーム境界を
        // 取り出せず、既存実装と同様に JSON テキストを手掛かりに境界を決定する。
        // チャンク境界で UTF-8 文字や JSON トークンが分断されても、完全に揃うまで
        // UTF-8 デコードを行わないため U+FFFD に固定化されない。
        let stream = stream::try_unfold(
            (
                response.bytes_stream(),
                BedrockJsonBuffer::new(),
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
                            push_bedrock_values(buffer.push(bytes.as_ref()), &mut pending);
                        }
                        Some(Err(err)) => {
                            return Err(ProviderError::StreamError(err.to_string()));
                        }
                        None => {
                            exhausted = true;
                            push_bedrock_values(buffer.flush(), &mut pending);
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

#[derive(Debug, Serialize)]
struct BedrockConverseRequest {
    messages: Vec<BedrockMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<BedrockSystemContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inference_config: Option<BedrockInferenceConfig>,
}

#[derive(Debug, Serialize)]
struct BedrockMessage {
    role: String,
    content: Vec<BedrockContentBlock>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BedrockContentBlock {
    text: String,
}

impl BedrockContentBlock {
    fn text(t: &str) -> Self {
        Self {
            text: t.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct BedrockSystemContent {
    text: String,
}

#[derive(Debug, Serialize)]
struct BedrockInferenceConfig {
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct BedrockConverseResponse {
    output: BedrockOutput,
    usage: BedrockUsage,
}

#[derive(Debug, Deserialize)]
struct BedrockOutput {
    message: BedrockResponseMessage,
}

#[derive(Debug, Deserialize)]
struct BedrockResponseMessage {
    _role: String,
    content: Vec<BedrockContentBlock>,
}

#[derive(Debug, Deserialize)]
struct BedrockUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: Option<u64>,
}

fn convert_bedrock_response(response: BedrockConverseResponse) -> ChatResponse {
    let content = response
        .output
        .message
        .content
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_default();

    ChatResponse {
        id: format!("bedrock_{}", uuid::Uuid::new_v4()),
        model: "amazon-bedrock".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message::assistant(&content),
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: response.usage.input_tokens,
            completion_tokens: response.usage.output_tokens,
            total_tokens: response
                .usage
                .total_tokens
                .unwrap_or_else(|| response.usage.input_tokens + response.usage.output_tokens),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
    }
}

/// Bedrock ConverseStream のトップレベルイベント名（JSON キー）。
///
/// eventstream のバイナリフレーム間に挟まった JSON を取り出すための手掛かりとして
/// 使う。完全なフレーム長/CRC デコーダは別タスク（TODO: M-55 の返済方針を参照）。
const BEDROCK_EVENT_KEYS: [&[u8]; 7] = [
    b"\"messageStart\"",
    b"\"contentBlockStart\"",
    b"\"contentBlockDelta\"",
    b"\"contentBlockStop\"",
    b"\"messageDelta\"",
    b"\"messageStop\"",
    b"\"metadata\"",
];

/// `bytes_stream()` の生バイトを保持し、完全な JSON オブジェクトが揃うまで
/// UTF-8 デコードを遅延するバッファ。
///
/// `SseLineBuffer` は改行で確定するため、改行を含まない eventstream バイナリ
/// フレームには使えない。ここでは既存実装と同じく JSON テキストを手掛かりに
/// オブジェクト境界を決定するが、境界判定はすべてバイト単位で行い、完全な
/// オブジェクトが揃って初めて `str` 化するため途中分割でも文字化けしない。
#[derive(Debug, Default)]
struct BedrockJsonBuffer {
    bytes: Vec<u8>,
}

impl BedrockJsonBuffer {
    fn new() -> Self {
        Self::default()
    }

    /// 生バイトを追加し、新たに確定した JSON オブジェクトを返す。
    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.bytes.extend_from_slice(chunk);
        self.drain_complete_objects()
    }

    /// ストリーム終端で未確定の尾部を破棄し、確定済みオブジェクトだけを返す。
    fn flush(&mut self) -> Vec<Value> {
        let values = self.drain_complete_objects();
        self.bytes.clear();
        values
    }

    fn drain_complete_objects(&mut self) -> Vec<Value> {
        let mut values = Vec::new();
        while let Some((start, end)) = Self::next_object_range(&self.bytes) {
            let object = self.bytes[start..end].to_vec();
            // 不正 JSON でも必ず範囲を消費し、ループが止まらないようにする。
            self.bytes.drain(..end);
            if let Ok(text) = std::str::from_utf8(&object) {
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    values.push(value);
                }
            }
        }
        values
    }

    /// 既知のイベントキーを手掛かりに、完全な JSON オブジェクトの範囲を返す。
    fn next_object_range(bytes: &[u8]) -> Option<(usize, usize)> {
        let key_pos = BEDROCK_EVENT_KEYS
            .iter()
            .filter_map(|key| find_subslice(bytes, key))
            .min()?;
        // イベントキーの直前に置かれたトップレベルの `{` を開始位置とする。
        let start = bytes[..key_pos].iter().rposition(|&byte| byte == b'{')?;
        let end = object_end(bytes, start)?;
        Some((start, end))
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `start` の `{` に対応する `}` の直後を返す。
///
/// 文字列とエスケープを考慮するため、テキスト中の `{` / `}` や改行、
/// マルチバイト文字の途中バイトで誤って境界を判定しない。
fn object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, &byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// 完全にデコードされた Bedrock イベント JSON を `StreamEvent` へ変換する。
///
/// テキストデルタと `messageStop` のみを扱い、それ以外のイベント
/// （messageStart / contentBlockStop / metadata / toolUse デルタ等）は `None`
/// を返して呼び出し側がスキップする。空の `TextDelta` プレースホルダは出さない。
fn parse_bedrock_stream(value: &Value) -> Option<StreamEvent> {
    if let Some(delta) = value.get("contentBlockDelta") {
        if let Some(text) = delta
            .get("delta")
            .and_then(|inner| inner.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            return Some(StreamEvent::TextDelta(text.to_string()));
        }
        return None;
    }

    if value.get("messageStop").is_some() {
        return Some(StreamEvent::Done);
    }

    None
}

/// 確定した JSON オブジェクト列を、発行可能なイベントだけキューへ積む。
fn push_bedrock_values(values: Vec<Value>, pending: &mut VecDeque<StreamEvent>) {
    for value in values {
        if let Some(event) = parse_bedrock_stream(&value) {
            pending.push_back(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sign_request_rejects_invalid_session_token() {
        let provider = BedrockProvider::with_config(BedrockConfig {
            region: "us-east-1".to_string(),
            access_key_id: "AKIA".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: Some("bad\ntoken".to_string()),
            endpoint_url: None,
        });
        let result = provider.sign_request("POST", "/model", b"{}").await;
        assert!(
            result.is_err(),
            "invalid header value must be an error, not a panic"
        );
    }

    #[test]
    fn sha256_known_vectors() {
        // "abc" -> SHA-256 known vector
        let digest = sha256(b"abc");
        assert_eq!(
            digest
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_empty_input() {
        let digest = sha256(b"");
        assert_eq!(
            digest
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// eventstream のフレームヘッダ相当のバイトを間に挟み、「こんにちは」の
    /// 先頭バイト直後で分割しても U+FFFD にならず全イベントが届くこと。
    #[test]
    fn bedrock_buffer_reassembles_split_utf8_and_multiple_events() {
        let first =
            r#"{"contentBlockDelta":{"delta":{"text":"こんにちは"},"contentBlockIndex":0}}"#;
        let second = r#"{"contentBlockDelta":{"delta":{"text":"world"},"contentBlockIndex":0}}"#;
        let stop = r#"{"messageStop":{"stopReason":"end_turn"}}"#;

        // 改行も JSON 特殊文字も含まないバイナリヘッダを模す。
        let frame_header = [0x00u8, 0x00, 0x00, 0x4a, 0x00, 0x00, 0x00, 0x0d, 0x12, 0x34];
        let mut payload = Vec::new();
        payload.extend_from_slice(&frame_header);
        payload.extend_from_slice(first.as_bytes());
        payload.extend_from_slice(&frame_header);
        payload.extend_from_slice(second.as_bytes());
        payload.extend_from_slice(&frame_header);
        payload.extend_from_slice(stop.as_bytes());

        let needle = "こんにちは".as_bytes();
        let split = payload
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("text should be present")
            + 1;
        assert!(split < payload.len());

        let mut buffer = BedrockJsonBuffer::new();
        let mut pending = VecDeque::new();

        // 途中のチャンクではオブジェクトが確定していないためイベントを出さない。
        push_bedrock_values(buffer.push(&payload[..split]), &mut pending);
        assert!(pending.is_empty());

        push_bedrock_values(buffer.push(&payload[split..]), &mut pending);
        push_bedrock_values(buffer.flush(), &mut pending);

        let events: Vec<StreamEvent> = pending.into_iter().collect();
        assert_eq!(events.len(), 3, "複数フレームを全て順序どおり返す");
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "こんにちは"));
        assert!(matches!(&events[1], StreamEvent::TextDelta(text) if text == "world"));
        assert!(matches!(&events[2], StreamEvent::Done));
    }

    /// 1 チャンクに複数イベントが入っていても、文字列内の `{` / `}` / エスケープ
    /// を誤って境界とみなさず、非イベントはスキップされること。
    #[test]
    fn bedrock_buffer_emits_all_events_in_single_chunk() {
        let payload = concat!(
            r#"{"messageStart":{"role":"assistant"}}"#,
            r#"{"contentBlockDelta":{"delta":{"text":"a\"b{c}"},"contentBlockIndex":0}}"#,
            r#"{"metadata":{"usage":{"inputTokens":1,"outputTokens":2}}}"#,
            r#"{"messageStop":{"stopReason":"end_turn"}}"#,
        );

        let mut buffer = BedrockJsonBuffer::new();
        let mut pending = VecDeque::new();
        push_bedrock_values(buffer.push(payload.as_bytes()), &mut pending);

        let events: Vec<StreamEvent> = pending.into_iter().collect();
        assert_eq!(events.len(), 2, "非イベント値はスキップする");
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "a\"b{c}"));
        assert!(matches!(&events[1], StreamEvent::Done));
    }

    /// 短い／不正なチャンクで panic せず、空 TextDelta プレースホルダも出ないこと。
    #[test]
    fn bedrock_buffer_skips_partial_and_non_event_chunks_without_panic() {
        let mut buffer = BedrockJsonBuffer::new();
        let mut pending = VecDeque::new();

        // 旧実装の `"text":` +8 固定オフセットは短いチャンクで境界外 panic しうかった。
        assert!(buffer.push(b"{\"contentBlockDelta\"").is_empty());
        assert!(buffer.push(b":{").is_empty());
        push_bedrock_values(buffer.push(br#""delta":{"text":"ok"}}}"#), &mut pending);
        assert_eq!(pending.len(), 1);

        push_bedrock_values(buffer.flush(), &mut pending);
        let events: Vec<StreamEvent> = pending.into_iter().collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "ok"));

        assert!(parse_bedrock_stream(&serde_json::json!({
            "metadata": {"usage": {"inputTokens": 1}}
        }))
        .is_none());
        assert!(parse_bedrock_stream(&serde_json::json!({
            "contentBlockDelta": {"delta": {"toolUse": {"input": "{}"}}, "contentBlockIndex": 1}
        }))
        .is_none());
    }
}
