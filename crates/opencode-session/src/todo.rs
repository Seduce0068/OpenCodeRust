use opencode_storage::TodoRepository;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoInfo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

pub struct TodoManager {
    state: Arc<RwLock<HashMap<String, Vec<TodoInfo>>>>,
    db: Option<Arc<TodoRepository>>,
}

impl TodoManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            db: None,
        }
    }

    pub fn with_database(pool: sqlx::SqlitePool) -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            db: Some(Arc::new(TodoRepository::new(pool))),
        }
    }

    /// Replace the todo list for a session in memory and, when a database is
    /// configured, in storage.
    ///
    /// Persistence is best-effort and is not part of this method's success
    /// contract: `delete_for_session` / `upsert` failures are recorded with
    /// `tracing::warn!` and do not prevent the in-memory update. Callers that
    /// require a durable write must use `opencode_storage::TodoRepository` directly.
    pub async fn update(&self, session_id: &str, todos: Vec<TodoInfo>) {
        if let Some(ref db) = self.db {
            if let Err(e) = db.delete_for_session(session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to delete persisted todos for session"
                );
            }
            for (i, todo) in todos.iter().enumerate() {
                let item = opencode_storage::TodoItem {
                    id: format!("{}_{}", session_id, i),
                    content: todo.content.clone(),
                    status: todo.status.clone(),
                    priority: todo.priority.clone(),
                    position: i as i64,
                };
                if let Err(e) = db.upsert(session_id, &item).await {
                    tracing::warn!(
                        session_id = %session_id,
                        todo_id = %item.id,
                        error = %e,
                        "Failed to persist todo"
                    );
                }
            }
        }

        let mut state = self.state.write().await;
        if todos.is_empty() {
            state.remove(session_id);
        } else {
            state.insert(session_id.to_string(), todos);
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
        state.get(session_id).cloned().unwrap_or_default()
    }

    /// Remove a session's todos from memory and, when a database is
    /// configured, from storage.
    ///
    /// Persistence is best-effort: a `delete_for_session` failure is logged
    /// with `tracing::warn!` but does not fail this method.
    pub async fn clear(&self, session_id: &str) {
        if let Some(ref db) = self.db {
            if let Err(e) = db.delete_for_session(session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to delete persisted todos for session"
                );
            }
        }

        let mut state = self.state.write().await;
        state.remove(session_id);
    }

    /// Change one todo's status and return whether `index` existed.
    ///
    /// The boolean reflects only the in-memory update. When a database is
    /// configured, an `upsert` failure is logged with `tracing::warn!` and does
    /// not change the returned value.
    pub async fn set_status(&self, session_id: &str, index: usize, status: &str) -> bool {
        let mut state = self.state.write().await;
        if let Some(todos) = state.get_mut(session_id) {
            if index < todos.len() {
                todos[index].status = status.to_string();

                if let Some(ref db) = self.db {
                    let item = opencode_storage::TodoItem {
                        id: format!("{}_{}", session_id, index),
                        content: todos[index].content.clone(),
                        status: status.to_string(),
                        priority: todos[index].priority.clone(),
                        position: index as i64,
                    };
                    if let Err(e) = db.upsert(session_id, &item).await {
                        tracing::warn!(
                            session_id = %session_id,
                            todo_id = %item.id,
                            error = %e,
                            "Failed to persist todo status"
                        );
                    }
                }

                return true;
            }
        }
        false
    }

    /// Append a todo to a session's list.
    ///
    /// When a database is configured, an `upsert` failure is logged with
    /// `tracing::warn!` and does not fail this method; the in-memory list is
    /// always updated first.
    pub async fn add(&self, session_id: &str, todo: TodoInfo) {
        let mut state = self.state.write().await;
        let todos = state.entry(session_id.to_string()).or_insert_with(Vec::new);
        let position = todos.len();
        todos.push(todo.clone());

        if let Some(ref db) = self.db {
            let item = opencode_storage::TodoItem {
                id: format!("{}_{}", session_id, position),
                content: todo.content,
                status: todo.status,
                priority: todo.priority,
                position: position as i64,
            };
            if let Err(e) = db.upsert(session_id, &item).await {
                tracing::warn!(
                    session_id = %session_id,
                    todo_id = %item.id,
                    error = %e,
                    "Failed to persist added todo"
                );
            }
        }
    }

    /// Remove a todo by index and return whether `index` existed.
    ///
    /// The boolean reflects only the in-memory removal. When a database is
    /// configured, a `delete` failure is logged with `tracing::warn!` and does
    /// not change the returned value.
    pub async fn remove(&self, session_id: &str, index: usize) -> bool {
        let mut state = self.state.write().await;
        if let Some(todos) = state.get_mut(session_id) {
            if index < todos.len() {
                todos.remove(index);

                if let Some(ref db) = self.db {
                    let todo_id = format!("{}_{}", session_id, index);
                    if let Err(e) = db.delete(session_id, &todo_id).await {
                        tracing::warn!(
                            session_id = %session_id,
                            todo_id = %todo_id,
                            error = %e,
                            "Failed to delete persisted todo"
                        );
                    }
                }

                return true;
            }
        }
        false
    }
}

impl Default for TodoManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
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

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum TodoPriority {
    High,
    Medium,
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

pub fn parse_status(status: &str) -> TodoStatus {
    match status.to_lowercase().as_str() {
        "pending" => TodoStatus::Pending,
        "in_progress" | "in progress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        "cancelled" => TodoStatus::Cancelled,
        _ => TodoStatus::Pending,
    }
}

pub fn parse_priority(priority: &str) -> TodoPriority {
    match priority.to_lowercase().as_str() {
        "high" => TodoPriority::High,
        "medium" => TodoPriority::Medium,
        "low" => TodoPriority::Low,
        _ => TodoPriority::Medium,
    }
}
