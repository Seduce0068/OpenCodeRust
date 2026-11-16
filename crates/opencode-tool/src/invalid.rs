use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Metadata, Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidParams {
    #[serde(alias = "toolName")]
    pub tool_name: String,
    #[serde(alias = "errorMessage")]
    pub error_message: String,
    #[serde(alias = "receivedArgs")]
    pub received_args: Option<serde_json::Value>,
}

pub struct InvalidTool;

#[async_trait]
impl Tool for InvalidTool {
    fn id(&self) -> &str {
        "invalid"
    }

    fn description(&self) -> &str {
        "Handle invalid tool calls. This tool is used when an invalid or unknown tool is requested."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "toolName": {
                    "type": "string",
                    "description": "The name of the invalid tool that was requested"
                },
                "errorMessage": {
                    "type": "string",
                    "description": "Description of why the tool call is invalid"
                },
                "receivedArgs": {
                    "type": "object",
                    "description": "The arguments that were passed to the invalid tool"
                }
            },
            "required": ["toolName", "errorMessage"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: InvalidParams = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid parameters: {}", e)))?;

        let output = format!(
            "⚠️ Invalid Tool Call\n\nTool: {}\nError: {}\n\nPlease check the available tools and try again with a valid tool name.",
            params.tool_name,
            params.error_message
        );

        let mut metadata = Metadata::new();
        metadata.insert("tool_name".to_string(), serde_json::json!(params.tool_name));
        metadata.insert(
            "error_message".to_string(),
            serde_json::json!(params.error_message),
        );

        Ok(ToolResult {
            output,
            title: format!("Invalid Tool: {}", params.tool_name),
            metadata,
            truncated: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ToolContext {
        ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            "/tmp".to_owned(),
        )
    }

    #[test]
    fn identity_and_parameters() {
        let tool = InvalidTool;
        assert_eq!(tool.id(), "invalid");
        assert!(tool.description().contains("invalid"));
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "toolName");
        assert_eq!(schema["required"][1], "errorMessage");
    }

    #[tokio::test]
    async fn execute_formats_output_and_metadata() {
        let args = serde_json::json!({
            "tool_name": "bogus",
            "error_message": "unknown tool",
            "received_args": { "x": 1 }
        });
        let result = InvalidTool.execute(args, context()).await.unwrap();

        assert!(result.output.contains("bogus"));
        assert!(result.output.contains("unknown tool"));
        assert_eq!(result.title, "Invalid Tool: bogus");
        assert_eq!(result.metadata["tool_name"], "bogus");
        assert_eq!(result.metadata["error_message"], "unknown tool");
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn execute_accepts_schema_camel_case_fields() {
        let args = serde_json::json!({
            "toolName": "bogus",
            "errorMessage": "unknown tool",
            "receivedArgs": { "x": 1 }
        });
        let result = InvalidTool.execute(args, context()).await.unwrap();

        assert_eq!(result.title, "Invalid Tool: bogus");
        assert_eq!(result.metadata["tool_name"], "bogus");
        assert_eq!(result.metadata["error_message"], "unknown tool");
    }

    #[tokio::test]
    async fn execute_rejects_missing_required_fields() {
        let args = serde_json::json!({ "tool_name": "bogus" });
        let error = InvalidTool.execute(args, context()).await.unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }
}
