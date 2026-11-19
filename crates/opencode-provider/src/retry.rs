//! 再試行判定トレイトを提供する。
//!
//! 再試行間隔の算出とキャンセル可能な sleep は `opencode-session::retry` が
//! 単一の情報源として保持する。本モジュールには重複実装を置かない。

/// Implement this on error types to expose whether an error is transient.
/// Return `Some(message)` when the error is retryable, `None` otherwise.
pub trait IsRetryable {
    fn is_retryable(&self) -> Option<String>;
}
