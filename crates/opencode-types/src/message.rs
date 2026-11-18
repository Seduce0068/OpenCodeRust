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
