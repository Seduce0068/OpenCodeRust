//! Lightweight JSON-RPC 2.0 types for the plugin subprocess protocol.
//!
//! These are intentionally independent of `opencode-mcp` so the plugin crate
//! does not pull in the full MCP dependency graph.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RpcResponse {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: u64,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct RpcNotification {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_new_populates_envelope() {
        let params = json!({"offset": 3});
        let request = RpcRequest::new(42, String::from("tools/list"), Some(params.clone()));

        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.id, 42);
        assert_eq!(request.method, "tools/list");
        assert_eq!(request.params, Some(params));
    }

    #[test]
    fn request_serialization_keeps_params() {
        let request = RpcRequest::new(7, "ping", Some(json!({"n": 1})));

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(
            value,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "ping",
                "params": {"n": 1}
            })
        );
    }

    #[test]
    fn request_serialization_omits_absent_params() {
        let request = RpcRequest::new(0, "initialize", None);

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(
            value,
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"})
        );
        assert!(value.get("params").is_none());
    }

    #[test]
    fn response_deserialization_keeps_success_result() {
        let response: RpcResponse = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {"ok": true},
            "error": null
        }))
        .expect("deserialize success response");

        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, 9);
        assert_eq!(response.result, Some(json!({"ok": true})));
        assert!(response.error.is_none());
    }

    #[test]
    fn response_deserialization_keeps_error_payload() {
        let response: RpcResponse = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 10,
            "result": null,
            "error": {"code": -32001, "message": "boom"}
        }))
        .expect("deserialize error response");

        assert!(response.result.is_none());
        let error = response.error.expect("error payload");
        assert_eq!(error.code, -32001);
        assert_eq!(error.message, "boom");
    }

    #[test]
    fn response_deserialization_defaults_missing_payloads() {
        let response: RpcResponse = serde_json::from_value(json!({"jsonrpc": "2.0", "id": 11}))
            .expect("deserialize minimal response");

        assert_eq!(response.id, 11);
        assert!(response.result.is_none());
        assert!(response.error.is_none());
    }

    #[test]
    fn response_deserialization_rejects_non_numeric_id() {
        let result = serde_json::from_value::<RpcResponse>(json!({
            "jsonrpc": "2.0",
            "id": "12",
            "result": null,
            "error": null
        }));

        assert!(result.is_err(), "id must deserialize as u64");
    }

    #[test]
    fn notification_deserialization_defaults_params() {
        let with_params: RpcNotification = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {"x": 1}
        }))
        .expect("deserialize notification with params");
        assert_eq!(with_params.method, "initialized");
        assert_eq!(with_params.params, Some(json!({"x": 1})));

        let without_params: RpcNotification =
            serde_json::from_value(json!({"jsonrpc": "2.0", "method": "exit"}))
                .expect("deserialize notification without params");
        assert_eq!(without_params.method, "exit");
        assert!(without_params.params.is_none());

        let null_params: RpcNotification = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }))
        .expect("deserialize notification with null params");
        assert!(null_params.params.is_none());
    }
}
