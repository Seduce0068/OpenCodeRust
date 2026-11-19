use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json;
use sqlx::{Executor, FromRow, SqlitePool};
use std::collections::HashMap;

use opencode_types::{MessagePart, MessageRole, Session, SessionMessage, SessionStatus};

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
    fn into_session(self) -> Session {
        Session {
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
            summary: None,
            share: self
                .share_url
                .map(|url| opencode_types::SessionShareInfo { url }),
            revert: self.revert.and_then(|s| serde_json::from_str(&s).ok()),
            permission: self.permission.and_then(|p| serde_json::from_str(&p).ok()),
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
            metadata: HashMap::new(),
            created_at: chrono::DateTime::from_timestamp_millis(self.created_at)
                .unwrap_or_else(chrono::Utc::now),
            updated_at: chrono::DateTime::from_timestamp_millis(self.updated_at)
                .unwrap_or_else(chrono::Utc::now),
        }
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

        let revert_json = session.revert.as_ref().map(sqlx::types::Json);
        let permission_json = session.permission.as_ref().map(sqlx::types::Json);
        let share_url = session.share.as_ref().map(|s| s.url.clone());

        sqlx::query(
            r#"
            INSERT INTO sessions (
                id, project_id, parent_id, slug, directory, title, version, share_url,
                summary_additions, summary_deletions, summary_files, summary_diffs,
                revert, permission,
                usage_input_tokens, usage_output_tokens, usage_reasoning_tokens,
                usage_cache_write_tokens, usage_cache_read_tokens, usage_total_cost,
                status, created_at, updated_at, time_compacting, time_archived
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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

        Ok(row.map(|r| r.into_session()))
    }

    pub async fn list(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Session>, DatabaseError> {
        let rows = match project_id {
            Some(pid) => sqlx::query_as::<_, SessionRow>(
                r#"SELECT 
                        id, project_id, parent_id, slug, directory, title, version, share_url,
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
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
            None => sqlx::query_as::<_, SessionRow>(
                r#"SELECT 
                        id, project_id, parent_id, slug, directory, title, version, share_url,
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
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
        };

        Ok(rows.into_iter().map(|r| r.into_session()).collect())
    }

    pub async fn update(&self, session: &Session) -> Result<(), DatabaseError> {
        let summary_diffs = session
            .summary
            .as_ref()
            .and_then(|s| serde_json::to_string(&s.diffs).ok());

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
                title = ?, version = ?, share_url = ?,
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

        Ok(rows.into_iter().map(|r| r.into_session()).collect())
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
