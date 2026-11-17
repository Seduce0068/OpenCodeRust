use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoInfo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub session_id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
    pub position: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
