use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params: None,
        }
    }

    pub fn with_params(mut self, params: serde_json::Value) -> Self {
        self.params = Some(params);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC message that can be either a response or a server notification.
#[derive(Debug, Clone)]
pub enum JsonRpcMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl JsonRpcMessage {
    /// Try to parse a raw JSON string as either a Response or a Notification.
    ///
    /// A frame carrying a `method` field is always treated as a Notification
    /// (a server request/notification), regardless of whether an `id` is also
    /// present. A frame with an `id` but no `method` is a Response. Frames with
    /// neither field are rejected.
    pub fn parse_json_str(s: &str) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(s)?;

        // `method` first: `JsonRpcResponse` makes `result`/`error` optional, so an
        // id-bearing request would otherwise deserialize into an empty Response
        // and silently lose its method. `JsonRpcNotification` has no `id` field,
        // so an accompanying `id` is intentionally not preserved here.
        if value.get("method").is_some() {
            let notif = serde_json::from_value::<JsonRpcNotification>(value)?;
            return Ok(JsonRpcMessage::Notification(notif));
        }

        if value.get("id").is_some() {
            let resp = serde_json::from_value::<JsonRpcResponse>(value)?;
            return Ok(JsonRpcMessage::Response(resp));
        }

        Err(<serde_json::Error as serde::de::Error>::custom(
            "JSON-RPC message requires a 'method' or 'id' field",
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

impl Default for InitializeParams {
    fn default() -> Self {
        Self {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ClientCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
            },
            client_info: ClientInfo {
                name: "opencode".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResourceParams {
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResourceResult {
    #[serde(default)]
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_rpc_request_new_sets_defaults() {
        let request = JsonRpcRequest::new(7, "tools/list");
        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.id, 7);
        assert_eq!(request.method, "tools/list");
        assert!(request.params.is_none());
    }

    #[test]
    fn json_rpc_request_new_accepts_owned_string() {
        let request = JsonRpcRequest::new(1, String::from("initialize"));
        assert_eq!(request.method, "initialize");
    }

    #[test]
    fn json_rpc_request_with_params_sets_params() {
        let request = JsonRpcRequest::new(2, "tools/call").with_params(json!({ "name": "echo" }));
        assert_eq!(request.params, Some(json!({ "name": "echo" })));
    }

    #[test]
    fn json_rpc_request_serialization_skips_absent_params() {
        let request = JsonRpcRequest::new(1, "tools/list");
        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(
            value,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })
        );
    }

    #[test]
    fn json_rpc_request_serialization_includes_params() {
        let request = JsonRpcRequest::new(2, "tools/call").with_params(json!({ "name": "echo" }));
        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["params"], json!({ "name": "echo" }));
    }

    #[test]
    fn json_rpc_response_serialization_skips_absent_result_and_error() {
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: 9,
            result: None,
            error: None,
        };
        let value = serde_json::to_value(&response).expect("serialize response");
        assert_eq!(value, json!({ "jsonrpc": "2.0", "id": 9 }));
    }

    #[test]
    fn json_rpc_error_serialization_skips_absent_data() {
        let error = JsonRpcError {
            code: -32601,
            message: "Method not found".to_string(),
            data: None,
        };
        let value = serde_json::to_value(&error).expect("serialize error");
        assert_eq!(
            value,
            json!({ "code": -32601, "message": "Method not found" })
        );
    }

    #[test]
    fn json_rpc_error_serialization_includes_data() {
        let error = JsonRpcError {
            code: -32602,
            message: "Invalid params".to_string(),
            data: Some(json!({ "field": "name" })),
        };
        let value = serde_json::to_value(&error).expect("serialize error");
        assert_eq!(value["data"], json!({ "field": "name" }));
    }

    #[test]
    fn json_rpc_notification_serialization_skips_absent_params() {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: "notifications/initialized".to_string(),
            params: None,
        };
        let value = serde_json::to_value(&notification).expect("serialize notification");
        assert_eq!(
            value,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        );
    }

    #[test]
    fn parse_json_str_parses_response() {
        let raw = r#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#;
        match JsonRpcMessage::parse_json_str(raw).expect("parse response") {
            JsonRpcMessage::Response(response) => {
                assert_eq!(response.id, 3);
                assert_eq!(response.result, Some(json!({ "ok": true })));
                assert!(response.error.is_none());
            }
            other => panic!("expected response, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_str_parses_notification() {
        let raw =
            r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":0.5}}"#;
        match JsonRpcMessage::parse_json_str(raw).expect("parse notification") {
            JsonRpcMessage::Notification(notification) => {
                assert_eq!(notification.method, "notifications/progress");
                assert_eq!(notification.params, Some(json!({ "progress": 0.5 })));
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_str_returns_error_for_invalid_input() {
        let error =
            JsonRpcMessage::parse_json_str("not-json").expect_err("invalid input must fail");
        assert!(!error.to_string().is_empty());
    }

    // Regression (MCP-JSONRPC-CLASSIFY-01): a frame with both `id` and `method`
    // is a request and must be classified as Notification, not an empty Response.
    #[test]
    fn parse_json_str_classifies_id_and_method_as_notification() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"sampling/createMessage"}"#;
        match JsonRpcMessage::parse_json_str(raw).expect("parse notification") {
            JsonRpcMessage::Notification(notification) => {
                assert_eq!(notification.method, "sampling/createMessage");
                assert!(notification.params.is_none());
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_str_keeps_id_only_frame_as_response() {
        let raw = r#"{"jsonrpc":"2.0","id":1}"#;
        match JsonRpcMessage::parse_json_str(raw).expect("parse response") {
            JsonRpcMessage::Response(response) => {
                assert_eq!(response.id, 1);
                assert!(response.result.is_none());
                assert!(response.error.is_none());
            }
            other => panic!("expected response, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_str_classifies_method_only_as_notification() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;
        match JsonRpcMessage::parse_json_str(raw).expect("parse notification") {
            JsonRpcMessage::Notification(notification) => {
                assert_eq!(notification.method, "notifications/cancelled");
                assert!(notification.params.is_none());
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_str_rejects_frame_without_method_or_id() {
        let error = JsonRpcMessage::parse_json_str(r#"{"jsonrpc":"2.0"}"#)
            .expect_err("frame without method or id must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn initialize_params_default_uses_expected_constants() {
        let params = InitializeParams::default();
        assert_eq!(params.protocol_version, "2024-11-05");
        assert_eq!(params.client_info.name, "opencode");
        assert_eq!(params.client_info.version, env!("CARGO_PKG_VERSION"));
        let tools = params.capabilities.tools.expect("tools capability");
        assert_eq!(tools.list_changed, Some(true));
    }

    #[test]
    fn initialize_params_serialization_renames_camel_case_fields() {
        let value = serde_json::to_value(InitializeParams::default()).expect("serialize params");
        assert_eq!(value["protocolVersion"], "2024-11-05");
        assert_eq!(value["clientInfo"]["name"], "opencode");
        assert_eq!(value["clientInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(value.get("protocol_version").is_none());
        assert!(value.get("client_info").is_none());
    }

    #[test]
    fn tool_definition_serialization_renames_input_schema() {
        let tool = ToolDefinition {
            name: "echo".to_string(),
            description: None,
            input_schema: json!({ "type": "object" }),
        };
        let value = serde_json::to_value(&tool).expect("serialize tool");
        assert_eq!(
            value,
            json!({ "name": "echo", "inputSchema": { "type": "object" } })
        );
    }

    #[test]
    fn resource_content_serialization_renames_mime_type_and_skips_absent_fields() {
        let content = ResourceContent {
            uri: Some("file:///tmp/a.txt".to_string()),
            mime_type: Some("text/plain".to_string()),
            text: None,
            blob: None,
        };
        let value = serde_json::to_value(&content).expect("serialize content");
        assert_eq!(
            value,
            json!({ "uri": "file:///tmp/a.txt", "mimeType": "text/plain" })
        );
    }

    #[test]
    fn read_resource_result_deserializes_missing_contents_as_empty() {
        let result: ReadResourceResult =
            serde_json::from_str("{}").expect("deserialize empty result");
        assert!(result.contents.is_empty());
    }

    // MCP 仕様の wire format は camelCase。snake_case で送出すると
    // サーバが capability を無視し、isError / mimeType を読み落とす。
    #[test]
    fn mcp_wire_format_uses_camel_case_field_names() {
        let capability = ToolsCapability {
            list_changed: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&capability).expect("serialize capability"),
            json!({ "listChanged": true })
        );

        let result = CallToolResult {
            content: vec![ContentBlock {
                content_type: "text".to_string(),
                text: Some("ok".to_string()),
                data: None,
                mime_type: Some("text/plain".to_string()),
            }],
            is_error: Some(false),
        };
        assert_eq!(
            serde_json::to_value(&result).expect("serialize call tool result"),
            json!({
                "content": [{ "type": "text", "text": "ok", "mimeType": "text/plain" }],
                "isError": false,
            })
        );

        let decoded: CallToolResult = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "isError": true,
        }))
        .expect("deserialize call tool result");
        assert_eq!(decoded.is_error, Some(true));
    }
}
