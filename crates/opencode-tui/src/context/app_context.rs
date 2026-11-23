use parking_lot::RwLock;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    /// SSE と resync の両経路で共有する要求 ID 集合。
    /// キーは `permission:{id}` / `question:{id}` 形式で、
    /// 同じ保留要求が二重に enqueue されるのを防ぐ（リスレッド共有）。
    pub seen_request_ids: RwLock<HashSet<String>>,
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
    /// 既定値のみで `AppContext` を構築する。
    ///
    /// `opencode-config` の設定ファイルは読み込まない。ただし UI 状態
    /// (`kv.json`) の読み込みと、破損時の `.bak` 退避は行う。
    /// UI 関連 Config を反映したい場合は [`Self::new_with_config`] を使う。
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
            seen_request_ids: RwLock::new(HashSet::new()),
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

    /// 既定値の上に UI 関連 Config（`theme` / `tui.sidebar`）を反映する。
    ///
    /// 設定ファイルの読み込みは行わず、反映する値は引数のみに依存する。
    pub fn new_with_config(config: &opencode_config::Config) -> Self {
        let context = Self::new();
        context.apply_ui_config(config);
        context
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

    /// opencode-config の UI 関連設定を既存状態へ反映する（後方互換）。
    ///
    /// 未設定の項目は `detect_terminal_theme_mode()` 由来の既定値を維持する。
    /// `theme` と `tui.sidebar` のみをマッピングし、それ以外は既存フィールドが
    /// 無いため反映しない（詳細は各ブロックのコメントを参照）。
    pub fn apply_ui_config(&self, config: &opencode_config::Config) {
        if let Some(name) = config
            .theme
            .as_deref()
            .filter(|name| !name.trim().is_empty())
        {
            // 未知名は set_theme_by_name の既存挙動（状態を変えず false）に委譲する。
            let _ = self.set_theme_by_name(name);
        }

        if let Some(sidebar) = config.tui.as_ref().and_then(|tui| tui.sidebar) {
            *self.show_sidebar.write() = sidebar;
        }
        // tui.mode / scroll_speed / scroll_acceleration / diff_style は
        // AppContext に対応フィールドが無いためマッピングしない。

        // config.layout (opencode_config::LayoutMode::Auto / Stretch) は
        // AppContext に対応フィールドが無いためスキップする。
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
        Self::load_from(&path)
    }

    /// 指定パスから kv を読み込む。破損/読取不能な既存ファイルは `.bak` へ
    /// 退避してから空の状態で起動する（初回起動の NotFound は通常ケースとして扱う）。
    fn load_from(path: &Path) -> Self {
        let values = match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<HashMap<String, Value>>(&content) {
                Ok(values) => values,
                Err(_) => {
                    Self::quarantine_corrupted(path);
                    HashMap::new()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(_) => {
                Self::quarantine_corrupted(path);
                HashMap::new()
            }
        };

        Self {
            path: Some(path.to_path_buf()),
            values,
        }
    }

    /// 破損した kv ファイルを `.bak` へ退避し、固定文言で warn を出す。
    /// パスや値はログに出さない。退避の成否を文言へ反映する（R4）。
    fn quarantine_corrupted(path: &Path) {
        if fs::rename(path, backup_sibling(path)).is_ok() {
            tracing::warn!(
                "corrupted TUI kv state detected; moved aside and starting with defaults"
            );
        } else {
            tracing::warn!("corrupted TUI kv state detected; starting with defaults");
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

        if let Err(err) = Self::atomic_write(path, payload.as_bytes()) {
            tracing::warn!(%err, "failed to persist TUI kv state");
        }
    }

    /// temp sibling に書き出して fsync してから rename することで、
    /// 途中失敗時に既存 kv を維持する atomic 保存を行う。
    fn atomic_write(path: &Path, payload: &[u8]) -> std::io::Result<()> {
        Self::atomic_write_with(path, payload, || Ok(()))
    }

    /// `before_rename` は sync_all 後・rename 直前の失敗注入ポイント。
    /// 本番の [`Self::atomic_write`] は常に Ok を渡し、テストで rename 前失敗を模擬する。
    fn atomic_write_with<F>(path: &Path, payload: &[u8], before_rename: F) -> std::io::Result<()>
    where
        F: FnOnce() -> std::io::Result<()>,
    {
        let temp = temp_sibling(path);
        // 既存ファイルの権限を temp へ引き継ぐ（0644 等での露出を防ぐ、R4）。
        let permissions = fs::metadata(path).ok().map(|m| m.permissions());
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&temp)?;
            if let Some(permissions) = permissions {
                fs::set_permissions(&temp, permissions)?;
            }
            file.write_all(payload)?;
            file.sync_all()?;
            before_rename()?;
            fs::rename(&temp, path)?;
            Ok(())
        })();

        if result.is_err() {
            // 失敗時に temp が残っても次回起動へ影響しないよう削除（失敗は無視）。
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

/// kv ファイルの退避先（sibling）を返す。
fn backup_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

/// kv ファイルの書き込み用 temp（sibling）を返す。
fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    // --- SER2-01: kv.json の退避と atomic 保存 ---

    /// テスト用の一意な一時ディレクトリ。Drop で片付ける。
    struct TempKvDir(PathBuf);

    impl TempKvDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "opencode-tui-kv-{tag}-{}-{nanos}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp kv dir");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.join("kv.json")
        }

        fn backup(&self) -> PathBuf {
            backup_sibling(&self.path())
        }

        fn temp(&self) -> PathBuf {
            temp_sibling(&self.path())
        }
    }

    impl Drop for TempKvDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn corrupted_kv_is_backed_up_and_values_reset() {
        let dir = TempKvDir::new("corrupt");
        let original = "{ this is not valid json";
        fs::write(dir.path(), original).expect("write corrupted kv");

        let kv = UiKv::load_from(&dir.path());

        assert!(kv.values.is_empty(), "corrupted values must not be used");
        assert_eq!(
            fs::read_to_string(dir.backup()).expect("read backup"),
            original,
            "corrupted file must be preserved as .bak"
        );
        assert!(!dir.path().exists(), "corrupted file must be moved aside");
    }

    #[test]
    fn missing_kv_loads_as_empty_without_backup() {
        let dir = TempKvDir::new("missing");

        let kv = UiKv::load_from(&dir.path());

        assert!(kv.values.is_empty());
        assert!(!dir.backup().exists());
    }

    #[test]
    fn valid_kv_loads_unchanged_without_backup() {
        let dir = TempKvDir::new("valid");
        let original = r#"{"header_visible":false,"message_density":"cozy","timestamps":"show"}"#;
        fs::write(dir.path(), original).expect("write valid kv");

        let kv = UiKv::load_from(&dir.path());

        assert!(!kv.get_bool("header_visible", true));
        assert_eq!(kv.get_string("message_density", "compact"), "cozy");
        assert!(kv.get_timestamps());
        assert!(!dir.backup().exists());
        assert_eq!(fs::read_to_string(dir.path()).expect("read kv"), original);
    }

    #[test]
    fn atomic_write_failure_before_rename_keeps_existing_file_and_removes_temp() {
        let dir = TempKvDir::new("atomic-fail");
        let original = r#"{"header_visible":true}"#;
        fs::write(dir.path(), original).expect("write original kv");

        let result = UiKv::atomic_write_with(&dir.path(), br#"{"header_visible":false}"#, || {
            Err(std::io::Error::from(std::io::ErrorKind::InvalidData))
        });

        assert!(
            result.is_err(),
            "simulated pre-rename failure must propagate"
        );
        assert_eq!(
            fs::read_to_string(dir.path()).expect("read original kv"),
            original,
            "existing kv must survive a failure before rename"
        );
        assert!(!dir.temp().exists(), "temp file must be removed on failure");
    }

    #[test]
    fn persist_writes_complete_json_and_reloads() {
        let dir = TempKvDir::new("persist");
        fs::write(dir.path(), r#"{"old":true}"#).expect("write original kv");
        let mut kv = UiKv {
            path: Some(dir.path()),
            values: HashMap::new(),
        };

        kv.set_bool("header_visible", false);
        kv.set_string("message_density", "cozy");

        let written = fs::read_to_string(dir.path()).expect("read persisted kv");
        let parsed: HashMap<String, Value> =
            serde_json::from_str(&written).expect("persisted kv must be valid JSON");
        assert_eq!(parsed.get("header_visible"), Some(&Value::Bool(false)));
        assert_eq!(
            parsed.get("message_density"),
            Some(&Value::String("cozy".to_string()))
        );
        assert!(!parsed.contains_key("old"), "old state must be replaced");
        assert!(
            !dir.temp().exists(),
            "no temp file must remain after success"
        );

        let reloaded = UiKv::load_from(&dir.path());
        assert!(!reloaded.get_bool("header_visible", true));
        assert_eq!(reloaded.get_string("message_density", "compact"), "cozy");
    }

    // --- CFGAPPLY-06: Config の theme / tui.sidebar 反映と後方互換 ---

    #[test]
    fn apply_ui_config_applies_theme_and_sidebar() {
        let context = AppContext::new();
        let config = opencode_config::Config {
            theme: Some("opencode@light".to_string()),
            tui: Some(opencode_config::TuiConfig {
                sidebar: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };

        context.apply_ui_config(&config);

        assert_eq!(context.current_theme_name(), "opencode@light");
        assert!(!*context.show_sidebar.read());
    }

    #[test]
    fn apply_ui_config_keeps_defaults_when_unset() {
        let context = AppContext::new();
        let initial_theme = context.current_theme_name();
        let initial_sidebar = *context.show_sidebar.read();

        context.apply_ui_config(&opencode_config::Config::default());
        assert_eq!(context.current_theme_name(), initial_theme);
        assert_eq!(*context.show_sidebar.read(), initial_sidebar);

        let blank = opencode_config::Config {
            theme: Some("   ".to_string()),
            tui: Some(opencode_config::TuiConfig::default()),
            ..Default::default()
        };
        context.apply_ui_config(&blank);
        assert_eq!(context.current_theme_name(), initial_theme);
        assert_eq!(*context.show_sidebar.read(), initial_sidebar);
    }

    #[test]
    fn apply_ui_config_delegates_unknown_theme_to_setter() {
        let context = AppContext::new();
        let initial_theme = context.current_theme_name();
        let config = opencode_config::Config {
            theme: Some("definitely-unknown-theme".to_string()),
            ..Default::default()
        };

        context.apply_ui_config(&config);

        // set_theme_by_name は未知名で false を返し、状態を変えない。
        assert_eq!(context.current_theme_name(), initial_theme);
        assert!(!context.set_theme_by_name("definitely-unknown-theme"));
    }

    // --- R4: new() の副作用除去と new_with_config の追加 ---

    #[test]
    fn new_with_config_applies_theme_and_sidebar() {
        let context = AppContext::new_with_config(&opencode_config::Config {
            theme: Some("dracula@dark".to_string()),
            tui: Some(opencode_config::TuiConfig {
                sidebar: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(context.current_theme_name(), "dracula@dark");
        assert!(!*context.show_sidebar.read());
    }

    #[test]
    fn new_with_config_default_matches_new() {
        let baseline = AppContext::new();
        let configured = AppContext::new_with_config(&opencode_config::Config::default());

        assert_eq!(
            configured.current_theme_name(),
            baseline.current_theme_name()
        );
        assert_eq!(
            *configured.show_sidebar.read(),
            *baseline.show_sidebar.read()
        );
    }

    /// cwd を一時変更し、Drop で必ず元に戻すガード。
    struct CwdGuard(PathBuf);

    impl CwdGuard {
        fn switch_to(path: &Path) -> Self {
            let original = std::env::current_dir().expect("read original cwd");
            std::env::set_current_dir(path).expect("switch to temporary cwd");
            Self(original)
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn new_ignores_config_file_in_current_directory() {
        // cwd はプロセス全体の状態なので、変更するテストどうしを直列化する。
        static CWD_LOCK: Mutex<()> = Mutex::new(());
        let _cwd_lock = CWD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // 一時ディレクトリ生成と自動削除は既存ヘルパーを再利用する。
        let dir = TempKvDir::new("cfg-ignore");
        fs::write(
            dir.0.join("opencode.json"),
            r#"{"theme":"dracula@dark","tui":{"sidebar":false}}"#,
        )
        .expect("write opencode.json in temporary cwd");

        let baseline = AppContext::new();
        let baseline_theme = baseline.current_theme_name();
        let baseline_sidebar = *baseline.show_sidebar.read();
        // 配置した値が既定値と異なることを確認し、検証が空振りしないようにする。
        assert_ne!(
            baseline_theme, "dracula@dark",
            "fixture theme must differ from the default"
        );

        let original_cwd = std::env::current_dir().expect("read original cwd");
        {
            let _cwd_guard = CwdGuard::switch_to(&dir.0);
            let context = AppContext::new();
            assert_eq!(context.current_theme_name(), baseline_theme);
            assert_eq!(*context.show_sidebar.read(), baseline_sidebar);
        }
        assert_eq!(
            std::env::current_dir().expect("read restored cwd"),
            original_cwd,
            "cwd must be restored after the test"
        );
    }
}
