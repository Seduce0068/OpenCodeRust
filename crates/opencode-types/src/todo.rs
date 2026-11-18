use serde::{Deserialize, Serialize};

/// Todo の本文と状態・優先度を表す情報。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoInfo {
    /// Todo の本文。
    pub content: String,
    /// 状態を表す文字列。
    pub status: String,
    /// 優先度を表す文字列。
    pub priority: String,
}

/// セッションに紐付いた Todo 項目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// 紐付先のセッション ID。
    pub session_id: String,
    /// Todo の本文。
    pub content: String,
    /// 状態を表す文字列。
    pub status: String,
    /// 優先度を表す文字列。
    pub priority: String,
    /// 一覧内での並び順を表す位置。
    pub position: u32,
}

/// Todo の実行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoStatus {
    /// 未着手。
    Pending,
    /// 進行中。
    InProgress,
    /// 完了。
    Completed,
    /// キャンセル済み。
    Cancelled,
}

impl TodoStatus {
    /// 状態を対応する小文字の文字列に変換する。
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

/// Todo の優先度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoPriority {
    /// 高。
    High,
    /// 中。
    Medium,
    /// 低。
    Low,
}

impl TodoPriority {
    /// 優先度を対応する小文字の文字列に変換する。
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

/// 文字列を [`TodoStatus`] に変換する。未知の値は [`TodoStatus::Pending`] となる。
pub fn parse_status(status: &str) -> TodoStatus {
    match status.to_lowercase().as_str() {
        "pending" => TodoStatus::Pending,
        "in_progress" | "in progress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        "cancelled" => TodoStatus::Cancelled,
        _ => TodoStatus::Pending,
    }
}

/// 文字列を [`TodoPriority`] に変換する。未知の値は [`TodoPriority::Medium`] となる。
pub fn parse_priority(priority: &str) -> TodoPriority {
    match priority.to_lowercase().as_str() {
        "high" => TodoPriority::High,
        "medium" => TodoPriority::Medium,
        "low" => TodoPriority::Low,
        _ => TodoPriority::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_variants_and_defaults() {
        assert_eq!(parse_status("pending"), TodoStatus::Pending);
        assert_eq!(parse_status("IN_PROGRESS"), TodoStatus::InProgress);
        assert_eq!(parse_status("in progress"), TodoStatus::InProgress);
        assert_eq!(parse_status("Completed"), TodoStatus::Completed);
        assert_eq!(parse_status("cancelled"), TodoStatus::Cancelled);
        assert_eq!(parse_status("unknown"), TodoStatus::Pending);
    }

    #[test]
    fn parse_priority_variants_and_defaults() {
        assert_eq!(parse_priority("high"), TodoPriority::High);
        assert_eq!(parse_priority("MEDIUM"), TodoPriority::Medium);
        assert_eq!(parse_priority("low"), TodoPriority::Low);
        assert_eq!(parse_priority("unknown"), TodoPriority::Medium);
    }

    #[test]
    fn as_str_and_display_roundtrip() {
        assert_eq!(TodoStatus::InProgress.as_str(), "in_progress");
        assert_eq!(TodoStatus::Completed.to_string(), "completed");
        assert_eq!(TodoPriority::High.as_str(), "high");
        assert_eq!(TodoPriority::Low.to_string(), "low");
    }
}
