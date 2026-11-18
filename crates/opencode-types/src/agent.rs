use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// エージェントを識別する文字列ベースの ID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentID(pub String);

/// エージェントの表示色。組み込み色または任意の色名を表す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentColor {
    /// 主色。
    Primary,
    /// 副色。
    Secondary,
    /// アクセント色。
    Accent,
    /// 成功を表す色。
    Success,
    /// 警告を表す色。
    Warning,
    /// エラーを表す色。
    Error,
    /// 情報を示す色。
    Info,
    /// 任意の色名。
    Custom(String),
}

/// エージェントの設定を表す情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// エージェントの ID。
    pub id: AgentID,
    /// 使用するモデルの名前。未指定の場合は `None`。
    pub model: Option<String>,
    /// プロバイダへのリクエスト設定。
    pub request: ProviderRequest,
    /// エージェントに渡すシステムプロンプト。
    pub system: Option<String>,
    /// エージェントの説明。
    pub description: Option<String>,
    /// エージェントの動作モード。
    pub mode: AgentMode,
    /// UI に表示しない非表示エージェントかどうか。
    pub hidden: bool,
    /// エージェントの表示色。
    pub color: Option<AgentColor>,
    /// 実行可能なステップ数の上限。
    pub steps: Option<u32>,
    /// エージェントに付与する権限のリスト（現在は簡略化）。
    pub permissions: Vec<String>, // Simplified for now
}

/// プロバイダへのリクエストに付随するヘッダとボディ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRequest {
    /// HTTP リクエストのヘッダ。
    pub headers: HashMap<String, String>,
    /// リクエストボディの JSON プロパティ。
    pub body: HashMap<String, serde_json::Value>,
}

/// エージェントがどの範囲で利用可能かを示すモード。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// サブエージェントとしてのみ利用可能。
    Subagent,
    /// プライマリエージェントとしてのみ利用可能。
    Primary,
    /// サブエージェントとプライマリの両方で利用可能。
    All,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_agent_info() {
        let agent = AgentInfo {
            id: AgentID("agent_1".to_string()),
            model: Some("gpt-4".to_string()),
            request: ProviderRequest {
                headers: HashMap::new(),
                body: HashMap::new(),
            },
            system: None,
            description: None,
            mode: AgentMode::All,
            hidden: false,
            color: Some(AgentColor::Primary),
            steps: Some(10),
            permissions: vec![],
        };
        let serialized = serde_json::to_string(&agent).unwrap();
        let deserialized: AgentInfo = serde_json::from_str(&serialized).unwrap();
        assert_eq!(agent, deserialized);
    }
}
