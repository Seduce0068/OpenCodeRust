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
///
/// JSON 表現は小文字（`active` / `completed` / `archived` / `compacting`）を正式とし、
/// 旧 PascalCase（`Active` 等）は `alias` で読み取り互換を維持する。
/// `#[sqlx(rename_all = "lowercase")]` は DB 側の表現を決めるだけなので、
/// この変更で既存の DB 行の読み書きは変わらない。
///
/// 方針メモ: `opencode-server::routes` の `opencode_session::SessionStatus` を
/// 小文字文字列へ変換する手動 match は、変換元が別クレートの別型であるため
/// 本次スコープ外で残置する（routes.rs は担当外）。session 側にも同じ serde 属性を
/// 付与できれば手動 match を `serde_json::to_value` ベースへ置き換えて一元化できる。
/// 本クレート側の変換は enum を直接 match しており、文字列の再変換は行わないため
/// 二重変換は発生しない。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum SessionStatus {
    /// 現在進行中のセッション（デフォルト）。
    #[default]
    #[serde(alias = "Active")]
    Active,
    /// 完了したセッション。
    #[serde(alias = "Completed")]
    Completed,
    /// アーカイブ済みのセッション。
    #[serde(alias = "Archived")]
    Archived,
    /// 会話の圧縮処理中。
    #[serde(alias = "Compacting")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_time() -> TimeInfo {
        TimeInfo {
            created: 1_000,
            updated: 2_000,
            compacting: None,
            archived: Some(3_000),
        }
    }

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let decoded: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn session_status_default_and_wire_format() {
        assert_eq!(SessionStatus::default(), SessionStatus::Active);

        // serde 側も #[serde(rename_all = "lowercase")] となり、JSON では小文字になる。
        assert_eq!(
            serde_json::to_value(SessionStatus::Active).unwrap(),
            json!("active")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Completed).unwrap(),
            json!("completed")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Archived).unwrap(),
            json!("archived")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Compacting).unwrap(),
            json!("compacting")
        );

        // 正規表現の小文字は当然デシリアライズできる。
        assert_eq!(
            serde_json::from_value::<SessionStatus>(json!("active")).unwrap(),
            SessionStatus::Active
        );
    }

    #[test]
    fn session_status_accepts_legacy_pascalcase_aliases() {
        // 旧 PascalCase の永続化データ / 外部 JSON も alias で受け付ける。
        let cases = [
            (json!("Active"), SessionStatus::Active),
            (json!("Completed"), SessionStatus::Completed),
            (json!("Archived"), SessionStatus::Archived),
            (json!("Compacting"), SessionStatus::Compacting),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                serde_json::from_value::<SessionStatus>(raw).unwrap(),
                expected
            );
        }

        // 小文字・PascalCase どちらから読んでもシリアライズ結果は小文字に正規化される。
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<SessionStatus>(json!("Active")).unwrap())
                .unwrap(),
            json!("active")
        );
    }

    #[test]
    fn session_value_structs_roundtrip() {
        roundtrip(&sample_time());
        roundtrip(&SessionSummary {
            additions: 3,
            deletions: 1,
            files: 2,
            diffs: vec![FileDiff {
                path: "src/main.rs".to_string(),
                additions: 3,
                deletions: 1,
            }],
        });
        roundtrip(&SessionShareInfo {
            url: "https://example.com/share".to_string(),
        });
        roundtrip(&TokenUsage {
            input: 1.5,
            output: 2.5,
            reasoning: 0.5,
            cache: CacheUsage {
                read: 10.0,
                write: 20.0,
            },
        });
        roundtrip(&SessionRevert {
            message_id: "msg_1".to_string(),
            part_id: Some("prt_1".to_string()),
            snapshot: None,
            diff: Some("-old\n+new".to_string()),
        });
        roundtrip(&PermissionRuleset {
            allow: vec!["read".to_string()],
            deny: vec!["write".to_string()],
            mode: Some("strict".to_string()),
        });
        roundtrip(&SessionInput {
            id: "in_1".to_string(),
            session_id: "sess_1".to_string(),
            prompt: "hi".to_string(),
            delivery: None,
            metadata: Some(json!({"source": "test"})),
            status: "pending".to_string(),
            created_at: 1_700_000_000_000,
        });
    }

    #[test]
    fn session_serialize_skips_transient_fields() {
        let session = Session {
            id: crate::identifier::SessionID("sess_1".to_string()),
            slug: "my-session".to_string(),
            project_id: crate::project::ProjectID("proj_1".to_string()),
            directory: "/work".to_string(),
            parent_id: None,
            title: "Title".to_string(),
            version: "1".to_string(),
            time: sample_time(),
            messages: vec![crate::message::SessionMessage::user("sess_1", "hi")],
            summary: None,
            share: None,
            revert: None,
            permission: None,
            usage: None,
            status: SessionStatus::Active,
            metadata: HashMap::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let value = serde_json::to_value(&session).unwrap();

        // #[serde(skip)] / skip_serializing のフィールドは JSON に現れない
        assert!(value.get("messages").is_none());
        assert!(value.get("created_at").is_none());
        assert!(value.get("updated_at").is_none());
        // None の Option は skip_serializing_if で省略される
        assert!(value.get("parent_id").is_none());
        assert!(value.get("summary").is_none());
        // status / metadata は #[serde(default)] だがシリアライズはされる
        assert_eq!(value["status"], json!("active"));
        assert_eq!(value["metadata"], json!({}));
        assert_eq!(value["id"], json!("sess_1"));
        assert_eq!(value["project_id"], json!("proj_1"));
        assert_eq!(value["time"]["created"], json!(1_000));

        // デシリアライズ時は skip 対象がデフォルト値に戻る
        let decoded: Session = serde_json::from_value(value.clone()).unwrap();
        assert!(decoded.messages.is_empty());
        assert_eq!(decoded.id, session.id);
        assert_eq!(decoded.project_id, session.project_id);
        assert_eq!(decoded.time, session.time);
        assert_eq!(decoded.status, SessionStatus::Active);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    }

    #[test]
    fn session_deserialize_applies_defaults_for_absent_fields() {
        let value = json!({
            "id": "sess_x",
            "slug": "slug",
            "project_id": "proj_x",
            "directory": "/d",
            "title": "T",
            "version": "1",
            "time": { "created": 1, "updated": 2, "compacting": null, "archived": null }
        });

        let session: Session = serde_json::from_value(value).unwrap();

        assert_eq!(session.status, SessionStatus::Active);
        assert!(session.metadata.is_empty());
        assert!(session.messages.is_empty());
        assert!(session.parent_id.is_none());
        assert!(session.summary.is_none());
        assert!(session.share.is_none());
        assert!(session.revert.is_none());
        assert!(session.permission.is_none());
        assert!(session.usage.is_none());
    }
}
