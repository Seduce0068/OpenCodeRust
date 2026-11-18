use serde::{Deserialize, Serialize};
use std::fmt;

/// プロジェクトを識別する文字列ベースの ID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectID(pub String);

impl fmt::Display for ProjectID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_display_and_serde_roundtrip() {
        let id = ProjectID("proj_42".to_string());

        assert_eq!(id.to_string(), "proj_42");

        // 新型構造体は内側の String と同じ JSON 文字列として表現される（rename 属性なし）
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"proj_42\"");

        let decoded: ProjectID = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }
}
