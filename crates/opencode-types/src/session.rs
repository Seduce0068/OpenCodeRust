use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// セッションの全体像を表す情報。メッセージ本文はシリアライズ対象外。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// セッションの ID。
    pub id: crate::identifier::SessionID,
    /// URL などに使われる短い識別名。
    pub slug: String,
    /// 所属するプロジェクトの ID。
    pub project_id: crate::project::ProjectID,
    /// セッションの作業ディレクトリ。
    pub directory: String,
    /// 親セッションの ID。子セッションの場合のみ `Some`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<crate::identifier::SessionID>,
    /// セッションのタイトル。
    pub title: String,
    /// セッションデータのフォーマットバージョン。
    pub version: String,
    /// 作成・更新などの時刻情報。
    pub time: crate::session::TimeInfo,
    /// セッションに含まれるメッセージ。シリアライズではスキップされる。
    #[serde(skip)]
    pub messages: Vec<crate::message::SessionMessage>,
    /// セッションの差分要約。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<crate::session::SessionSummary>,
    /// セッションの共有情報。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<crate::session::SessionShareInfo>,
    /// セッションの巻き戻し情報。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert: Option<crate::session::SessionRevert>,
    /// セッション固有の権限ルール。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<crate::session::PermissionRuleset>,
    /// セッションのトークン使用量。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::session::TokenUsage>,
    /// セッションの状態。
    #[serde(default)]
    pub status: crate::session::SessionStatus,
    /// セッションに付随する任意のメタデータ。
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
    /// 作成時刻。シリアライズではスキップされる。
    #[serde(default, skip_serializing)]
    pub created_at: DateTime<Utc>,
    /// 最終更新時刻。シリアライズではスキップされる。
    #[serde(default, skip_serializing)]
    pub updated_at: DateTime<Utc>,
}

/// セッション内でのファイル変更を集計した要約。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// 追加された行数。
    pub additions: u64,
    /// 削除された行数。
    pub deletions: u64,
    /// 変更されたファイル数。
    pub files: u64,
    /// ファイルごとの差分。
    pub diffs: Vec<FileDiff>,
}

/// 単一ファイルの追加・削除行数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    /// 変更されたファイルのパス。
    pub path: String,
    /// 追加された行数。
    pub additions: u64,
    /// 削除された行数。
    pub deletions: u64,
}

/// セッションのライフサイクル上の状態。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq, Default)]
#[sqlx(rename_all = "lowercase")]
pub enum SessionStatus {
    /// 現在進行中のセッション（デフォルト）。
    #[default]
    Active,
    /// 完了したセッション。
    Completed,
    /// アーカイブ済みのセッション。
    Archived,
    /// 会話の圧縮処理中。
    Compacting,
}

/// セッションの共有先を表す情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionShareInfo {
    /// 共有用の URL。
    pub url: String,
}

/// セッションのトークン使用量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// 入力トークン数。
    pub input: f64,
    /// 出力トークン数。
    pub output: f64,
    /// 推論トークン数。
    pub reasoning: f64,
    /// キャッシュの使用量。
    pub cache: CacheUsage,
}

/// トークンキャッシュの読み書き使用量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheUsage {
    /// キャッシュから読み出したトークン数。
    pub read: f64,
    /// キャッシュへ書き込んだトークン数。
    pub write: f64,
}

/// セッションを特定のメッセージ時点へ巻き戻すための情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRevert {
    /// 巻き戻し先のメッセージ ID。
    pub message_id: String,
    /// 巻き戻し先のメッセージ部分 ID。
    pub part_id: Option<String>,
    /// 復元するスナップショット。
    pub snapshot: Option<String>,
    /// 復元に使う差分。
    pub diff: Option<String>,
}

/// セッションに適用する許可・拒否のルール集合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRuleset {
    /// 許可する操作のリスト。
    pub allow: Vec<String>,
    /// 拒否する操作のリスト。
    pub deny: Vec<String>,
    /// ルールの適用モード。
    pub mode: Option<String>,
}

/// セッションの各種イベント時刻（ミリ秒単位の UNIX タイムスタンプ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeInfo {
    /// 作成時刻。
    pub created: u64,
    /// 最終更新時刻。
    pub updated: u64,
    /// 圧縮を開始した時刻。
    pub compacting: Option<u64>,
    /// アーカイブした時刻。
    pub archived: Option<u64>,
}

/// セッションへの入力を記録するデータ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInput {
    /// 入力の ID。
    pub id: String,
    /// 入力先のセッション ID。
    pub session_id: String,
    /// 入力されたプロンプト。
    pub prompt: String,
    /// 入力の配送方法。
    pub delivery: Option<String>,
    /// 入力に付随する任意のメタデータ。
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// 入力の処理状態を表す文字列。
    pub status: String,
    /// 入力が作成された時刻（ミリ秒単位の UNIX タイムスタンプ）。
    pub created_at: u64,
}
