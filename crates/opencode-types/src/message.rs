use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// セッション内で送受信される一つのメッセージ。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessage {
    /// メッセージの ID。
    pub id: String,
    /// 所属するセッションの ID。
    pub session_id: String,
    /// メッセージの送信者種別。
    pub role: MessageRole,
    /// メッセージを構成する部分のリスト。
    pub parts: Vec<MessagePart>,
    /// 作成時刻。
    pub created_at: DateTime<Utc>,
    /// メッセージに付随する任意のメタデータ。
    pub metadata: HashMap<String, serde_json::Value>,
}

/// メッセージの送信者種別。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MessageRole {
    /// ユーザーによるメッセージ。
    User,
    /// アシスタントによるメッセージ。
    Assistant,
    /// システムからのメッセージ。
    System,
    /// ツール実行に関するメッセージ。
    Tool,
}

/// メッセージを構成する個々の部分。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessagePart {
    /// 部分の ID。
    pub id: String,
    /// 部分の種別と内容。
    pub part_type: PartType,
    /// 作成時刻。
    pub created_at: DateTime<Utc>,
    /// 属するメッセージの ID。独立した部分の場合は `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// メッセージ部分の種別と、それぞれの種別が保持する内容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PartType {
    /// プレーンテキスト。
    Text {
        /// テキスト本文。
        text: String,
    },
    /// ツールの呼び出し要求。
    ToolCall {
        /// ツール呼び出しの ID。
        id: String,
        /// 呼び出すツールの名前。
        name: String,
        /// ツールに渡す入力。
        input: serde_json::Value,
    },
    /// ツールの実行結果。
    ToolResult {
        /// 対応するツール呼び出しの ID。
        tool_call_id: String,
        /// ツールの出力内容。
        content: String,
        /// 実行が失敗したかどうか。
        is_error: bool,
    },
    /// モデルの推論内容。
    Reasoning {
        /// 推論のテキスト。
        text: String,
    },
    /// 添付ファイル。
    File {
        /// ファイルの URL。
        url: String,
        /// ファイル名。
        filename: String,
        /// ファイルの MIME タイプ。
        mime: String,
    },
    /// ステップの開始。
    StepStart {
        /// ステップの ID。
        id: String,
        /// ステップの名前。
        name: String,
    },
    /// ステップの終了。
    StepFinish {
        /// ステップの ID。
        id: String,
        /// ステップの出力。
        output: Option<String>,
    },
    /// ある時点の状態を記録したスナップショット。
    Snapshot {
        /// スナップショットの内容。
        content: String,
    },
    /// 文字列の置換を表すパッチ。
    Patch {
        /// 置換対象の文字列。
        old_string: String,
        /// 置換後の文字列。
        new_string: String,
        /// 対象ファイルのパス。
        filepath: String,
    },
    /// エージェントの状態変化。
    Agent {
        /// エージェントの名前。
        name: String,
        /// エージェントの状態を表す文字列。
        status: String,
    },
    /// サブタスクの状態変化。
    Subtask {
        /// サブタスクの ID。
        id: String,
        /// サブタスクの説明。
        description: String,
        /// サブタスクの状態を表す文字列。
        status: String,
    },
    /// リトライの発生。
    Retry {
        /// リトライ回数。
        count: u32,
        /// リトライの理由。
        reason: String,
    },
    /// 会話の圧縮（コンパクション）。
    Compaction {
        /// 圧縮後に残された要約。
        summary: String,
    },
}

impl SessionMessage {
    /// 指定したセッションに属するユーザーメッセージを、テキスト部分を一つ持つ形で生成する。
    pub fn user(session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: format!("msg_{}", uuid::Uuid::new_v4()),
            session_id: session_id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart {
                id: format!("prt_{}", uuid::Uuid::new_v4()),
                part_type: PartType::Text { text: text.into() },
                created_at: Utc::now(),
                message_id: None,
            }],
            created_at: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    /// 指定したセッションに属する空のアシスタントメッセージを生成する。
    pub fn assistant(session_id: impl Into<String>) -> Self {
        Self {
            id: format!("msg_{}", uuid::Uuid::new_v4()),
            session_id: session_id.into(),
            role: MessageRole::Assistant,
            parts: Vec::new(),
            created_at: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    /// メッセージ内のテキスト部分をすべて連結して返す。
    pub fn get_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match &p.part_type {
                PartType::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_part(text: &str) -> MessagePart {
        MessagePart {
            id: "prt_fixed".to_string(),
            part_type: PartType::Text {
                text: text.to_string(),
            },
            created_at: Utc::now(),
            message_id: None,
        }
    }

    #[test]
    fn user_constructor_builds_single_text_part() {
        let msg = SessionMessage::user("sess_1", "hello");

        assert!(msg.id.starts_with("msg_"));
        assert_eq!(msg.session_id, "sess_1");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.parts.len(), 1);
        assert!(msg.parts[0].id.starts_with("prt_"));
        assert_eq!(
            msg.parts[0].part_type,
            PartType::Text {
                text: "hello".to_string()
            }
        );
        assert!(msg.parts[0].message_id.is_none());
        assert!(msg.metadata.is_empty());
        assert_eq!(msg.get_text(), "hello");
    }

    #[test]
    fn assistant_constructor_starts_with_no_parts() {
        let msg = SessionMessage::assistant("sess_2");

        assert!(msg.id.starts_with("msg_"));
        assert_eq!(msg.session_id, "sess_2");
        assert_eq!(msg.role, MessageRole::Assistant);
        assert!(msg.parts.is_empty());
        assert!(msg.metadata.is_empty());
        assert_eq!(msg.get_text(), "");
    }

    #[test]
    fn get_text_concatenates_only_text_parts_in_order() {
        let mut msg = SessionMessage::assistant("sess_3");
        msg.parts.push(text_part("foo"));
        msg.parts.push(MessagePart {
            id: "prt_reason".to_string(),
            part_type: PartType::Reasoning {
                text: "hidden".to_string(),
            },
            created_at: Utc::now(),
            message_id: None,
        });
        msg.parts.push(text_part("bar"));

        assert_eq!(msg.get_text(), "foobar");
    }

    #[test]
    fn session_message_serde_roundtrip() {
        let msg = SessionMessage::user("sess_4", "roundtrip");
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: SessionMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, msg);
    }

    #[test]
    fn message_role_wire_format_has_no_serde_rename() {
        // MessageRole には serde rename が無いため、バリアント名がそのまま使われる
        assert_eq!(
            serde_json::to_value(MessageRole::User).unwrap(),
            json!("User")
        );
        assert_eq!(
            serde_json::to_value(MessageRole::Assistant).unwrap(),
            json!("Assistant")
        );
        assert_eq!(
            serde_json::to_value(MessageRole::System).unwrap(),
            json!("System")
        );
        assert_eq!(
            serde_json::to_value(MessageRole::Tool).unwrap(),
            json!("Tool")
        );

        assert_eq!(
            serde_json::from_value::<MessageRole>(json!("User")).unwrap(),
            MessageRole::User
        );
    }

    #[test]
    fn part_type_tag_wire_format_is_camel_case() {
        // #[serde(tag = "type", rename_all = "camelCase")] によりタグ値は camelCase
        let cases = [
            (PartType::Text { text: "t".into() }, "text"),
            (
                PartType::ToolCall {
                    id: "c".into(),
                    name: "n".into(),
                    input: json!({}),
                },
                "toolCall",
            ),
            (
                PartType::ToolResult {
                    tool_call_id: "tc".into(),
                    content: "ok".into(),
                    is_error: false,
                },
                "toolResult",
            ),
            (PartType::Reasoning { text: "r".into() }, "reasoning"),
            (
                PartType::File {
                    url: "u".into(),
                    filename: "f".into(),
                    mime: "m".into(),
                },
                "file",
            ),
            (
                PartType::StepStart {
                    id: "s".into(),
                    name: "n".into(),
                },
                "stepStart",
            ),
            (
                PartType::StepFinish {
                    id: "s".into(),
                    output: None,
                },
                "stepFinish",
            ),
            (
                PartType::Snapshot {
                    content: "c".into(),
                },
                "snapshot",
            ),
            (
                PartType::Patch {
                    old_string: "a".into(),
                    new_string: "b".into(),
                    filepath: "f".into(),
                },
                "patch",
            ),
            (
                PartType::Agent {
                    name: "n".into(),
                    status: "s".into(),
                },
                "agent",
            ),
            (
                PartType::Subtask {
                    id: "st".into(),
                    description: "d".into(),
                    status: "s".into(),
                },
                "subtask",
            ),
            (
                PartType::Retry {
                    count: 1,
                    reason: "r".into(),
                },
                "retry",
            ),
            (
                PartType::Compaction {
                    summary: "s".into(),
                },
                "compaction",
            ),
        ];

        for (part, expected_tag) in cases {
            let value = serde_json::to_value(&part).unwrap();
            assert_eq!(value["type"], json!(expected_tag));
        }
    }

    #[test]
    fn part_type_inner_fields_stay_snake_case() {
        // rename_all はバリアント名にのみ適用され、フィールド名は snake_case のまま
        let part = PartType::ToolResult {
            tool_call_id: "tc_1".to_string(),
            content: "ok".to_string(),
            is_error: false,
        };
        assert_eq!(
            serde_json::to_value(&part).unwrap(),
            json!({
                "type": "toolResult",
                "tool_call_id": "tc_1",
                "content": "ok",
                "is_error": false,
            })
        );

        let decoded: PartType = serde_json::from_value(json!({
            "type": "toolResult",
            "tool_call_id": "tc_1",
            "content": "ok",
            "is_error": false,
        }))
        .unwrap();
        assert_eq!(decoded, part);
    }

    #[test]
    fn message_part_omits_none_message_id() {
        let part = text_part("solo");
        let value = serde_json::to_value(&part).unwrap();

        assert!(value.get("message_id").is_none());

        let with_parent = MessagePart {
            message_id: Some("msg_1".to_string()),
            ..text_part("child")
        };
        let value = serde_json::to_value(&with_parent).unwrap();
        assert_eq!(value["message_id"], json!("msg_1"));
    }
}
