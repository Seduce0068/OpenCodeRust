use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use opencode_plugin::{HookContext, HookEvent, HookOutput};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionInfo {
    pub id: String,
    pub permission_type: String,
    pub pattern: Option<Pattern>,
    pub session_id: String,
    pub message_id: String,
    pub call_id: Option<String>,
    pub message: String,
    pub metadata: HashMap<String, serde_json::Value>,
    pub time: TimeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeInfo {
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Pattern {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Once,
    Always,
    Reject,
}

#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub info: PermissionInfo,
}

pub struct PermissionEngine {
    pending: HashMap<String, HashMap<String, PendingPermission>>,
    approved: HashMap<String, HashMap<String, bool>>,
}

impl PermissionEngine {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            approved: HashMap::new(),
        }
    }

    pub fn pending(&self) -> &HashMap<String, HashMap<String, PendingPermission>> {
        &self.pending
    }

    pub fn list(&self) -> Vec<&PermissionInfo> {
        let mut result: Vec<&PermissionInfo> = Vec::new();
        for items in self.pending.values() {
            for item in items.values() {
                result.push(&item.info);
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }

    fn to_keys(pattern: Option<&Pattern>, permission_type: &str) -> Vec<String> {
        match pattern {
            None => vec![permission_type.to_string()],
            Some(Pattern::Single(s)) => vec![s.clone()],
            Some(Pattern::Multiple(v)) => v.clone(),
        }
    }

    fn covered(keys: &[String], approved: &HashMap<String, bool>) -> bool {
        let patterns: Vec<&String> = approved.keys().collect();
        keys.iter()
            .all(|k| patterns.iter().any(|p| wildcard_match(k, p)))
    }

    pub fn is_approved(
        &self,
        session_id: &str,
        pattern: Option<&Pattern>,
        permission_type: &str,
    ) -> bool {
        let empty = HashMap::new();
        let approved_for_session = self.approved.get(session_id).unwrap_or(&empty);
        let keys = Self::to_keys(pattern, permission_type);
        Self::covered(&keys, approved_for_session)
    }

    pub async fn ask(&mut self, info: PermissionInfo) -> Result<(), PermissionError> {
        let session_id = info.session_id.clone();
        let permission_id = info.id.clone();

        if self.is_approved(&session_id, info.pattern.as_ref(), &info.permission_type) {
            return Ok(());
        }

        // Plugin hook: permission.ask — plugins may decide "ask" | "deny" | "allow".
        let mut hook_ctx = HookContext::new(HookEvent::PermissionAsk)
            .with_session(&session_id)
            .with_data("permission_type", serde_json::json!(&info.permission_type))
            .with_data("permission_id", serde_json::json!(&permission_id))
            .with_data("permission", serde_json::json!(&info))
            .with_data("status", serde_json::json!("ask"));
        if let Some(call_id) = &info.call_id {
            hook_ctx = hook_ctx.with_data("call_id", serde_json::json!(call_id));
        }

        let hook_outputs = opencode_plugin::trigger_collect(hook_ctx).await;
        self.decide_hook_status(info, hook_outputs)
    }

    /// `permission.ask` hook の結果を適用して承否を決定する。
    ///
    /// `ask()` が収集した hook 出力を受け取る。テストからはプロセスグローバルな
    /// PluginSystem を汚さずに同じ決定経路を直接検証できる。
    /// `is_approved` による事前判定と pending 登録のフローは変更しない。
    fn decide_hook_status(
        &mut self,
        info: PermissionInfo,
        hook_outputs: Vec<HookOutput>,
    ) -> Result<(), PermissionError> {
        let session_id = info.session_id.clone();
        let permission_id = info.id.clone();

        match aggregate_permission_status(&hook_outputs) {
            "allow" => return Ok(()),
            "deny" => {
                return Err(PermissionError::Rejected {
                    session_id: session_id.clone(),
                    permission_id: permission_id.clone(),
                    tool_call_id: info.call_id.clone(),
                });
            }
            _ => {}
        }

        self.pending
            .entry(session_id)
            .or_default()
            .insert(permission_id, PendingPermission { info });

        Ok(())
    }

    pub fn respond(
        &mut self,
        session_id: &str,
        permission_id: &str,
        response: Response,
    ) -> Result<(), PermissionError> {
        let session_pending = self.pending.get_mut(session_id).ok_or_else(|| {
            PermissionError::NotFound(session_id.to_string(), permission_id.to_string())
        })?;

        let match_item = session_pending.remove(permission_id).ok_or_else(|| {
            PermissionError::NotFound(session_id.to_string(), permission_id.to_string())
        })?;

        if response == Response::Reject {
            return Err(PermissionError::Rejected {
                session_id: session_id.to_string(),
                permission_id: permission_id.to_string(),
                tool_call_id: match_item.info.call_id.clone(),
            });
        }

        if response == Response::Always {
            let approved_session = self.approved.entry(session_id.to_string()).or_default();
            let approve_keys = Self::to_keys(
                match_item.info.pattern.as_ref(),
                &match_item.info.permission_type,
            );
            for k in approve_keys {
                approved_session.insert(k, true);
            }
        }

        Ok(())
    }

    pub fn clear_session(&mut self, session_id: &str) {
        self.pending.remove(session_id);
        self.approved.remove(session_id);
    }
}

impl Default for PermissionEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn hook_payload_object(
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    payload
        .get("output")
        .and_then(|value| value.as_object())
        .or_else(|| payload.as_object())
        .or_else(|| payload.get("data").and_then(|value| value.as_object()))
}

fn extract_permission_status(payload: &serde_json::Value) -> Option<String> {
    hook_payload_object(payload)
        .and_then(|object| object.get("status"))
        .and_then(|value| value.as_str())
        .filter(|status| matches!(*status, "ask" | "deny" | "allow"))
        .map(ToString::to_string)
}

/// 複数の `permission.ask` hook 結果を順序に依存せず集約する。
///
/// - 1 つでも `deny` を返した hook があれば `deny`（最優先・他を上書きしない）。
/// - `deny` が無く、明示的に `allow` を返した hook があれば `allow`。
/// - それ以外は既定の `ask` を維持し、通常の pending / 承認フローへ進める。
///
/// 以前は hook 出力を順に `status` へ代入していたため（最後勝ち）、
/// `deny` の後に `allow` が来ると権限承認を強制できた。ここでは各出力を
/// フラグへ畳み込むため、hook の登録順・優先度に結果が左右されない。
fn aggregate_permission_status(outputs: &[HookOutput]) -> &'static str {
    let mut has_allow = false;
    let mut has_deny = false;

    for output in outputs {
        let Some(payload) = output.payload.as_ref() else {
            continue;
        };
        match extract_permission_status(payload).as_deref() {
            Some("deny") => has_deny = true,
            Some("allow") => has_allow = true,
            _ => {}
        }
    }

    if has_deny {
        "deny"
    } else if has_allow {
        "allow"
    } else {
        "ask"
    }
}

fn wildcard_match(text: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if pattern.starts_with('*') && pattern.ends_with('*') {
        let middle = &pattern[1..pattern.len() - 1];
        return text.contains(middle);
    }

    if let Some(suffix) = pattern.strip_prefix('*') {
        return text.ends_with(suffix);
    }

    if let Some(prefix) = pattern.strip_suffix('*') {
        return text.starts_with(prefix);
    }

    text == pattern
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("Permission not found: {0}/{1}")]
    NotFound(String, String),

    #[error("Permission rejected")]
    Rejected {
        session_id: String,
        permission_id: String,
        tool_call_id: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_permission_engine() {
        let mut engine = PermissionEngine::new();

        let info = PermissionInfo {
            id: "per_test".to_string(),
            permission_type: "bash".to_string(),
            pattern: Some(Pattern::Single("ls".to_string())),
            session_id: "ses_test".to_string(),
            message_id: "msg_test".to_string(),
            call_id: None,
            message: "Execute ls command".to_string(),
            metadata: HashMap::new(),
            time: TimeInfo { created: 0 },
        };

        engine.ask(info).await.unwrap();
        assert!(!engine.list().is_empty());

        engine
            .respond("ses_test", "per_test", Response::Once)
            .unwrap();
        assert!(engine.list().is_empty());
    }

    #[test]
    fn test_wildcard_match() {
        assert!(wildcard_match("foo", "*"));
        assert!(wildcard_match("foo/bar", "foo/*"));
        assert!(wildcard_match("foo/bar/baz", "*/baz"));
        assert!(wildcard_match("foo/bar/baz", "*bar*"));
        assert!(!wildcard_match("foo", "bar"));
    }

    fn hook_output(status: &str) -> HookOutput {
        HookOutput::with_payload(serde_json::json!({ "status": status }))
    }

    fn hook_info(id: &str) -> PermissionInfo {
        PermissionInfo {
            id: id.to_string(),
            permission_type: "bash".to_string(),
            pattern: Some(Pattern::Single("ls".to_string())),
            session_id: "ses_hook".to_string(),
            message_id: "msg_hook".to_string(),
            call_id: Some("call_hook".to_string()),
            message: "Execute ls command".to_string(),
            metadata: HashMap::new(),
            time: TimeInfo { created: 0 },
        }
    }

    // 複数 hook: deny→allow / allow→deny は共に deny（順序非依存）。
    #[test]
    fn test_hook_status_deny_wins_regardless_of_order() {
        assert_eq!(
            aggregate_permission_status(&[hook_output("deny"), hook_output("allow")]),
            "deny"
        );
        assert_eq!(
            aggregate_permission_status(&[hook_output("allow"), hook_output("deny")]),
            "deny"
        );
        assert_eq!(
            aggregate_permission_status(&[
                hook_output("ask"),
                hook_output("deny"),
                hook_output("allow"),
            ]),
            "deny"
        );
        assert_eq!(
            aggregate_permission_status(&[
                hook_output("allow"),
                hook_output("ask"),
                hook_output("deny"),
            ]),
            "deny"
        );
    }

    // 複数 hook: ask→allow は明示 allow が存在し deny が無いため allow。
    #[test]
    fn test_hook_status_ask_then_allow_is_allow() {
        assert_eq!(
            aggregate_permission_status(&[hook_output("ask"), hook_output("allow")]),
            "allow"
        );
        assert_eq!(
            aggregate_permission_status(&[hook_output("allow")]),
            "allow"
        );
    }

    // deny 単体、および入れ子 payload でも deny が優先されることを確認。
    #[test]
    fn test_hook_status_deny_only_and_nested_payload() {
        assert_eq!(aggregate_permission_status(&[hook_output("deny")]), "deny");
        let nested = HookOutput::with_payload(serde_json::json!({
            "output": { "status": "deny" }
        }));
        assert_eq!(
            aggregate_permission_status(&[nested, hook_output("allow")]),
            "deny"
        );
    }

    // hook が無い／既定 ask のみの場合は "ask" を維持する。
    #[test]
    fn test_hook_status_default_ask_is_preserved() {
        assert_eq!(aggregate_permission_status(&[]), "ask");
        assert_eq!(aggregate_permission_status(&[hook_output("ask")]), "ask");
        let payload_less = HookOutput::default();
        assert_eq!(aggregate_permission_status(&[payload_less]), "ask");
    }

    // 決定経路そのもの: deny は Rejected を返し pending を作らない。
    #[test]
    fn test_decide_hook_status_deny_rejects_without_pending() {
        let mut engine = PermissionEngine::new();
        let result = engine.decide_hook_status(
            hook_info("per_deny"),
            vec![hook_output("deny"), hook_output("allow")],
        );
        assert!(matches!(result, Err(PermissionError::Rejected { .. })));
        assert!(engine.list().is_empty());
    }

    // 決定経路そのもの: allow は pending を作らず Ok を返す。
    #[test]
    fn test_decide_hook_status_allow_skips_pending() {
        let mut engine = PermissionEngine::new();
        let result = engine.decide_hook_status(
            hook_info("per_allow"),
            vec![hook_output("ask"), hook_output("allow")],
        );
        assert!(result.is_ok());
        assert!(engine.list().is_empty());
    }

    // 決定経路そのもの: ask は従前どおり pending へ積む。
    #[test]
    fn test_decide_hook_status_ask_queues_pending() {
        let mut engine = PermissionEngine::new();
        let result = engine.decide_hook_status(hook_info("per_ask"), vec![hook_output("ask")]);
        assert!(result.is_ok());
        assert_eq!(engine.list().len(), 1);
    }
}
