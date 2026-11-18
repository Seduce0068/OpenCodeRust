use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub parent_id: Option<String>,
    pub share: Option<ShareInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareInfo {
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub finish: Option<String>,
    pub error: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cost: f64,
    pub tokens: TokenUsage,
    pub parts: Vec<MessagePart>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessagePart {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    File {
        path: String,
        mime: String,
    },
    Image {
        url: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        result: String,
        is_error: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct SessionContext {
    pub sessions: HashMap<String, Session>,
    pub messages: HashMap<String, Vec<Message>>,
    pub current_session_id: Option<String>,
    pub session_status: HashMap<String, SessionStatus>,
    pub session_diff: HashMap<String, Vec<DiffEntry>>,
    pub todos: HashMap<String, Vec<TodoItem>>,
    pub revert: HashMap<String, RevertInfo>,
}

#[derive(Clone, Debug, Default)]
pub enum SessionStatus {
    #[default]
    Idle,
    Running,
    Retrying {
        message: String,
        attempt: u32,
        next: i64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffEntry {
    pub file: String,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevertInfo {
    pub message_id: String,
    pub part_id: Option<String>,
    pub snapshot: Option<String>,
    pub diff: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl SessionContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_session(&self) -> Option<&Session> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
    }

    pub fn current_messages(&self) -> Vec<&Message> {
        self.current_session_id
            .as_ref()
            .and_then(|id| self.messages.get(id))
            .map(|m| m.iter().collect())
            .unwrap_or_default()
    }

    pub fn create_session(&mut self, title: Option<String>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let session = Session {
            id: id.clone(),
            title: title.unwrap_or_else(|| "New Session".to_string()),
            created_at: now,
            updated_at: now,
            parent_id: None,
            share: None,
        };
        self.sessions.insert(id.clone(), session);
        self.messages.insert(id.clone(), Vec::new());
        self.session_status.insert(id.clone(), SessionStatus::Idle);
        self.current_session_id = Some(id.clone());
        id
    }

    pub fn upsert_session(&mut self, session: Session) {
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        self.messages.entry(id.clone()).or_default();
        self.session_status
            .entry(id.clone())
            .or_insert(SessionStatus::Idle);
        self.current_session_id = Some(id);
    }

    pub fn set_messages(&mut self, session_id: &str, messages: Vec<Message>) {
        self.messages.insert(session_id.to_string(), messages);
    }

    pub fn add_message(&mut self, session_id: &str, message: Message) {
        if let Some(messages) = self.messages.get_mut(session_id) {
            messages.push(message);
        }
    }

    pub fn set_status(&mut self, session_id: &str, status: SessionStatus) {
        self.session_status.insert(session_id.to_string(), status);
    }

    pub fn status(&self, session_id: &str) -> &SessionStatus {
        self.session_status
            .get(session_id)
            .unwrap_or(&SessionStatus::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde::{Deserialize, Serialize};

    fn dummy_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn dummy_token_usage() -> TokenUsage {
        TokenUsage {
            input: 10,
            output: 20,
            reasoning: 3,
            cache_read: 4,
            cache_write: 5,
        }
    }

    fn dummy_session() -> Session {
        Session {
            id: "sess-1".to_string(),
            title: "テストセッション".to_string(),
            created_at: dummy_time(),
            updated_at: dummy_time(),
            parent_id: None,
            share: None,
        }
    }

    fn dummy_parts() -> Vec<MessagePart> {
        vec![
            MessagePart::Text {
                text: "text".to_string(),
            },
            MessagePart::Reasoning {
                text: "reason".to_string(),
            },
            MessagePart::File {
                path: "/tmp/a.txt".to_string(),
                mime: "text/plain".to_string(),
            },
            MessagePart::Image {
                url: "https://example.com/a.png".to_string(),
            },
            MessagePart::ToolCall {
                id: "call-1".to_string(),
                name: "read".to_string(),
                arguments: "{}".to_string(),
            },
            MessagePart::ToolResult {
                id: "call-1".to_string(),
                result: "ok".to_string(),
                is_error: true,
            },
        ]
    }

    fn dummy_message() -> Message {
        Message {
            id: "msg-1".to_string(),
            role: MessageRole::Assistant,
            content: "hello".to_string(),
            created_at: dummy_time(),
            agent: Some("build".to_string()),
            model: Some("test-model".to_string()),
            mode: Some("primary".to_string()),
            finish: Some("stop".to_string()),
            error: None,
            completed_at: Some(dummy_time()),
            cost: 0.125,
            tokens: dummy_token_usage(),
            parts: dummy_parts(),
        }
    }

    fn assert_serde_roundtrip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let before = serde_json::to_value(value).expect("serialize");
        let restored: T = serde_json::from_value(before.clone()).expect("deserialize");
        let after = serde_json::to_value(&restored).expect("re-serialize");
        assert_eq!(before, after, "serde roundtrip changed the value");
    }

    #[test]
    fn session_context_new_is_empty_and_uses_defaults() {
        let ctx = SessionContext::new();
        assert!(ctx.sessions.is_empty());
        assert!(ctx.messages.is_empty());
        assert!(ctx.session_status.is_empty());
        assert!(ctx.session_diff.is_empty());
        assert!(ctx.todos.is_empty());
        assert!(ctx.revert.is_empty());
        assert!(ctx.current_session_id.is_none());
        assert!(ctx.current_session().is_none());
        assert!(ctx.current_messages().is_empty());
        assert!(matches!(ctx.status("missing"), SessionStatus::Idle));
    }

    #[test]
    fn create_session_registers_defaults_and_sets_current() {
        let mut ctx = SessionContext::new();
        let id = ctx.create_session(None);

        assert!(!id.is_empty());
        assert_eq!(ctx.current_session_id.as_deref(), Some(id.as_str()));
        let session = ctx.current_session().expect("current session exists");
        assert_eq!(session.id, id);
        assert_eq!(session.title, "New Session");
        assert_eq!(session.created_at, session.updated_at);
        assert!(session.parent_id.is_none());
        assert!(session.share.is_none());
        assert!(ctx.current_messages().is_empty());
        assert!(matches!(ctx.status(&id), SessionStatus::Idle));
        assert!(ctx.messages.get(&id).expect("messages entry").is_empty());
    }

    #[test]
    fn create_session_uses_custom_title() {
        let mut ctx = SessionContext::new();
        let id = ctx.create_session(Some("Custom".to_string()));
        assert_eq!(
            ctx.current_session().expect("current session exists").title,
            "Custom"
        );
        assert!(ctx.sessions.contains_key(&id));
    }

    #[test]
    fn upsert_session_initializes_missing_entries_and_preserves_existing() {
        let mut ctx = SessionContext::new();
        ctx.upsert_session(dummy_session());

        assert_eq!(ctx.current_session_id.as_deref(), Some("sess-1"));
        assert!(ctx.messages.get("sess-1").expect("entry").is_empty());
        assert!(matches!(ctx.status("sess-1"), SessionStatus::Idle));

        ctx.set_messages("sess-1", vec![dummy_message()]);
        ctx.set_status(
            "sess-1",
            SessionStatus::Retrying {
                message: "retry".to_string(),
                attempt: 2,
                next: 1234,
            },
        );
        let mut updated = dummy_session();
        updated.title = "Renamed".to_string();
        ctx.upsert_session(updated);

        assert_eq!(ctx.current_session().expect("session").title, "Renamed");
        assert_eq!(ctx.messages.get("sess-1").expect("messages").len(), 1);
        match ctx.status("sess-1") {
            SessionStatus::Retrying {
                message,
                attempt,
                next,
            } => {
                assert_eq!(message, "retry");
                assert_eq!(*attempt, 2);
                assert_eq!(*next, 1234);
            }
            other => panic!("unexpected status: {:?}", other),
        }
    }

    #[test]
    fn set_messages_add_message_and_current_messages() {
        let mut ctx = SessionContext::new();
        let id = ctx.create_session(None);
        ctx.set_messages(&id, vec![dummy_message()]);
        assert_eq!(ctx.current_messages().len(), 1);

        let mut second = dummy_message();
        second.id = "msg-2".to_string();
        ctx.add_message(&id, second.clone());
        assert_eq!(ctx.current_messages().len(), 2);
        assert_eq!(ctx.current_messages()[1].id, "msg-2");

        ctx.add_message("unknown-session", second);
        assert!(!ctx.messages.contains_key("unknown-session"));
    }

    #[test]
    fn set_status_and_status_fallback() {
        let mut ctx = SessionContext::new();
        ctx.set_status("s", SessionStatus::Running);
        assert!(matches!(ctx.status("s"), SessionStatus::Running));
        assert!(matches!(ctx.status("other"), SessionStatus::Idle));
    }

    #[test]
    fn defaults_are_zero_and_idle() {
        let tokens = TokenUsage::default();
        assert_eq!(tokens.input, 0);
        assert_eq!(tokens.output, 0);
        assert_eq!(tokens.reasoning, 0);
        assert_eq!(tokens.cache_read, 0);
        assert_eq!(tokens.cache_write, 0);
        assert!(matches!(SessionStatus::default(), SessionStatus::Idle));
    }

    #[test]
    fn serde_roundtrip_core_data_types() {
        assert_serde_roundtrip(&dummy_session());
        assert_serde_roundtrip(&dummy_message());
        assert_serde_roundtrip(&dummy_token_usage());
        assert_eq!(MessageRole::User, MessageRole::User);
        assert_ne!(MessageRole::User, MessageRole::Assistant);
    }

    #[test]
    fn serde_roundtrip_every_message_part_variant() {
        for part in dummy_parts() {
            assert_serde_roundtrip(&part);
        }
    }

    #[test]
    fn serde_roundtrip_auxiliary_data_types() {
        assert_serde_roundtrip(&ShareInfo {
            url: "https://example.com/s".to_string(),
        });
        assert_serde_roundtrip(&DiffEntry {
            file: "src/main.rs".to_string(),
            additions: 3,
            deletions: 1,
        });
        assert_serde_roundtrip(&RevertInfo {
            message_id: "msg-1".to_string(),
            part_id: Some("part-1".to_string()),
            snapshot: None,
            diff: None,
        });
        for status in [
            TodoStatus::Pending,
            TodoStatus::InProgress,
            TodoStatus::Completed,
            TodoStatus::Cancelled,
        ] {
            assert_serde_roundtrip(&TodoItem {
                content: "task".to_string(),
                status,
            });
        }
    }

    // これらの型は内部状態用で wire format の rename 属性を持たない。
    // camelCase の別名（cacheRead 等）を持つのは api.rs のワイヤ型側であり、
    // ここでは snake_case のまま往復することを固定する（不整合を検出したら修正せず報告する）。
    #[test]
    fn wire_format_is_snake_case_without_camelcase_rename() {
        let tokens = serde_json::to_value(dummy_token_usage()).unwrap();
        assert!(tokens.get("cache_read").is_some());
        assert!(tokens.get("cache_write").is_some());
        assert!(tokens.get("cacheRead").is_none());
        assert!(tokens.get("cacheWrite").is_none());

        let session = serde_json::to_value(dummy_session()).unwrap();
        assert!(session.get("created_at").is_some());
        assert!(session.get("updated_at").is_some());
        assert!(session.get("parent_id").is_some());
        assert!(session.get("createdAt").is_none());
        assert!(session.get("updatedAt").is_none());
        assert!(session.get("parentId").is_none());

        let message = serde_json::to_value(dummy_message()).unwrap();
        assert!(message.get("created_at").is_some());
        assert!(message.get("completed_at").is_some());
        assert!(message.get("createdAt").is_none());
        assert!(message.get("completedAt").is_none());

        let tool_result = serde_json::to_value(MessagePart::ToolResult {
            id: "call-1".to_string(),
            result: "ok".to_string(),
            is_error: true,
        })
        .unwrap();
        let inner = tool_result.get("ToolResult").expect("externally tagged");
        assert_eq!(inner.get("is_error"), Some(&serde_json::json!(true)));
        assert!(inner.get("isError").is_none());

        assert_eq!(
            serde_json::to_value(MessageRole::User).unwrap(),
            serde_json::json!("User")
        );
    }
}
