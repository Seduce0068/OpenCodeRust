use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip)]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default = "default_content", deserialize_with = "deserialize_content")]
    pub content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(rename = "providerOptions", skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<HashMap<String, serde_json::Value>>,
}

fn default_content() -> Content {
    Content::Text(String::new())
}

/// Deserialize content that may be null (some OpenAI-compatible APIs return
/// `"content": null` for tool-call-only responses).
fn deserialize_content<'de, D>(deserializer: D) -> Result<Content, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<Content> = Option::deserialize(deserializer)?;
    Ok(value.unwrap_or_else(|| Content::Text(String::new())))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<ImageUrl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<ToolUse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(rename = "providerOptions", skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<HashMap<String, serde_json::Value>>,
}

impl Default for ContentPart {
    fn default() -> Self {
        Self {
            content_type: "text".to_string(),
            text: None,
            image_url: None,
            tool_use: None,
            tool_result: None,
            cache_control: None,
            filename: None,
            media_type: None,
            provider_options: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub index: u32,
    pub message: Message,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(content.into()),
            cache_control: None,
            provider_options: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(content.into()),
            cache_control: None,
            provider_options: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Content::Text(content.into()),
            cache_control: None,
            provider_options: None,
        }
    }

    pub fn with_cache_control(mut self, cache_control: CacheControl) -> Self {
        self.cache_control = Some(cache_control);
        self
    }
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens: None,
            temperature: None,
            top_p: None,
            system: None,
            tools: None,
            stream: Some(true),
            provider_options: None,
            variant: None,
        }
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    pub fn with_top_p(mut self, top_p: f32) -> Self {
        self.top_p = Some(top_p);
        self
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = Some(stream);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_request_new_defaults_and_builders() {
        let request = ChatRequest::new("dummy-model", vec![Message::user("dummy-user")]);
        assert_eq!(request.model, "dummy-model");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.stream, Some(true));
        assert!(request.max_tokens.is_none());
        assert!(request.temperature.is_none());
        assert!(request.top_p.is_none());
        assert!(request.system.is_none());
        assert!(request.tools.is_none());
        assert!(request.provider_options.is_none());
        assert!(request.variant.is_none());

        let tools = vec![ToolDefinition {
            name: "dummy-tool".to_string(),
            description: Some("dummy description".to_string()),
            parameters: json!({ "type": "object" }),
        }];
        let request = request
            .with_temperature(0.25)
            .with_top_p(0.75)
            .with_stream(false)
            .with_max_tokens(128)
            .with_system("dummy-system")
            .with_tools(tools);

        assert_eq!(request.temperature, Some(0.25));
        assert_eq!(request.top_p, Some(0.75));
        assert_eq!(request.stream, Some(false));
        assert_eq!(request.max_tokens, Some(128));
        assert_eq!(request.system.as_deref(), Some("dummy-system"));
        assert_eq!(request.tools.as_ref().map(Vec::len), Some(1));
        // ビルダーは provider_options / variant を変更しない
        assert!(request.provider_options.is_none());
        assert!(request.variant.is_none());
    }

    #[test]
    fn chat_request_wire_format_skips_none_and_variant() {
        let request = ChatRequest::new("dummy-model", vec![Message::user("hello")]);
        let value = serde_json::to_value(&request).expect("dummy request should serialize");
        let obj = value.as_object().expect("request should be a JSON object");

        assert_eq!(obj.get("model"), Some(&json!("dummy-model")));
        assert_eq!(obj.get("stream"), Some(&json!(true)));
        assert!(obj.contains_key("messages"));
        for key in [
            "max_tokens",
            "temperature",
            "top_p",
            "system",
            "tools",
            "provider_options",
            "variant",
        ] {
            assert!(!obj.contains_key(key), "field `{key}` should be absent");
        }
    }

    #[test]
    fn chat_request_wire_format_with_all_options() {
        let mut provider_options = HashMap::new();
        provider_options.insert("dummy-key".to_string(), json!("dummy-value"));
        let mut request = ChatRequest::new("dummy-model", vec![Message::user("hello")])
            .with_temperature(0.5)
            .with_top_p(0.75)
            .with_stream(false)
            .with_max_tokens(64)
            .with_system("dummy-system");
        request.provider_options = Some(provider_options);
        request.variant = Some("dummy-variant".to_string());

        let value = serde_json::to_value(&request).expect("dummy request should serialize");
        assert_eq!(value.get("max_tokens"), Some(&json!(64)));
        assert_eq!(value.get("temperature"), Some(&json!(0.5)));
        assert_eq!(value.get("top_p"), Some(&json!(0.75)));
        assert_eq!(value.get("stream"), Some(&json!(false)));
        assert_eq!(value.get("system"), Some(&json!("dummy-system")));
        // ChatRequest の provider_options は rename なし（snake_case）。
        // openai.rs がこのキーを取り出してトップレベルへ展開する。
        assert_eq!(
            value.get("provider_options"),
            Some(&json!({ "dummy-key": "dummy-value" }))
        );
        // variant は #[serde(skip)] のため wire に出ない
        assert!(value.get("variant").is_none());
    }

    #[test]
    fn chat_request_serde_roundtrip_preserves_options_but_not_variant() {
        let request = ChatRequest::new("dummy-model", vec![Message::user("dummy")])
            .with_max_tokens(32)
            .with_system("dummy-system");
        let value = serde_json::to_value(&request).expect("dummy request should serialize");
        let decoded: ChatRequest =
            serde_json::from_value(value).expect("dummy request should deserialize");

        assert_eq!(decoded.model, "dummy-model");
        assert_eq!(decoded.max_tokens, Some(32));
        assert_eq!(decoded.system.as_deref(), Some("dummy-system"));
        assert_eq!(decoded.stream, Some(true));
        assert!(decoded.variant.is_none());
        assert!(matches!(
            &decoded.messages[0].content,
            Content::Text(text) if text == "dummy"
        ));
    }

    #[test]
    fn message_constructors_set_role_and_text_content() {
        let user = Message::user("dummy-user-text");
        assert!(matches!(&user.role, Role::User));
        assert!(matches!(&user.content, Content::Text(t) if t == "dummy-user-text"));
        assert!(user.cache_control.is_none());
        assert!(user.provider_options.is_none());

        let assistant = Message::assistant("dummy-assistant-text");
        assert!(matches!(&assistant.role, Role::Assistant));
        assert!(matches!(&assistant.content, Content::Text(t) if t == "dummy-assistant-text"));

        let system = Message::system("dummy-system-text");
        assert!(matches!(&system.role, Role::System));
        assert!(matches!(&system.content, Content::Text(t) if t == "dummy-system-text"));
    }

    #[test]
    fn message_with_cache_control_sets_ephemeral() {
        let message = Message::user("dummy").with_cache_control(CacheControl::ephemeral());
        let cache_control = message
            .cache_control
            .as_ref()
            .expect("cache control should be set");
        assert_eq!(cache_control.cache_type, "ephemeral");

        let value = serde_json::to_value(&message).expect("dummy message should serialize");
        assert_eq!(
            value.get("cache_control"),
            Some(&json!({ "type": "ephemeral" }))
        );
    }

    #[test]
    fn role_uses_lowercase_wire_format() {
        assert_eq!(
            serde_json::to_value(Role::System).expect("role should serialize"),
            json!("system")
        );
        assert_eq!(
            serde_json::to_value(Role::User).expect("role should serialize"),
            json!("user")
        );
        assert_eq!(
            serde_json::to_value(Role::Assistant).expect("role should serialize"),
            json!("assistant")
        );
        assert_eq!(
            serde_json::to_value(Role::Tool).expect("role should serialize"),
            json!("tool")
        );

        let decoded: Role =
            serde_json::from_str("\"assistant\"").expect("lowercase role should deserialize");
        assert!(matches!(decoded, Role::Assistant));
    }

    #[test]
    fn cache_control_ephemeral_wire_format_and_roundtrip() {
        let value = serde_json::to_value(CacheControl::ephemeral())
            .expect("dummy cache control should serialize");
        assert_eq!(value, json!({ "type": "ephemeral" }));

        let decoded: CacheControl =
            serde_json::from_value(value).expect("dummy cache control should deserialize");
        assert_eq!(decoded.cache_type, "ephemeral");
    }

    #[test]
    fn content_untagged_text_and_parts_wire_format() {
        let text = Content::Text("dummy-text".to_string());
        let value = serde_json::to_value(&text).expect("text content should serialize");
        assert_eq!(value, json!("dummy-text"));
        let decoded: Content = serde_json::from_value(value).expect("text should deserialize");
        assert!(matches!(decoded, Content::Text(t) if t == "dummy-text"));

        let part = ContentPart::default();
        assert_eq!(part.content_type, "text");
        let parts = Content::Parts(vec![part]);
        let value = serde_json::to_value(&parts).expect("parts content should serialize");
        assert_eq!(value, json!([{ "type": "text" }]));

        let decoded: Content = serde_json::from_value(value).expect("parts should deserialize");
        match decoded {
            Content::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].content_type, "text");
            }
            Content::Text(_) => panic!("expected Content::Parts"),
        }
    }

    #[test]
    fn content_part_provider_options_uses_camel_case_rename() {
        let mut provider_options = HashMap::new();
        provider_options.insert("dummy".to_string(), json!(1));
        let part = ContentPart {
            provider_options: Some(provider_options),
            ..ContentPart::default()
        };
        let value = serde_json::to_value(&part).expect("dummy part should serialize");
        assert_eq!(value.get("providerOptions"), Some(&json!({ "dummy": 1 })));
        assert!(!value
            .as_object()
            .expect("part should be an object")
            .contains_key("provider_options"));

        let decoded: ContentPart =
            serde_json::from_value(value).expect("dummy part should deserialize");
        assert!(decoded.provider_options.is_some());
    }

    #[test]
    fn content_part_tool_payload_wire_format() {
        let tool_use = ContentPart {
            content_type: "tool_use".to_string(),
            tool_use: Some(ToolUse {
                id: "tool-1".to_string(),
                name: "dummy".to_string(),
                input: json!({ "a": 1 }),
            }),
            ..ContentPart::default()
        };
        let value = serde_json::to_value(&tool_use).expect("tool_use part should serialize");
        assert_eq!(value.get("type"), Some(&json!("tool_use")));
        assert_eq!(
            value.get("tool_use"),
            Some(&json!({ "id": "tool-1", "name": "dummy", "input": { "a": 1 } }))
        );
        assert!(value.get("toolUse").is_none());

        let tool_result = ContentPart {
            content_type: "tool_result".to_string(),
            tool_result: Some(ToolResult {
                tool_use_id: "tool-1".to_string(),
                content: "done".to_string(),
                is_error: Some(false),
            }),
            ..ContentPart::default()
        };
        let value = serde_json::to_value(&tool_result).expect("tool_result part should serialize");
        assert_eq!(
            value.get("tool_result"),
            Some(&json!({
                "tool_use_id": "tool-1",
                "content": "done",
                "is_error": false
            }))
        );
    }

    #[test]
    fn message_provider_options_uses_camel_case_rename() {
        let mut provider_options = HashMap::new();
        provider_options.insert("anthropic".to_string(), json!({ "dummy": true }));
        let message = Message {
            provider_options: Some(provider_options),
            ..Message::user("dummy")
        };
        let value = serde_json::to_value(&message).expect("dummy message should serialize");
        assert_eq!(
            value.get("providerOptions"),
            Some(&json!({ "anthropic": { "dummy": true } }))
        );
        assert!(!value
            .as_object()
            .expect("message should be an object")
            .contains_key("provider_options"));

        let decoded: Message =
            serde_json::from_value(value).expect("dummy message should deserialize");
        assert!(decoded.provider_options.is_some());
        assert!(matches!(&decoded.content, Content::Text(t) if t == "dummy"));
    }

    #[test]
    fn deserialize_content_null_falls_back_to_empty_text() {
        let message: Message = serde_json::from_value(json!({
            "role": "assistant",
            "content": null
        }))
        .expect("null content should be accepted");
        assert!(matches!(&message.content, Content::Text(t) if t.is_empty()));
    }

    #[test]
    fn deserialize_content_missing_falls_back_to_empty_text() {
        let message: Message = serde_json::from_value(json!({ "role": "tool" }))
            .expect("missing content should be accepted");
        assert!(matches!(&message.content, Content::Text(t) if t.is_empty()));
    }
}
