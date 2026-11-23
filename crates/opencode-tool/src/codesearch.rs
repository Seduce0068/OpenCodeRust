use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{Metadata, PermissionRequest, Tool, ToolContext, ToolError, ToolResult};

const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";
/// 検索クエリの上限（char 数）。External API と `PermissionRequest` の pattern、
/// および `ToolResult` の metadata/title へ渡る値を有界にし、過大な入力がそのまま
/// 外部送信・権限情報へ流れるのを防ぐ。WebSearch の `MAX_QUERY_CHARS` と揃える。
const MAX_QUERY_CHARS: usize = 4096;
/// 空クエリを拒否する際の固定エラー文言。クエリ値・パス・トークンを含めない。
const INVALID_QUERY_MESSAGE: &str = "Code search query must not be empty";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSearchParams {
    pub query: String,
    #[serde(default = "default_tokens", alias = "tokens_num")]
    pub tokens_num: u32,
}

fn default_tokens() -> u32 {
    5000
}

/// Exa Code API へ渡す検索クエリを検証・正規化する。
///
/// - 前後の空白を除去し、空になった場合は固定文言の `InvalidArguments` を返す。
/// - `MAX_QUERY_CHARS` を超える場合は char 単位で打ち切り、外部 API と
///   `PermissionRequest` / `ToolResult` へ渡る値の上限を保証する。
/// - エラー文言にはクエリ値・パス・トークンを一切含めない。
fn validated_query(query: &str) -> Result<String, ToolError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidArguments(
            INVALID_QUERY_MESSAGE.to_string(),
        ));
    }

    Ok(trimmed.chars().take(MAX_QUERY_CHARS).collect())
}

#[derive(Debug, Clone, Serialize)]
struct McpRequest {
    jsonrpc: String,
    id: u32,
    method: String,
    params: McpParams,
}

#[derive(Debug, Clone, Serialize)]
struct McpParams {
    name: String,
    arguments: McpArguments,
}

#[derive(Debug, Clone, Serialize)]
struct McpArguments {
    query: String,
    #[serde(rename = "tokensNum")]
    tokens_num: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct McpResponse {
    result: Option<McpResult>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpResult {
    content: Vec<McpContent>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

pub struct CodeSearchTool;

impl CodeSearchTool {
    /// `execute` の本体。公開 API（`CodeSearchTool` と `Tool` トレイト）および
    /// 成功系の wire 形式は変えず、HTTP モックによる回帰テストを可能にするため
    /// だけにエンドポイントを引数で受け取る。
    async fn execute_with_endpoint(
        args: serde_json::Value,
        ctx: ToolContext,
        endpoint: &str,
    ) -> Result<ToolResult, ToolError> {
        let params: CodeSearchParams = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid parameters: {}", e)))?;

        // 権限確認より前にクエリを検証・正規化し、以降の経路（MCP 引数、
        // permission pattern、metadata、title）で検証済み値を共有する。
        let query = validated_query(&params.query)?;

        ctx.ask_permission(
            PermissionRequest::new("codesearch")
                .with_pattern(&query)
                .always_allow(),
        )
        .await?;

        let tokens_num = params.tokens_num.clamp(1000, 50000);

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::execution_with_source("Failed to create HTTP client", e))?;

        let request = McpRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "tools/call".to_string(),
            params: McpParams {
                name: "get_code_context_exa".to_string(),
                arguments: McpArguments {
                    query: query.clone(),
                    tokens_num,
                },
            },
        };

        let abort_token = ctx.abort.clone();

        let request_future = async {
            client
                .post(endpoint)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .json(&request)
                .send()
                .await
                .map_err(|e| ToolError::execution_with_source("HTTP request failed", e))
        };

        tokio::select! {
            result = request_future => {
                let response = result?;

                let status = response.status();
                if !status.is_success() {
                    // サーバのエラー本文にはリクエストのクエリが反復される可能性があるため、
                    // 本文を読まずステータスコードのみを返す（websearch と同方針）。
                    return Err(ToolError::ExecutionError(format!(
                        "Code search error ({status})"
                    )));
                }

                let response_text = response.text().await
                    .map_err(|e| ToolError::execution_with_source("Failed to read response", e))?;

                let output = parse_sse_response(&response_text)
                    .unwrap_or_else(|| {
                        "No code snippets or documentation found. Please try a different query, be more specific about the library or programming concept, or check the spelling of framework names.".to_string()
                    });

                let mut metadata = Metadata::new();
                metadata.insert(
                    "query".to_string(),
                    serde_json::Value::String(query.clone()),
                );
                metadata.insert("tokens_num".to_string(), serde_json::json!(tokens_num));

                Ok(ToolResult {
                    output,
                    title: format!("Code search: {}", query),
                    metadata,
                    truncated: false,
                })
            }
            _ = abort_token.cancelled() => {
                Err(ToolError::Cancelled)
            }
        }
    }
}

#[async_trait]
impl Tool for CodeSearchTool {
    fn id(&self) -> &str {
        "codesearch"
    }

    fn description(&self) -> &str {
        "Search for relevant context for APIs, Libraries, and SDKs using Exa Code API. Find code examples, documentation, and best practices for any programming library or framework."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to find relevant context for APIs, Libraries, and SDKs. For example, 'React useState hook examples', 'Python pandas dataframe filtering', 'Express.js middleware', 'Next.js partial prerendering configuration'"
                },
                "tokensNum": {
                    "type": "integer",
                    "minimum": 1000,
                    "maximum": 50000,
                    "default": 5000,
                    "description": "Number of tokens to return (1000-50000). Default is 5000 tokens. Adjust based on how much context you need."
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
        Self::execute_with_endpoint(args, ctx, EXA_MCP_URL).await
    }
}

fn parse_sse_response(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(response) = serde_json::from_str::<McpResponse>(data) {
                if let Some(result) = response.result {
                    if let Some(content) = result.content.first() {
                        if content.content_type == "text" {
                            return Some(content.text.clone());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_schema() {
        let tool = CodeSearchTool;
        assert_eq!(tool.id(), "codesearch");
        assert!(tool.description().contains("Exa Code API"));
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "query");
        assert_eq!(schema["properties"]["tokensNum"]["default"], 5000);
    }

    #[test]
    fn default_tokens_is_five_thousand() {
        assert_eq!(default_tokens(), 5000);
    }

    #[test]
    fn params_apply_default_tokens() {
        let params: CodeSearchParams =
            serde_json::from_value(serde_json::json!({ "query": "react hooks" })).unwrap();
        assert_eq!(params.query, "react hooks");
        assert_eq!(params.tokens_num, 5000);
    }

    #[test]
    fn params_accept_tokens_num_alias() {
        let params: CodeSearchParams = serde_json::from_value(serde_json::json!({
            "query": "react hooks",
            "tokensNum": 1000
        }))
        .unwrap();
        assert_eq!(params.tokens_num, 1000);
    }

    #[test]
    fn params_accept_legacy_tokens_num_key_and_normalize() {
        let params: CodeSearchParams = serde_json::from_value(serde_json::json!({
            "query": "react hooks",
            "tokens_num": 1000
        }))
        .unwrap();
        assert_eq!(params.tokens_num, 1000);

        let serialized = serde_json::to_value(&params).unwrap();
        assert_eq!(serialized["tokensNum"], 1000);
        assert!(serialized.get("tokens_num").is_none());
    }

    /// 回帰テスト（AV-05）: 空／空白のみの query は固定文言の `InvalidArguments`
    /// になり、権限確認や HTTP リクエストより前に拒否されること。
    #[tokio::test]
    async fn empty_query_is_rejected_before_permission_and_http() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let permission_called = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&permission_called);
        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned()).with_ask(
            move |_| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        );

        for raw in ["", "   \t\n ", "\u{3000}"] {
            let args = serde_json::json!({ "query": raw });
            // 到達不能なエンドポイントを指定しても、検証が先に走るため即座に失敗する。
            let error =
                CodeSearchTool::execute_with_endpoint(args, ctx.clone(), "http://127.0.0.1:1/mcp")
                    .await
                    .expect_err("empty query must be rejected");
            let message = error.to_string();

            assert!(
                matches!(&error, ToolError::InvalidArguments(_)),
                "error type must remain InvalidArguments: {error}"
            );
            assert_eq!(
                message,
                format!("Invalid arguments: {INVALID_QUERY_MESSAGE}"),
                "fixed error wording must not carry the query value, path or token"
            );
            assert_eq!(validated_query(raw).unwrap_err().to_string(), message);
            assert!(
                !permission_called.load(Ordering::SeqCst),
                "permission must not be requested for an empty query"
            );
        }
    }

    /// 回帰テスト（AV-05）: 前後の空白は trim され、内部の空白は保持される。
    #[test]
    fn query_surrounding_whitespace_is_trimmed() {
        assert_eq!(
            validated_query("  react hooks \n\t").expect("valid query"),
            "react hooks"
        );
        assert_eq!(
            validated_query("\u{3000}React useState\u{3000}").expect("valid query"),
            "React useState"
        );
        assert_eq!(validated_query("a b  c").expect("valid query"), "a b  c");
    }

    /// 回帰テスト（AV-05）: 上限超えは char 単位で打ち切られる（バイト単位ではない）。
    #[test]
    fn oversized_query_is_truncated_by_char_count() {
        let ascii = "a".repeat(MAX_QUERY_CHARS + 100);
        let out = validated_query(&ascii).expect("valid query");
        assert_eq!(out.chars().count(), MAX_QUERY_CHARS);
        assert_eq!(out, "a".repeat(MAX_QUERY_CHARS));

        let multibyte = "あ".repeat(MAX_QUERY_CHARS + 100);
        let out = validated_query(&multibyte).expect("valid query");
        assert_eq!(out.chars().count(), MAX_QUERY_CHARS);
        assert_eq!(out, "あ".repeat(MAX_QUERY_CHARS));
        assert_eq!(
            out.len(),
            MAX_QUERY_CHARS * "あ".len(),
            "limit must count chars, not UTF-8 bytes"
        );
    }

    /// 回帰テスト（AV-05）: 通常の query は不変。
    #[test]
    fn normal_query_is_unchanged() {
        for query in ["react hooks", "Express.js middleware", "Rust tokio select!"] {
            assert_eq!(validated_query(query).expect("valid query"), query);
        }
    }

    /// 回帰テスト（AV-05）: 検証済みクエリが MCP リクエスト、permission pattern、
    /// および `ToolResult` の metadata/title で共有されること。
    #[tokio::test]
    async fn sanitized_query_is_used_by_permission_and_result() {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.expect("read request");
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                // JSON ボディの末尾（`"}}"`）が揃った時点で検証に必要な情報は得られる。
                if request.windows(2).any(|window| window == b"}}") {
                    break;
                }
            }
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
            String::from_utf8_lossy(&request).into_owned()
        });

        let patterns = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&patterns);
        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned()).with_ask(
            move |request| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.lock().expect("lock").extend(request.patterns);
                    Ok(())
                }
            },
        );

        let args = serde_json::json!({ "query": "  react hooks  " });
        let endpoint = format!("http://127.0.0.1:{port}/mcp");
        let result = CodeSearchTool::execute_with_endpoint(args, ctx, &endpoint)
            .await
            .expect("request must succeed");

        assert_eq!(result.metadata["query"], serde_json::json!("react hooks"));
        assert_eq!(result.title, "Code search: react hooks");
        assert_eq!(
            *patterns.lock().expect("lock"),
            vec!["react hooks".to_owned()]
        );

        let wire = server.await.expect("server task");
        assert!(
            wire.contains("\"query\":\"react hooks\""),
            "MCP request must carry sanitized query: {wire}"
        );
        assert!(
            !wire.contains("  react hooks  "),
            "raw query leaked into MCP request: {wire}"
        );
    }

    #[test]
    fn parse_sse_extracts_text_content() {
        let text = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"snippet\"}]}}\n\n";
        assert_eq!(parse_sse_response(text).as_deref(), Some("snippet"));
    }

    #[test]
    fn parse_sse_ignores_non_text_content() {
        let text = "data: {\"result\":{\"content\":[{\"type\":\"image\",\"text\":\"x\"}]}}";
        assert!(parse_sse_response(text).is_none());
    }

    #[test]
    fn parse_sse_ignores_malformed_json() {
        assert!(parse_sse_response("data: not-json").is_none());
    }

    #[test]
    fn parse_sse_ignores_missing_result() {
        assert!(parse_sse_response("data: {}").is_none());
    }

    #[test]
    fn parse_sse_ignores_empty_content() {
        let text = "data: {\"result\":{\"content\":[]}}";
        assert!(parse_sse_response(text).is_none());
    }

    /// 回帰テスト（AV-01）: 500 レスポンスの本文に SENTINEL が含まれても、
    /// `execute` は本文を読まずエラーへ埋め込まない。HTTP status のみが伝わる。
    #[tokio::test]
    async fn error_response_does_not_leak_body_but_keeps_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const SENTINEL: &str = "SENTINEL_BODY_MARKER_9d3a";

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await.expect("read request");
            // サーバ側がクエリを反復した生本文を返す状況を模擬する。
            let body = format!("error: {SENTINEL} (query echoed back)");
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let ctx = ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned());
        let args = serde_json::json!({ "query": "react hooks" });
        let endpoint = format!("http://127.0.0.1:{port}/mcp");

        let error = CodeSearchTool::execute_with_endpoint(args, ctx, &endpoint)
            .await
            .expect_err("500 response must produce an error");
        let message = error.to_string();

        assert!(
            matches!(&error, ToolError::ExecutionError(_)),
            "error type must remain ExecutionError: {error}"
        );
        assert!(
            !message.contains(SENTINEL),
            "raw server body leaked into ToolError: {message}"
        );
        assert!(
            message.contains("500"),
            "HTTP status was not propagated: {message}"
        );

        server.await.expect("server task");
    }
}
