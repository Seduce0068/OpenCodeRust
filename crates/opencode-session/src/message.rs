use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::message_v2::FilePart;

fn default_message_created_at() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

/// Usage statistics for a single message
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    #[serde(default = "default_message_created_at")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MessageUsage>,
}

/// メッセージの送信者種別。
///
/// JSON 表現は小文字（`user` / `assistant` / `system` / `tool`）を正式とし、
/// 旧 PascalCase（`User` 等）は `alias` で読み取り互換を維持する。
/// `opencode-types` 側の `MessageRole` と wire 形式を揃える。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    #[serde(alias = "User")]
    User,
    #[serde(alias = "Assistant")]
    Assistant,
    #[serde(alias = "System")]
    System,
    #[serde(alias = "Tool")]
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePart {
    pub id: String,
    pub part_type: PartType,
    #[serde(default = "default_message_created_at")]
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PartType {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        synthetic: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ignored: Option<bool>,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
        is_error: bool,
        /// provider が ToolResult に添付したファイル。`None` のときは wire に出しない。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<FilePart>>,
    },

    Reasoning {
        text: String,
    },
    File {
        url: String,
        filename: String,
        mime: String,
    },
    StepStart {
        id: String,
        name: String,
    },
    StepFinish {
        id: String,
        output: Option<String>,
    },
    Snapshot {
        content: String,
    },
    Patch {
        old_string: String,
        new_string: String,
        filepath: String,
    },
    Agent {
        name: String,
        status: String,
    },
    Subtask {
        id: String,
        description: String,
        status: String,
    },
    Retry {
        count: u32,
        reason: String,
    },
    Compaction {
        summary: String,
    },
}

impl SessionMessage {
    pub fn user(session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: format!("msg_{}", uuid::Uuid::new_v4()),
            session_id: session_id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart {
                id: format!("prt_{}", uuid::Uuid::new_v4()),
                part_type: PartType::Text {
                    text: text.into(),
                    synthetic: None,
                    ignored: None,
                },
                created_at: Utc::now(),
                message_id: None,
            }],
            created_at: Utc::now(),
            metadata: HashMap::new(),
            usage: None,
        }
    }

    pub fn assistant(session_id: impl Into<String>) -> Self {
        Self {
            id: format!("msg_{}", uuid::Uuid::new_v4()),
            session_id: session_id.into(),
            role: MessageRole::Assistant,
            parts: Vec::new(),
            created_at: Utc::now(),
            metadata: HashMap::new(),
            usage: None,
        }
    }

    pub fn add_text(&mut self, text: impl Into<String>) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::Text {
                text: text.into(),
                synthetic: None,
                ignored: None,
            },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn add_reasoning(&mut self, text: impl Into<String>) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::Reasoning { text: text.into() },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn add_tool_call(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::ToolCall {
                id: id.into(),
                name: name.into(),
                input,
            },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::ToolResult {
                tool_call_id: tool_call_id.into(),
                content: content.into(),
                is_error,
                attachments: None,
            },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn add_file(
        &mut self,
        url: impl Into<String>,
        filename: impl Into<String>,
        mime: impl Into<String>,
    ) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::File {
                url: url.into(),
                filename: filename.into(),
                mime: mime.into(),
            },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn add_agent(&mut self, name: impl Into<String>) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::Agent {
                name: name.into(),
                status: "pending".to_string(),
            },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn add_subtask(&mut self, id: impl Into<String>, description: impl Into<String>) {
        self.parts.push(MessagePart {
            id: format!("prt_{}", uuid::Uuid::new_v4()),
            part_type: PartType::Subtask {
                id: id.into(),
                description: description.into(),
                status: "pending".to_string(),
            },
            created_at: Utc::now(),
            message_id: None,
        });
    }

    pub fn get_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match &p.part_type {
                PartType::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn get_reasoning(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match &p.part_type {
                PartType::Reasoning { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_has_one_text_part() {
        let msg = SessionMessage::user("ses_1", "hello");
        assert_eq!(msg.session_id, "ses_1");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.parts.len(), 1);
        assert_eq!(msg.get_text(), "hello");
        assert!(msg.id.starts_with("msg_"));
    }

    #[test]
    fn message_role_serde_lowercase_and_pascal_alias() {
        let cases = [
            (MessageRole::User, "user", "User"),
            (MessageRole::Assistant, "assistant", "Assistant"),
            (MessageRole::System, "system", "System"),
            (MessageRole::Tool, "tool", "Tool"),
        ];

        for (role, lower, pascal) in cases {
            // 新 wire 形式は小文字。
            assert_eq!(
                serde_json::to_value(&role).expect("role must serialize"),
                serde_json::json!(lower)
            );
            // 小文字は往復復元できる。
            let from_lower: MessageRole =
                serde_json::from_value(serde_json::json!(lower)).expect("lowercase must decode");
            assert_eq!(from_lower, role);
            // 旧 PascalCase も alias で復元できる。
            let from_pascal: MessageRole =
                serde_json::from_value(serde_json::json!(pascal)).expect("alias must decode");
            assert_eq!(from_pascal, role);
        }
    }

    #[test]
    fn assistant_message_starts_empty() {
        let msg = SessionMessage::assistant("ses_1");
        assert_eq!(msg.role, MessageRole::Assistant);
        assert!(msg.parts.is_empty());
    }

    #[test]
    fn add_text_and_reasoning_are_collected_by_getters() {
        let mut msg = SessionMessage::assistant("ses_1");
        msg.add_text("hello ");
        msg.add_reasoning("thinking");
        msg.add_text("world");

        assert_eq!(msg.get_text(), "hello world");
        assert_eq!(msg.get_reasoning(), "thinking");
        assert_eq!(msg.parts.len(), 3);
    }

    #[test]
    fn add_tool_call_and_result() {
        let mut msg = SessionMessage::assistant("ses_1");
        msg.add_tool_call("call_1", "bash", serde_json::json!({ "command": "ls" }));
        msg.add_tool_result("call_1", "output", false);

        assert_eq!(msg.parts.len(), 2);
        assert_eq!(msg.get_text(), "");
    }

    #[test]
    fn add_file_agent_and_subtask() {
        let mut msg = SessionMessage::assistant("ses_1");
        msg.add_file("https://example.com/a.png", "a.png", "image/png");
        msg.add_agent("build");
        msg.add_subtask("task_1", "investigate");

        assert_eq!(msg.parts.len(), 3);
    }

    #[test]
    fn legacy_json_without_metadata_or_created_at_is_accepted() {
        let value = serde_json::json!({
            "id": "msg_1",
            "session_id": "ses_1",
            "role": "user",
            "parts": [],
        });
        let msg: SessionMessage = serde_json::from_value(value).expect("legacy JSON must load");
        assert!(msg.metadata.is_empty());
        assert_eq!(msg.created_at, DateTime::<Utc>::UNIX_EPOCH);
        assert!(msg.usage.is_none());
    }

    #[test]
    fn legacy_json_part_without_created_at_is_accepted() {
        // 旧メッセージ JSON では part に created_at が無い場合がある。
        let value = serde_json::json!({
            "id": "msg_1",
            "session_id": "ses_1",
            "role": "user",
            "parts": [{
                "id": "prt_1",
                "part_type": { "type": "text", "text": "hello" }
            }]
        });
        let msg: SessionMessage =
            serde_json::from_value(value).expect("legacy part JSON must load");
        assert_eq!(msg.parts.len(), 1);
        assert_eq!(msg.parts[0].created_at, DateTime::<Utc>::UNIX_EPOCH);
        assert!(msg.parts[0].message_id.is_none());

        // 現行 JSON の出力形は不変（created_at は従来どおり出力される）。
        let value = serde_json::to_value(&msg).expect("must serialize");
        assert!(value["parts"][0].get("created_at").is_some());
        let roundtrip: SessionMessage = serde_json::from_value(value).expect("roundtrip must load");
        assert_eq!(roundtrip.parts[0].created_at, msg.parts[0].created_at);
        assert_eq!(roundtrip.get_text(), "hello");
    }

    #[test]
    fn part_ids_are_unique() {
        let mut msg = SessionMessage::assistant("ses_1");
        msg.add_text("a");
        msg.add_text("b");
        assert_ne!(msg.parts[0].id, msg.parts[1].id);
    }
}
