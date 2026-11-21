//! Todo 型と status / priority の parse 関数の単一情報源。
//!
//! storage / session 層はこのモジュールの定義を再エクスポートし、同名の重複定義を持たない。

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
///
/// JSON 表現は [`TodoStatus::as_str`] と同じ snake_case を正式とし、
/// 旧 serde 出力だった PascalCase は `alias` で読み取り互換を維持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// 未着手。
    #[serde(alias = "Pending")]
    Pending,
    /// 進行中。
    #[serde(alias = "InProgress")]
    InProgress,
    /// 完了。
    #[serde(alias = "Completed")]
    Completed,
    /// キャンセル済み。
    #[serde(alias = "Cancelled")]
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
///
/// JSON 表現は [`TodoPriority::as_str`] と同じ小文字を正式とし、
/// 旧 serde 出力だった PascalCase は `alias` で読み取り互換を維持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    /// 高。
    #[serde(alias = "High")]
    High,
    /// 中。
    #[serde(alias = "Medium")]
    Medium,
    /// 低。
    #[serde(alias = "Low")]
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

/// 比較用に文字列を小文字化し、区切り文字を除去する。
///
/// `as_str()` 由来の snake_case、serde の snake_case / 旧 PascalCase、
/// および `"in progress"` のような表記ゆれを同じキーへ正規化する。
fn normalize_todo_word(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(*c, '_' | '-' | ' '))
        .collect()
}

/// 文字列を [`TodoStatus`] に変換する。未知の値は [`TodoStatus::Pending`] となる。
///
/// serde の snake_case 表現・旧 PascalCase 表現・`as_str()` 由来の DB 値を
/// すべて受け付け、往復で状態が退化しないようにする。
pub fn parse_status(status: &str) -> TodoStatus {
    match normalize_todo_word(status).as_str() {
        "pending" => TodoStatus::Pending,
        "inprogress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        "cancelled" => TodoStatus::Cancelled,
        _ => TodoStatus::Pending,
    }
}

/// 文字列を [`TodoPriority`] に変換する。未知の値は [`TodoPriority::Medium`] となる。
///
/// serde の小文字表現・旧 PascalCase 表現・`as_str()` 由来の DB 値を
/// すべて受け付ける。
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

    // (variant, 新 serde / as_str 表現, 旧 serde 表現)
    const STATUS_WIRE: &[(TodoStatus, &str, &str)] = &[
        (TodoStatus::Pending, "pending", "Pending"),
        (TodoStatus::InProgress, "in_progress", "InProgress"),
        (TodoStatus::Completed, "completed", "Completed"),
        (TodoStatus::Cancelled, "cancelled", "Cancelled"),
    ];

    const PRIORITY_WIRE: &[(TodoPriority, &str, &str)] = &[
        (TodoPriority::High, "high", "High"),
        (TodoPriority::Medium, "medium", "Medium"),
        (TodoPriority::Low, "low", "Low"),
    ];

    #[test]
    fn status_serde_wire_is_snake_case_with_pascal_aliases() {
        use serde_json::json;

        for (variant, snake, pascal) in STATUS_WIRE {
            assert_eq!(serde_json::to_value(variant).unwrap(), json!(snake));
            assert_eq!(
                serde_json::from_value::<TodoStatus>(json!(snake)).unwrap(),
                *variant
            );
            assert_eq!(
                serde_json::from_value::<TodoStatus>(json!(pascal)).unwrap(),
                *variant
            );
        }
    }

    #[test]
    fn priority_serde_wire_is_lowercase_with_pascal_aliases() {
        use serde_json::json;

        for (variant, lower, pascal) in PRIORITY_WIRE {
            assert_eq!(serde_json::to_value(variant).unwrap(), json!(lower));
            assert_eq!(
                serde_json::from_value::<TodoPriority>(json!(lower)).unwrap(),
                *variant
            );
            assert_eq!(
                serde_json::from_value::<TodoPriority>(json!(pascal)).unwrap(),
                *variant
            );
        }
    }

    #[test]
    fn status_parse_is_symmetric_with_serde_and_as_str() {
        for (variant, snake, pascal) in STATUS_WIRE {
            // 新 serde 表現 = as_str() 由来の DB 値 = parse 入力。
            assert_eq!(parse_status(snake), *variant);
            assert_eq!(parse_status(variant.as_str()), *variant);
            // 旧 serde 出力（PascalCase）を文字列として渡しても退化しない。
            assert_eq!(parse_status(pascal), *variant);
            let wire = serde_json::to_string(variant).unwrap();
            let wire: String = serde_json::from_str(&wire).unwrap();
            assert_eq!(parse_status(&wire), *variant);
        }
    }

    #[test]
    fn priority_parse_is_symmetric_with_serde_and_as_str() {
        for (variant, lower, pascal) in PRIORITY_WIRE {
            assert_eq!(parse_priority(lower), *variant);
            assert_eq!(parse_priority(variant.as_str()), *variant);
            assert_eq!(parse_priority(pascal), *variant);
            let wire = serde_json::to_string(variant).unwrap();
            let wire: String = serde_json::from_str(&wire).unwrap();
            assert_eq!(parse_priority(&wire), *variant);
        }
    }

    #[test]
    fn legacy_db_values_still_parse() {
        // as_str() 由来の既存 DB 値（snake_case / 小文字）。
        assert_eq!(parse_status("in_progress"), TodoStatus::InProgress);
        assert_eq!(parse_status("cancelled"), TodoStatus::Cancelled);
        assert_eq!(parse_priority("medium"), TodoPriority::Medium);
        // 旧 serde 由来の PascalCase も parse で受け付ける。
        assert_eq!(parse_status("InProgress"), TodoStatus::InProgress);
        assert_eq!(parse_priority("High"), TodoPriority::High);
    }

    /// storage / session が再エクスポートする parse の正規結果として扱う基準表。
    /// 層をまたいで同じ結果になることのアンカーとして固定する。
    #[test]
    fn canonical_parse_table_is_stable() {
        for (input, expected) in [
            ("pending", "pending"),
            ("Pending", "pending"),
            ("IN_PROGRESS", "in_progress"),
            ("in progress", "in_progress"),
            ("completed", "completed"),
            ("Completed", "completed"),
            ("cancelled", "cancelled"),
            ("Cancelled", "cancelled"),
            ("unknown", "pending"),
            ("", "pending"),
        ] {
            assert_eq!(parse_status(input).as_str(), expected);
        }

        for (input, expected) in [
            ("high", "high"),
            ("High", "high"),
            ("MEDIUM", "medium"),
            ("medium", "medium"),
            ("low", "low"),
            ("Low", "low"),
            ("unknown", "medium"),
            ("", "medium"),
        ] {
            assert_eq!(parse_priority(input).as_str(), expected);
        }
    }
}
