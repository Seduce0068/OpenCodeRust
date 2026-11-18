use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub role: MessageRole,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// `MessageRole::Tool` が参照する tool call の ID。
    #[serde(default)]
    pub tool_call_id: String,
    /// `MessageRole::Tool` が対応するツール名。
    #[serde(default)]
    pub tool_name: String,
    /// ツール実行がエラーだったかどうか。
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

impl AgentMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            is_error: false,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            is_error: false,
        }
    }

    pub fn assistant(content: impl Into<String>, reasoning: Option<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            reasoning,
            tool_calls: Vec::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            is_error: false,
        }
    }

    pub fn assistant_with_tools(
        content: impl Into<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            reasoning,
            tool_calls,
            tool_call_id: String::new(),
            tool_name: String::new(),
            is_error: false,
        }
    }

    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            reasoning: None,
            tool_calls: vec![],
            tool_call_id: tool_call_id.into(),
            tool_name: name.into(),
            is_error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub messages: Vec<AgentMessage>,
}

impl Conversation {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn with_system_prompt(prompt: impl Into<String>) -> Self {
        let mut conv = Self::new();
        conv.messages.push(AgentMessage::system(prompt));
        conv
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.messages.push(AgentMessage::user(content));
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>, reasoning: Option<String>) {
        self.messages
            .push(AgentMessage::assistant(content, reasoning));
    }

    /// assistant の応答と、それに伴う tool_use 呼び出しを一緒に記録する。
    pub fn add_assistant_message_with_tools(
        &mut self,
        content: impl Into<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) {
        self.messages.push(AgentMessage::assistant_with_tools(
            content, reasoning, tool_calls,
        ));
    }

    pub fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) {
        self.messages.push(AgentMessage::tool_result(
            tool_call_id,
            name,
            content,
            is_error,
        ));
    }

    pub fn to_provider_messages(&self) -> Vec<opencode_provider::Message> {
        self.messages
            .iter()
            .map(|m| match m.role {
                MessageRole::System => opencode_provider::Message::system(&m.content),
                MessageRole::User => opencode_provider::Message::user(&m.content),
                MessageRole::Assistant if m.tool_calls.is_empty() => {
                    opencode_provider::Message::assistant(&m.content)
                }
                MessageRole::Assistant => opencode_provider::Message {
                    role: opencode_provider::Role::Assistant,
                    content: opencode_provider::Content::Parts(
                        std::iter::once(opencode_provider::ContentPart {
                            content_type: "text".to_string(),
                            text: Some(m.content.clone()),
                            ..opencode_provider::ContentPart::default()
                        })
                        .chain(
                            m.tool_calls
                                .iter()
                                .map(|call| opencode_provider::ContentPart {
                                    content_type: "tool_use".to_string(),
                                    tool_use: Some(opencode_provider::ToolUse {
                                        id: call.id.clone(),
                                        name: call.name.clone(),
                                        input: call.arguments.clone(),
                                    }),
                                    ..opencode_provider::ContentPart::default()
                                }),
                        )
                        .collect(),
                    ),
                    cache_control: None,
                    provider_options: None,
                },
                // 旧永続化データの Tool メッセージは tool_call_id が空のため、
                // tool_result ブロックを送らず content を text として残す。
                MessageRole::Tool if m.tool_call_id.is_empty() => opencode_provider::Message {
                    role: opencode_provider::Role::Tool,
                    content: opencode_provider::Content::Text(m.content.clone()),
                    cache_control: None,
                    provider_options: None,
                },
                MessageRole::Tool => opencode_provider::Message {
                    role: opencode_provider::Role::Tool,
                    content: opencode_provider::Content::Parts(vec![
                        opencode_provider::ContentPart {
                            content_type: "tool_result".to_string(),
                            tool_result: Some(opencode_provider::ToolResult {
                                tool_use_id: m.tool_call_id.clone(),
                                content: m.content.clone(),
                                is_error: Some(m.is_error),
                            }),
                            ..opencode_provider::ContentPart::default()
                        },
                    ]),
                    cache_control: None,
                    provider_options: None,
                },
            })
            .collect()
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dummy_tool_call() -> ToolCall {
        ToolCall {
            id: "call-1".to_string(),
            name: "read".to_string(),
            arguments: json!({"path": "dummy.txt"}),
        }
    }

    #[test]
    fn constructors_set_expected_fields() {
        let system = AgentMessage::system("dummy-system");
        assert!(matches!(&system.role, MessageRole::System));
        assert_eq!(system.content, "dummy-system");
        assert_eq!(system.reasoning, None);
        assert!(system.tool_calls.is_empty());

        let user = AgentMessage::user("dummy-user");
        assert!(matches!(&user.role, MessageRole::User));
        assert_eq!(user.content, "dummy-user");
        assert_eq!(user.reasoning, None);
        assert!(user.tool_calls.is_empty());

        let assistant = AgentMessage::assistant("dummy-answer", Some("dummy-thought".to_string()));
        assert!(matches!(&assistant.role, MessageRole::Assistant));
        assert_eq!(assistant.content, "dummy-answer");
        assert_eq!(assistant.reasoning.as_deref(), Some("dummy-thought"));
        assert!(assistant.tool_calls.is_empty());

        let plain = AgentMessage::assistant("dummy-answer", None);
        assert_eq!(plain.reasoning, None);
        assert!(plain.tool_calls.is_empty());
    }

    #[test]
    fn assistant_with_tools_preserves_tool_calls() {
        let message = AgentMessage::assistant_with_tools(
            "dummy-answer",
            Some("dummy-thought".to_string()),
            vec![dummy_tool_call()],
        );

        assert!(matches!(&message.role, MessageRole::Assistant));
        assert_eq!(message.content, "dummy-answer");
        assert_eq!(message.reasoning.as_deref(), Some("dummy-thought"));
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].id, "call-1");
        assert_eq!(message.tool_calls[0].name, "read");
        assert_eq!(
            message.tool_calls[0].arguments,
            json!({"path": "dummy.txt"})
        );
    }

    #[test]
    fn tool_result_constructor_uses_tool_role() {
        let message = AgentMessage::tool_result("call-1", "read", "dummy-result", true);

        assert!(matches!(&message.role, MessageRole::Tool));
        assert_eq!(message.content, "dummy-result");
        assert_eq!(message.reasoning, None);
        assert!(message.tool_calls.is_empty());
        assert_eq!(message.tool_call_id, "call-1");
        assert_eq!(message.tool_name, "read");
        assert!(message.is_error);
    }

    #[test]
    fn to_provider_messages_maps_tool_result_to_content_parts() {
        let conversation = Conversation {
            messages: vec![
                AgentMessage::tool_result("call-42", "read", "dummy-failure", true),
                AgentMessage::tool_result("call-43", "write", "dummy-success", false),
            ],
        };

        let provider_messages = conversation.to_provider_messages();
        assert_eq!(provider_messages.len(), 2);

        let expectations = [("call-42", true), ("call-43", false)];
        for (message, (expected_id, expected_is_error)) in
            provider_messages.iter().zip(expectations)
        {
            assert!(matches!(&message.role, opencode_provider::Role::Tool));
            let parts = match &message.content {
                opencode_provider::Content::Parts(parts) => parts,
                other => panic!("expected Content::Parts, got {other:?}"),
            };
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].content_type, "tool_result");
            let result = parts[0]
                .tool_result
                .as_ref()
                .expect("tool_result should be set");
            assert_eq!(result.tool_use_id, expected_id);
            assert_eq!(result.is_error, Some(expected_is_error));
            assert!(result.content.starts_with("dummy-"));
        }
    }

    #[test]
    fn legacy_agent_message_json_without_tool_fields_deserializes() {
        let legacy = json!({
            "role": "Tool",
            "content": "dummy-result",
            "reasoning": null,
            "tool_calls": []
        });
        let message: AgentMessage =
            serde_json::from_value(legacy).expect("legacy AgentMessage should deserialize");

        assert!(matches!(&message.role, MessageRole::Tool));
        assert_eq!(message.content, "dummy-result");
        assert_eq!(message.tool_call_id, "");
        assert_eq!(message.tool_name, "");
        assert!(!message.is_error);
    }

    #[test]
    fn to_provider_messages_maps_assistant_tool_calls_to_content_parts() {
        let conversation = Conversation {
            messages: vec![AgentMessage::assistant_with_tools(
                "dummy-answer",
                None,
                vec![ToolCall {
                    id: "call-9".to_string(),
                    name: "read".to_string(),
                    arguments: json!({"path": "dummy.txt"}),
                }],
            )],
        };

        let provider_messages = conversation.to_provider_messages();
        assert_eq!(provider_messages.len(), 1);
        let message = &provider_messages[0];
        assert!(matches!(&message.role, opencode_provider::Role::Assistant));
        let parts = match &message.content {
            opencode_provider::Content::Parts(parts) => parts,
            other => panic!("expected Content::Parts, got {other:?}"),
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].content_type, "text");
        assert_eq!(parts[0].text.as_deref(), Some("dummy-answer"));
        assert_eq!(parts[1].content_type, "tool_use");
        let tool_use = parts[1].tool_use.as_ref().expect("tool_use should be set");
        assert_eq!(tool_use.id, "call-9");
        assert_eq!(tool_use.name, "read");
        assert_eq!(tool_use.input, json!({"path": "dummy.txt"}));
    }

    #[test]
    fn add_assistant_message_with_tools_keeps_plain_assistant_as_text() {
        let mut conversation = Conversation::new();
        conversation.add_assistant_message("plain", None);
        conversation.add_assistant_message_with_tools(
            "with-tools",
            Some("thought".to_string()),
            vec![dummy_tool_call()],
        );

        assert_eq!(conversation.messages.len(), 2);
        assert!(conversation.messages[0].tool_calls.is_empty());
        let message = &conversation.messages[1];
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].id, "call-1");

        let provider_messages = conversation.to_provider_messages();
        assert!(matches!(
            &provider_messages[0].content,
            opencode_provider::Content::Text(_)
        ));
        assert!(matches!(
            &provider_messages[1].content,
            opencode_provider::Content::Parts(_)
        ));
    }

    #[test]
    fn to_provider_messages_falls_back_to_text_for_legacy_tool_message() {
        let legacy = json!({
            "role": "Tool",
            "content": "dummy-result",
            "reasoning": null,
            "tool_calls": []
        });
        let message: AgentMessage =
            serde_json::from_value(legacy).expect("legacy AgentMessage should deserialize");
        let conversation = Conversation {
            messages: vec![message],
        };

        let provider_messages = conversation.to_provider_messages();
        assert_eq!(provider_messages.len(), 1);
        assert!(matches!(
            &provider_messages[0].role,
            opencode_provider::Role::Tool
        ));
        assert!(matches!(
            &provider_messages[0].content,
            opencode_provider::Content::Text(text) if text == "dummy-result"
        ));
    }

    #[test]
    fn agent_message_serde_roundtrip_preserves_fields() {
        let messages = [
            AgentMessage::system("dummy-system"),
            AgentMessage::user("dummy-user"),
            AgentMessage::assistant("dummy-answer", Some("dummy-thought".to_string())),
            AgentMessage::assistant_with_tools("dummy-answer", None, vec![dummy_tool_call()]),
            AgentMessage::tool_result("call-1", "read", "dummy-result", false),
        ];

        for message in messages {
            let serialized = serde_json::to_value(&message).expect("serialize AgentMessage");
            let restored: AgentMessage =
                serde_json::from_value(serialized.clone()).expect("deserialize AgentMessage");
            let reserialized = serde_json::to_value(&restored).expect("reserialize AgentMessage");

            assert_eq!(reserialized, serialized);
        }
    }

    #[test]
    fn serde_uses_unrenamed_field_and_variant_names() {
        let value = serde_json::to_value(AgentMessage::assistant_with_tools(
            "dummy-answer",
            None,
            vec![dummy_tool_call()],
        ))
        .expect("serialize AgentMessage");

        assert_eq!(value["role"], json!("Assistant"));
        assert_eq!(value["content"], json!("dummy-answer"));
        assert!(value["reasoning"].is_null());
        assert_eq!(value["tool_calls"][0]["id"], json!("call-1"));
        assert_eq!(
            value["tool_calls"][0]["arguments"]["path"],
            json!("dummy.txt")
        );

        let system =
            serde_json::to_value(AgentMessage::system("dummy-system")).expect("serialize system");
        assert_eq!(system["role"], json!("System"));
    }

    #[test]
    fn conversation_builders_append_messages_in_order() {
        let mut conversation = Conversation::with_system_prompt("dummy-system");
        conversation.add_user_message("dummy-user");
        conversation.add_assistant_message("dummy-answer", Some("dummy-thought".to_string()));
        conversation.add_tool_result("call-1", "read", "dummy-result", false);

        assert_eq!(conversation.messages.len(), 4);
        assert!(matches!(
            &conversation.messages[0].role,
            MessageRole::System
        ));
        assert_eq!(conversation.messages[0].content, "dummy-system");
        assert!(matches!(&conversation.messages[1].role, MessageRole::User));
        assert_eq!(conversation.messages[1].content, "dummy-user");
        assert!(matches!(
            &conversation.messages[2].role,
            MessageRole::Assistant
        ));
        assert_eq!(conversation.messages[2].content, "dummy-answer");
        assert!(matches!(&conversation.messages[3].role, MessageRole::Tool));
        assert_eq!(conversation.messages[3].content, "dummy-result");

        assert!(Conversation::new().messages.is_empty());
        assert!(Conversation::default().messages.is_empty());
    }

    #[test]
    fn conversation_serde_roundtrip_preserves_messages() {
        let mut conversation = Conversation::with_system_prompt("dummy-system");
        conversation.add_user_message("dummy-user");

        let serialized = serde_json::to_value(&conversation).expect("serialize Conversation");
        let restored: Conversation =
            serde_json::from_value(serialized.clone()).expect("deserialize Conversation");
        let reserialized = serde_json::to_value(&restored).expect("reserialize Conversation");

        assert_eq!(reserialized, serialized);
    }
}
