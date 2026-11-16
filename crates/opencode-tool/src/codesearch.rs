use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{Metadata, PermissionRequest, Tool, ToolContext, ToolError, ToolResult};

const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSearchParams {
    pub query: String,
    #[serde(default = "default_tokens")]
    pub tokens_num: u32,
}

fn default_tokens() -> u32 {
    5000
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
        let params: CodeSearchParams = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid parameters: {}", e)))?;

        ctx.ask_permission(
            PermissionRequest::new("codesearch")
                .with_pattern(&params.query)
                .always_allow(),
        )
        .await?;

        let tokens_num = params.tokens_num.clamp(1000, 50000);
        let query = params.query.clone();

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| {
                ToolError::ExecutionError(format!("Failed to create HTTP client: {}", e))
            })?;

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
                .post(EXA_MCP_URL)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .json(&request)
                .send()
                .await
                .map_err(|e| ToolError::ExecutionError(format!("HTTP request failed: {}", e)))
        };

        tokio::select! {
            result = request_future => {
                let response = result?;

                if !response.status().is_success() {
                    let status = response.status();
                    let error_text = response.text().await.unwrap_or_default();
                    return Err(ToolError::ExecutionError(format!("Code search error ({}): {}", status, error_text)));
                }

                let response_text = response.text().await
                    .map_err(|e| ToolError::ExecutionError(format!("Failed to read response: {}", e)))?;

                let output = parse_sse_response(&response_text)
                    .unwrap_or_else(|| {
                        "No code snippets or documentation found. Please try a different query, be more specific about the library or programming concept, or check the spelling of framework names.".to_string()
                    });

                let mut metadata = Metadata::new();
                metadata.insert("query".to_string(), serde_json::json!(query));
                metadata.insert("tokens_num".to_string(), serde_json::json!(tokens_num));

                Ok(ToolResult {
                    output,
                    title: format!("Code search: {}", params.query),
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
}
