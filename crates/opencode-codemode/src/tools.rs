//! Host tool boundary for the confined interpreter.
//!
//! Upstream CodeMode resolves `tools.*` references against a host-provided tool
//! tree and invokes them asynchronously. The Rust port keeps the reference
//! model but uses a synchronous host: a tool call produces data immediately and
//! `await` passes that data through. Arguments and results cross the same
//! plain-data boundary as every other host interaction.

use std::fmt::{self, Display, Formatter};

use serde_json::Value as JsonValue;

/// A safe tool failure reported to the sandboxed program.
///
/// The message is surfaced to the model, so hosts must not include secrets or
/// raw internal diagnostics. Optional structured data is exposed as
/// `error.data` inside `catch` blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    message: String,
    data: Option<JsonValue>,
}

impl ToolError {
    /// Creates a failure with a model-safe message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            data: None,
        }
    }

    /// Creates a failure with a model-safe message and structured data.
    pub fn with_data(message: impl Into<String>, data: JsonValue) -> Self {
        Self {
            message: message.into(),
            data: Some(data),
        }
    }

    /// Model-safe failure message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Optional structured failure data.
    pub fn data(&self) -> Option<&JsonValue> {
        self.data.as_ref()
    }
}

impl Display for ToolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

/// Synchronous host interface for `tools.*` references.
///
/// `path` contains the namespace segments after the `tools` root. For
/// `tools.math.add`, it is `["math", "add"]`.
pub trait ToolHost {
    /// Returns the child namespace and tool names under `path`.
    ///
    /// Used by `Object.keys(tools)` and `Object.keys(tools.ns)`. Unknown paths
    /// return an empty list.
    fn keys(&self, path: &[String]) -> Vec<String> {
        let _ = path;
        Vec::new()
    }

    /// Invokes the tool at `path` with plain-data arguments.
    ///
    /// Returning `Err` makes the call throw inside the sandbox, so a program can
    /// handle it with `try`/`catch`.
    fn invoke(
        &mut self,
        path: &[String],
        arguments: Vec<JsonValue>,
    ) -> Result<JsonValue, ToolError>;
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value as JsonValue};

    use super::{ToolError, ToolHost};

    /// Host stub: no external execution, only a fixed plain-data result.
    struct DummyHost;

    impl ToolHost for DummyHost {
        fn invoke(
            &mut self,
            _path: &[String],
            _arguments: Vec<JsonValue>,
        ) -> Result<JsonValue, ToolError> {
            Ok(json!("dummy-result"))
        }
    }

    fn assert_std_error<T: std::error::Error>() {}

    #[test]
    fn tool_error_without_data_exposes_message_only() {
        let error = ToolError::new("safe failure");

        assert_eq!(error.message(), "safe failure");
        assert_eq!(error.data(), None);
        assert_eq!(error.to_string(), "safe failure");
        assert_eq!(error, ToolError::new("safe failure"));
        assert_ne!(error, ToolError::new("different failure"));
        assert_std_error::<ToolError>();
    }

    #[test]
    fn tool_error_with_data_keeps_structured_payload() {
        let payload = json!({ "code": 7, "items": [1, 2] });
        let error = ToolError::with_data("structured failure", payload.clone());

        assert_eq!(error.message(), "structured failure");
        assert_eq!(error.data(), Some(&payload));
        assert_eq!(error.to_string(), "structured failure");
        assert_eq!(error, ToolError::with_data("structured failure", payload));
    }

    #[test]
    fn tool_error_distinguishes_json_null_from_absent_data() {
        let error = ToolError::with_data("", JsonValue::Null);

        assert_eq!(error.message(), "");
        assert_eq!(error.data(), Some(&JsonValue::Null));
        assert_ne!(error, ToolError::new(""));
        assert!(error.to_string().is_empty());
    }

    #[test]
    fn default_keys_returns_empty_for_any_path() {
        let mut host = DummyHost;
        let path = vec!["math".to_string(), "add".to_string()];

        assert!(host.keys(&path).is_empty());
        assert!(host.keys(&[]).is_empty());

        let result = host
            .invoke(&path, vec![json!(1), json!(2)])
            .expect("dummy host returns a plain value");
        assert_eq!(result, json!("dummy-result"));
    }
}
