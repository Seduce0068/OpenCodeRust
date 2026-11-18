use serde::{Deserialize, Serialize};
use std::fmt;

/// PTY（疑似端末）を識別する文字列ベースの ID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PtyID(pub String);

impl fmt::Display for PtyID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// セッションを識別する文字列ベースの ID。SQLx では透明型として扱われる。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(transparent)]
pub struct SessionID(pub String);

impl fmt::Display for SessionID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
