//! Lightweight JSON-RPC 2.0 types for the plugin subprocess protocol.
//!
//! These are intentionally independent of `opencode-mcp` so the plugin crate
//! does not pull in the full MCP dependency graph.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only JSON-RPC version this plugin protocol accepts.
pub const JSONRPC_VERSION: &str = "2.0";

/// Version of the `initialize` handshake contract shared by the Rust client
/// and the TypeScript host. Bump when the shape of initialize params/result
/// changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// serde default for `InitializeResult.protocol_version`.
///
/// Older hosts that omit the field are treated as `PROTOCOL_VERSION` so the
/// handshake stays backward compatible; a *different* explicit value is still
/// rejected. Both sides always send the field.
pub fn default_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

/// Validate a JSON-RPC version value, returning a protocol-level error message.
fn validate_jsonrpc_version(version: &str) -> Result<(), String> {
    if version == JSONRPC_VERSION {
        Ok(())
    } else {
        Err(format!(
            "unsupported jsonrpc version {version:?}, expected {JSONRPC_VERSION:?}"
        ))
    }
}

/// serde `deserialize_with` hook: reject any envelope whose `jsonrpc` is not
/// exactly `"2.0"` instead of silently reading and discarding the field.
fn deserialize_jsonrpc_version<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let version = String::deserialize(deserializer)?;
    validate_jsonrpc_version(&version).map_err(serde::de::Error::custom)?;
    Ok(version)
}

/// Validate the `jsonrpc` field of a raw incoming envelope *before* it is
/// deserialized into a typed message.
///
/// Deserialization itself also validates (see `deserialize_jsonrpc_version`),
/// but this early check lets callers surface a version mismatch as
/// `PluginSubprocessError::Protocol` rather than a generic JSON error.
pub fn validate_jsonrpc_envelope(value: &Value) -> Result<(), String> {
    match value.get("jsonrpc") {
        Some(Value::String(version)) => validate_jsonrpc_version(version),
        Some(_) => Err("jsonrpc field must be a string".to_string()),
        None => Err("missing jsonrpc field".to_string()),
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcRequest {
    #[serde(deserialize_with = "deserialize_jsonrpc_version")]
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RpcResponse {
    #[serde(deserialize_with = "deserialize_jsonrpc_version")]
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
    #[serde(deserialize_with = "deserialize_jsonrpc_version")]
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

    // -- JSON-RPC version validation (regression) ---------------------------

    #[test]
    fn request_deserialization_rejects_wrong_jsonrpc_version() {
        let result = serde_json::from_value::<RpcRequest>(json!({
            "jsonrpc": "1.0",
            "id": 1,
            "method": "ping"
        }));
        let err = result.expect_err("non-2.0 request must be rejected");
        assert!(
            err.to_string().contains("unsupported jsonrpc version"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn request_deserialization_accepts_jsonrpc_2() {
        let request: RpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping"
        }))
        .expect("JSON-RPC 2.0 request must be accepted");
        assert_eq!(request.jsonrpc, JSONRPC_VERSION);
        assert_eq!(request.id, 1);
        assert_eq!(request.method, "ping");
    }

    #[test]
    fn response_deserialization_rejects_wrong_jsonrpc_version() {
        for version in ["1.0", "2", "", "2.0.0"] {
            let result = serde_json::from_value::<RpcResponse>(json!({
                "jsonrpc": version,
                "id": 1,
                "result": null,
                "error": null
            }));
            let err = result.expect_err("non-2.0 response must be rejected");
            assert!(
                err.to_string().contains("unsupported jsonrpc version"),
                "unexpected error for {version:?}: {err}"
            );
        }
    }

    #[test]
    fn response_deserialization_rejects_missing_jsonrpc_version() {
        let result = serde_json::from_value::<RpcResponse>(json!({"id": 1}));
        assert!(result.is_err(), "missing jsonrpc must be rejected");
    }

    #[test]
    fn notification_deserialization_rejects_wrong_jsonrpc_version() {
        let result = serde_json::from_value::<RpcNotification>(json!({
            "jsonrpc": "1.0",
            "method": "initialized"
        }));
        let err = result.expect_err("non-2.0 notification must be rejected");
        assert!(
            err.to_string().contains("unsupported jsonrpc version"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn raw_envelope_validation_rejects_bad_or_missing_jsonrpc() {
        assert!(validate_jsonrpc_envelope(&json!({"jsonrpc": "2.0"})).is_ok());
        assert!(validate_jsonrpc_envelope(&json!({"jsonrpc": "1.0"})).is_err());
        assert!(validate_jsonrpc_envelope(&json!({"jsonrpc": 2})).is_err());
        assert!(validate_jsonrpc_envelope(&json!({})).is_err());
    }
}
