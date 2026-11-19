use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::ApiClient;
use crate::context::{KeybindRegistry, SessionContext};
use crate::event::EventBus;
use crate::router::Router;
use crate::theme::Theme;

#[derive(Clone)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelInfo>,
}

#[derive(Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub supports_vision: bool,
    pub supports_tools: bool,
}

#[derive(Clone)]
pub struct McpServerStatus {
    pub name: String,
    pub status: McpConnectionStatus,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum McpConnectionStatus {
    Connected,
    Disconnected,
    Failed,
    NeedsAuth,
    NeedsClientRegistration,
    Disabled,
}

#[derive(Clone)]
pub struct LspStatus {
    pub id: String,
    pub root: String,
    pub status: LspConnectionStatus,
}

#[derive(Clone, Debug)]
pub enum LspConnectionStatus {
    Connected,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    Auto,
    Show,
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageDensity {
    Compact,
    Cozy,
}

impl MessageDensity {
    pub fn from_str_lossy(s: &str) -> Self {
        if s.eq_ignore_ascii_case("cozy") {
            Self::Cozy
        } else {
            Self::Compact
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Cozy => "cozy",
        }
    }
}

pub struct AppContext {
    pub theme: RwLock<Theme>,
    pub theme_name: RwLock<String>,
    pub router: RwLock<Router>,
    pub keybind: RwLock<KeybindRegistry>,
    pub session: RwLock<SessionContext>,
    pub providers: RwLock<Vec<ProviderInfo>>,
    pub mcp_servers: RwLock<Vec<McpServerStatus>>,
    pub lsp_status: RwLock<Vec<LspStatus>>,
    pub event_bus: EventBus,
    pub current_agent: RwLock<String>,
    pub current_model: RwLock<Option<String>>,
    pub current_provider: RwLock<Option<String>>,
    pub current_variant: RwLock<Option<String>>,
    pub directory: RwLock<String>,
    pub show_sidebar: RwLock<bool>,
    pub show_header: RwLock<bool>,
    pub show_scrollbar: RwLock<bool>,
    pub tips_hidden: RwLock<bool>,
    pub sidebar_mode: RwLock<SidebarMode>,
    pub animations_enabled: RwLock<bool>,
    pub pending_permissions: RwLock<usize>,
    pub show_timestamps: RwLock<bool>,
    pub show_thinking: RwLock<bool>,
    pub show_tool_details: RwLock<bool>,
    pub message_density: RwLock<MessageDensity>,
    pub semantic_highlight: RwLock<bool>,
    pub has_connected_provider: RwLock<bool>,
    ui_kv: RwLock<UiKv>,
    // attach -p/--password で渡された認証トークン。secret として扱い、
    // Debug/ログ/エラーへ出力しないため private フィールドに保持する。
    auth_token: RwLock<Option<String>>,
    pub api_client: RwLock<Option<Arc<ApiClient>>>,
}

impl AppContext {
    pub fn new() -> Self {
        let ui_kv = UiKv::load();
        let default_theme_name = format!("opencode@{}", detect_terminal_theme_mode());
        let default_theme = Theme::by_name(&default_theme_name).unwrap_or_else(Theme::dark);
        Self {
            theme: RwLock::new(default_theme),
            theme_name: RwLock::new(default_theme_name),
            router: RwLock::new(Router::new()),
            keybind: RwLock::new(KeybindRegistry::new()),
            session: RwLock::new(SessionContext::new()),
            providers: RwLock::new(Vec::new()),
            mcp_servers: RwLock::new(Vec::new()),
            lsp_status: RwLock::new(Vec::new()),
            event_bus: EventBus::new(),
            current_agent: RwLock::new("build".to_string()),
            current_model: RwLock::new(None),
            current_provider: RwLock::new(None),
            current_variant: RwLock::new(None),
            directory: RwLock::new(String::new()),
            show_sidebar: RwLock::new(true),
            show_header: RwLock::new(ui_kv.get_bool("header_visible", true)),
            show_scrollbar: RwLock::new(ui_kv.get_bool("scrollbar_visible", false)),
            tips_hidden: RwLock::new(ui_kv.get_bool("tips_hidden", false)),
            sidebar_mode: RwLock::new(SidebarMode::Auto),
            animations_enabled: RwLock::new(true),
            pending_permissions: RwLock::new(0),
            show_timestamps: RwLock::new(ui_kv.get_timestamps()),
            show_thinking: RwLock::new(ui_kv.get_bool("thinking_visibility", true)),
            show_tool_details: RwLock::new(ui_kv.get_bool("tool_details_visibility", true)),
            message_density: RwLock::new(MessageDensity::from_str_lossy(
                &ui_kv.get_string("message_density", "compact"),
            )),
            semantic_highlight: RwLock::new(ui_kv.get_bool("semantic_highlight", true)),
            has_connected_provider: RwLock::new(false),
            ui_kv: RwLock::new(ui_kv),
            auth_token: RwLock::new(None),
            api_client: RwLock::new(None),
        }
    }

    pub fn navigate(&self, route: crate::router::Route) {
        self.router.write().navigate(route);
    }

    pub fn current_route(&self) -> crate::router::Route {
        self.router.read().current().clone()
    }

    pub fn toggle_sidebar(&self) {
        let mut sidebar = self.show_sidebar.write();
        *sidebar = !*sidebar;
    }

    pub fn toggle_header(&self) {
        let mut show = self.show_header.write();
        *show = !*show;
        self.ui_kv.write().set_bool("header_visible", *show);
    }

    pub fn toggle_scrollbar(&self) {
        let mut show = self.show_scrollbar.write();
        *show = !*show;
        self.ui_kv.write().set_bool("scrollbar_visible", *show);
    }

    pub fn toggle_tips_hidden(&self) {
        let mut hidden = self.tips_hidden.write();
        *hidden = !*hidden;
        self.ui_kv.write().set_bool("tips_hidden", *hidden);
    }

    pub fn set_model(&self, model: String, provider: String) {
        self.set_model_selection(model, Some(provider));
    }

    pub fn set_model_selection(&self, model: String, provider: Option<String>) {
        *self.current_model.write() = Some(model);
        *self.current_provider.write() = provider;
    }

    pub fn set_model_variant(&self, variant: Option<String>) {
        *self.current_variant.write() = variant;
    }

    pub fn current_model_variant(&self) -> Option<String> {
        self.current_variant.read().clone()
    }

    pub fn set_agent(&self, agent: String) {
        *self.current_agent.write() = agent;
    }

    pub fn toggle_animations(&self) {
        let mut enabled = self.animations_enabled.write();
        *enabled = !*enabled;
    }

    pub fn set_pending_permissions(&self, count: usize) {
        *self.pending_permissions.write() = count;
    }

    pub fn set_has_connected_provider(&self, connected: bool) {
        *self.has_connected_provider.write() = connected;
    }

    pub fn toggle_timestamps(&self) {
        let mut show = self.show_timestamps.write();
        *show = !*show;
        self.ui_kv.write().set_timestamps(*show);
    }

    pub fn toggle_thinking(&self) {
        let mut show = self.show_thinking.write();
        *show = !*show;
        self.ui_kv.write().set_bool("thinking_visibility", *show);
    }

    pub fn toggle_tool_details(&self) {
        let mut show = self.show_tool_details.write();
        *show = !*show;
        self.ui_kv
            .write()
            .set_bool("tool_details_visibility", *show);
    }

    pub fn toggle_message_density(&self) {
        let mut density = self.message_density.write();
        *density = match *density {
            MessageDensity::Compact => MessageDensity::Cozy,
            MessageDensity::Cozy => MessageDensity::Compact,
        };
        self.ui_kv
            .write()
            .set_string("message_density", density.as_str());
    }

    pub fn toggle_semantic_highlight(&self) {
        let mut enabled = self.semantic_highlight.write();
        *enabled = !*enabled;
        self.ui_kv.write().set_bool("semantic_highlight", *enabled);
    }

    pub fn toggle_theme_mode(&self) -> bool {
        let current = normalize_theme_name(&self.current_theme_name());
        let Some((base, variant)) = split_theme_variant(&current) else {
            return false;
        };
        let next = if variant == "dark" { "light" } else { "dark" };
        self.set_theme_by_name(&format!("{base}@{next}"))
    }

    pub fn set_theme_by_name(&self, name: &str) -> bool {
        if let Some(theme) = Theme::by_name(name) {
            *self.theme.write() = theme;
            *self.theme_name.write() = normalize_theme_name(name);
            return true;
        }
        false
    }

    pub fn current_theme_name(&self) -> String {
        self.theme_name.read().clone()
    }

    pub fn available_theme_names(&self) -> Vec<String> {
        let mut names = Theme::builtin_theme_names()
            .into_iter()
            .flat_map(|name| [format!("{name}@dark"), format!("{name}@light")])
            .collect::<Vec<_>>();
        names.sort_by_key(|a| a.to_lowercase());
        names
    }

    /// attach で指定された認証トークンを保持する。値はログへ出力しない。
    pub(crate) fn set_auth_token(&self, token: Option<String>) {
        *self.auth_token.write() = token;
    }

    /// 保持している認証トークンを取得する（呼び出し側が secret として扱う）。
    pub(crate) fn auth_token(&self) -> Option<String> {
        self.auth_token.read().clone()
    }

    pub fn set_api_client(&self, client: Arc<ApiClient>) {
        *self.api_client.write() = Some(client);
    }

    pub fn get_api_client(&self) -> Option<Arc<ApiClient>> {
        self.api_client.read().clone()
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_theme_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return format!("opencode@{}", detect_terminal_theme_mode());
    }

    if let Some((base, variant)) = split_theme_variant(trimmed) {
        return format!("{base}@{variant}");
    }

    if trimmed.eq_ignore_ascii_case("dark") {
        return "opencode@dark".to_string();
    }
    if trimmed.eq_ignore_ascii_case("light") {
        return "opencode@light".to_string();
    }

    format!("{trimmed}@dark")
}

fn detect_terminal_theme_mode() -> &'static str {
    if let Ok(mode) = std::env::var("OPENCODE_THEME_MODE") {
        if mode.eq_ignore_ascii_case("light") {
            return "light";
        }
        if mode.eq_ignore_ascii_case("dark") {
            return "dark";
        }
    }

    // Common terminal convention: COLORFGBG="fg;bg", where bg in 0..=6 is dark
    // and 7..=15 is light.
    if let Ok(colorfgbg) = std::env::var("COLORFGBG") {
        if let Some(last) = colorfgbg.split(';').next_back() {
            if let Ok(code) = last.parse::<u8>() {
                return if code <= 6 { "dark" } else { "light" };
            }
        }
    }

    "dark"
}

fn split_theme_variant(name: &str) -> Option<(&str, &str)> {
    let (base, variant) = name.rsplit_once('@').or_else(|| name.rsplit_once(':'))?;
    if base.is_empty() || !matches!(variant, "dark" | "light") {
        return None;
    }
    Some((base, variant))
}

#[derive(Default)]
struct UiKv {
    path: Option<PathBuf>,
    values: HashMap<String, Value>,
}

impl UiKv {
    fn load() -> Self {
        let Some(path) = ui_kv_path() else {
            return Self::default();
        };

        let values = fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str::<HashMap<String, Value>>(&content).ok())
            .unwrap_or_default();

        Self {
            path: Some(path),
            values,
        }
    }

    fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.values.get(key) {
            Some(Value::Bool(flag)) => *flag,
            _ => default,
        }
    }

    fn get_timestamps(&self) -> bool {
        match self.values.get("timestamps") {
            Some(Value::String(value)) if value.eq_ignore_ascii_case("show") => true,
            Some(Value::Bool(value)) => *value,
            _ => false,
        }
    }

    fn set_timestamps(&mut self, show: bool) {
        let value = if show { "show" } else { "hide" };
        self.values
            .insert("timestamps".to_string(), Value::String(value.to_string()));
        self.persist();
    }

    fn set_bool(&mut self, key: &str, value: bool) {
        self.values.insert(key.to_string(), Value::Bool(value));
        self.persist();
    }

    fn get_string(&self, key: &str, default: &str) -> String {
        match self.values.get(key) {
            Some(Value::String(s)) => s.clone(),
            _ => default.to_string(),
        }
    }

    fn set_string(&mut self, key: &str, value: &str) {
        self.values
            .insert(key.to_string(), Value::String(value.to_string()));
        self.persist();
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                tracing::warn!(%err, "failed to create TUI kv directory");
                return;
            }
        }

        let payload = match serde_json::to_string_pretty(&self.values) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(%err, "failed to encode TUI kv state");
                return;
            }
        };

        if let Err(err) = fs::write(path, payload) {
            tracing::warn!(%err, "failed to persist TUI kv state");
        }
    }
}

fn ui_kv_path() -> Option<PathBuf> {
    dirs::state_dir()
        .map(|dir| dir.join("opencode").join("kv.json"))
        .or_else(|| {
            dirs::home_dir().map(|home| {
                home.join(".local")
                    .join("state")
                    .join("opencode")
                    .join("kv.json")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ProviderInfo / ModelInfo: データ保持 ---

    #[test]
    fn model_info_retains_all_fields() {
        let model = ModelInfo {
            id: "model-1".to_string(),
            name: "Test Model".to_string(),
            context_window: 128_000,
            max_output_tokens: 8_192,
            supports_vision: true,
            supports_tools: false,
        };

        assert_eq!(model.id, "model-1");
        assert_eq!(model.name, "Test Model");
        assert_eq!(model.context_window, 128_000);
        assert_eq!(model.max_output_tokens, 8_192);
        assert!(model.supports_vision);
        assert!(!model.supports_tools);
    }

    #[test]
    fn provider_info_retains_fields_and_models() {
        let model = ModelInfo {
            id: "model-1".to_string(),
            name: "Test Model".to_string(),
            context_window: 4_096,
            max_output_tokens: 1_024,
            supports_vision: false,
            supports_tools: true,
        };
        let provider = ProviderInfo {
            id: "provider-1".to_string(),
            name: "Test Provider".to_string(),
            models: vec![model],
        };

        assert_eq!(provider.id, "provider-1");
        assert_eq!(provider.name, "Test Provider");
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].id, "model-1");
        assert_eq!(provider.models[0].name, "Test Model");
        assert_eq!(provider.models[0].context_window, 4_096);
        assert_eq!(provider.models[0].max_output_tokens, 1_024);
        assert!(!provider.models[0].supports_vision);
        assert!(provider.models[0].supports_tools);
    }

    // --- McpServerStatus / LspStatus: データ保持 ---

    #[test]
    fn mcp_server_status_retains_connected_state() {
        let server = McpServerStatus {
            name: "mcp-example".to_string(),
            status: McpConnectionStatus::Connected,
            error: None,
        };

        assert_eq!(server.name, "mcp-example");
        assert!(matches!(server.status, McpConnectionStatus::Connected));
        assert!(server.error.is_none());
    }

    #[test]
    fn mcp_server_status_retains_failed_state_with_error() {
        let server = McpServerStatus {
            name: "mcp-broken".to_string(),
            status: McpConnectionStatus::Failed,
            error: Some("handshake failed".to_string()),
        };

        assert_eq!(server.name, "mcp-broken");
        assert!(matches!(server.status, McpConnectionStatus::Failed));
        assert_eq!(server.error.as_deref(), Some("handshake failed"));
    }

    #[test]
    fn lsp_status_retains_fields_and_connected_state() {
        let status = LspStatus {
            id: "rust-analyzer".to_string(),
            root: "/workspace/project".to_string(),
            status: LspConnectionStatus::Connected,
        };

        assert_eq!(status.id, "rust-analyzer");
        assert_eq!(status.root, "/workspace/project");
        assert!(matches!(status.status, LspConnectionStatus::Connected));
    }

    #[test]
    fn lsp_status_retains_fields_and_error_state() {
        let status = LspStatus {
            id: "pyright".to_string(),
            root: "/workspace/other".to_string(),
            status: LspConnectionStatus::Error,
        };

        assert_eq!(status.id, "pyright");
        assert_eq!(status.root, "/workspace/other");
        assert!(matches!(status.status, LspConnectionStatus::Error));
    }

    // --- 列挙型の識別 (PartialEq 未実装型は matches! で判定) ---

    #[test]
    fn mcp_connection_status_variants_are_distinguishable() {
        let connected = McpConnectionStatus::Connected;
        let disconnected = McpConnectionStatus::Disconnected;
        let failed = McpConnectionStatus::Failed;
        let needs_auth = McpConnectionStatus::NeedsAuth;
        let needs_registration = McpConnectionStatus::NeedsClientRegistration;
        let disabled = McpConnectionStatus::Disabled;

        assert!(matches!(connected, McpConnectionStatus::Connected));
        assert!(matches!(disconnected, McpConnectionStatus::Disconnected));
        assert!(matches!(failed, McpConnectionStatus::Failed));
        assert!(matches!(needs_auth, McpConnectionStatus::NeedsAuth));
        assert!(matches!(
            needs_registration,
            McpConnectionStatus::NeedsClientRegistration
        ));
        assert!(matches!(disabled, McpConnectionStatus::Disabled));

        // 別バリアントとは一致しない（識別可能であることの確認）。
        assert!(!matches!(connected, McpConnectionStatus::Failed));
        assert!(!matches!(disabled, McpConnectionStatus::NeedsAuth));
    }

    #[test]
    fn lsp_connection_status_variants_are_distinguishable() {
        let connected = LspConnectionStatus::Connected;
        let error = LspConnectionStatus::Error;

        assert!(matches!(connected, LspConnectionStatus::Connected));
        assert!(matches!(error, LspConnectionStatus::Error));
        assert!(!matches!(connected, LspConnectionStatus::Error));
        assert!(!matches!(error, LspConnectionStatus::Connected));
    }

    #[test]
    fn sidebar_mode_variants_are_distinguishable() {
        let auto = SidebarMode::Auto;
        let show = SidebarMode::Show;
        let hide = SidebarMode::Hide;

        // Clone 後も元の値を使うため、clone() は必要（redundant_clone 回避）。
        let cloned = show.clone();
        assert_eq!(cloned, SidebarMode::Show);

        assert_eq!(auto, SidebarMode::Auto);
        assert_eq!(show, SidebarMode::Show);
        assert_eq!(hide, SidebarMode::Hide);
        assert_ne!(auto, show);
        assert_ne!(auto, hide);
        assert_ne!(show, hide);
    }

    #[test]
    fn message_density_variants_are_distinguishable() {
        let compact = MessageDensity::Compact;
        let cozy = MessageDensity::Cozy;

        assert_eq!(compact, MessageDensity::Compact);
        assert_eq!(cozy, MessageDensity::Cozy);
        assert_ne!(compact, cozy);
    }

    // --- MessageDensity: from_str_lossy / as_str の往復 ---
    // 期待値は実装から導出: from_str_lossy は "cozy" の大小文字無視一致のみを判定し、
    // それ以外の全入力は Compact を返す。as_str は Compact => "compact", Cozy => "cozy"。

    #[test]
    fn message_density_as_str_returns_implementation_literals() {
        assert_eq!(MessageDensity::Compact.as_str(), "compact");
        assert_eq!(MessageDensity::Cozy.as_str(), "cozy");
    }

    #[test]
    fn message_density_from_str_lossy_matches_cozy_case_insensitively() {
        assert_eq!(MessageDensity::from_str_lossy("cozy"), MessageDensity::Cozy);
        assert_eq!(MessageDensity::from_str_lossy("COZY"), MessageDensity::Cozy);
        assert_eq!(MessageDensity::from_str_lossy("CoZy"), MessageDensity::Cozy);
    }

    #[test]
    fn message_density_from_str_lossy_falls_back_to_compact() {
        // "compact" の明示分岐は無く、"cozy" でない全入力が Compact になる。
        for input in [
            "compact", "", "COMPACT", " cozy", "cozy ", "dense", "Compact",
        ] {
            assert_eq!(
                MessageDensity::from_str_lossy(input),
                MessageDensity::Compact,
                "input={input}"
            );
        }
    }

    #[test]
    fn message_density_round_trip_through_str() {
        for density in [MessageDensity::Compact, MessageDensity::Cozy] {
            let serialized = density.as_str();
            let parsed = MessageDensity::from_str_lossy(serialized);
            assert_eq!(parsed, density, "serialized={serialized}");
        }
    }

    #[test]
    fn auth_token_is_held_and_can_be_cleared() {
        let context = AppContext::new();
        assert!(context.auth_token().is_none(), "token must start unset");

        context.set_auth_token(Some("dummy-token".to_string()));
        assert_eq!(context.auth_token().as_deref(), Some("dummy-token"));

        context.set_auth_token(None);
        assert!(context.auth_token().is_none(), "token must be clearable");
    }
}
