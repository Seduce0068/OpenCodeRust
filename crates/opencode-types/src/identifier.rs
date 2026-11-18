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

/// セッションを識別する文字列ベースの ID。
///
/// DB へのバインドは storage 側で `.0`（`String`）を直接使用するため、
/// ここでは SQLx のトレイトを実装しない。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionID(pub String);

impl fmt::Display for SessionID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn pty_id_display_and_serde_roundtrip() {
        let id = PtyID("pty_123".to_string());

        assert_eq!(id.to_string(), "pty_123");

        // 新型構造体は内側の String と同じ JSON 文字列として表現される（rename 属性なし）
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"pty_123\"");

        let decoded: PtyID = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn session_id_display_and_serde_roundtrip() {
        let id = SessionID("sess_abc".to_string());

        assert_eq!(id.to_string(), "sess_abc");

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"sess_abc\"");

        let decoded: SessionID = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn id_types_support_equality_and_hashing() {
        let mut session_ids = HashSet::new();
        session_ids.insert(SessionID("sess_1".to_string()));
        let mut pty_ids = HashSet::new();
        pty_ids.insert(PtyID("pty_1".to_string()));

        assert!(session_ids.contains(&SessionID("sess_1".to_string())));
        assert!(pty_ids.contains(&PtyID("pty_1".to_string())));
        assert_ne!(
            SessionID("sess_1".to_string()),
            SessionID("sess_2".to_string())
        );
        assert_ne!(PtyID("pty_1".to_string()), PtyID("pty_2".to_string()));
    }
}
