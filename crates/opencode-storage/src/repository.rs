use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json;
use sqlx::{Executor, FromRow, SqlitePool};
use std::collections::HashMap;

use opencode_types::{
    FileDiff, MessagePart, MessageRole, Session, SessionMessage, SessionStatus, SessionSummary,
};

use crate::database::DatabaseError;

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct SessionRow {
    id: String,
    project_id: String,
    parent_id: Option<String>,
    slug: String,
    directory: String,
    title: String,
    version: String,
    share_url: Option<String>,
    /// sessions.metadata の JSON 文字列。旧行は NULL（空 HashMap へフォールバック）。
    metadata: Option<String>,
    summary_additions: Option<i64>,
    summary_deletions: Option<i64>,
    summary_files: Option<i64>,
    summary_diffs: Option<String>,
    revert: Option<String>,
    permission: Option<String>,
    usage_input_tokens: Option<i64>,
    usage_output_tokens: Option<i64>,
    usage_reasoning_tokens: Option<i64>,
    usage_cache_write_tokens: Option<i64>,
    usage_cache_read_tokens: Option<i64>,
    usage_total_cost: Option<f64>,
    status: SessionStatus,
    created_at: i64,
    updated_at: i64,
    time_compacting: Option<i64>,
    time_archived: Option<i64>,
}

impl SessionRow {
    /// 保存済み JSON 列を復元する。破損時は黙って捨てず `CorruptData` を返す。
    ///
    /// 特に `permission` を `.ok()` で None にすると権限制限が無い状態として
    /// 扱われ（fail-open）、破損した設定がセキュリティ上安全に失われるため、
    /// 読み取り側で明示的に失敗させる。
    fn decode_json_field<T>(field: &str, raw: Option<&str>) -> Result<Option<T>, DatabaseError>
    where
        T: serde::de::DeserializeOwned,
    {
        match raw {
            None => Ok(None),
            Some(value) => serde_json::from_str(value).map(Some).map_err(|e| {
                tracing::warn!(
                    field = %field,
                    error = %e,
                    "failed to parse stored session JSON field"
                );
                DatabaseError::CorruptData(format!("session has invalid {field} JSON: {e}"))
            }),
        }
    }

    /// `summary_additions` / `summary_deletions` / `summary_files` / `summary_diffs`
    /// を `SessionSummary` に復元する。
    ///
    /// `summary_diffs` が NULL でカウントも 0 の行は旧スキーマ／デフォルト値の
    /// 「要約なし」として扱う。カウントだけが非零で diffs が欠けた行は復元
    /// できないため fail-loud とし、黙って要約を失わないようにする。
    fn decode_summary(&self) -> Result<Option<SessionSummary>, DatabaseError> {
        let additions = self.summary_additions.unwrap_or(0);
        let deletions = self.summary_deletions.unwrap_or(0);
        let files = self.summary_files.unwrap_or(0);

        match self.summary_diffs.as_deref() {
            None => {
                if additions == 0 && deletions == 0 && files == 0 {
                    return Ok(None);
                }
                tracing::warn!("session summary counts exist but summary_diffs is missing");
                Err(DatabaseError::CorruptData(
                    "session has summary counts but no summary_diffs JSON".to_string(),
                ))
            }
            Some(raw) => {
                let diffs: Vec<FileDiff> = serde_json::from_str(raw).map_err(|e| {
                    tracing::warn!(
                        error = %e,
                        "failed to parse stored summary_diffs"
                    );
                    DatabaseError::CorruptData(format!(
                        "session has invalid summary_diffs JSON: {e}"
                    ))
                })?;
                Ok(Some(SessionSummary {
                    additions: additions.max(0) as u64,
                    deletions: deletions.max(0) as u64,
                    files: files.max(0) as u64,
                    diffs,
                }))
            }
        }
    }

    /// 行を `Session` に復元する。破損列があった場合は呼び出し側へ伝播する（fail-loud）。
    fn into_session(self) -> Result<Session, DatabaseError> {
        self.into_session_with_field().map_err(|(_, error)| error)
    }

    /// `into_session` と同じ復元を行うが、破損時にフィールド名も一緒に返す。
    ///
    /// 起動時一覧 (`SessionRepository::list_skipping_corrupt`) が warn に
    /// フィールド名だけを記録するために使う。エラー詳細や設定値・トークン値は
    /// ログに出さない。
    fn into_session_with_field(self) -> Result<Session, (Option<&'static str>, DatabaseError)> {
        let summary = self
            .decode_summary()
            .map_err(|error| (Some("summary_diffs"), error))?;
        let revert = Self::decode_json_field("revert", self.revert.as_deref())
            .map_err(|error| (Some("revert"), error))?;
        let permission = Self::decode_json_field("permission", self.permission.as_deref())
            .map_err(|error| (Some("permission"), error))?;
        // metadata=NULL の旧行は空 HashMap にフォールバックする。
        // 破損 JSON は decode_json_field が CorruptData を返すため fail-loud。
        let metadata: HashMap<String, serde_json::Value> =
            Self::decode_json_field("metadata", self.metadata.as_deref())
                .map_err(|error| (Some("metadata"), error))?
                .unwrap_or_default();

        Ok(Session {
            id: opencode_types::SessionID(self.id),
            slug: self.slug,
            project_id: opencode_types::ProjectID(self.project_id),
            directory: self.directory,
            parent_id: self.parent_id.map(opencode_types::SessionID),
            title: self.title,
            version: self.version.to_string(),
            time: opencode_types::TimeInfo {
                created: self.created_at as u64,
                updated: self.updated_at as u64,
                compacting: self.time_compacting.map(|t| t as u64),
                archived: self.time_archived.map(|t| t as u64),
            },
            messages: Vec::new(),
            summary,
            share: self
                .share_url
                .map(|url| opencode_types::SessionShareInfo { url }),
            revert,
            permission,
            usage: Some(opencode_types::TokenUsage {
                input: self.usage_input_tokens.unwrap_or(0) as f64,
                output: self.usage_output_tokens.unwrap_or(0) as f64,
                reasoning: self.usage_reasoning_tokens.unwrap_or(0) as f64,
                cache: opencode_types::CacheUsage {
                    read: self.usage_cache_read_tokens.unwrap_or(0) as f64,
                    write: self.usage_cache_write_tokens.unwrap_or(0) as f64,
                },
            }),
            status: self.status,
            metadata,
            created_at: chrono::DateTime::from_timestamp_millis(self.created_at)
                .unwrap_or_else(chrono::Utc::now),
            updated_at: chrono::DateTime::from_timestamp_millis(self.updated_at)
                .unwrap_or_else(chrono::Utc::now),
        })
    }
}

pub struct SessionRepository {
    pool: SqlitePool,
}

impl SessionRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, session: &Session) -> Result<(), DatabaseError> {
        let summary_diffs = session
            .summary
            .as_ref()
            .and_then(|s| serde_json::to_string(&s.diffs).ok());
        let metadata_json = serde_json::to_string(&session.metadata)
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        let revert_json = session.revert.as_ref().map(sqlx::types::Json);
        let permission_json = session.permission.as_ref().map(sqlx::types::Json);
        let share_url = session.share.as_ref().map(|s| s.url.clone());

        sqlx::query(
            r#"
            INSERT INTO sessions (
                id, project_id, parent_id, slug, directory, title, version, share_url,
                metadata,
                summary_additions, summary_deletions, summary_files, summary_diffs,
                revert, permission,
                usage_input_tokens, usage_output_tokens, usage_reasoning_tokens,
                usage_cache_write_tokens, usage_cache_read_tokens, usage_total_cost,
                status, created_at, updated_at, time_compacting, time_archived
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(session.id.0.clone())
        .bind(session.project_id.0.clone())
        .bind(session.parent_id.as_ref().map(|id| id.0.clone()))
        .bind(&session.slug)
        .bind(&session.directory)
        .bind(&session.title)
        .bind(&session.version)
        .bind(share_url)
        .bind(&metadata_json)
        .bind(session.summary.as_ref().map(|s| s.additions as i64))
        .bind(session.summary.as_ref().map(|s| s.deletions as i64))
        .bind(session.summary.as_ref().map(|s| s.files as i64))
        .bind(summary_diffs)
        .bind(revert_json.map(sqlx::types::Json))
        .bind(permission_json.map(sqlx::types::Json))
        .bind(session.usage.as_ref().map(|u| u.input).unwrap_or(0.0))
        .bind(session.usage.as_ref().map(|u| u.output).unwrap_or(0.0))
        .bind(session.usage.as_ref().map(|u| u.reasoning).unwrap_or(0.0))
        .bind(session.usage.as_ref().map(|u| u.cache.write).unwrap_or(0.0))
        .bind(session.usage.as_ref().map(|u| u.cache.read).unwrap_or(0.0))
        .bind(
            session
                .usage
                .as_ref()
                .map(|u| u.input + u.output)
                .unwrap_or(0.0),
        ) // Placeholder for total cost
        .bind(&session.status)
        .bind(session.created_at.timestamp_millis())
        .bind(session.updated_at.timestamp_millis())
        .bind(session.time.compacting.map(|t| t as i64))
        .bind(session.time.archived.map(|t| t as i64))
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<Session>, DatabaseError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"SELECT 
                id, project_id, parent_id, slug, directory, title, version, share_url,
                metadata,
                summary_additions, summary_deletions, summary_files, summary_diffs,
                revert, permission,
                usage_input_tokens, usage_output_tokens, usage_reasoning_tokens,
                usage_cache_write_tokens, usage_cache_read_tokens, usage_total_cost,
                status, created_at, updated_at, time_compacting, time_archived
            FROM sessions WHERE id = ?"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        row.map(|r| r.into_session()).transpose()
    }

    /// セッション行を取得する共通ヘルパー。`list` と `list_skipping_corrupt` が共有する。
    async fn fetch_session_rows(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SessionRow>, DatabaseError> {
        match project_id {
            Some(pid) => sqlx::query_as::<_, SessionRow>(
                r#"SELECT 
                        id, project_id, parent_id, slug, directory, title, version, share_url,
                        metadata,
                        summary_additions, summary_deletions, summary_files, summary_diffs,
                        revert, permission,
                        usage_input_tokens, usage_output_tokens, usage_reasoning_tokens,
                        usage_cache_write_tokens, usage_cache_read_tokens, usage_total_cost,
                        status, created_at, updated_at, time_compacting, time_archived
                    FROM sessions WHERE project_id = ? 
                    ORDER BY updated_at DESC LIMIT ?"#,
            )
            .bind(pid)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string())),
            None => sqlx::query_as::<_, SessionRow>(
                r#"SELECT 
                        id, project_id, parent_id, slug, directory, title, version, share_url,
                        metadata,
                        summary_additions, summary_deletions, summary_files, summary_diffs,
                        revert, permission,
                        usage_input_tokens, usage_output_tokens, usage_reasoning_tokens,
                        usage_cache_write_tokens, usage_cache_read_tokens, usage_total_cost,
                        status, created_at, updated_at, time_compacting, time_archived
                    FROM sessions 
                    ORDER BY updated_at DESC LIMIT ?"#,
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string())),
        }
    }

    /// 既存の fail-loud 一覧。1 行でも破損していれば `CorruptData` を返す。
    pub async fn list(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Session>, DatabaseError> {
        let rows = self.fetch_session_rows(project_id, limit).await?;
        rows.into_iter().map(|r| r.into_session()).collect()
    }

    /// 起動時一覧用の tolerant 版。破損行は warn を出してスキップし、他の行は返す。
    ///
    /// `get()` / `list()` の fail-loud 方針は維持したまま、1 行の破損でサーバ
    /// 全体の起動が止まらないようにするための二層方針の tolerant 側。
    /// warn にはフィールド名と session id のみを記録し、設定値やトークン値は
    /// ログに出さない。
    pub async fn list_skipping_corrupt(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Session>, DatabaseError> {
        let rows = self.fetch_session_rows(project_id, limit).await?;
        let mut sessions = Vec::with_capacity(rows.len());
        for row in rows {
            let session_id = row.id.clone();
            match row.into_session_with_field() {
                Ok(session) => sessions.push(session),
                Err((field, _)) => {
                    tracing::warn!(
                        session_id = %session_id,
                        field = field.unwrap_or("unknown"),
                        "skipping corrupt session row during startup list"
                    );
                }
            }
        }
        Ok(sessions)
    }

    pub async fn update(&self, session: &Session) -> Result<(), DatabaseError> {
        let summary_diffs = session
            .summary
            .as_ref()
            .and_then(|s| serde_json::to_string(&s.diffs).ok());
        let metadata_json = serde_json::to_string(&session.metadata)
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        let revert_json = session.revert.clone();
        let permission_json = session
            .permission
            .as_ref()
            .and_then(|p| serde_json::to_string(p).ok());

        let share_url = session.share.as_ref().map(|s| s.url.clone());
        let usage = session.usage.as_ref();

        sqlx::query(
            r#"
            UPDATE sessions SET
                title = ?, version = ?, share_url = ?, metadata = ?,
                summary_additions = ?, summary_deletions = ?, summary_files = ?, summary_diffs = ?,
                revert = ?, permission = ?,
                usage_input_tokens = ?, usage_output_tokens = ?, usage_reasoning_tokens = ?,
                usage_cache_write_tokens = ?, usage_cache_read_tokens = ?, usage_total_cost = ?,
                status = ?, updated_at = ?, time_compacting = ?, time_archived = ?
            WHERE id = ?
            "#,
        )
        .bind(&session.title)
        .bind(&session.version)
        .bind(share_url)
        .bind(&metadata_json)
        .bind(session.summary.as_ref().map(|s| s.additions as i64))
        .bind(session.summary.as_ref().map(|s| s.deletions as i64))
        .bind(session.summary.as_ref().map(|s| s.files as i64))
        .bind(summary_diffs)
        .bind(revert_json.map(sqlx::types::Json))
        .bind(permission_json.map(sqlx::types::Json))
        .bind(usage.map(|u| u.input).unwrap_or(0.0))
        .bind(usage.map(|u| u.output).unwrap_or(0.0))
        .bind(usage.map(|u| u.reasoning).unwrap_or(0.0))
        .bind(usage.map(|u| u.cache.write).unwrap_or(0.0))
        .bind(usage.map(|u| u.cache.read).unwrap_or(0.0))
        .bind(usage.map(|u| u.input + u.output).unwrap_or(0.0))
        .bind(&session.status)
        .bind(session.updated_at.timestamp_millis())
        .bind(session.time.compacting.map(|t| t as i64))
        .bind(session.time.archived.map(|t| t as i64))
        .bind(&session.id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), DatabaseError> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn list_children(&self, parent_id: &str) -> Result<Vec<Session>, DatabaseError> {
        let rows = sqlx::query_as::<_, SessionRow>(
            r#"SELECT 
                id, project_id, parent_id, slug, directory, title, version, share_url,
                metadata,
                summary_additions, summary_deletions, summary_files, summary_diffs,
                revert, permission,
                usage_input_tokens, usage_output_tokens, usage_reasoning_tokens,
                usage_cache_write_tokens, usage_cache_read_tokens, usage_total_cost,
                status, created_at, updated_at, time_compacting, time_archived
            FROM sessions WHERE parent_id = ? 
            ORDER BY created_at DESC"#,
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        rows.into_iter().map(|r| r.into_session()).collect()
    }
}

/// `messages` テーブルの生行。`list_for_session` / `get` / 置換前検証で共有する。
#[derive(FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    created_at: i64,
    data: Option<String>,
}

/// 保存済みメッセージを `SessionMessage` に復元する。
///
/// 未知の role や壊れた parts JSON を黙って捨て（filter_map / unwrap_or_default）と、
/// その結果を `replace_for_session` へ渡した際に元行が物理削除されうる。
/// そのため、ここでは warn ログを残した上で `DatabaseError::CorruptData` を返す
/// （fail-loud 方針）。`data` が NULL の旧行はパース失敗ではないので空の parts として扱う。
fn decode_message_row(row: &MessageRow) -> Result<SessionMessage, DatabaseError> {
    let role = match row.role.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" => MessageRole::Tool,
        other => {
            tracing::warn!(
                message_id = %row.id.as_str(),
                role = %other,
                "unknown message role; refusing to silently drop the row"
            );
            return Err(DatabaseError::CorruptData(format!(
                "message '{}' has unknown role '{}'",
                row.id, other
            )));
        }
    };

    let parts: Vec<MessagePart> = match row.data.as_deref() {
        None => Vec::new(),
        Some(data) => serde_json::from_str(data).map_err(|e| {
            tracing::warn!(
                message_id = %row.id.as_str(),
                error = %e,
                "failed to parse stored message parts"
            );
            DatabaseError::CorruptData(format!(
                "message '{}' has invalid parts JSON: {}",
                row.id, e
            ))
        })?,
    };

    let created = DateTime::from_timestamp_millis(row.created_at).unwrap_or_else(Utc::now);

    Ok(SessionMessage {
        id: row.id.clone(),
        session_id: row.session_id.clone(),
        role,
        parts,
        created_at: created,
        metadata: HashMap::new(),
    })
}

/// セッション内の破損行数を数える（`replace_for_session` の事前検証用）。
async fn count_corrupt_messages(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<usize, DatabaseError> {
    let rows = sqlx::query_as::<_, MessageRow>(
        r#"SELECT id, session_id, role, created_at, data
           FROM messages WHERE session_id = ?"#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    Ok(rows
        .iter()
        .filter(|row| decode_message_row(row).is_err())
        .count())
}

pub struct MessageRepository {
    pool: SqlitePool,
}

impl MessageRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 指定した executor にメッセージを 1 件挿入する内部ヘルパー。
    ///
    /// `create` と `replace_for_session` が同じ INSERT 文を共有し、
    /// 片方だけが修正されることによる不整合を防ぐ。
    async fn insert_message<'e, E>(
        executor: E,
        message: &SessionMessage,
    ) -> Result<(), DatabaseError>
    where
        E: Executor<'e, Database = sqlx::Sqlite>,
    {
        let data_json = serde_json::to_string(&message.parts)
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        let role_str = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };

        sqlx::query(
            r#"
            INSERT INTO messages (id, session_id, role, created_at, data)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(&message.id)
        .bind(&message.session_id)
        .bind(role_str)
        .bind(message.created_at.timestamp_millis())
        .bind(&data_json)
        .execute(executor)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    /// メッセージを 1 件挿入する（従来 API と同じ動作）。
    pub async fn create(&self, message: &SessionMessage) -> Result<(), DatabaseError> {
        Self::insert_message(&self.pool, message).await
    }

    pub async fn upsert(&self, message: &SessionMessage) -> Result<(), DatabaseError> {
        let data_json = serde_json::to_string(&message.parts)
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        let role_str = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };

        sqlx::query(
            r#"
            INSERT INTO messages (id, session_id, role, created_at, data)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                session_id = excluded.session_id,
                role = excluded.role,
                created_at = excluded.created_at,
                data = excluded.data
            "#,
        )
        .bind(&message.id)
        .bind(&message.session_id)
        .bind(role_str)
        .bind(message.created_at.timestamp_millis())
        .bind(&data_json)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn list_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, DatabaseError> {
        let rows = sqlx::query_as::<_, MessageRow>(
            r#"SELECT id, session_id, role, created_at, data 
               FROM messages WHERE session_id = ? ORDER BY created_at ASC, rowid ASC"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        rows.iter()
            .map(decode_message_row)
            .collect::<Result<Vec<_>, DatabaseError>>()
    }

    pub async fn get(&self, id: &str) -> Result<Option<SessionMessage>, DatabaseError> {
        let row = sqlx::query_as::<_, MessageRow>(
            r#"SELECT id, session_id, role, created_at, data 
               FROM messages WHERE id = ?"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        match row {
            Some(row) => decode_message_row(&row).map(Some),
            None => Ok(None),
        }
    }

    pub async fn delete(&self, id: &str) -> Result<(), DatabaseError> {
        sqlx::query("DELETE FROM messages WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn delete_for_session(&self, session_id: &str) -> Result<(), DatabaseError> {
        sqlx::query("DELETE FROM messages WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    /// 指定セッションのメッセージを `messages` の内容で原子的に置き換える。
    ///
    /// DELETE と各 INSERT を単一トランザクションで実行する。途中でエラーが
    /// 発生した場合は既存メッセージの削除を含む変更全体がロールバックされる。
    /// `server.rs` の `delete_for_session` + 1 件ずつ `create` の置き換えを想定する。
    ///
    /// 事前に保存済み行を検証し、破損行（未知 role / 壊れた parts JSON）が
    /// 残っていれば DELETE を行わず `DatabaseError::CorruptData` を返す。
    /// 読み取り側も fail-loud にしたことで「読んで書き戻す」経路では破損が
    /// 表面化するが、別経路で組み立てた入力から元行を消さないための防御でもある。
    pub async fn replace_for_session(
        &self,
        session_id: &str,
        messages: &[SessionMessage],
    ) -> Result<(), DatabaseError> {
        let corrupt = count_corrupt_messages(&self.pool, session_id).await?;
        if corrupt > 0 {
            tracing::warn!(
                session_id = %session_id,
                corrupt_rows = corrupt,
                "refusing replace_for_session: corrupted rows would be lost"
            );
            return Err(DatabaseError::CorruptData(format!(
                "session '{session_id}' has {corrupt} corrupted message row(s); refusing replace to avoid data loss"
            )));
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DatabaseError::TransactionError(e.to_string()))?;

        sqlx::query("DELETE FROM messages WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        for message in messages {
            Self::insert_message(&mut *tx, message).await?;
        }

        tx.commit()
            .await
            .map_err(|e| DatabaseError::TransactionError(e.to_string()))?;

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
    pub position: i64,
}

pub struct TodoRepository {
    pool: SqlitePool,
}

impl TodoRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list_for_session(&self, session_id: &str) -> Result<Vec<TodoItem>, DatabaseError> {
        #[derive(FromRow)]
        struct TodoRow {
            todo_id: String,
            content: String,
            status: String,
            priority: String,
            position: i64,
        }

        let rows = sqlx::query_as::<_, TodoRow>(
            r#"SELECT todo_id, content, status, priority, position 
               FROM todos WHERE session_id = ? ORDER BY position ASC"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| TodoItem {
                id: row.todo_id,
                content: row.content,
                status: row.status,
                priority: row.priority,
                position: row.position,
            })
            .collect())
    }

    /// 指定した executor に Todo を 1 件 UPSERT する内部ヘルパー。
    ///
    /// `upsert` と `replace_for_session` が同じ SQL を共有し、片方だけが
    /// 修正されることによる不整合を防ぐ。
    async fn upsert_todo<'e, E>(
        executor: E,
        session_id: &str,
        todo: &TodoItem,
    ) -> Result<(), DatabaseError>
    where
        E: Executor<'e, Database = sqlx::Sqlite>,
    {
        let now = Utc::now().timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO todos (session_id, todo_id, content, status, priority, position, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(session_id, todo_id) DO UPDATE SET
                content = excluded.content,
                status = excluded.status,
                priority = excluded.priority,
                position = excluded.position,
                updated_at = excluded.updated_at
            "#
        )
        .bind(session_id)
        .bind(&todo.id)
        .bind(&todo.content)
        .bind(&todo.status)
        .bind(&todo.priority)
        .bind(todo.position)
        .bind(now)
        .bind(now)
        .execute(executor)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    /// Todo を 1 件 UPSERT する（従来 API と同じ動作）。
    pub async fn upsert(&self, session_id: &str, todo: &TodoItem) -> Result<(), DatabaseError> {
        Self::upsert_todo(&self.pool, session_id, todo).await
    }

    /// 対象 ID の現在の position を DB から解決する。行が無い場合は `Ok(None)`。
    ///
    /// `TodoManager::set_status` がスナップショット時点の index ではなく、
    /// 書き込み時点の DB position を使うための API（R4 指摘 (2)）。
    pub async fn current_position(
        &self,
        session_id: &str,
        todo_id: &str,
    ) -> Result<Option<i64>, DatabaseError> {
        sqlx::query_scalar::<_, i64>(
            "SELECT position FROM todos WHERE session_id = ? AND todo_id = ?",
        )
        .bind(session_id)
        .bind(todo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))
    }

    /// 指定セッションの Todo を `todos` の内容で原子的に置き換える。
    ///
    /// DELETE と各 UPSERT を単一トランザクションで実行する。途中でエラーが
    /// 発生した場合は既存行の削除を含む変更全体がロールバックされる。
    /// `TodoManager` が `delete_for_session` と複数 `upsert` を別 auto-commit で
    /// 実行していた問題（R4 指摘 (1)）を解消する。`MessageRepository` の
    /// `replace_for_session` と同じトランザクション型／エラー型を再利用する。
    pub async fn replace_for_session(
        &self,
        session_id: &str,
        todos: &[TodoItem],
    ) -> Result<(), DatabaseError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DatabaseError::TransactionError(e.to_string()))?;

        sqlx::query("DELETE FROM todos WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        for todo in todos {
            Self::upsert_todo(&mut *tx, session_id, todo).await?;
        }

        tx.commit()
            .await
            .map_err(|e| DatabaseError::TransactionError(e.to_string()))?;

        Ok(())
    }

    pub async fn delete(&self, session_id: &str, todo_id: &str) -> Result<(), DatabaseError> {
        sqlx::query("DELETE FROM todos WHERE session_id = ? AND todo_id = ?")
            .bind(session_id)
            .bind(todo_id)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn delete_for_session(&self, session_id: &str) -> Result<(), DatabaseError> {
        sqlx::query("DELETE FROM todos WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }
}

#[derive(Clone)]
pub struct InputRepository {
    pool: SqlitePool,
}

impl InputRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn enqueue(
        &self,
        session_id: &str,
        prompt: &str,
        delivery: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<opencode_types::SessionInput, DatabaseError> {
        let created_at = chrono::Utc::now().timestamp_millis();
        let id = format!("in_{}_{}", created_at, uuid::Uuid::new_v4());
        let metadata_json = metadata
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        let returned_metadata = metadata.cloned();
        sqlx::query(
            "INSERT INTO session_input (id, session_id, prompt, delivery, metadata, status, created_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)",
        )
        .bind(&id)
        .bind(session_id)
        .bind(prompt)
        .bind(delivery)
        .bind(metadata_json)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(opencode_types::SessionInput {
            id,
            session_id: session_id.to_owned(),
            prompt: prompt.to_owned(),
            delivery: delivery.map(str::to_owned),
            metadata: returned_metadata,
            status: "pending".to_owned(),
            created_at: created_at as u64,
        })
    }

    pub async fn list_pending_all(
        &self,
    ) -> Result<Vec<opencode_types::SessionInput>, DatabaseError> {
        #[derive(FromRow)]
        struct InputRow {
            id: String,
            session_id: String,
            prompt: String,
            delivery: Option<String>,
            metadata: Option<String>,
            status: String,
            created_at: i64,
        }

        let rows = sqlx::query_as::<_, InputRow>(
            "SELECT id, session_id, prompt, delivery, metadata, status, created_at FROM session_input WHERE status = 'pending' ORDER BY created_at ASC, rowid ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| opencode_types::SessionInput {
                id: row.id,
                session_id: row.session_id,
                prompt: row.prompt,
                delivery: row.delivery,
                metadata: row
                    .metadata
                    .and_then(|value| serde_json::from_str(&value).ok()),
                status: row.status,
                created_at: row.created_at as u64,
            })
            .collect())
    }

    pub async fn claim_pending(&self, id: &str) -> Result<bool, DatabaseError> {
        let result = sqlx::query(
            r#"UPDATE session_input SET status = 'processing'
               WHERE id = ? AND status = 'pending'
                 AND NOT EXISTS (
                     SELECT 1 FROM session_input earlier
                     WHERE earlier.session_id = session_input.session_id
                       AND earlier.status IN ('pending', 'processing')
                       AND (
                           earlier.created_at < session_input.created_at
                           OR (earlier.created_at = session_input.created_at AND earlier.rowid < session_input.rowid)
                       )
                 )"#,
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn release_for_retry(&self, id: &str) -> Result<(), DatabaseError> {
        sqlx::query(
            "UPDATE session_input SET status = 'pending' WHERE id = ? AND status = 'processing'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(())
    }

    pub async fn list_pending(
        &self,
        session_id: &str,
    ) -> Result<Vec<opencode_types::SessionInput>, DatabaseError> {
        #[derive(FromRow)]
        struct InputRow {
            id: String,
            session_id: String,
            prompt: String,
            delivery: Option<String>,
            metadata: Option<String>,
            status: String,
            created_at: i64,
        }

        let rows = sqlx::query_as::<_, InputRow>(
            r#"SELECT id, session_id, prompt, delivery, metadata, status, created_at
               FROM session_input WHERE session_id = ? AND status = 'pending' ORDER BY created_at ASC, rowid ASC"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| opencode_types::SessionInput {
                id: row.id,
                session_id: row.session_id,
                prompt: row.prompt,
                delivery: row.delivery,
                metadata: row
                    .metadata
                    .and_then(|value| serde_json::from_str(&value).ok()),
                status: row.status,
                created_at: row.created_at as u64,
            })
            .collect())
    }

    pub async fn mark_as_promoted(&self, id: &str) -> Result<(), DatabaseError> {
        sqlx::query(
            "UPDATE session_input SET status = 'promoted' WHERE id = ? AND status IN ('pending', 'processing')",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

        Ok(())
    }

    pub async fn mark_as_failed(&self, id: &str) -> Result<(), DatabaseError> {
        sqlx::query(
            "UPDATE session_input SET status = 'failed' WHERE id = ? AND status IN ('pending', 'processing')",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(())
    }

    pub async fn reset_processing(&self) -> Result<(), DatabaseError> {
        sqlx::query("UPDATE session_input SET status = 'pending' WHERE status = 'processing'")
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod message_repository_tests {
    use super::MessageRepository;
    use crate::database::DatabaseError;
    use crate::Database;
    use chrono::{TimeZone, Utc};
    use opencode_types::SessionMessage;

    #[tokio::test]
    async fn list_for_session_breaks_created_at_ties_by_insertion_order() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");
        let repository = MessageRepository::new(database.pool().clone());
        let same_time = Utc.timestamp_millis_opt(1_000).unwrap();

        for text in ["first", "second", "third"] {
            let mut message = SessionMessage::user("session-1", text);
            message.created_at = same_time;
            repository.create(&message).await.expect("message created");
        }

        let messages = repository
            .list_for_session("session-1")
            .await
            .expect("list works");
        let texts: Vec<String> = messages.iter().map(|m| m.get_text()).collect();
        assert_eq!(texts, ["first", "second", "third"]);
    }

    #[tokio::test]
    async fn replace_for_session_swaps_messages_atomically() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");
        let repository = MessageRepository::new(database.pool().clone());

        for text in ["old-first", "old-second"] {
            repository
                .create(&SessionMessage::user("session-1", text))
                .await
                .expect("old message created");
        }

        let replacement = vec![
            SessionMessage::user("session-1", "new-first"),
            SessionMessage::user("session-1", "new-second"),
        ];
        repository
            .replace_for_session("session-1", &replacement)
            .await
            .expect("replace succeeds");

        let texts: Vec<String> = repository
            .list_for_session("session-1")
            .await
            .expect("list works")
            .iter()
            .map(|m| m.get_text())
            .collect();
        assert_eq!(texts, ["new-first", "new-second"]);
    }

    #[tokio::test]
    async fn replace_for_session_rolls_back_on_insert_failure() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");
        let repository = MessageRepository::new(database.pool().clone());

        repository
            .create(&SessionMessage::user("session-1", "original"))
            .await
            .expect("original message created");

        // 2 件目の id を 1 件目と衝突させ、2 回目の INSERT を PK 違反で失敗させる。
        // DELETE は先行して成功しているため、ロールバックされなければ
        // "original" は消えたままになる。
        let first = SessionMessage::user("session-1", "replacement-1");
        let mut second = SessionMessage::user("session-1", "replacement-2");
        second.id = first.id.clone();
        assert!(
            repository
                .replace_for_session("session-1", &[first, second])
                .await
                .is_err(),
            "duplicate id must fail the transaction"
        );

        let texts: Vec<String> = repository
            .list_for_session("session-1")
            .await
            .expect("list works")
            .iter()
            .map(|m| m.get_text())
            .collect();
        assert_eq!(
            texts,
            ["original"],
            "failed replace must roll back both DELETE and INSERT"
        );
    }

    #[tokio::test]
    async fn replace_for_session_with_empty_slice_deletes_all_messages() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");
        let repository = MessageRepository::new(database.pool().clone());

        repository
            .create(&SessionMessage::user("session-1", "stale"))
            .await
            .expect("message created");
        repository
            .replace_for_session("session-1", &[])
            .await
            .expect("empty replace succeeds");

        assert!(
            repository
                .list_for_session("session-1")
                .await
                .expect("list works")
                .is_empty(),
            "empty replacement must clear the session"
        );
    }

    /// 破損行の挿入に使う共通フィクスチャ（session-1 と 3 種類のメッセージ）。
    async fn seed_with_corrupt_rows(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(pool)
        .await
        .expect("session row exists");
        for (id, role, data) in [
            ("good", "user", Some("[]")),
            ("bad-role", "alien", Some("[]")),
            ("bad-json", "user", Some("not-json")),
            ("null-data", "assistant", None),
        ] {
            sqlx::query(
                "INSERT INTO messages (id, session_id, role, created_at, data) VALUES (?, 'session-1', ?, 1, ?)",
            )
            .bind(id)
            .bind(role)
            .bind(data)
            .execute(pool)
            .await
            .expect("seed message");
        }
    }

    #[tokio::test]
    async fn list_for_session_fails_loud_on_unknown_role() {
        let database = Database::in_memory().await.expect("database initializes");
        seed_with_corrupt_rows(database.pool()).await;
        let repository = MessageRepository::new(database.pool().clone());

        let result = repository.list_for_session("session-1").await;
        assert!(
            matches!(result, Err(DatabaseError::CorruptData(_))),
            "unknown role must not be silently dropped"
        );
    }

    #[tokio::test]
    async fn list_for_session_fails_loud_on_invalid_parts_json() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, created_at, data) VALUES ('bad-json', 'session-1', 'user', 1, 'not-json')",
        )
        .execute(database.pool())
        .await
        .expect("corrupt message exists");
        let repository = MessageRepository::new(database.pool().clone());

        let result = repository.list_for_session("session-1").await;
        assert!(
            matches!(result, Err(DatabaseError::CorruptData(_))),
            "invalid parts JSON must not be silently emptied"
        );
    }

    #[tokio::test]
    async fn get_tolerates_null_data_but_rejects_corrupt_rows() {
        let database = Database::in_memory().await.expect("database initializes");
        seed_with_corrupt_rows(database.pool()).await;
        let repository = MessageRepository::new(database.pool().clone());

        // data=NULL は旧スキーマの正当な行として空の parts に復元される。
        let legacy = repository
            .get("null-data")
            .await
            .expect("get works")
            .expect("legacy row found");
        assert!(legacy.parts.is_empty());

        // 未知 role / 壊れた JSON は get() でも fail-loud。
        assert!(matches!(
            repository.get("bad-role").await,
            Err(DatabaseError::CorruptData(_))
        ));
        assert!(matches!(
            repository.get("bad-json").await,
            Err(DatabaseError::CorruptData(_))
        ));

        // 存在しない id は従来どおり None。
        assert!(repository
            .get("missing")
            .await
            .expect("get works")
            .is_none());
    }

    #[tokio::test]
    async fn replace_for_session_preserves_corrupted_rows() {
        let database = Database::in_memory().await.expect("database initializes");
        seed_with_corrupt_rows(database.pool()).await;
        let repository = MessageRepository::new(database.pool().clone());

        let result = repository
            .replace_for_session(
                "session-1",
                &[SessionMessage::user("session-1", "replacement")],
            )
            .await;
        assert!(
            matches!(result, Err(DatabaseError::CorruptData(_))),
            "replace must refuse while corrupted rows remain"
        );

        // DELETE が走っていないことを、全行が残っていることでも確認する。
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM messages WHERE session_id = ? ORDER BY id")
                .bind("session-1")
                .fetch_all(database.pool())
                .await
                .expect("read remaining ids");
        assert_eq!(ids, ["bad-json", "bad-role", "good", "null-data"]);
    }
}

#[cfg(test)]
mod input_repository_tests {
    use super::InputRepository;
    use crate::Database;

    #[tokio::test]
    async fn queue_claim_and_restart_recovery_preserve_fifo() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");

        let repository = InputRepository::new(database.pool().clone());
        let first = repository
            .enqueue("session-1", "first", Some("queue"), None)
            .await
            .expect("first input enqueued");
        let second = repository
            .enqueue("session-1", "second", Some("queue"), None)
            .await
            .expect("second input enqueued");

        assert!(repository.claim_pending(&first.id).await.unwrap());
        assert!(!repository.claim_pending(&first.id).await.unwrap());
        assert!(!repository.claim_pending(&second.id).await.unwrap());
        repository.reset_processing().await.unwrap();

        let pending = repository.list_pending("session-1").await.unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            [first.id.as_str(), second.id.as_str()]
        );
        assert!(repository.claim_pending(&first.id).await.unwrap());
        repository.mark_as_promoted(&first.id).await.unwrap();
        assert!(repository.claim_pending(&second.id).await.unwrap());
    }

    #[tokio::test]
    async fn failed_queue_head_allows_next_item_with_tied_timestamps() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('session-1', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("session row exists");

        let repository = InputRepository::new(database.pool().clone());
        let first = repository
            .enqueue("session-1", "first", Some("queue"), None)
            .await
            .expect("first input enqueued");
        let second = repository
            .enqueue("session-1", "second", Some("queue"), None)
            .await
            .expect("second input enqueued");

        sqlx::query("UPDATE session_input SET created_at = 42 WHERE id IN (?, ?)")
            .bind(&first.id)
            .bind(&second.id)
            .execute(database.pool())
            .await
            .expect("timestamps are tied");

        let pending = repository.list_pending("session-1").await.unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            [first.id.as_str(), second.id.as_str()]
        );
        assert!(repository.claim_pending(&first.id).await.unwrap());
        repository.mark_as_failed(&first.id).await.unwrap();
        assert!(repository.claim_pending(&second.id).await.unwrap());
    }
}

#[cfg(test)]
mod todo_repository_tests {
    use super::{TodoItem, TodoRepository};
    use crate::Database;

    const SESSION: &str = "todo-session";

    async fn test_database() -> Database {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) \
             VALUES (?1, 'project', 'slug', '.', 'title', 1, 1)",
        )
        .bind(SESSION)
        .execute(database.pool())
        .await
        .expect("session row");
        database
    }

    fn todo(id: &str, content: &str, position: i64) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: content.to_string(),
            status: "pending".to_string(),
            priority: "medium".to_string(),
            position,
        }
    }

    #[tokio::test]
    async fn replace_for_session_swaps_todos_atomically() {
        let database = test_database().await;
        let repository = TodoRepository::new(database.pool().clone());

        repository
            .upsert(SESSION, &todo("old-1", "old-first", 0))
            .await
            .expect("seed first");
        repository
            .upsert(SESSION, &todo("old-2", "old-second", 1))
            .await
            .expect("seed second");

        repository
            .replace_for_session(
                SESSION,
                &[
                    todo("new-1", "new-first", 0),
                    todo("new-2", "new-second", 1),
                ],
            )
            .await
            .expect("replace succeeds");

        let rows = repository
            .list_for_session(SESSION)
            .await
            .expect("list works");
        assert_eq!(
            rows.iter()
                .map(|row| row.content.as_str())
                .collect::<Vec<_>>(),
            ["new-first", "new-second"]
        );
        assert_eq!(rows[0].id, "new-1");
        assert_eq!(rows[1].id, "new-2");
    }

    #[tokio::test]
    async fn replace_for_session_rolls_back_on_upsert_failure() {
        let database = test_database().await;
        let repository = TodoRepository::new(database.pool().clone());

        repository
            .upsert(SESSION, &todo("original", "original", 0))
            .await
            .expect("seed original");

        // 2 件目の UPSERT を RAISE(ABORT) で失敗させる。DELETE は先行して成功
        // するので、ロールバックされなければ "original" が消えたままになる。
        sqlx::raw_sql(
            "CREATE TRIGGER reject_poison BEFORE INSERT ON todos \
             WHEN NEW.content = 'poison' \
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .execute(database.pool())
        .await
        .expect("trigger created");

        assert!(
            repository
                .replace_for_session(
                    SESSION,
                    &[todo("new-1", "new-1", 0), todo("new-2", "poison", 1)],
                )
                .await
                .is_err(),
            "second upsert must fail the transaction"
        );

        let rows = repository
            .list_for_session(SESSION)
            .await
            .expect("list works");
        assert_eq!(rows.len(), 1, "failed replace must roll back the DELETE");
        assert_eq!(rows[0].id, "original");
        assert_eq!(rows[0].content, "original");
    }

    #[tokio::test]
    async fn replace_for_session_with_empty_slice_deletes_all_todos() {
        let database = test_database().await;
        let repository = TodoRepository::new(database.pool().clone());

        repository
            .upsert(SESSION, &todo("stale", "stale", 0))
            .await
            .expect("seed stale");
        repository
            .replace_for_session(SESSION, &[])
            .await
            .expect("empty replace succeeds");

        assert!(
            repository
                .list_for_session(SESSION)
                .await
                .expect("list works")
                .is_empty(),
            "empty replacement must clear the session"
        );
    }

    #[tokio::test]
    async fn current_position_reads_saved_position() {
        let database = test_database().await;
        let repository = TodoRepository::new(database.pool().clone());

        assert!(repository
            .current_position(SESSION, "missing")
            .await
            .expect("lookup works")
            .is_none());

        repository
            .upsert(SESSION, &todo("first", "first", 3))
            .await
            .expect("seed first");
        repository
            .upsert(SESSION, &todo("second", "second", 7))
            .await
            .expect("seed second");

        assert_eq!(
            repository
                .current_position(SESSION, "second")
                .await
                .expect("lookup works"),
            Some(7)
        );
    }
}

#[cfg(test)]
mod session_repository_restore_tests {
    use super::SessionRepository;
    use crate::database::DatabaseError;
    use crate::Database;
    use chrono::{TimeZone, Utc};
    use opencode_types::{
        CacheUsage, FileDiff, PermissionRuleset, ProjectID, Session, SessionID, SessionRevert,
        SessionStatus, SessionSummary, TokenUsage,
    };

    fn sample_session(id: &str) -> Session {
        let instant = Utc
            .timestamp_millis_opt(1_700_000_000_000)
            .single()
            .expect("valid timestamp");
        Session {
            id: SessionID(id.to_string()),
            slug: "test".to_string(),
            project_id: ProjectID("project-1".to_string()),
            directory: ".".to_string(),
            parent_id: None,
            title: "Test".to_string(),
            version: "1.0.0".to_string(),
            time: opencode_types::TimeInfo {
                created: 1_700_000_000_000,
                updated: 1_700_000_000_000,
                compacting: None,
                archived: None,
            },
            messages: Vec::new(),
            summary: None,
            share: None,
            revert: None,
            permission: None,
            usage: Some(TokenUsage {
                input: 1.0,
                output: 2.0,
                reasoning: 0.0,
                cache: CacheUsage {
                    read: 0.0,
                    write: 0.0,
                },
            }),
            status: SessionStatus::Active,
            metadata: std::collections::HashMap::new(),
            created_at: instant,
            updated_at: instant,
        }
    }

    #[tokio::test]
    async fn summary_round_trips_through_create_and_get() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        let mut session = sample_session("summary-session");
        session.summary = Some(SessionSummary {
            additions: 3,
            deletions: 2,
            files: 1,
            diffs: vec![FileDiff {
                path: "src/main.rs".to_string(),
                additions: 3,
                deletions: 2,
            }],
        });

        repository.create(&session).await.expect("session created");
        let loaded = repository
            .get("summary-session")
            .await
            .expect("get works")
            .expect("session found");
        assert_eq!(loaded.summary, session.summary);

        // list 経由でも同じ列が復元されること。
        let listed = repository.list(None, 10).await.expect("list works");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].summary, session.summary);

        // list_children 経由でも同じ列が復元されること。
        let mut child = sample_session("child-summary");
        child.parent_id = Some(SessionID("summary-session".to_string()));
        child.summary = session.summary.clone();
        repository
            .create(&child)
            .await
            .expect("child session created");
        let children = repository
            .list_children("summary-session")
            .await
            .expect("list_children works");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].summary, session.summary);
    }

    #[tokio::test]
    async fn summary_round_trips_through_update() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        let mut session = sample_session("summary-update");
        repository.create(&session).await.expect("session created");

        session.summary = Some(SessionSummary {
            additions: 7,
            deletions: 4,
            files: 2,
            diffs: vec![FileDiff {
                path: "docs/readme.md".to_string(),
                additions: 7,
                deletions: 4,
            }],
        });
        repository.update(&session).await.expect("session updated");
        let loaded = repository
            .get("summary-update")
            .await
            .expect("get works")
            .expect("session found");
        assert_eq!(loaded.summary, session.summary);
    }

    #[tokio::test]
    async fn legacy_row_without_summary_columns_yields_none() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) \
             VALUES ('legacy', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("legacy session row");
        let repository = SessionRepository::new(database.pool().clone());

        let loaded = repository
            .get("legacy")
            .await
            .expect("get works")
            .expect("session found");
        assert!(loaded.summary.is_none());
    }

    #[tokio::test]
    async fn summary_counts_without_diffs_fail_loud() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions \
             (id, project_id, slug, directory, title, created_at, updated_at, \
              summary_additions, summary_deletions, summary_files) \
             VALUES ('partial-summary', 'project-1', 'test', '.', 'Test', 1, 1, 3, 2, 1)",
        )
        .execute(database.pool())
        .await
        .expect("partial summary row");
        let repository = SessionRepository::new(database.pool().clone());

        assert!(matches!(
            repository.get("partial-summary").await,
            Err(DatabaseError::CorruptData(_))
        ));
    }

    #[tokio::test]
    async fn corrupt_revert_fails_loud_in_get_and_list() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("bad-revert"))
            .await
            .expect("session created");
        sqlx::query("UPDATE sessions SET revert = 'not-json' WHERE id = 'bad-revert'")
            .execute(database.pool())
            .await
            .expect("corrupt revert");

        assert!(matches!(
            repository.get("bad-revert").await,
            Err(DatabaseError::CorruptData(_))
        ));
        assert!(matches!(
            repository.list(None, 10).await,
            Err(DatabaseError::CorruptData(_))
        ));
    }

    #[tokio::test]
    async fn corrupt_permission_fails_loud_instead_of_failing_open() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("bad-permission"))
            .await
            .expect("session created");
        sqlx::query("UPDATE sessions SET permission = '{broken' WHERE id = 'bad-permission'")
            .execute(database.pool())
            .await
            .expect("corrupt permission");

        let result = repository.get("bad-permission").await;
        assert!(
            matches!(result, Err(DatabaseError::CorruptData(_))),
            "corrupt permission must not be dropped into an unrestricted session"
        );
    }

    #[tokio::test]
    async fn valid_revert_and_permission_round_trip() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        let mut session = sample_session("json-session");
        session.revert = Some(SessionRevert {
            message_id: "message-1".to_string(),
            part_id: Some("part-1".to_string()),
            snapshot: None,
            diff: None,
        });
        session.permission = Some(PermissionRuleset {
            allow: vec!["read".to_string()],
            deny: vec!["write".to_string()],
            mode: None,
        });

        repository.create(&session).await.expect("session created");
        let loaded = repository
            .get("json-session")
            .await
            .expect("get works")
            .expect("session found");
        assert_eq!(loaded.revert, session.revert);
        assert_eq!(loaded.permission, session.permission);
    }

    #[tokio::test]
    async fn list_skipping_corrupt_returns_healthy_rows_and_keeps_get_fail_loud() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("good-session"))
            .await
            .expect("good session created");
        repository
            .create(&sample_session("bad-session"))
            .await
            .expect("bad session created");
        // 1 行だけ permission を壊す（正常行はそのまま）。
        sqlx::query("UPDATE sessions SET permission = '{broken' WHERE id = 'bad-session'")
            .execute(database.pool())
            .await
            .expect("corrupt permission");

        // (a) 起動時一覧（tolerant）: 破損行をスキップし、正常行を返す。
        let listed = repository
            .list_skipping_corrupt(None, 100_000)
            .await
            .expect("tolerant list works");
        assert_eq!(listed.len(), 1, "only the healthy row must be returned");
        assert_eq!(listed[0].id.0, "good-session");

        // 既存 list() は fail-loud のまま（回帰確認）。
        assert!(matches!(
            repository.list(None, 100_000).await,
            Err(DatabaseError::CorruptData(_))
        ));

        // (b) 個別 get() は破損行で Err、正常行は Ok のまま（fail-loud 維持）。
        assert!(matches!(
            repository.get("bad-session").await,
            Err(DatabaseError::CorruptData(_))
        ));
        assert!(repository
            .get("good-session")
            .await
            .expect("get works")
            .is_some());
    }

    /// `into_session_with_field()` の summary_diffs 経路（3 経路のうち 1 つ）が
    /// tolerant 一覧でスキップされることを固定する。
    #[tokio::test]
    async fn list_skipping_corrupt_skips_row_with_corrupt_summary_diffs() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("good-summary-session"))
            .await
            .expect("good session created");
        repository
            .create(&sample_session("bad-summary-session"))
            .await
            .expect("bad session created");
        // summary_diffs だけを壊す（正常行はそのまま）。
        sqlx::query(
            "UPDATE sessions SET summary_diffs = 'not-json' WHERE id = 'bad-summary-session'",
        )
        .execute(database.pool())
        .await
        .expect("corrupt summary_diffs");

        let listed = repository
            .list_skipping_corrupt(None, 100_000)
            .await
            .expect("tolerant list works");
        assert_eq!(listed.len(), 1, "only the healthy row must be returned");
        assert_eq!(listed[0].id.0, "good-summary-session");
    }

    /// `into_session_with_field()` の revert 経路が tolerant 一覧でスキップされることを固定する。
    #[tokio::test]
    async fn list_skipping_corrupt_skips_row_with_corrupt_revert() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("good-revert-session"))
            .await
            .expect("good session created");
        repository
            .create(&sample_session("bad-revert-session"))
            .await
            .expect("bad session created");
        // revert だけを壊す（正常行はそのまま）。
        sqlx::query("UPDATE sessions SET revert = 'not-json' WHERE id = 'bad-revert-session'")
            .execute(database.pool())
            .await
            .expect("corrupt revert");

        let listed = repository
            .list_skipping_corrupt(None, 100_000)
            .await
            .expect("tolerant list works");
        assert_eq!(listed.len(), 1, "only the healthy row must be returned");
        assert_eq!(listed[0].id.0, "good-revert-session");
    }

    /// 複数破損行（permission + summary_diffs）が連続しても、正常行のみが返ることを固定する。
    #[tokio::test]
    async fn list_skipping_corrupt_skips_multiple_corrupt_rows_consecutively() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("good-session"))
            .await
            .expect("good session created");
        repository
            .create(&sample_session("bad-permission-session"))
            .await
            .expect("bad permission session created");
        repository
            .create(&sample_session("bad-summary-session"))
            .await
            .expect("bad summary session created");
        // 2 行を別々のフィールドで壊す（正常行はそのまま）。
        sqlx::query(
            "UPDATE sessions SET permission = '{broken' WHERE id = 'bad-permission-session'",
        )
        .execute(database.pool())
        .await
        .expect("corrupt permission");
        sqlx::query(
            "UPDATE sessions SET summary_diffs = 'not-json' WHERE id = 'bad-summary-session'",
        )
        .execute(database.pool())
        .await
        .expect("corrupt summary_diffs");

        let listed = repository
            .list_skipping_corrupt(None, 100_000)
            .await
            .expect("tolerant list works");
        assert_eq!(
            listed.len(),
            1,
            "only the healthy row must survive consecutive skips"
        );
        assert_eq!(listed[0].id.0, "good-session");
    }

    fn metadata_fixture() -> std::collections::HashMap<String, serde_json::Value> {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("source".to_string(), serde_json::json!("test"));
        metadata.insert("priority".to_string(), serde_json::json!(7));
        metadata.insert("tags".to_string(), serde_json::json!(["a", "b"]));
        metadata
    }

    #[tokio::test]
    async fn metadata_round_trips_through_create_get_update_list_and_children() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        let mut session = sample_session("metadata-session");
        session.metadata = metadata_fixture();
        repository
            .create(&session)
            .await
            .expect("session created with metadata");

        let loaded = repository
            .get("metadata-session")
            .await
            .expect("get works")
            .expect("session found");
        assert_eq!(loaded.metadata, session.metadata);

        // list 経由でも往復すること。
        let listed = repository.list(None, 10).await.expect("list works");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].metadata, session.metadata);

        // list_children 経由でも往復すること。
        let mut child = sample_session("metadata-child");
        child.parent_id = Some(SessionID("metadata-session".to_string()));
        child.metadata = session.metadata.clone();
        repository.create(&child).await.expect("child created");
        let children = repository
            .list_children("metadata-session")
            .await
            .expect("list_children works");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].metadata, session.metadata);

        // update 後の再保存でも失われないこと。
        session
            .metadata
            .insert("after_update".to_string(), serde_json::json!(true));
        repository.update(&session).await.expect("session updated");
        let reloaded = repository
            .get("metadata-session")
            .await
            .expect("get works")
            .expect("session found");
        assert_eq!(reloaded.metadata, session.metadata);
    }

    #[tokio::test]
    async fn legacy_row_with_null_metadata_yields_empty_map() {
        let database = Database::in_memory().await.expect("database initializes");
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) \
             VALUES ('legacy', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(database.pool())
        .await
        .expect("legacy session row");
        let repository = SessionRepository::new(database.pool().clone());

        let loaded = repository
            .get("legacy")
            .await
            .expect("get works")
            .expect("session found");
        assert!(
            loaded.metadata.is_empty(),
            "NULL metadata of a legacy row must fall back to an empty HashMap"
        );
    }

    #[tokio::test]
    async fn corrupt_metadata_fails_loud_in_get_and_list() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("bad-metadata"))
            .await
            .expect("session created");
        sqlx::query("UPDATE sessions SET metadata = 'not-json' WHERE id = 'bad-metadata'")
            .execute(database.pool())
            .await
            .expect("corrupt metadata");

        assert!(matches!(
            repository.get("bad-metadata").await,
            Err(DatabaseError::CorruptData(_))
        ));
        assert!(matches!(
            repository.list(None, 10).await,
            Err(DatabaseError::CorruptData(_))
        ));
    }

    #[tokio::test]
    async fn list_skipping_corrupt_skips_row_with_corrupt_metadata() {
        let database = Database::in_memory().await.expect("database initializes");
        let repository = SessionRepository::new(database.pool().clone());
        repository
            .create(&sample_session("good-metadata-session"))
            .await
            .expect("good session created");
        repository
            .create(&sample_session("bad-metadata-session"))
            .await
            .expect("bad session created");
        sqlx::query("UPDATE sessions SET metadata = 'not-json' WHERE id = 'bad-metadata-session'")
            .execute(database.pool())
            .await
            .expect("corrupt metadata");

        let listed = repository
            .list_skipping_corrupt(None, 100_000)
            .await
            .expect("tolerant list works");
        assert_eq!(listed.len(), 1, "only the healthy row must be returned");
        assert_eq!(listed[0].id.0, "good-metadata-session");
        assert!(matches!(
            repository.get("bad-metadata-session").await,
            Err(DatabaseError::CorruptData(_))
        ));
    }

    #[tokio::test]
    async fn startup_migration_adds_metadata_column_to_sessions_table() {
        use sqlx::Row;

        let database = Database::in_memory().await.expect("database initializes");
        let rows = sqlx::query("PRAGMA table_info(sessions)")
            .fetch_all(database.pool())
            .await
            .expect("table_info readable");
        let names: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>("name").expect("column name"))
            .collect();
        assert!(
            names.iter().any(|name| name == "metadata"),
            "起動時マイグレーションで sessions.metadata 列が追加されること"
        );
    }

    #[tokio::test]
    async fn migration_adds_metadata_to_preexisting_legacy_sessions_table() {
        use sqlx::Row;

        // 旧 DB 相当（metadata 列なしの sessions + user_version=0）を用意する。
        let path = std::env::temp_dir().join(format!(
            "storage-session-metadata-{}.db",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"").expect("create temp db file");
        let path_str = path.to_string_lossy().into_owned();
        {
            let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path_str}"))
                .await
                .expect("connect temp db");
            sqlx::raw_sql(crate::schema::CREATE_SESSIONS_TABLE_LEGACY)
                .execute(&pool)
                .await
                .expect("legacy sessions table");
            sqlx::query(
                "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) \
                 VALUES ('legacy-row', 'project-1', 'test', '.', 'Test', 1, 1)",
            )
            .execute(&pool)
            .await
            .expect("legacy row");
            assert_eq!(
                sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                    .fetch_one(&pool)
                    .await
                    .expect("user_version"),
                0,
                "fresh file DB starts at version 0"
            );
        }

        let storage = crate::Storage::new(&path_str)
            .await
            .expect("storage connects");
        storage.migrate().await.expect("migration succeeds");
        drop(storage);

        let verify = sqlx::SqlitePool::connect(&format!("sqlite:{path_str}"))
            .await
            .expect("reopen migrated db");
        let rows = sqlx::query("PRAGMA table_info(sessions)")
            .fetch_all(&verify)
            .await
            .expect("table_info readable");
        let names: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>("name").expect("column name"))
            .collect();
        assert!(
            names.iter().any(|name| name == "metadata"),
            "既存テーブルにも ALTER で metadata 列が追加されること"
        );

        let repository = SessionRepository::new(verify.clone());
        let legacy = repository
            .get("legacy-row")
            .await
            .expect("get works")
            .expect("legacy row survives migration");
        assert!(
            legacy.metadata.is_empty(),
            "旧行は空 HashMap へフォールバックすること"
        );
        drop(verify);
        let _ = std::fs::remove_file(&path);
    }
}
