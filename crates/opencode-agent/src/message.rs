use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub role: MessageRole,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
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
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            reasoning: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>, reasoning: Option<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            reasoning,
            tool_calls: Vec::new(),
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
        }
    }

    // ... (rest of methods)

    pub fn tool_result(
        _tool_call_id: impl Into<String>,
        _name: impl Into<String>,
        content: impl Into<String>,
        _is_error: bool,
    ) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            reasoning: None,
            tool_calls: vec![],
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
                MessageRole::Assistant => opencode_provider::Message::assistant(&m.content),
                MessageRole::Tool => opencode_provider::Message::assistant(&m.content),
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
