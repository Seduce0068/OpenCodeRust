use opencode_storage::{TodoItem, TodoRepository};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoInfo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

/// 安定 ID を保持するための内部エントリ。
///
/// 公開 API (`TodoInfo`) は変更せず、index に依存しない ID を内側で持つことで
/// `remove` 後の重複 upsert / 既存行の上書き (M-61) を防ぐ。
#[derive(Debug, Clone)]
struct TodoEntry {
    id: String,
    todo: TodoInfo,
}

/// セッション単位の writer ロック。`state` の RwLock とは独立で、
/// readers (`get`) をブロックせず writers 同士だけを直列化する (R5)。
type SessionWriterLock = Arc<tokio::sync::Mutex<()>>;

pub struct TodoManager {
    state: Arc<RwLock<HashMap<String, Vec<TodoEntry>>>>,
    db: Option<Arc<TodoRepository>>,
    /// セッション単位の writer ロックを保持するレジストリ。
    writer_locks: Arc<tokio::sync::Mutex<HashMap<String, SessionWriterLock>>>,
    /// テスト専用: DB フェーズに遅延を挿入し、ロックを保持していないことを検証する。
    #[cfg(test)]
    db_delay: std::time::Duration,
    /// テスト専用: 遅延付き DB フェーズに入ったことをテストへ通知する。
    #[cfg(test)]
    db_started: Arc<tokio::sync::Notify>,
}

impl TodoManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            db: None,
            writer_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(test)]
            db_delay: std::time::Duration::ZERO,
            #[cfg(test)]
            db_started: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn with_database(pool: sqlx::SqlitePool) -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            db: Some(Arc::new(TodoRepository::new(pool))),
            writer_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(test)]
            db_delay: std::time::Duration::ZERO,
            #[cfg(test)]
            db_started: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// 位置と無関係な安定 ID を生成する。
    ///
    /// 旧形式（`{session}_{index}`）の行は `update` の `replace_for_session` /
    /// `clear` の `delete_for_session` でまとめて削除されるため、
    /// 後方互換上の衝突はない。
    fn new_todo_id(session_id: &str) -> String {
        format!("{}_{}", session_id, uuid::Uuid::new_v4())
    }

    /// セッション専用の writer ロックを取得する。
    ///
    /// レジストリの Mutex はロック本体のクローンを得た時点で解放し、
    /// 待機中は保持しない。これにより同じセッションの writers 同士だけが
    /// 直列化され、`get` などの readers は `state` の RwLock だけで動く (R5)。
    async fn writer_lock(&self, session_id: &str) -> SessionWriterLock {
        let mut registry = self.writer_locks.lock().await;
        registry
            .entry(session_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn db_item(entry: &TodoEntry, position: usize) -> TodoItem {
        TodoItem {
            id: entry.id.clone(),
            content: entry.todo.content.clone(),
            status: entry.todo.status.clone(),
            priority: entry.todo.priority.clone(),
            position: position as i64,
        }
    }

    /// 実際の DB I/O の直前に置くテスト用遅延。
    ///
    /// 本番では何もしない。DB 呼び出しと同じ場所に置くことで、将来 DB I/O が
    /// `state` の RwLock 内へ戻された場合も回帰テストが必ず検出できるようにする。
    /// セッション単位の writer ロックは意図的に保持したままで、`get` はこの
    /// ロックを見ないため readers は blocking しない (R5)。
    #[cfg(not(test))]
    async fn db_pause(&self) {}

    #[cfg(test)]
    async fn db_pause(&self) {
        if !self.db_delay.is_zero() {
            self.db_started.notify_one();
            tokio::time::sleep(self.db_delay).await;
        }
    }

    /// DB への永続化を試みる。DB 未設定または成功時は `true`、失敗時は警告を
    /// 出して `false` を返す。呼び出し側は `false` のときメモリ更新をスキップし、
    /// DB／メモリの不一致を残さない（R4 指摘 (5)）。
    async fn persist_upsert(&self, session_id: &str, item: &TodoItem) -> bool {
        if let Some(ref db) = self.db {
            self.db_pause().await;
            if let Err(e) = db.upsert(session_id, item).await {
                tracing::warn!(
                    session_id = %session_id,
                    todo_id = %item.id,
                    error = %e,
                    "Failed to persist todo; skipping in-memory update"
                );
                return false;
            }
        }
        true
    }

    async fn persist_delete_session(&self, session_id: &str) -> bool {
        if let Some(ref db) = self.db {
            self.db_pause().await;
            if let Err(e) = db.delete_for_session(session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to delete persisted todos for session; skipping in-memory update"
                );
                return false;
            }
        }
        true
    }

    /// Replace the todo list for a session in memory and, when a database is
    /// configured, in storage.
    ///
    /// The storage replacement uses a single `replace_for_session` transaction
    /// (DELETE + all UPSERTs) so a failure or crash cannot leave a partial list
    /// behind (R4 finding (1)). On failure the error is logged and the in-memory
    /// update is skipped, keeping memory in sync with the rolled-back DB instead
    /// of silently diverging (R4 finding (5)).
    ///
    /// DB I/O is performed before taking the write lock, matching the ordering
    /// of every other mutating method (M-62).
    pub async fn update(&self, session_id: &str, todos: Vec<TodoInfo>) {
        // DB フェーズを含む操作全体をセッション単位で直列化する (R5)。
        let lock = self.writer_lock(session_id).await;
        let _guard = lock.lock().await;

        let entries: Vec<TodoEntry> = todos
            .into_iter()
            .map(|todo| TodoEntry {
                id: Self::new_todo_id(session_id),
                todo,
            })
            .collect();

        if let Some(ref db) = self.db {
            self.db_pause().await;
            let items: Vec<TodoItem> = entries
                .iter()
                .enumerate()
                .map(|(position, entry)| Self::db_item(entry, position))
                .collect();
            if let Err(e) = db.replace_for_session(session_id, &items).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to replace persisted todos; skipping in-memory update"
                );
                return;
            }
        }

        let mut state = self.state.write().await;
        if entries.is_empty() {
            state.remove(session_id);
        } else {
            state.insert(session_id.to_string(), entries);
        }
    }

    /// Return the stored todo list for a session.
    ///
    /// A configured database is read first; if the read fails, the error is
    /// logged with `tracing::warn!` and the in-memory state is used as a
    /// fallback rather than reported to the caller as an error.
    pub async fn get(&self, session_id: &str) -> Vec<TodoInfo> {
        if let Some(ref db) = self.db {
            match db.list_for_session(session_id).await {
                Ok(items) => {
                    return items
                        .into_iter()
                        .map(|item| TodoInfo {
                            content: item.content,
                            status: item.status,
                            priority: item.priority,
                        })
                        .collect();
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "Failed to read persisted todos, falling back to in-memory state"
                    );
                }
            }
        }

        let state = self.state.read().await;
        state
            .get(session_id)
            .map(|entries| entries.iter().map(|entry| entry.todo.clone()).collect())
            .unwrap_or_default()
    }

    /// Remove a session's todos from memory and, when a database is
    /// configured, from storage.
    ///
    /// A `delete_for_session` failure is logged with `tracing::warn!` and the
    /// in-memory removal is skipped, so a failed delete does not hide rows that
    /// are still persisted (R4 finding (5)).
    pub async fn clear(&self, session_id: &str) {
        let lock = self.writer_lock(session_id).await;
        let _guard = lock.lock().await;

        if !self.persist_delete_session(session_id).await {
            return;
        }

        let mut state = self.state.write().await;
        state.remove(session_id);
    }

    /// Change one todo's status and return whether the entry still exists in
    /// memory after the update.
    ///
    /// The DB write is performed outside the state lock. The position is
    /// resolved from the DB by stable ID at write time instead of using the
    /// snapshot index, so a concurrent `remove` renumbering cannot leave a
    /// duplicate/gap position behind (R4 finding (2)). A persistence failure is
    /// logged with `tracing::warn!` and the in-memory update is skipped (R4
    /// finding (5)).
    pub async fn set_status(&self, session_id: &str, index: usize, status: &str) -> bool {
        // スナップショット〜DB 書き込み〜メモリ更新を他 writer と直列化する (R5)。
        let lock = self.writer_lock(session_id).await;
        let _guard = lock.lock().await;

        // 1) 短い read lock で ID と内容だけをスナップショットする。
        //    position はスナップショットに含めない（書き込み時点の DB から解決する）。
        let snapshot = {
            let state = self.state.read().await;
            state
                .get(session_id)
                .and_then(|entries| entries.get(index))
                .map(|entry| (entry.id.clone(), entry.todo.clone()))
        };
        let Some((id, todo)) = snapshot else {
            return false;
        };

        // 2) DB I/O はロックを一切保持せずに行う。position は対象 ID から
        //    書き込み時点の DB を読んで解決する。
        if let Some(ref db) = self.db {
            self.db_pause().await;
            // スナップショット index は使わない。DB に行が無い／読めない場合は
            // 更新をスキップして DB／メモリの不一致を残さない。
            let position = match db.current_position(session_id, &id).await {
                Ok(Some(position)) => position,
                Ok(None) => {
                    tracing::warn!(
                        session_id = %session_id,
                        todo_id = %id,
                        error = "persisted row not found",
                        "Failed to resolve persisted todo position; skipping in-memory update"
                    );
                    return false;
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        todo_id = %id,
                        error = %e,
                        "Failed to resolve persisted todo position; skipping in-memory update"
                    );
                    return false;
                }
            };
            let item = TodoItem {
                id: id.clone(),
                content: todo.content,
                status: status.to_string(),
                priority: todo.priority,
                position,
            };
            if let Err(e) = db.upsert(session_id, &item).await {
                tracing::warn!(
                    session_id = %session_id,
                    todo_id = %id,
                    error = %e,
                    "Failed to persist todo status; skipping in-memory update"
                );
                return false;
            }
        }

        // 3) 短い write lock でメモリを更新する。index ではなく安定 ID で
        //    特定するため、並行操作で並びが変わっても別の項目を汚さない。
        let mut state = self.state.write().await;
        if let Some(entry) = state
            .get_mut(session_id)
            .and_then(|entries| entries.iter_mut().find(|entry| entry.id == id))
        {
            entry.todo.status = status.to_string();
            true
        } else {
            false
        }
    }

    /// Append a todo to a session's list.
    ///
    /// The DB upsert runs outside the state lock and before the short critical
    /// section that appends to memory; an `upsert` failure is logged with
    /// `tracing::warn!` and the in-memory append is skipped so memory stays in
    /// sync with the DB (R4 finding (5)).
    pub async fn add(&self, session_id: &str, todo: TodoInfo) {
        let lock = self.writer_lock(session_id).await;
        let _guard = lock.lock().await;

        let id = Self::new_todo_id(session_id);
        let position = {
            let state = self.state.read().await;
            state.get(session_id).map_or(0, Vec::len)
        };
        let item = TodoItem {
            id: id.clone(),
            content: todo.content.clone(),
            status: todo.status.clone(),
            priority: todo.priority.clone(),
            position: position as i64,
        };

        if !self.persist_upsert(session_id, &item).await {
            return;
        }

        let mut state = self.state.write().await;
        state
            .entry(session_id.to_string())
            .or_default()
            .push(TodoEntry { id, todo });
    }

    /// Remove a todo by index and return whether it still exists in memory
    /// after the update.
    ///
    /// DB work (deleting the removed row and renumbering the surviving rows'
    /// positions) happens via one `replace_for_session` transaction outside the
    /// state lock, so a failure cannot leave a partially renumbered list (R4
    /// finding (1)). On failure the error is logged and no memory removal is
    /// performed (R4 finding (5)).
    pub async fn remove(&self, session_id: &str, index: usize) -> bool {
        // スナップショット〜置換〜メモリ更新を他 writer と直列化する (R5)。
        let lock = self.writer_lock(session_id).await;
        let _guard = lock.lock().await;

        // 1) 短い read lock で削除対象と残り全件をスナップショットする。
        let (removed_id, mut remaining) = {
            let state = self.state.read().await;
            let Some(entries) = state.get(session_id) else {
                return false;
            };
            let Some(removed) = entries.get(index) else {
                return false;
            };
            let removed_id = removed.id.clone();
            let remaining: Vec<TodoItem> = entries
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, entry)| Self::db_item(entry, 0))
                .collect();
            (removed_id, remaining)
        };

        // 2) DB I/O はロック外。削除と残り行の再採番を 1 トランザクションで実行する。
        if let Some(ref db) = self.db {
            self.db_pause().await;
            for (position, item) in remaining.iter_mut().enumerate() {
                item.position = position as i64;
            }
            if let Err(e) = db.replace_for_session(session_id, &remaining).await {
                tracing::warn!(
                    session_id = %session_id,
                    todo_id = %removed_id,
                    error = %e,
                    "Failed to replace persisted todos after remove; skipping in-memory removal"
                );
                return false;
            }
        }

        // 3) 短い write lock でメモリから削除する。
        let mut state = self.state.write().await;
        match state.get_mut(session_id) {
            Some(entries) => match entries.iter().position(|entry| entry.id == removed_id) {
                Some(position) => {
                    entries.remove(position);
                    true
                }
                None => false,
            },
            None => false,
        }
    }
}

impl Default for TodoManager {
    fn default() -> Self {
        Self::new()
    }
}

/// セッション側の Todo 状態。
///
/// JSON 表現は [`TodoStatus::as_str`] と同じ snake_case を正式とし、
/// 旧 serde 出力だった PascalCase は alias で読み取り互換を維持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[serde(alias = "Pending")]
    Pending,
    #[serde(alias = "InProgress")]
    InProgress,
    #[serde(alias = "Completed")]
    Completed,
    #[serde(alias = "Cancelled")]
    Cancelled,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// セッション側の Todo 優先度。
///
/// 状態と同じく、JSON 表現は [`TodoPriority::as_str`] と同じ小文字を正式とする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    #[serde(alias = "High")]
    High,
    #[serde(alias = "Medium")]
    Medium,
    #[serde(alias = "Low")]
    Low,
}

impl TodoPriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoPriority::High => "high",
            TodoPriority::Medium => "medium",
            TodoPriority::Low => "low",
        }
    }
}

impl std::fmt::Display for TodoPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 比較用に小文字化し、区切り文字を除去して serde / as_str / 旧 PascalCase を統一する。
fn normalize_todo_word(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(*c, '_' | '-' | ' '))
        .collect()
}

/// serde の snake_case 表現・旧 PascalCase 表現・`as_str()` 由来の DB 値を
/// すべて受け付けて状態へ復元する。未知の値は `Pending` にフォールバックする。
pub fn parse_status(status: &str) -> TodoStatus {
    match normalize_todo_word(status).as_str() {
        "pending" => TodoStatus::Pending,
        "inprogress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        "cancelled" => TodoStatus::Cancelled,
        _ => TodoStatus::Pending,
    }
}

/// serde の小文字表現・旧 PascalCase 表現・`as_str()` 由来の DB 値を
/// すべて受け付けて優先度へ復元する。未知の値は `Medium` にフォールバックする。
pub fn parse_priority(priority: &str) -> TodoPriority {
    match normalize_todo_word(priority).as_str() {
        "high" => TodoPriority::High,
        "medium" => TodoPriority::Medium,
        "low" => TodoPriority::Low,
        _ => TodoPriority::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencode_storage::Database;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    const SESSION: &str = "todo-session";

    fn todo(content: &str, status: &str, priority: &str) -> TodoInfo {
        TodoInfo {
            content: content.to_string(),
            status: status.to_string(),
            priority: priority.to_string(),
        }
    }

    async fn test_database() -> Database {
        let database = Database::in_memory().await.expect("in-memory database");
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

    /// remove(0) -> set_status(0) -> add -> set_status(2) の連続操作で
    /// DB に旧 ID の残骸や重複行が残らないことを検証する (M-61)。
    #[tokio::test]
    async fn consecutive_mutations_keep_unique_ids_and_db_in_sync() {
        let database = test_database().await;
        let manager = TodoManager::with_database(database.pool().clone());

        manager
            .update(
                SESSION,
                vec![
                    todo("A", "pending", "high"),
                    todo("B", "pending", "medium"),
                    todo("C", "pending", "low"),
                ],
            )
            .await;
        assert_eq!(manager.get(SESSION).await.len(), 3);

        assert!(manager.remove(SESSION, 0).await, "index 0 existed");
        assert!(manager.set_status(SESSION, 0, "completed").await);
        manager.add(SESSION, todo("D", "pending", "high")).await;
        assert!(manager.set_status(SESSION, 2, "in_progress").await);

        let expected = vec![
            todo("B", "completed", "medium"),
            todo("C", "pending", "low"),
            todo("D", "in_progress", "high"),
        ];
        assert_eq!(manager.get(SESSION).await, expected);

        let rows = TodoRepository::new(database.pool().clone())
            .list_for_session(SESSION)
            .await
            .expect("db rows");
        assert_eq!(rows.len(), 3, "remove must not leave an orphan row");
        let ids: HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "todo ids must be unique");
        assert_eq!(
            rows.iter()
                .map(|row| row.content.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "C", "D"]
        );
        for (position, row) in rows.iter().enumerate() {
            assert_eq!(
                row.position, position as i64,
                "positions must be renumbered"
            );
            assert_eq!(row.status, expected[position].status);
            assert_eq!(row.priority, expected[position].priority);
        }
    }

    /// DB 書き込みが遅延している間も get() が state の read lock を取得でき、
    /// ブロックしないことを検証する (M-62)。
    #[tokio::test]
    async fn readers_are_not_blocked_while_db_write_is_in_flight() {
        let database = test_database().await;
        let mut manager = TodoManager::with_database(database.pool().clone());
        manager
            .update(SESSION, vec![todo("A", "pending", "medium")])
            .await;

        // get() を memory fallback 経路に流し、state の RwLock で競合させる。
        sqlx::query("DROP TABLE todos")
            .execute(database.pool())
            .await
            .expect("drop todos table");

        manager.db_delay = Duration::from_millis(500);
        let manager = Arc::new(manager);

        let started = Arc::clone(&manager.db_started);
        let worker = Arc::clone(&manager);
        let task = tokio::spawn(async move { worker.set_status(SESSION, 0, "completed").await });

        // ワーカーが DB フェーズ（遅延）に入ったことを確認してから読み取りを試みる。
        tokio::time::timeout(Duration::from_millis(1000), started.notified())
            .await
            .expect("set_status reached its DB phase");
        let read = tokio::time::timeout(Duration::from_millis(200), manager.get(SESSION)).await;
        assert!(
            read.is_ok(),
            "get() must not wait for an in-flight DB write"
        );
        assert_eq!(read.expect("timeout result")[0].content, "A");

        // テーブルを落としているため position 解決が失敗する。R4 の契約変更に
        // より、DB 書き込み失敗時はメモリも更新しない（DB／メモリ不一致を残さない）。
        assert!(
            !task.await.expect("set_status task"),
            "failed DB write must skip the in-memory update"
        );
        assert_eq!(manager.get(SESSION).await[0].status, "pending");
    }

    /// DB 置換が失敗したとき update / remove がメモリを更新せず、
    /// ロールバックされた DB と同じ状態を保つことを検証する (R4 指摘 (1)(5))。
    #[tokio::test]
    async fn failed_db_replace_skips_in_memory_update() {
        let database = test_database().await;
        let manager = TodoManager::with_database(database.pool().clone());

        manager
            .update(SESSION, vec![todo("A", "pending", "medium")])
            .await;
        let original = vec![todo("A", "pending", "medium")];
        assert_eq!(manager.get(SESSION).await, original);

        // 2 件目の UPSERT を強制失敗させる。DELETE は先行して成功するため、
        // ロールバックされなければ元行が消えたままになる。
        sqlx::raw_sql(
            "CREATE TRIGGER reject_poison BEFORE INSERT ON todos \
             WHEN NEW.content = 'poison' \
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .execute(database.pool())
        .await
        .expect("trigger created");

        manager
            .update(
                SESSION,
                vec![
                    todo("B", "pending", "high"),
                    todo("poison", "pending", "low"),
                ],
            )
            .await;
        assert_eq!(
            manager.get(SESSION).await,
            original,
            "failed replace must leave both DB and memory at the original state"
        );

        // remove も同じ API を通るため、失敗時はメモリからも削除しない。
        sqlx::raw_sql(
            "CREATE TRIGGER reject_delete BEFORE DELETE ON todos \
             WHEN OLD.content = 'A' \
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .execute(database.pool())
        .await
        .expect("delete trigger created");
        assert!(!manager.remove(SESSION, 0).await);
        assert_eq!(manager.get(SESSION).await, original);
    }

    /// UPSERT 失敗時に set_status がメモリを更新しないことを検証する (R4 指摘 (5))。
    #[tokio::test]
    async fn failed_status_persist_skips_in_memory_update() {
        let database = test_database().await;
        let manager = TodoManager::with_database(database.pool().clone());

        manager
            .update(SESSION, vec![todo("A", "pending", "medium")])
            .await;

        sqlx::raw_sql(
            "CREATE TRIGGER reject_status BEFORE UPDATE ON todos \
             WHEN NEW.status = 'completed' \
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .execute(database.pool())
        .await
        .expect("status trigger created");

        assert!(!manager.set_status(SESSION, 0, "completed").await);
        assert_eq!(manager.get(SESSION).await[0].status, "pending");
    }

    /// set_status がスナップショットの index ではなく、書き込み時点の DB
    /// position を使うことを検証する (R4 指摘 (2))。
    /// DB フェーズ中に remove 相当の再採番が走っても重複／欠番にならない。
    #[tokio::test]
    async fn set_status_resolves_position_from_db_at_write_time() {
        let database = test_database().await;
        let mut manager = TodoManager::with_database(database.pool().clone());
        manager
            .update(
                SESSION,
                vec![todo("A", "pending", "medium"), todo("B", "pending", "low")],
            )
            .await;

        manager.db_delay = Duration::from_millis(500);
        let manager = Arc::new(manager);
        let started = Arc::clone(&manager.db_started);
        let worker = Arc::clone(&manager);
        let task = tokio::spawn(async move { worker.set_status(SESSION, 1, "completed").await });

        tokio::time::timeout(Duration::from_millis(1000), started.notified())
            .await
            .expect("set_status reached its DB phase");

        // スナップショット後・DB 書き込み前に A を削除し、B を position 0 へ採番し直す。
        let repository = TodoRepository::new(database.pool().clone());
        let rows = repository.list_for_session(SESSION).await.expect("db rows");
        let mut renumbered = rows
            .iter()
            .find(|row| row.content == "B")
            .expect("B row")
            .clone();
        sqlx::query("DELETE FROM todos WHERE session_id = ?")
            .bind(SESSION)
            .execute(database.pool())
            .await
            .expect("simulated remove");
        renumbered.position = 0;
        repository
            .upsert(SESSION, &renumbered)
            .await
            .expect("renumbered row");

        assert!(
            task.await.expect("set_status task"),
            "index 1 existed at snapshot"
        );
        let rows = repository.list_for_session(SESSION).await.expect("db rows");
        assert_eq!(rows.len(), 1, "only the renumbered B row must remain");
        assert_eq!(rows[0].id, renumbered.id);
        assert_eq!(
            rows[0].position, 0,
            "position must follow the DB state, not the stale snapshot index"
        );
        assert_eq!(rows[0].status, "completed");
    }

    /// set_status の DB フェーズ中に remove が走っても、セッション単位の
    /// writer ロックで直列化され、削除済み ID が DB に復活しないことを検証する (R5)。
    #[tokio::test]
    async fn set_status_and_remove_are_serialized_per_session() {
        let database = test_database().await;
        let mut manager = TodoManager::with_database(database.pool().clone());
        manager
            .update(
                SESSION,
                vec![todo("A", "pending", "high"), todo("B", "pending", "low")],
            )
            .await;

        manager.db_delay = Duration::from_millis(300);
        let manager = Arc::new(manager);

        let started = Arc::clone(&manager.db_started);
        let worker = Arc::clone(&manager);
        let status_task =
            tokio::spawn(async move { worker.set_status(SESSION, 0, "completed").await });

        // set_status が DB フェーズ（スリープ）に入ったところで remove を開始する。
        tokio::time::timeout(Duration::from_millis(1000), started.notified())
            .await
            .expect("set_status reached its DB phase");

        let remover = Arc::clone(&manager);
        let mut remove_task = tokio::spawn(async move { remover.remove(SESSION, 0).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut remove_task)
                .await
                .is_err(),
            "remove must wait until the in-flight set_status releases the session lock"
        );

        assert!(status_task.await.expect("set_status task"));
        assert!(remove_task.await.expect("remove task"));

        let rows = TodoRepository::new(database.pool().clone())
            .list_for_session(SESSION)
            .await
            .expect("db rows");
        assert_eq!(rows.len(), 1, "removed A must not be resurrected in DB");
        assert_eq!(rows[0].content, "B");
        assert_eq!(rows[0].status, "pending");
        assert_eq!(rows[0].position, 0);
        assert_eq!(
            manager.get(SESSION).await,
            vec![todo("B", "pending", "low")]
        );
    }

    /// add の DB フェーズ中に remove が走っても直列化され、
    /// DB とメモリが同じ状態になることを検証する (R5)。
    #[tokio::test]
    async fn add_and_remove_are_serialized_per_session() {
        let database = test_database().await;
        let mut manager = TodoManager::with_database(database.pool().clone());
        manager
            .update(SESSION, vec![todo("A", "pending", "medium")])
            .await;

        manager.db_delay = Duration::from_millis(300);
        let manager = Arc::new(manager);

        let started = Arc::clone(&manager.db_started);
        let adder = Arc::clone(&manager);
        let add_task =
            tokio::spawn(async move { adder.add(SESSION, todo("B", "pending", "low")).await });

        tokio::time::timeout(Duration::from_millis(1000), started.notified())
            .await
            .expect("add reached its DB phase");

        let remover = Arc::clone(&manager);
        let mut remove_task = tokio::spawn(async move { remover.remove(SESSION, 0).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut remove_task)
                .await
                .is_err(),
            "remove must wait until the in-flight add releases the session lock"
        );

        add_task.await.expect("add task");
        assert!(remove_task.await.expect("remove task"));

        let rows = TodoRepository::new(database.pool().clone())
            .list_for_session(SESSION)
            .await
            .expect("db rows");
        assert_eq!(
            rows.len(),
            1,
            "add and remove must not lose the surviving row"
        );
        assert_eq!(rows[0].content, "B");
        assert_eq!(rows[0].position, 0);
        assert_eq!(
            manager.get(SESSION).await,
            vec![todo("B", "pending", "low")]
        );
    }

    /// serde 表現・as_str() 由来の DB 値・旧 PascalCase が parse で一致すること。
    #[test]
    fn todo_status_priority_serde_and_parse_are_symmetric() {
        use serde_json::json;

        let statuses = [
            (TodoStatus::Pending, "pending", "Pending"),
            (TodoStatus::InProgress, "in_progress", "InProgress"),
            (TodoStatus::Completed, "completed", "Completed"),
            (TodoStatus::Cancelled, "cancelled", "Cancelled"),
        ];
        for (variant, snake, pascal) in statuses {
            assert_eq!(serde_json::to_value(variant).unwrap(), json!(snake));
            assert_eq!(
                serde_json::from_value::<TodoStatus>(json!(snake)).unwrap(),
                variant
            );
            assert_eq!(
                serde_json::from_value::<TodoStatus>(json!(pascal)).unwrap(),
                variant
            );
            assert_eq!(parse_status(snake), variant);
            assert_eq!(parse_status(variant.as_str()), variant);
            assert_eq!(parse_status(pascal), variant);
            let wire: String =
                serde_json::from_str(&serde_json::to_string(&variant).unwrap()).unwrap();
            assert_eq!(parse_status(&wire), variant);
        }

        let priorities = [
            (TodoPriority::High, "high", "High"),
            (TodoPriority::Medium, "medium", "Medium"),
            (TodoPriority::Low, "low", "Low"),
        ];
        for (variant, lower, pascal) in priorities {
            assert_eq!(serde_json::to_value(variant).unwrap(), json!(lower));
            assert_eq!(
                serde_json::from_value::<TodoPriority>(json!(lower)).unwrap(),
                variant
            );
            assert_eq!(
                serde_json::from_value::<TodoPriority>(json!(pascal)).unwrap(),
                variant
            );
            assert_eq!(parse_priority(lower), variant);
            assert_eq!(parse_priority(variant.as_str()), variant);
            assert_eq!(parse_priority(pascal), variant);
            let wire: String =
                serde_json::from_str(&serde_json::to_string(&variant).unwrap()).unwrap();
            assert_eq!(parse_priority(&wire), variant);
        }
    }
}
