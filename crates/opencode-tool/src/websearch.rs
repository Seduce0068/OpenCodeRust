use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{Tool, ToolContext, ToolError, ToolResult};

const API_BASE_URL: &str = "https://mcp.exa.ai";
const DEFAULT_NUM_RESULTS: usize = 8;
/// Exa が受け付ける `numResults` の範囲。CodeSearch の `.clamp()` と同じ方針で
/// 境界値へ丸め、不正値をエラーやログに出さない。
const MIN_NUM_RESULTS: usize = 1;
const MAX_NUM_RESULTS: usize = 20;
const MIN_CONTEXT_MAX_CHARACTERS: usize = 1_000;
const MAX_CONTEXT_MAX_CHARACTERS: usize = 10_000;
/// WebFetch と同じ 5MB 上限を Exa レスポンス本文にも適用する。
const MAX_RESPONSE_SIZE: usize = 5 * 1024 * 1024;

pub struct WebSearchTool {
    client: Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WebSearchInput {
    query: String,
    #[serde(default = "default_num_results", alias = "numResults")]
    num_results: usize,
    #[serde(default)]
    livecrawl: Option<String>,
    #[serde(rename = "type", default)]
    search_type: Option<String>,
    #[serde(default, alias = "contextMaxCharacters")]
    context_max_characters: Option<usize>,
}

fn default_num_results() -> usize {
    DEFAULT_NUM_RESULTS
}

#[derive(Debug, Serialize)]
struct McpSearchRequest {
    jsonrpc: String,
    id: u32,
    method: String,
    params: McpSearchParams,
}

#[derive(Debug, Serialize)]
struct McpSearchParams {
    name: String,
    arguments: McpSearchArguments,
}

#[derive(Debug, Serialize)]
struct McpSearchArguments {
    query: String,
    #[serde(rename = "type")]
    search_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "numResults")]
    num_results: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    livecrawl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "contextMaxCharacters")]
    context_max_characters: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct McpSearchResponse {
    result: McpSearchResult,
}

#[derive(Debug, Deserialize)]
struct McpSearchResult {
    content: Vec<McpContent>,
}

#[derive(Debug, Deserialize)]
struct McpContent {
    #[serde(rename = "type")]
    _content_type: String,
    text: String,
}

/// CodeSearch の `tokens_num.clamp(1000, 50000)` と同じ検証アプローチ。
/// 値をエラー・ログに含めず、境界への丸めで対処する（失敗しない）。
fn clamp_num_results(value: usize) -> usize {
    value.clamp(MIN_NUM_RESULTS, MAX_NUM_RESULTS)
}

fn clamp_context_max_characters(value: usize) -> usize {
    value.clamp(MIN_CONTEXT_MAX_CHARACTERS, MAX_CONTEXT_MAX_CHARACTERS)
}

/// Exa へ送信する引数を組み立てる。全ての数値をここで clamp し、
/// `execute()` とテストが同じ経路を通るようにする。
fn clamped_search_arguments(input: &WebSearchInput) -> McpSearchArguments {
    McpSearchArguments {
        query: input.query.clone(),
        search_type: Some(
            input
                .search_type
                .clone()
                .unwrap_or_else(|| "auto".to_string()),
        ),
        num_results: Some(clamp_num_results(input.num_results)),
        livecrawl: Some(
            input
                .livecrawl
                .clone()
                .unwrap_or_else(|| "fallback".to_string()),
        ),
        context_max_characters: input
            .context_max_characters
            .map(clamp_context_max_characters),
    }
}

/// Exa レスポンス本文を `bytes_stream()` の累計で 5MB に制限して読む。
/// サーバ本文（クエリの反復やトークンを含み得る）をエラー・ログへ含めない。
async fn read_response_limited(response: reqwest::Response) -> Result<String, ToolError> {
    use futures::StreamExt;

    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| ToolError::execution_with_source("Failed to read response", e))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_SIZE {
            // ここで stream を drop し、これ以上のボディ受信を破棄する。
            return Err(ToolError::ExecutionError(
                "Search response too large (exceeds 5MB limit)".to_string(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

static DESCRIPTION: &str = r#"Search the web for real-time information using Exa AI search engine.

This tool provides access to current information from across the web. Use it when you need:
- Current events or news
- Latest documentation or library updates
- Real-time data (weather, stock prices, etc.)
- Recent research or publications
- Any information that may have changed since the knowledge cutoff date

The search returns relevant web pages with their content, optimized for LLM context."#;

#[async_trait]
impl Tool for WebSearchTool {
    fn id(&self) -> &str {
        "websearch"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Web search query"
                },
                "numResults": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "default": 8,
                    "description": "Number of search results to return (1-20)"
                },
                "num_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "default": 8,
                    "description": "Number of search results to return (1-20, snake_case alias)"
                },
                "livecrawl": {
                    "type": "string",
                    "enum": ["fallback", "preferred"],
                    "default": "fallback",
                    "description": "Live crawl mode - 'fallback': use live crawling as backup if cached content unavailable, 'preferred': prioritize live crawling"
                },
                "type": {
                    "type": "string",
                    "enum": ["auto", "fast", "deep"],
                    "default": "auto",
                    "description": "Search type - 'auto': balanced search, 'fast': quick results, 'deep': comprehensive search"
                },
                "contextMaxCharacters": {
                    "type": "integer",
                    "minimum": 1000,
                    "maximum": 10000,
                    "description": "Maximum characters for context string optimized for LLMs (1000-10000); omitted uses the Exa default"
                },
                "context_max_characters": {
                    "type": "integer",
                    "minimum": 1000,
                    "maximum": 10000,
                    "description": "Maximum characters for context string optimized for LLMs (1000-10000, snake_case alias); omitted uses the Exa default"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: WebSearchInput =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        // Exa へ渡す前に範囲を強制する。CodeSearch の clamp と同じく、
        // 不正値はエラーにせず境界へ丸め、入力値をログ・エラーに出さない。
        let arguments = clamped_search_arguments(&input);
        let num_results = arguments.num_results.unwrap_or(DEFAULT_NUM_RESULTS);

        ctx.ask_permission(
            crate::PermissionRequest::new("websearch")
                .with_pattern(&input.query)
                .with_metadata("query", serde_json::Value::String(input.query.clone()))
                .with_metadata("numResults", serde_json::Value::Number(num_results.into()))
                .always_allow(),
        )
        .await?;

        let search_request = McpSearchRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "tools/call".to_string(),
            params: McpSearchParams {
                name: "web_search_exa".to_string(),
                arguments,
            },
        };

        let response = self
            .client
            .post(format!("{}/mcp", API_BASE_URL))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&search_request)
            .timeout(std::time::Duration::from_secs(25))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ToolError::ExecutionError("Search request timed out".to_string())
                } else {
                    ToolError::execution_with_source("Search request failed", e)
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            // サーバのエラー本文にはリクエストのクエリが反復される可能性があるため、
            // 本文をエラーへ含めずステータスコードのみを返す。
            return Err(ToolError::ExecutionError(format!(
                "Search error ({status})"
            )));
        }

        let response_text = read_response_limited(response).await?;

        let output = parse_sse_response(&response_text).unwrap_or_else(|| {
            "No search results found. Please try a different query.".to_string()
        });

        Ok(ToolResult {
            title: format!("Web search: {}", input.query),
            output,
            metadata: std::collections::HashMap::new(),
            truncated: false,
        })
    }
}

fn parse_sse_response(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(response) = serde_json::from_str::<McpSearchResponse>(data) {
                if !response.result.content.is_empty() {
                    return Some(response.result.content[0].text.clone());
                }
            }
        }
    }
    None
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_schema() {
        let tool = WebSearchTool::new();
        assert_eq!(tool.id(), "websearch");
        assert!(tool.description().contains("Exa"));
        let schema = tool.parameters();
        assert_eq!(schema["required"][0], "query");
        assert_eq!(schema["properties"]["numResults"]["default"], 8);
    }

    #[test]
    fn default_num_results_is_eight() {
        assert_eq!(default_num_results(), 8);
    }

    #[test]
    fn input_applies_defaults() {
        let input: WebSearchInput =
            serde_json::from_value(serde_json::json!({ "query": "rust" })).unwrap();
        assert_eq!(input.query, "rust");
        assert_eq!(input.num_results, 8);
        assert!(input.livecrawl.is_none());
        assert!(input.search_type.is_none());
        assert!(input.context_max_characters.is_none());
    }

    #[test]
    fn input_accepts_camel_case_aliases() {
        let input: WebSearchInput = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "numResults": 3,
            "contextMaxCharacters": 1234,
            "type": "fast",
            "livecrawl": "preferred"
        }))
        .unwrap();
        assert_eq!(input.num_results, 3);
        assert_eq!(input.context_max_characters, Some(1234));
        assert_eq!(input.search_type.as_deref(), Some("fast"));
        assert_eq!(input.livecrawl.as_deref(), Some("preferred"));
    }

    #[test]
    fn input_accepts_snake_case_aliases() {
        let input: WebSearchInput = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "num_results": 5,
            "context_max_characters": 999
        }))
        .unwrap();
        assert_eq!(input.num_results, 5);
        assert_eq!(input.context_max_characters, Some(999));
    }

    #[test]
    fn parse_sse_extracts_first_text() {
        let text = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"result one\"},{\"type\":\"text\",\"text\":\"result two\"}]}}";
        assert_eq!(parse_sse_response(text).as_deref(), Some("result one"));
    }

    #[test]
    fn parse_sse_ignores_malformed_data() {
        assert!(parse_sse_response("data: not-json").is_none());
        assert!(parse_sse_response("data: {}").is_none());
    }

    #[test]
    fn parse_sse_ignores_empty_content() {
        let text = "data: {\"result\":{\"content\":[]}}";
        assert!(parse_sse_response(text).is_none());
    }

    /// 回帰テスト（R2 medium）: `num_results = usize::MAX` でも Exa へ渡す値は
    /// 20 に制限され、生の入力値がシリアライズ値に残らないこと。
    #[test]
    fn num_results_usize_max_is_clamped_to_twenty() {
        assert_eq!(clamp_num_results(usize::MAX), MAX_NUM_RESULTS);
        assert_eq!(clamp_num_results(0), MIN_NUM_RESULTS);
        assert_eq!(clamp_num_results(8), 8);

        let input: WebSearchInput = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "numResults": usize::MAX
        }))
        .expect("usize::MAX must deserialize into usize");
        let arguments = clamped_search_arguments(&input);
        assert_eq!(arguments.num_results, Some(20));

        let serialized = serde_json::to_value(&arguments).expect("serialize arguments");
        let raw_max = usize::MAX.to_string();
        assert_eq!(serialized["numResults"], 20);
        assert_ne!(serialized["numResults"], serde_json::json!(usize::MAX));
        assert!(
            !serialized.to_string().contains(raw_max.as_str()),
            "raw input value leaked into serialized request"
        );
    }

    /// 回帰テスト（R2 medium）: `context_max_characters` も 1000..=10000 に制限されること。
    #[test]
    fn context_max_characters_is_clamped_to_exa_limits() {
        assert_eq!(
            clamp_context_max_characters(usize::MAX),
            MAX_CONTEXT_MAX_CHARACTERS
        );
        assert_eq!(clamp_context_max_characters(0), MIN_CONTEXT_MAX_CHARACTERS);
        assert_eq!(clamp_context_max_characters(999), 1_000);
        assert_eq!(clamp_context_max_characters(5_000), 5_000);

        let input: WebSearchInput = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "contextMaxCharacters": usize::MAX
        }))
        .expect("usize::MAX must deserialize");
        let arguments = clamped_search_arguments(&input);
        let serialized = serde_json::to_value(&arguments).expect("serialize arguments");
        let raw_max = usize::MAX.to_string();
        assert_eq!(serialized["contextMaxCharacters"], 10_000);
        assert!(
            !serialized.to_string().contains(raw_max.as_str()),
            "raw input value leaked into serialized request"
        );
    }

    /// 省略時は None のまま（Exa 側デフォルトを使う）で、clamp が None を
    /// 勝手に埋めないことを固定する。
    #[test]
    fn omitted_context_max_characters_stays_none() {
        let input: WebSearchInput =
            serde_json::from_value(serde_json::json!({ "query": "rust" })).unwrap();
        let arguments = clamped_search_arguments(&input);
        assert_eq!(arguments.num_results, Some(DEFAULT_NUM_RESULTS));
        assert!(arguments.context_max_characters.is_none());
        assert_eq!(arguments.search_type.as_deref(), Some("auto"));
        assert_eq!(arguments.livecrawl.as_deref(), Some("fallback"));
    }

    /// 回帰テスト（R2 medium）: Content-Length なしの巨大レスポンスが
    /// ストリーム累計で拒否され、本文中のクエリ風マーカーがエラーに漏れないこと。
    #[tokio::test]
    async fn oversized_search_response_is_rejected_without_body_in_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const CHUNK_SIZE: usize = 64 * 1024;
        let total = MAX_RESPONSE_SIZE + CHUNK_SIZE;
        let marker = "query_secret_marker_7c1f";

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 512];
            let _ = socket.read(&mut request).await.expect("read request");
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await;

            // 先頭チャンクにクエリ風マーカーを埋め、エラーへの漏洩を検証する。
            let mut first = format!("data: {marker}\n").into_bytes();
            first.resize(CHUNK_SIZE, b'a');
            let header = format!("{:X}\r\n", first.len());
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&first).await;
            let _ = socket.write_all(b"\r\n").await;

            let payload = vec![b'a'; CHUNK_SIZE];
            let mut sent = first.len();
            while sent < total {
                let n = payload.len().min(total - sent);
                let header = format!("{n:X}\r\n");
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&payload[..n]).await;
                let _ = socket.write_all(b"\r\n").await;
                sent += n;
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });

        let response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect("loopback request must succeed");
        assert!(
            response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .is_none(),
            "test response must not carry Content-Length"
        );

        let error = read_response_limited(response)
            .await
            .expect_err("oversized response must be rejected");
        let message = error.to_string();
        assert!(
            !message.contains(marker),
            "error leaked body/query marker: {message}"
        );
        assert!(matches!(error, ToolError::ExecutionError(_)));

        server.await.expect("server task");
    }
}
