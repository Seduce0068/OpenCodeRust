use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;

/// エージェントを識別する文字列ベースの ID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentID(pub String);

/// エージェントの表示色。組み込み色または任意の色名を表す。
///
/// ワイヤ形式は常に単一の文字列（組み込み色は小文字名、任意色はその色名）。
/// 旧形式の `{"custom":"red"}` も読み取り互換のために受け付ける。
///
/// 実装方針: 公開 enum へ素の `#[serde(untagged)]` を付けると unit バリアントが
/// `null` として扱われ、組み込み色が文字列として往復しなくなる。そのため
/// 内部の wire enum に `untagged` を適用し、`Serialize` は常に文字列を出力する
/// 手実装で「タグ無しの文字列表現」を実現する。
///
/// 採用した退化対策: wire にタグは残さず、`Custom(known)` を**生成時に組み込み
/// バリアントへ正規化**する。`custom()` コンストラクタとデシリアライズの両方で
/// 正規化するため、`Custom("primary")` のような値は生成されず、プレーン文字列
/// wire を経ても同じ値へ往復する。`Custom` バリアントを直接構築した呼び出し元は
/// `normalized()` で同じ正規化を適用できる。
#[derive(Debug, Clone, PartialEq)]
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

impl AgentColor {
    /// 任意の色名から色を生成する。
    ///
    /// 組み込み色と同名の小文字名が渡された場合は対応する組み込みバリアントへ
    /// 正規化する（`Custom("primary")` を生成しない）。これにより wire を常に
    /// プレーン文字列のまま保ちつつ、シリアライズ→デシリアライズの往復が安定する。
    pub fn custom(name: impl Into<String>) -> Self {
        Self::from_custom_name(name.into())
    }

    /// 直接構築された `Custom` も含め、組み込み色名を組み込みバリアントへ正規化する。
    pub fn normalized(self) -> Self {
        match self {
            Self::Custom(name) => Self::from_custom_name(name),
            other => other,
        }
    }

    fn from_custom_name(name: String) -> Self {
        match BuiltinAgentColor::from_name(&name) {
            Some(builtin) => builtin.into(),
            None => Self::Custom(name),
        }
    }

    /// ワイヤ上の文字列表現を返す（任意色は保持している色名そのもの）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Accent => "accent",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Info => "info",
            Self::Custom(name) => name,
        }
    }
}

impl Serialize for AgentColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// 組み込み色の文字列表現（内部 wire 用）。
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum BuiltinAgentColor {
    Primary,
    Secondary,
    Accent,
    Success,
    Warning,
    Error,
    Info,
}

impl BuiltinAgentColor {
    /// ワイヤ上の小文字名から組み込み色を引く（正規化判定用）。
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "primary" => Some(Self::Primary),
            "secondary" => Some(Self::Secondary),
            "accent" => Some(Self::Accent),
            "success" => Some(Self::Success),
            "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "info" => Some(Self::Info),
            _ => None,
        }
    }
}

impl From<BuiltinAgentColor> for AgentColor {
    fn from(value: BuiltinAgentColor) -> Self {
        match value {
            BuiltinAgentColor::Primary => Self::Primary,
            BuiltinAgentColor::Secondary => Self::Secondary,
            BuiltinAgentColor::Accent => Self::Accent,
            BuiltinAgentColor::Success => Self::Success,
            BuiltinAgentColor::Warning => Self::Warning,
            BuiltinAgentColor::Error => Self::Error,
            BuiltinAgentColor::Info => Self::Info,
        }
    }
}

/// 旧外部タグ形式 `{"custom":"red"}` 用（内部 wire 用）。
#[derive(Deserialize)]
struct LegacyCustomAgentColor {
    #[serde(alias = "Custom")]
    custom: String,
}

/// 新形式は常に文字列、旧オブジェクト形式も許容する内部 wire 表現。
#[derive(Deserialize)]
#[serde(untagged)]
enum AgentColorWire {
    Builtin(BuiltinAgentColor),
    Custom(String),
    Legacy(LegacyCustomAgentColor),
}

impl From<AgentColorWire> for AgentColor {
    fn from(wire: AgentColorWire) -> Self {
        match wire {
            AgentColorWire::Builtin(builtin) => builtin.into(),
            // 旧オブジェクト形式で "primary" 等が来ても Custom として保持しない。
            AgentColorWire::Custom(name) => Self::from_custom_name(name),
            AgentColorWire::Legacy(LegacyCustomAgentColor { custom }) => {
                Self::from_custom_name(custom)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AgentColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(AgentColorWire::deserialize(deserializer)?.into())
    }
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

    #[test]
    fn agent_color_wire_is_always_a_plain_string() {
        use serde_json::json;

        let cases = [
            (AgentColor::Primary, json!("primary")),
            (AgentColor::Secondary, json!("secondary")),
            (AgentColor::Accent, json!("accent")),
            (AgentColor::Success, json!("success")),
            (AgentColor::Warning, json!("warning")),
            (AgentColor::Error, json!("error")),
            (AgentColor::Info, json!("info")),
            (AgentColor::Custom("red".to_string()), json!("red")),
        ];

        for (color, expected) in cases {
            let value = serde_json::to_value(&color).unwrap();
            assert_eq!(value, expected, "色は常に文字列で表現される");
            assert!(!value.is_object(), "旧 custom オブジェクト形式を出力しない");
            assert_eq!(serde_json::from_value::<AgentColor>(value).unwrap(), color);
            assert_eq!(color.as_str(), expected.as_str().unwrap());
        }
    }

    #[test]
    fn agent_color_accepts_legacy_custom_object() {
        use serde_json::json;

        let decoded: AgentColor = serde_json::from_value(json!({"custom": "red"})).unwrap();
        assert_eq!(decoded, AgentColor::Custom("red".to_string()));

        let value = json!({
            "id": "agent_legacy",
            "model": null,
            "request": {"headers": {}, "body": {}},
            "system": null,
            "description": null,
            "mode": "all",
            "hidden": false,
            "color": {"custom": "teal"},
            "steps": null,
            "permissions": []
        });
        let info: AgentInfo = serde_json::from_value(value).unwrap();
        assert_eq!(info.color, Some(AgentColor::Custom("teal".to_string())));

        let reserialized = serde_json::to_value(&info).unwrap();
        assert_eq!(reserialized["color"], json!("teal"));
    }

    #[test]
    fn agent_color_unknown_string_becomes_custom() {
        use serde_json::json;

        let decoded: AgentColor = serde_json::from_value(json!("#ff0000")).unwrap();
        assert_eq!(decoded, AgentColor::Custom("#ff0000".to_string()));
    }

    /// 回帰: `Custom("primary")` は生成時に `Primary` へ正規化され、
    /// プレーン文字列 wire を経ても同じ値へ戻る（Builtin へ退化しない）。
    #[test]
    fn agent_color_custom_with_builtin_name_normalizes_to_builtin() {
        use serde_json::json;

        // 直接構築した値も normalized() で正規化できる。
        let direct = AgentColor::Custom("primary".to_string());
        assert_eq!(direct.clone().normalized(), AgentColor::Primary);

        // 生成入口では常に正規化される。
        assert_eq!(AgentColor::custom("primary"), AgentColor::Primary);
        assert_eq!(AgentColor::custom("info"), AgentColor::Info);
        assert_eq!(
            AgentColor::custom("red"),
            AgentColor::Custom("red".to_string())
        );
        assert_eq!(
            serde_json::from_value::<AgentColor>(json!("primary")).unwrap(),
            AgentColor::Primary
        );
        // 旧オブジェクト形式の {"custom":"primary"} も Custom として残さない。
        assert_eq!(
            serde_json::from_value::<AgentColor>(json!({ "custom": "primary" })).unwrap(),
            AgentColor::Primary
        );

        // 正規化済みの値は往復しても変化しない。
        for color in [
            AgentColor::custom("primary"),
            AgentColor::Custom("red".to_string()),
        ] {
            let value = serde_json::to_value(&color).unwrap();
            assert_eq!(serde_json::from_value::<AgentColor>(value).unwrap(), color);
        }
    }
}
