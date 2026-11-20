//! プラグイン機構とフック（Hook）の登録・実行を提供するクレート。
//!
//! `HookEvent` ごとに `Hook` を登録すると `PluginSystem` が優先度順に実行し、
//! `PluginRegistry` がプラグインの登録とフックシステムへの接続を管理する。
//! `subprocess` は TypeScript 製プラグインを子プロセスとして起動し、
//! Content-Length フレームの JSON-RPC で通信するサポートを提供する。
//!
//! # 使い方
//!
//! `Hook::new` でフックを作り `PluginSystem::register` に登録した上で、
//! `HookContext` を渡して `trigger` / `trigger_collect` を呼び出す。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub mod subprocess;

/// 1 つのフックハンドラの実行時間上限。
///
/// `PluginSystem::trigger` がハンドラごとに適用するため、1 件がハングしても
/// 後続フックの実行はブロックされない。タイムアウトしたハンドラの結果は
/// `HookError::Timeout` となる。
pub const PLUGIN_HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// 自由形式のエラー文字列に含まれる認証情報を伏せる。
///
/// プラグインフックのエラーやツール実行エラーは `key=value` 形式のトークン、
/// `Bearer <value>`、URL の userinfo (`user:pass@host`) をそのまま含むことがある。
/// これをフックデータや `tracing` ログへ渡す前に caller が必ず通すこと。
///
/// 依存を増やさないため regex は使わず、区切り文字を基準にした走査で処理する。
/// 既知のキー名・トークン接頭辞・スキーム付き URL の userinfo だけを対象とし、
/// ホストやパスなど障害追跡に必要な情報は残す。
pub fn redact_sensitive_text(input: &str) -> String {
    const REDACTED: &str = "<redacted>";
    const SECRET_KEYS: &[&str] = &[
        "x-goog-api-key",
        "x-api-key",
        "x-auth-token",
        "api_key",
        "apikey",
        "api-key",
        "access_token",
        "access-token",
        "refresh_token",
        "refresh-token",
        "auth_token",
        "auth-token",
        "client_secret",
        "client-secret",
        "authorization",
        "signature",
        "secret",
        "password",
        "token",
        "key",
        "code",
        "sig",
    ];
    const TOKEN_PREFIXES: &[&str] = &[
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "AKIA",
        "ASIA",
        "eyJ",
    ];

    fn is_edge(byte: u8) -> bool {
        byte.is_ascii_whitespace()
            || matches!(
                byte,
                b'"' | b'\''
                    | b'<'
                    | b'>'
                    | b'('
                    | b')'
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b','
                    | b';'
                    | b'='
                    | b'&'
                    | b'?'
                    | b'#'
                    | b':'
                    | b'@'
            )
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        // scheme://user:pass@host -> scheme://host
        if bytes[index].is_ascii_alphabetic() {
            let mut scheme_end = index;
            while scheme_end < bytes.len()
                && (bytes[scheme_end].is_ascii_alphanumeric()
                    || matches!(bytes[scheme_end], b'+' | b'-' | b'.'))
            {
                scheme_end += 1;
            }
            if bytes
                .get(scheme_end..)
                .is_some_and(|tail| tail.starts_with(b"://"))
            {
                let authority_start = scheme_end + 3;
                let mut authority_end = authority_start;
                while authority_end < bytes.len()
                    && !matches!(
                        bytes[authority_end],
                        b'/' | b'?'
                            | b'#'
                            | b' '
                            | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'"'
                            | b'\''
                            | b'<'
                            | b'>'
                            | b')'
                            | b','
                            | b';'
                    )
                {
                    authority_end += 1;
                }
                let authority = &input[authority_start..authority_end];
                if let Some(at) = authority.rfind('@') {
                    out.push_str(&input[index..authority_start]);
                    index = authority_start + at + 1;
                    continue;
                }
                out.push_str(&input[index..authority_start]);
                index = authority_start;
                continue;
            }
        }

        // Bearer <value> -> Bearer <redacted>
        if index == 0 || is_edge(bytes[index - 1]) {
            let rest = &input[index..];
            if rest.len() >= 6 && rest.as_bytes()[..6].eq_ignore_ascii_case(b"bearer") {
                let after = index + 6;
                if bytes.get(after) == Some(&b' ') {
                    out.push_str(&input[index..after]);
                    let mut value_end = after + 1;
                    while value_end < bytes.len() && !is_edge(bytes[value_end]) {
                        value_end += 1;
                    }
                    out.push_str(REDACTED);
                    index = value_end;
                    continue;
                }
            }
        }

        // key=value -> key=<redacted>
        let mut key_end = index;
        while key_end < bytes.len()
            && (bytes[key_end].is_ascii_alphanumeric() || matches!(bytes[key_end], b'_' | b'-'))
        {
            key_end += 1;
        }
        if key_end > index && bytes.get(key_end) == Some(&b'=') {
            let key = input[index..key_end].to_ascii_lowercase();
            if SECRET_KEYS.contains(&key.as_str()) {
                out.push_str(&input[index..=key_end]);
                out.push_str(REDACTED);
                let mut value_end = key_end + 1;
                while value_end < bytes.len()
                    && !matches!(
                        bytes[value_end],
                        b'&' | b'#'
                            | b' '
                            | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'"'
                            | b'\''
                            | b'<'
                            | b'>'
                            | b','
                            | b';'
                            | b')'
                    )
                {
                    value_end += 1;
                }
                index = value_end;
                continue;
            }
        }

        // Well-known token prefixes (sk-, ghp_, JWT ...).
        if index == 0 || is_edge(bytes[index - 1]) {
            if let Some(prefix) = TOKEN_PREFIXES
                .iter()
                .find(|&&prefix| input[index..].starts_with(prefix))
            {
                let mut value_end = index + prefix.len();
                while value_end < bytes.len()
                    && (bytes[value_end].is_ascii_alphanumeric()
                        || matches!(bytes[value_end], b'-' | b'_' | b'.' | b'+' | b'/'))
                {
                    value_end += 1;
                }
                out.push_str(REDACTED);
                index = value_end;
                continue;
            }
        }

        let ch = input[index..].chars().next().expect("non-empty suffix");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// HookError をログへ出す直前に通す整形ヘルパ。
fn redacted_hook_error(error: &HookError) -> String {
    redact_sensitive_text(&error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

impl HookOutput {
    pub fn with_payload(payload: serde_json::Value) -> Self {
        Self {
            payload: Some(payload),
        }
    }
}

impl From<()> for HookOutput {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

impl From<serde_json::Value> for HookOutput {
    fn from(payload: serde_json::Value) -> Self {
        HookOutput::with_payload(payload)
    }
}

pub type HookResult = Result<HookOutput, HookError>;

pub type HookHandler =
    Box<dyn Fn(HookContext) -> Pin<Box<dyn Future<Output = HookResult> + Send>> + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum HookEvent {
    // Original events
    ConfigLoaded,
    SessionStart,
    SessionEnd,
    ToolCall,
    ToolResult,
    MessageSent,
    MessageReceived,
    Error,
    FileChange,
    ProviderChange,

    // Tool lifecycle hooks (matches TS "tool.execute.before" / "tool.execute.after")
    ToolExecuteBefore,
    ToolExecuteAfter,

    // Tool definition transform (matches TS "tool.definition")
    ToolDefinition,

    // Chat / LLM hooks
    ChatSystemTransform,
    ChatMessagesTransform,
    ChatParams,
    ChatHeaders,
    ChatMessage,

    // Session compaction (matches TS "experimental.session.compacting")
    SessionCompacting,

    // Text completion (matches TS "experimental.text.complete")
    TextComplete,

    // Shell environment (matches TS "shell.env")
    ShellEnv,

    // Command execution (matches TS "command.execute.before")
    CommandExecuteBefore,

    // Permission (matches TS "permission.ask")
    PermissionAsk,
}

#[derive(Debug, Clone)]
pub struct HookContext {
    pub event: HookEvent,
    pub data: HashMap<String, serde_json::Value>,
    pub session_id: Option<String>,
    pub timestamp: DateTime<Utc>,
}

impl HookContext {
    pub fn new(event: HookEvent) -> Self {
        Self {
            event,
            data: HashMap::new(),
            session_id: None,
            timestamp: Utc::now(),
        }
    }

    pub fn with_data(mut self, key: &str, value: serde_json::Value) -> Self {
        self.data.insert(key.to_string(), value);
        self
    }

    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("Hook execution failed: {0}")]
    ExecutionError(String),

    #[error("Hook not found: {0}")]
    NotFound(String),

    #[error("Hook timeout")]
    Timeout,
}

pub struct Hook {
    pub name: String,
    pub event: HookEvent,
    pub handler: HookHandler,
    pub priority: i32,
    pub enabled: bool,
}

impl Hook {
    pub fn new<F, Fut, R>(name: &str, event: HookEvent, handler: F) -> Self
    where
        F: Fn(HookContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, HookError>> + Send + 'static,
        R: Into<HookOutput> + Send + 'static,
    {
        Self {
            name: name.to_string(),
            event,
            handler: Box::new(move |ctx| {
                let future = handler(ctx);
                Box::pin(async move { future.await.map(Into::into) })
            }),
            priority: 0,
            enabled: true,
        }
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

pub struct PluginSystem {
    hooks: RwLock<HashMap<HookEvent, Vec<Arc<Hook>>>>,
}

impl PluginSystem {
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, hook: Hook) {
        let mut hooks = self.hooks.write().await;
        let entry = hooks.entry(hook.event.clone()).or_insert_with(Vec::new);
        entry.push(Arc::new(hook));
        entry.sort_by_key(|h| std::cmp::Reverse(h.priority));
    }

    /// 登録済みフックを優先度順に実行する。
    ///
    /// `PLUGIN_HOOK_TIMEOUT` は trigger 全体ではなく各ハンドラに個別適用する。
    /// 1 件がハングしても timeout で打ち切って後続フックへ進むため、
    /// trigger 全体が止まることはない。
    pub async fn trigger(&self, context: HookContext) -> Vec<HookResult> {
        self.trigger_with_timeout(context, PLUGIN_HOOK_TIMEOUT)
            .await
    }

    /// `trigger` の実体。`hook_timeout` を差し替えられるようにして
    /// 回帰テストで短い timeout を使えるようにする。
    async fn trigger_with_timeout(
        &self,
        context: HookContext,
        hook_timeout: Duration,
    ) -> Vec<HookResult> {
        // 読み取りロックはスナップショットを作るためだけに保持する。
        // ハンドラ実行中にロックを保持すると register/remove の write ロック待ちで
        // デッドロックするため、Arc だけをクローンしてガードを先に drop する。
        let snapshot: Vec<Arc<Hook>> = {
            let hooks = self.hooks.read().await;
            hooks
                .get(&context.event)
                .map(|hook_list| {
                    hook_list
                        .iter()
                        .filter(|hook| hook.enabled)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };

        let mut results = Vec::with_capacity(snapshot.len());
        for hook in snapshot {
            let outcome =
                match tokio::time::timeout(hook_timeout, (hook.handler)(context.clone())).await {
                    Ok(result) => result,
                    Err(_) => Err(HookError::Timeout),
                };
            results.push(outcome);
        }

        results
    }

    pub async fn remove(&self, event: &HookEvent, name: &str) -> bool {
        let mut hooks = self.hooks.write().await;
        if let Some(hook_list) = hooks.get_mut(event) {
            let initial_len = hook_list.len();
            hook_list.retain(|h| h.name != name);
            return hook_list.len() < initial_len;
        }
        false
    }

    pub async fn list(&self) -> Vec<(HookEvent, String, bool)> {
        let hooks = self.hooks.read().await;
        let mut result = Vec::new();

        for (event, hook_list) in hooks.iter() {
            for hook in hook_list {
                result.push((event.clone(), hook.name.clone(), hook.enabled));
            }
        }

        result
    }
}

impl Default for PluginSystem {
    fn default() -> Self {
        Self::new()
    }
}

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn register_hooks(
        &self,
        system: &PluginSystem,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

pub struct PluginRegistry {
    plugins: RwLock<Vec<Arc<dyn Plugin>>>,
    hook_system: Arc<PluginSystem>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(Vec::new()),
            hook_system: Arc::new(PluginSystem::new()),
        }
    }

    pub async fn register(&self, plugin: Arc<dyn Plugin>) {
        plugin.register_hooks(&self.hook_system).await;
        let mut plugins = self.plugins.write().await;
        plugins.push(plugin);
    }

    pub fn hook_system(&self) -> Arc<PluginSystem> {
        self.hook_system.clone()
    }

    pub async fn list(&self) -> Vec<(String, String)> {
        let plugins = self.plugins.read().await;
        plugins
            .iter()
            .map(|p| (p.name().to_string(), p.version().to_string()))
            .collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Global Plugin System
// ============================================================================

static GLOBAL_PLUGIN_SYSTEM: std::sync::OnceLock<Arc<PluginSystem>> = std::sync::OnceLock::new();

/// Initialize the global plugin system. Call once at startup.
pub fn init_global(system: Arc<PluginSystem>) {
    let existing = GLOBAL_PLUGIN_SYSTEM.get_or_init(|| Arc::clone(&system));
    if !Arc::ptr_eq(existing, &system) {
        tracing::debug!("global plugin system already initialized; ignoring duplicate init");
    }
}

/// Get the global plugin system, creating a default one if not initialized.
pub fn global() -> Arc<PluginSystem> {
    GLOBAL_PLUGIN_SYSTEM
        .get_or_init(|| Arc::new(PluginSystem::new()))
        .clone()
}

/// Convenience: trigger a hook event on the global plugin system.
/// Errors from individual hooks are logged but do not propagate.
/// Each handler is bounded by `PLUGIN_HOOK_TIMEOUT`, so one hanging hook does
/// not prevent later hooks from running.
pub async fn trigger(context: HookContext) {
    let system = global();
    let results = system.trigger(context).await;
    for result in results {
        if let Err(e) = result {
            tracing::warn!("Plugin hook error: {}", redacted_hook_error(&e));
        }
    }
}

/// Convenience: trigger a hook event and collect successful outputs.
/// Errors from individual hooks are logged but do not propagate.
/// As with `trigger`, `PLUGIN_HOOK_TIMEOUT` applies to each handler
/// individually, not to the whole trigger.
pub async fn trigger_collect(context: HookContext) -> Vec<HookOutput> {
    let system = global();
    let results = system.trigger(context).await;
    let mut outputs = Vec::new();
    for result in results {
        match result {
            Ok(output) => outputs.push(output),
            Err(e) => tracing::warn!("Plugin hook error: {}", redacted_hook_error(&e)),
        }
    }
    outputs
}

/// Convenience: build a HookContext and trigger it on the global system.
pub async fn trigger_event(event: HookEvent) {
    trigger(HookContext::new(event)).await;
}

/// Convenience: build a HookContext with session and trigger it.
pub async fn trigger_session_event(event: HookEvent, session_id: &str) {
    trigger(HookContext::new(event).with_session(session_id)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hook_new_supports_unit_result() {
        let system = PluginSystem::new();
        system
            .register(Hook::new("unit", HookEvent::SessionStart, |_ctx| async {
                Ok(())
            }))
            .await;

        let result = system
            .trigger(HookContext::new(HookEvent::SessionStart))
            .await;
        assert_eq!(result.len(), 1);
        assert!(result[0].as_ref().unwrap().payload.is_none());
    }

    #[tokio::test]
    async fn hook_new_supports_payload_result() {
        let system = PluginSystem::new();
        system
            .register(Hook::new(
                "payload",
                HookEvent::SessionCompacting,
                |_ctx| async {
                    Ok(serde_json::json!({
                        "prompt": "override",
                        "context": ["ctx1"]
                    }))
                },
            ))
            .await;

        let result = system
            .trigger(HookContext::new(HookEvent::SessionCompacting))
            .await;
        assert_eq!(result.len(), 1);
        let payload = result[0]
            .as_ref()
            .unwrap()
            .payload
            .as_ref()
            .expect("payload should be present");
        assert_eq!(
            payload.get("prompt").and_then(|v| v.as_str()),
            Some("override")
        );
    }

    #[tokio::test]
    async fn trigger_releases_read_lock_before_running_handlers() {
        // ハンドラ内から write ロックを要する remove を呼ぶ。旧実装では
        // trigger が read ロックを保持したまま await するためデッドロックする。
        let system = Arc::new(PluginSystem::new());
        let system_for_hook = Arc::clone(&system);
        system
            .register(Hook::new("reentrant", HookEvent::ToolCall, move |_ctx| {
                let system = Arc::clone(&system_for_hook);
                async move {
                    system.remove(&HookEvent::ToolCall, "reentrant").await;
                    Ok(())
                }
            }))
            .await;

        let results = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            system.trigger(HookContext::new(HookEvent::ToolCall)),
        )
        .await
        .expect("trigger must not hold the read lock while a handler takes the write lock");

        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        assert!(!system
            .list()
            .await
            .iter()
            .any(|(_, name, _)| name == "reentrant"));
    }

    #[tokio::test]
    async fn trigger_snapshot_is_stable_when_handler_registers_a_hook() {
        let system = Arc::new(PluginSystem::new());
        let system_for_hook = Arc::clone(&system);
        system
            .register(Hook::new("first", HookEvent::MessageSent, move |_ctx| {
                let system = Arc::clone(&system_for_hook);
                async move {
                    system
                        .register(Hook::new("late", HookEvent::MessageSent, |_ctx| async {
                            Ok::<(), HookError>(())
                        }))
                        .await;
                    Ok(())
                }
            }))
            .await;

        let results = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            system.trigger(HookContext::new(HookEvent::MessageSent)),
        )
        .await
        .expect("register from a handler must not deadlock");

        // 実行中に登録したフックは今回の実行には含まれない（スナップショット）。
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        assert_eq!(system.list().await.len(), 2);
    }

    #[tokio::test]
    async fn trigger_keeps_priority_order_and_skips_disabled_hooks() {
        let system = PluginSystem::new();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        for (name, priority, enabled) in [("low", 0, true), ("high", 10, true), ("off", 100, false)]
        {
            let order = Arc::clone(&order);
            let observed = name.to_string();
            system
                .register(
                    Hook::new(name, HookEvent::SessionStart, move |_ctx| {
                        order.lock().unwrap().push(observed.clone());
                        async { Ok(()) }
                    })
                    .with_priority(priority)
                    .enabled(enabled),
                )
                .await;
        }

        let results = system
            .trigger(HookContext::new(HookEvent::SessionStart))
            .await;
        assert_eq!(results.len(), 2, "disabled hooks must be skipped");
        assert_eq!(
            *order.lock().unwrap(),
            vec!["high".to_string(), "low".to_string()]
        );
    }

    /// R5(medium) 回帰: 1 件のハングしたフックで trigger 全体が止まらず、
    /// timeout 後に後続フックが実行されること。
    #[tokio::test]
    async fn trigger_timeout_on_one_hook_does_not_block_later_hooks() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let system = PluginSystem::new();
        let later_ran = Arc::new(AtomicBool::new(false));
        let later_ran_for_hook = Arc::clone(&later_ran);

        system
            .register(
                Hook::new("hang", HookEvent::Error, |_ctx| async {
                    std::future::pending::<Result<(), HookError>>().await
                })
                .with_priority(10),
            )
            .await;
        system
            .register(
                Hook::new("later", HookEvent::Error, move |_ctx| {
                    let later_ran = Arc::clone(&later_ran_for_hook);
                    async move {
                        later_ran.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                })
                .with_priority(0),
            )
            .await;

        // 実運用は PLUGIN_HOOK_TIMEOUT(5s)。テストは短い timeout に差し替える。
        let results = system
            .trigger_with_timeout(
                HookContext::new(HookEvent::Error),
                Duration::from_millis(50),
            )
            .await;

        assert_eq!(results.len(), 2, "both hooks must appear in the results");
        assert!(matches!(results[0], Err(HookError::Timeout)));
        assert!(results[1].is_ok(), "later hook must complete successfully");
        assert!(
            later_ran.load(Ordering::SeqCst),
            "later hook must run after the first one times out"
        );
    }

    #[test]
    fn redacted_hook_error_hides_token_values_and_url_userinfo() {
        let token = "tok_super_secret_9f8e7d";
        let error = HookError::ExecutionError(format!(
            "request to https://svc-user:{token}@api.example.com failed: token={token} Bearer {token}"
        ));

        let redacted = redacted_hook_error(&error);
        assert!(
            !redacted.contains(token),
            "token leaked into log: {redacted}"
        );
        assert!(
            !redacted.contains("svc-user"),
            "URL userinfo leaked into log: {redacted}"
        );
        assert!(
            !redacted.contains('@'),
            "URL userinfo separator leaked into log: {redacted}"
        );
        assert!(
            redacted.contains("<redacted>"),
            "no redaction marker: {redacted}"
        );
        assert!(
            redacted.contains("api.example.com"),
            "host must stay for diagnosis: {redacted}"
        );
    }

    #[test]
    fn redact_sensitive_text_masks_known_token_prefixes() {
        let raw = "denied for sk-abcdefghijklmnopqrstuvwxyz and ghp_1234567890";
        let redacted = redact_sensitive_text(raw);
        assert!(
            !redacted.contains("sk-abcdefghijklmnopqrstuvwxyz"),
            "token leaked: {redacted}"
        );
        assert!(
            !redacted.contains("ghp_1234567890"),
            "token leaked: {redacted}"
        );
    }
}
