use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

/// A string list that records whether its JSON key was explicitly present.
///
/// Config sources are merged from low to high precedence. Previously an omitted
/// key and an explicit empty array were indistinguishable, which made it
/// impossible to clear accumulated values such as `plugin`. Keeping the
/// presence bit lets `Config::merge` retain additive behavior for omitted keys
/// while treating an explicit empty array as a replacement (clear) operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExplicitStringList {
    items: Vec<String>,
    explicitly_set: bool,
}

impl ExplicitStringList {
    /// Create a list as if its key was absent from the source document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Values held by this list.
    pub fn as_slice(&self) -> &[String] {
        &self.items
    }

    /// Consume the list and return its values.
    pub fn into_inner(self) -> Vec<String> {
        self.items
    }

    /// Whether the source document explicitly contained this key.
    pub fn is_explicit(&self) -> bool {
        self.explicitly_set
    }

    /// Whether the list holds no values.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether serialization should omit this key.
    ///
    /// Only a key that was absent from every config source is omitted. An
    /// explicit empty array must be written back so a later load still sees the
    /// "clear the list" directive instead of treating the key as missing.
    pub fn should_skip_serializing(&self) -> bool {
        !self.explicitly_set
    }

    /// Build a list from values, recording the key as explicitly present.
    pub fn from_values(items: Vec<String>) -> Self {
        Self {
            items,
            explicitly_set: true,
        }
    }

    /// Rebuild values while preserving the original presence bit.
    pub fn map_values(self, f: impl FnOnce(Vec<String>) -> Vec<String>) -> Self {
        Self {
            items: f(self.items),
            explicitly_set: self.explicitly_set,
        }
    }

    /// Replace values and record the key as explicitly present.
    pub fn replace_values(&mut self, items: Vec<String>) {
        self.items = items;
        self.explicitly_set = true;
    }
}

impl Deref for ExplicitStringList {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl DerefMut for ExplicitStringList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.items
    }
}

impl From<Vec<String>> for ExplicitStringList {
    fn from(items: Vec<String>) -> Self {
        Self::from_values(items)
    }
}

impl FromIterator<String> for ExplicitStringList {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_values(iter.into_iter().collect())
    }
}

impl IntoIterator for ExplicitStringList {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a ExplicitStringList {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl PartialEq<Vec<String>> for ExplicitStringList {
    fn eq(&self, other: &Vec<String>) -> bool {
        self.items == *other
    }
}

impl Serialize for ExplicitStringList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.items.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExplicitStringList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Vec::<String>::deserialize(deserializer)?;
        Ok(Self {
            items,
            explicitly_set: true,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub keybinds: Option<KeybindsConfig>,

    #[serde(
        rename = "logLevel",
        alias = "log_level",
        skip_serializing_if = "Option::is_none"
    )]
    pub log_level: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tui: Option<TuiConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<HashMap<String, CommandConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillsConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub watcher: Option<WatcherConfig>,

    // An explicit `plugin: []` is a clear directive, not "nothing to write":
    // omitting it would let a later load resurrect plugins from other layers.
    #[serde(
        default,
        skip_serializing_if = "ExplicitStringList::should_skip_serializing"
    )]
    pub plugin: ExplicitStringList,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareMode>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoshare: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoupdate: Option<AutoUpdateMode>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_providers: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_providers: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// IANA time zone used for schedules (upstream `Config.Info.timezone`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionsConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<HashMap<String, AgentConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<AgentConfigs>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfigs>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<HashMap<String, ProviderConfig>>,

    /// Current-schema provider map (`providers`, plural).
    ///
    /// Normalized into `provider` by `ConfigLoader::load_all`; kept here so
    /// that both spellings can be parsed without changing the runtime shape.
    #[serde(rename = "providers", skip_serializing_if = "Option::is_none")]
    pub providers: Option<HashMap<String, ProviderConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatter: Option<FormatterConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp: Option<LspConfig>,

    // `instructions: []` も `plugin: []` と同じく明示的なクリア命令なので、
    // 空配列であっても書き戻す（presence bit で判定する）。
    #[serde(
        default,
        skip_serializing_if = "ExplicitStringList::should_skip_serializing"
    )]
    pub instructions: ExplicitStringList,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutMode>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<HashMap<String, bool>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub enterprise: Option<EnterpriseConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<ExperimentalConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Top-level config keys that `Config` recognizes on input.
///
/// This lists every serde field name plus its `rename`/`alias` spelling. The
/// loader uses it to *warn* about unknown keys while still ignoring them, so a
/// config from a newer OpenCode version never prevents startup (see the
/// compatibility notes on `PermissionsConfig` and `PermissionListEntry`).
/// Keep this list in sync when adding or renaming a top-level field.
pub(crate) fn known_config_keys() -> &'static [&'static str] {
    &[
        "$schema",
        "theme",
        "keybinds",
        "logLevel",
        "log_level",
        "tui",
        "server",
        "command",
        "skills",
        "watcher",
        "plugin",
        "snapshot",
        "share",
        "autoshare",
        "autoupdate",
        "disabled_providers",
        "enabled_providers",
        "model",
        "small_model",
        "default_agent",
        "username",
        "timezone",
        "permissions",
        "agents",
        "mode",
        "agent",
        "provider",
        "providers",
        "mcp",
        "formatter",
        "lsp",
        "instructions",
        "layout",
        "permission",
        "tools",
        "enterprise",
        "compaction",
        "experimental",
        "env",
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareMode {
    Manual,
    Auto,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AutoUpdateMode {
    Boolean(bool),
    Notify(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeybindsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_exit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_open: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_toggle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollbar_toggle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username_toggle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_export: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_new: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_timeline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_fork: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_rename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_delete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stash_delete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_favorite_toggle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_share: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_unshare: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_interrupt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_compact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_page_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_page_down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_line_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_line_down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_half_page_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_half_page_down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_first: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_last: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_previous: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_last_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_copy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_undo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_redo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_toggle_conceal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_cycle_recent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_cycle_recent_reverse: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_cycle_favorite: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_cycle_favorite_reverse: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_cycle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_cycle_reverse: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant_cycle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_clear: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_paste: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_submit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_newline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_move_left: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_move_right: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_move_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_move_down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_left: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_right: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_line_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_line_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_line_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_line_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_visual_line_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_visual_line_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_visual_line_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_visual_line_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_buffer_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_buffer_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_buffer_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_buffer_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_delete_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_delete_to_line_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_delete_to_line_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_backspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_delete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_undo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_redo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_word_forward: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_word_backward: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_word_forward: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_select_word_backward: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_delete_word_forward: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_delete_word_backward: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_previous: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_child_cycle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_child_cycle_reverse: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_suspend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_title_toggle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tips_toggle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_thinking: Option<String>,

    // Legacy fields kept for backward compatibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_acceleration: Option<ScrollAccelerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_style: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScrollAccelerationConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mdns: Option<bool>,
    #[serde(
        rename = "mdnsDomain",
        alias = "mdns_domain",
        skip_serializing_if = "Option::is_none"
    )]
    pub mdns_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cors: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtask: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WatcherConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfigs {
    #[serde(flatten)]
    pub entries: HashMap<String, AgentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<AgentMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "maxSteps")]
    pub max_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    Primary,
    Subagent,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<HashMap<String, ModelConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    /// Current-schema provider package (an npm module). Normalized into `npm`
    /// by `ConfigLoader::load_all`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Current-schema provider settings such as `baseURL` / `apiKey`.
    /// Normalized into `base_url`, `api_key`, or `options`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<HashMap<String, serde_json::Value>>,
    /// Current-schema request headers. Normalized into `options.headers`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, serde_json::Value>>,
    /// Current-schema compatibility flags. Normalized into
    /// `options.compatibility`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<HashMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whitelist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blacklist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(alias = "modelID", skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<HashMap<String, ModelVariantConfig>>,
    /// Current-schema model limits (context / output token counts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<ModelLimitConfig>,
    /// Current-schema model capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilitiesConfig>,
}

/// Token limits of a model in the current config schema.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelLimitConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
}

/// Capabilities of a model in the current config schema.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelCapabilitiesConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Either a boolean or an object such as `{"supported": true}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<String>>,
}

impl ModelCapabilitiesConfig {
    /// Returns the reasoning capability, accepting both `true` and
    /// `{"supported": true}` shapes.
    pub fn reasoning_supported(&self) -> Option<bool> {
        match self.reasoning.as_ref()? {
            serde_json::Value::Bool(supported) => Some(*supported),
            serde_json::Value::Object(object) => {
                object.get("supported").and_then(serde_json::Value::as_bool)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelVariantConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    /// Full server definition. Tried first so that a complete config which
    /// also carries `enabled` is not mistaken for a bare toggle.
    Full(Box<McpServer>),
    /// Bare toggle for an existing server.
    Enabled { enabled: bool },
}

/// MCP protocol version negotiation mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum McpProtocol {
    /// Speak up to protocol revision 2025-11-25 (default).
    #[default]
    Legacy,
    /// Intended to probe for 2026-07-28 and fall back to legacy, but the
    /// probe/fallback is not implemented: `revision()` currently returns the
    /// same legacy revision as [`McpProtocol::Legacy`].
    Auto,
    /// Require protocol revision 2026-07-28.
    #[serde(rename = "2026-07-28")]
    V20260728,
}

impl McpProtocol {
    /// The protocol revision string to send in `initialize`.
    ///
    /// [`McpProtocol::Auto`] currently returns the legacy revision, because
    /// the capability-negotiation probe/fallback is not implemented. Only an
    /// explicit [`McpProtocol::V20260728`] selects the newer revision.
    pub fn revision(&self) -> &'static str {
        match self {
            McpProtocol::Legacy | McpProtocol::Auto => "2025-11-25",
            McpProtocol::V20260728 => "2026-07-28",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServer {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub server_type: Option<String>,

    /// For local: command array; for remote: unused
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,

    /// For local: environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<HashMap<String, String>>,

    /// For local: working directory to spawn the server in
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// For remote: URL of the MCP server
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<McpTimeoutConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<McpProtocol>,

    /// For remote: headers to send
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,

    /// For remote: OAuth config (or false to disable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,

    // Legacy fields kept for backward compatibility
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
}

/// MCP request timeout.
///
/// The legacy schema uses a single integer in milliseconds; the current
/// schema uses a per-operation object (`startup` / `catalog` / `execution`,
/// also in milliseconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpTimeoutConfig {
    Millis(u64),
    PerOperation(McpPerOperationTimeout),
}

/// Default MCP `startup` timeout in ms (transport connect + initialize).
pub const DEFAULT_MCP_STARTUP_MS: u64 = 30_000;
/// Default MCP `catalog` timeout in ms (tools/prompts/resources listing).
pub const DEFAULT_MCP_CATALOG_MS: u64 = 30_000;
/// Default MCP `execution` timeout in ms (tool calls). 12 hours.
pub const DEFAULT_MCP_EXECUTION_MS: u64 = 43_200_000;

impl Default for McpTimeoutConfig {
    fn default() -> Self {
        McpTimeoutConfig::PerOperation(McpPerOperationTimeout::default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpPerOperationTimeout {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<u64>,
}

impl McpTimeoutConfig {
    /// The explicitly configured values, before defaults are applied.
    pub(crate) fn explicit(&self) -> (Option<u64>, Option<u64>, Option<u64>) {
        match self {
            McpTimeoutConfig::Millis(millis) => (Some(*millis), Some(*millis), Some(*millis)),
            McpTimeoutConfig::PerOperation(timeout) => {
                (timeout.startup, timeout.catalog, timeout.execution)
            }
        }
    }

    /// Returns `(startup, catalog, execution)` in milliseconds, applying the
    /// documented defaults (30s / 30s / 12h) to unset values.
    pub fn per_operation(&self) -> (u64, u64, u64) {
        let (startup, catalog, execution) = self.explicit();
        (
            startup.unwrap_or(DEFAULT_MCP_STARTUP_MS),
            catalog.unwrap_or(DEFAULT_MCP_CATALOG_MS),
            execution.unwrap_or(DEFAULT_MCP_EXECUTION_MS),
        )
    }

    /// Resolves the effective timeouts for one server.
    ///
    /// Per the OpenCode schema, the global `mcp.timeout` provides defaults and
    /// a server's own `timeout` overrides matching keys. Anything still unset
    /// falls back to the documented defaults.
    pub fn resolve(global: Option<&Self>, server: Option<&Self>) -> (u64, u64, u64) {
        let mut effective = (None, None, None);
        for layer in [global, server].into_iter().flatten() {
            let (startup, catalog, execution) = layer.explicit();
            if startup.is_some() {
                effective.0 = startup;
            }
            if catalog.is_some() {
                effective.1 = catalog;
            }
            if execution.is_some() {
                effective.2 = execution;
            }
        }
        (
            effective.0.unwrap_or(DEFAULT_MCP_STARTUP_MS),
            effective.1.unwrap_or(DEFAULT_MCP_CATALOG_MS),
            effective.2.unwrap_or(DEFAULT_MCP_EXECUTION_MS),
        )
    }
}

/// OAuth configuration for remote MCP servers.
/// Can be a full config object or `false` to disable OAuth auto-detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpOAuthConfig {
    Disabled(bool),
    Config(McpOAuth),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpOAuth {
    #[serde(
        rename = "clientId",
        alias = "client_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub client_id: Option<String>,
    #[serde(
        rename = "clientSecret",
        alias = "client_secret",
        skip_serializing_if = "Option::is_none"
    )]
    pub client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Local callback port; shorthand for `redirect_uri` when only the port
    /// differs from the default.
    #[serde(
        rename = "callbackPort",
        alias = "callback_port",
        skip_serializing_if = "Option::is_none"
    )]
    pub callback_port: Option<u16>,
    /// Pre-registered loopback URI whose path and port reach the local
    /// callback listener.
    #[serde(
        rename = "redirectUri",
        alias = "redirect_uri",
        skip_serializing_if = "Option::is_none"
    )]
    pub redirect_uri: Option<String>,
    /// Authorization server metadata URL, set when the MCP server does not
    /// publish protected resource metadata naming its authorization server.
    #[serde(
        rename = "authServerMetadataUrl",
        alias = "auth_server_metadata_url",
        skip_serializing_if = "Option::is_none"
    )]
    pub auth_server_metadata_url: Option<String>,
}

/// The `mcp` configuration section.
///
/// The current schema keeps servers under `servers` with global request
/// timeouts in a sibling `timeout` object; the legacy shape places servers
/// directly under `mcp`. Both are accepted on read, and the current shape is
/// written back.
#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    pub timeout: Option<McpTimeoutConfig>,
    pub servers: Option<HashMap<String, McpServerConfig>>,
}

impl Serialize for McpConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(None)?;
        if let Some(timeout) = &self.timeout {
            map.serialize_entry("timeout", timeout)?;
        }
        if let Some(servers) = &self.servers {
            map.serialize_entry("servers", servers)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for McpConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let Some(object) = value.as_object() else {
            return Err(serde::de::Error::custom("mcp config must be an object"));
        };

        // `mcp.timeout` is a global default object in the current schema, but
        // a legacy config may legitimately define a server named `timeout`.
        // Only treat the value as the global timeout when it is not a server.
        let timeout = object
            .get("timeout")
            .filter(|value| !looks_like_mcp_server(value))
            .and_then(|timeout| serde_json::from_value::<McpTimeoutConfig>(timeout.clone()).ok());

        if is_mcp_servers_wrapper(object) {
            let servers = object.get("servers").cloned().unwrap_or_default();
            let servers: HashMap<String, McpServerV2> =
                serde_json::from_value(servers).map_err(serde::de::Error::custom)?;
            let servers = servers
                .into_iter()
                .map(|(name, server)| {
                    (
                        name,
                        McpServerConfig::Full(Box::new(server.into_mcp_server())),
                    )
                })
                .collect();
            return Ok(McpConfig {
                timeout,
                servers: Some(servers),
            });
        }

        // Legacy shape: servers are the object entries. `timeout` is removed
        // only when it was recognized as the global default above.
        let mut legacy = object.clone();
        if timeout.is_some() {
            legacy.remove("timeout");
        }
        let servers: HashMap<String, McpServerConfig> =
            serde_json::from_value(serde_json::Value::Object(legacy))
                .map_err(serde::de::Error::custom)?;
        Ok(McpConfig {
            timeout,
            servers: Some(servers),
        })
    }
}

/// True when a value looks like an MCP server entry rather than a timeout
/// object, so that a legacy server named `timeout` is not swallowed.
fn looks_like_mcp_server(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    ["type", "command", "url", "enabled", "disabled"]
        .iter()
        .any(|key| object.contains_key(*key))
}

/// Detects the current-schema `mcp.servers` wrapper without misreading a
/// legacy server that happens to be named `servers`.
fn is_mcp_servers_wrapper(object: &serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(servers) = object.get("servers").and_then(serde_json::Value::as_object) else {
        return false;
    };
    // The current schema may add siblings such as `mcp.timeout`; they are not
    // servers. Any other sibling means this is a legacy direct map.
    let has_only_known_siblings = object
        .keys()
        .all(|key| key == "servers" || key == "timeout");
    if !has_only_known_siblings {
        return false;
    }
    if servers.is_empty() {
        return true;
    }
    servers.values().all(serde_json::Value::is_object)
        && servers.values().any(|server| {
            let Some(server) = server.as_object() else {
                return false;
            };
            [
                "type",
                "url",
                "command",
                "disabled",
                "environment",
                "headers",
            ]
            .iter()
            .any(|key| server.contains_key(*key))
        })
}

/// Current-schema MCP server entry (`mcp.servers.<name>`).
#[derive(Debug, Clone, Default, Deserialize)]
struct McpServerV2 {
    #[serde(rename = "type", default)]
    server_type: Option<String>,
    #[serde(default)]
    command: Option<McpCommandV2>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    environment: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    env: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    headers: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    oauth: Option<serde_json::Value>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout: Option<McpTimeoutConfig>,
    #[serde(default)]
    protocol: Option<McpProtocol>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum McpCommandV2 {
    String(String),
    Array(Vec<String>),
}

impl McpServerV2 {
    fn into_mcp_server(self) -> McpServer {
        let command = match self.command {
            Some(McpCommandV2::String(command)) => vec![command],
            Some(McpCommandV2::Array(parts)) => parts,
            None => Vec::new(),
        };

        McpServer {
            server_type: self.server_type,
            command,
            environment: self
                .environment
                .map(string_map)
                .or_else(|| self.env.map(string_map)),
            cwd: self.cwd,
            url: self.url,
            enabled: self
                .enabled
                .or_else(|| self.disabled.map(|disabled| !disabled)),
            timeout: self.timeout,
            protocol: self.protocol,
            headers: self.headers.map(string_map),
            oauth: self.oauth.and_then(oauth_config_from_value),
            args: self.args,
            env: None,
            client_id: None,
            authorization_url: None,
        }
    }
}

impl DeepMerge for McpConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_map_deep_values(&mut self.servers, other.servers);
        merge_option_deep(&mut self.timeout, other.timeout);
    }
}

fn string_map(values: HashMap<String, serde_json::Value>) -> HashMap<String, String> {
    values
        .into_iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_string())))
        .collect()
}

fn oauth_config_from_value(value: serde_json::Value) -> Option<McpOAuthConfig> {
    match value {
        serde_json::Value::Bool(false) => Some(McpOAuthConfig::Disabled(false)),
        // `true` means "auto-detect", which is the default (no explicit config).
        serde_json::Value::Bool(true) => None,
        serde_json::Value::Object(_) => serde_json::from_value(value)
            .ok()
            .map(McpOAuthConfig::Config),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FormatterConfig {
    Disabled(bool),
    Enabled(HashMap<String, FormatterEntry>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FormatterEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LspConfig {
    Disabled(bool),
    Enabled(HashMap<String, LspServerConfig>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LspServerConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization: Option<HashMap<String, serde_json::Value>>,
}

/// Layout mode: "auto" or "stretch" (TS: z.enum(["auto", "stretch"]))
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutMode {
    Auto,
    Stretch,
}

/// Permission config: either a global action string ("allow"/"deny"/"ask") or
/// a record of tool name -> permission rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionConfig {
    Action(PermissionAction),
    Map(HashMap<String, PermissionRule>),
}

impl Default for PermissionConfig {
    fn default() -> Self {
        PermissionConfig::Map(HashMap::new())
    }
}

impl PermissionConfig {
    pub fn rules(&self) -> HashMap<String, PermissionRule> {
        match self {
            PermissionConfig::Action(action) => {
                let mut map = HashMap::new();
                map.insert("*".to_string(), PermissionRule::Action(action.clone()));
                map
            }
            PermissionConfig::Map(map) => map.clone(),
        }
    }

    pub fn rules_mut(&mut self) -> &mut HashMap<String, PermissionRule> {
        match self {
            PermissionConfig::Action(action) => {
                let mut map = HashMap::new();
                map.insert("*".to_string(), PermissionRule::Action(action.clone()));
                *self = PermissionConfig::Map(map);
                match self {
                    PermissionConfig::Map(map) => map,
                    _ => unreachable!(),
                }
            }
            PermissionConfig::Map(map) => map,
        }
    }
}

/// The top-level `permissions` list.
///
/// Older configs store permission names as a list of strings; newer configs
/// store explicit `{action, resource, effect}` rules. Both forms are accepted
/// so that a newer global config does not prevent the CLI from starting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionsConfig {
    /// Legacy form: `["read", "edit", ...]`.
    Names(Vec<String>),
    /// Current form: `[{"action": "read", "resource": ".env", "effect": "deny"}]`.
    Rules(Vec<PermissionListEntry>),
}

impl PermissionsConfig {
    /// Returns the number of entries regardless of the representation.
    pub fn len(&self) -> usize {
        match self {
            PermissionsConfig::Names(entries) => entries.len(),
            PermissionsConfig::Rules(entries) => entries.len(),
        }
    }

    /// Returns `true` when the list has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A single entry of the list-form `permissions` config.
///
/// Fields are optional on input because the list is informational for this
/// implementation; rejecting an unknown future shape would break startup.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionListEntry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resource: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effect: String,
}

/// A permission rule: either a simple action or a map of sub-keys to actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionRule {
    Action(PermissionAction),
    Object(HashMap<String, PermissionAction>),
}

/// Permission action: "ask", "allow", or "deny".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionAction {
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnterpriseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_config_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompactionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExperimentalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_paste_summary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_tool: Option<bool>,
    #[serde(alias = "openTelemetry", skip_serializing_if = "Option::is_none")]
    pub open_telemetry: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_loop_on_deny: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_timeout: Option<u64>,
}

trait DeepMerge {
    fn deep_merge(&mut self, other: Self);
}

fn merge_option_replace<T>(target: &mut Option<T>, source: Option<T>) {
    if let Some(value) = source {
        *target = Some(value);
    }
}

fn merge_option_deep<T: DeepMerge>(target: &mut Option<T>, source: Option<T>) {
    if let Some(source_value) = source {
        if let Some(target_value) = target {
            target_value.deep_merge(source_value);
        } else {
            *target = Some(source_value);
        }
    }
}

fn merge_map_deep_values<T: DeepMerge>(
    target: &mut HashMap<String, T>,
    source: HashMap<String, T>,
) {
    for (key, source_value) in source {
        if let Some(target_value) = target.get_mut(&key) {
            target_value.deep_merge(source_value);
        } else {
            target.insert(key, source_value);
        }
    }
}

fn merge_option_map_deep_values<T: DeepMerge>(
    target: &mut Option<HashMap<String, T>>,
    source: Option<HashMap<String, T>>,
) {
    if let Some(source_map) = source {
        if let Some(target_map) = target {
            merge_map_deep_values(target_map, source_map);
        } else {
            *target = Some(source_map);
        }
    }
}

/// Merge the two provider spellings from a higher-precedence layer.
///
/// `provider` (legacy) and `providers` (current) name the same config section.
/// When one layer uses one spelling for an id and another layer uses the other
/// spelling, the higher-precedence (`source`) layer must win. Leaving both
/// entries in place would let `normalize_provider_configs` unconditionally
/// prefer the legacy spelling, so a global `provider.x` could shadow a project
/// `providers.x` -- the opposite of the documented layer precedence.
///
/// On a conflict only the key name and the two source spellings are logged;
/// provider values can contain credentials and must never reach the logs.
fn merge_provider_maps(
    target_provider: &mut Option<HashMap<String, ProviderConfig>>,
    target_providers: &mut Option<HashMap<String, ProviderConfig>>,
    source_provider: Option<HashMap<String, ProviderConfig>>,
    mut source_providers: Option<HashMap<String, ProviderConfig>>,
) {
    // A single source layer that defines both spellings for one id keeps the
    // historical legacy-wins policy used by `normalize_provider_configs`.
    if let (Some(current), Some(legacy)) = (source_providers.as_mut(), source_provider.as_ref()) {
        current.retain(|id, _| !legacy.contains_key(id));
    }

    // An incoming legacy entry beats an accumulated current-schema entry.
    if let (Some(incoming), Some(accumulated)) =
        (source_provider.as_ref(), target_providers.as_mut())
    {
        for id in incoming.keys() {
            if accumulated.remove(id).is_some() {
                tracing::warn!(
                    key = %id,
                    lower_source = "providers",
                    higher_source = "provider",
                    "provider config spelling conflict; higher-precedence layer wins"
                );
            }
        }
    }

    // An incoming current-schema entry beats an accumulated legacy entry.
    if let (Some(incoming), Some(accumulated)) =
        (source_providers.as_ref(), target_provider.as_mut())
    {
        for id in incoming.keys() {
            if accumulated.remove(id).is_some() {
                tracing::warn!(
                    key = %id,
                    lower_source = "provider",
                    higher_source = "providers",
                    "provider config spelling conflict; higher-precedence layer wins"
                );
            }
        }
    }

    merge_option_map_deep_values(target_provider, source_provider);
    merge_option_map_deep_values(target_providers, source_providers);
}

fn merge_option_map_overwrite_values<T>(
    target: &mut Option<HashMap<String, T>>,
    source: Option<HashMap<String, T>>,
) {
    if let Some(source_map) = source {
        if let Some(target_map) = target {
            for (key, value) in source_map {
                target_map.insert(key, value);
            }
        } else {
            *target = Some(source_map);
        }
    }
}

fn merge_json_value(target: &mut serde_json::Value, source: serde_json::Value) {
    match (target, source) {
        (serde_json::Value::Object(target_map), serde_json::Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                if let Some(target_value) = target_map.get_mut(&key) {
                    merge_json_value(target_value, source_value);
                } else {
                    target_map.insert(key, source_value);
                }
            }
        }
        (target_value, source_value) => *target_value = source_value,
    }
}

fn merge_option_json_map(
    target: &mut Option<HashMap<String, serde_json::Value>>,
    source: Option<HashMap<String, serde_json::Value>>,
) {
    if let Some(source_map) = source {
        if let Some(target_map) = target {
            for (key, source_value) in source_map {
                if let Some(target_value) = target_map.get_mut(&key) {
                    merge_json_value(target_value, source_value);
                } else {
                    target_map.insert(key, source_value);
                }
            }
        } else {
            *target = Some(source_map);
        }
    }
}

fn append_unique_keep_order(target: &mut Vec<String>, source: Vec<String>) {
    for item in source {
        if !target.contains(&item) {
            target.push(item);
        }
    }
}

/// Merge one config layer's string list into the accumulated one.
///
/// Omitted keys (`explicitly_set == false`) leave the target untouched so
/// lower-precedence sources still contribute values. An explicit empty array
/// is a deletion list: it replaces the target with an empty list instead of
/// being silently ignored as "nothing to add". Non-empty explicit lists keep
/// the additive, order-preserving behavior across config layers.
fn merge_explicit_string_list(target: &mut ExplicitStringList, source: ExplicitStringList) {
    if !source.is_explicit() {
        return;
    }
    if source.is_empty() {
        target.replace_values(Vec::new());
        return;
    }
    append_unique_keep_order(&mut target.items, source.into_inner());
    target.explicitly_set = true;
}

impl DeepMerge for KeybindsConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.leader, other.leader);
        merge_option_replace(&mut self.app_exit, other.app_exit);
        merge_option_replace(&mut self.editor_open, other.editor_open);
        merge_option_replace(&mut self.theme_list, other.theme_list);
        merge_option_replace(&mut self.sidebar_toggle, other.sidebar_toggle);
        merge_option_replace(&mut self.scrollbar_toggle, other.scrollbar_toggle);
        merge_option_replace(&mut self.username_toggle, other.username_toggle);
        merge_option_replace(&mut self.status_view, other.status_view);
        merge_option_replace(&mut self.session_export, other.session_export);
        merge_option_replace(&mut self.session_new, other.session_new);
        merge_option_replace(&mut self.session_list, other.session_list);
        merge_option_replace(&mut self.session_timeline, other.session_timeline);
        merge_option_replace(&mut self.session_fork, other.session_fork);
        merge_option_replace(&mut self.session_rename, other.session_rename);
        merge_option_replace(&mut self.session_delete, other.session_delete);
        merge_option_replace(&mut self.stash_delete, other.stash_delete);
        merge_option_replace(&mut self.model_provider_list, other.model_provider_list);
        merge_option_replace(&mut self.model_favorite_toggle, other.model_favorite_toggle);
        merge_option_replace(&mut self.session_share, other.session_share);
        merge_option_replace(&mut self.session_unshare, other.session_unshare);
        merge_option_replace(&mut self.session_interrupt, other.session_interrupt);
        merge_option_replace(&mut self.session_compact, other.session_compact);
        merge_option_replace(&mut self.messages_page_up, other.messages_page_up);
        merge_option_replace(&mut self.messages_page_down, other.messages_page_down);
        merge_option_replace(&mut self.messages_line_up, other.messages_line_up);
        merge_option_replace(&mut self.messages_line_down, other.messages_line_down);
        merge_option_replace(&mut self.messages_half_page_up, other.messages_half_page_up);
        merge_option_replace(
            &mut self.messages_half_page_down,
            other.messages_half_page_down,
        );
        merge_option_replace(&mut self.messages_first, other.messages_first);
        merge_option_replace(&mut self.messages_last, other.messages_last);
        merge_option_replace(&mut self.messages_next, other.messages_next);
        merge_option_replace(&mut self.messages_previous, other.messages_previous);
        merge_option_replace(&mut self.messages_last_user, other.messages_last_user);
        merge_option_replace(&mut self.messages_copy, other.messages_copy);
        merge_option_replace(&mut self.messages_undo, other.messages_undo);
        merge_option_replace(&mut self.messages_redo, other.messages_redo);
        merge_option_replace(
            &mut self.messages_toggle_conceal,
            other.messages_toggle_conceal,
        );
        merge_option_replace(&mut self.tool_details, other.tool_details);
        merge_option_replace(&mut self.model_list, other.model_list);
        merge_option_replace(&mut self.model_cycle_recent, other.model_cycle_recent);
        merge_option_replace(
            &mut self.model_cycle_recent_reverse,
            other.model_cycle_recent_reverse,
        );
        merge_option_replace(&mut self.model_cycle_favorite, other.model_cycle_favorite);
        merge_option_replace(
            &mut self.model_cycle_favorite_reverse,
            other.model_cycle_favorite_reverse,
        );
        merge_option_replace(&mut self.command_list, other.command_list);
        merge_option_replace(&mut self.agent_list, other.agent_list);
        merge_option_replace(&mut self.agent_cycle, other.agent_cycle);
        merge_option_replace(&mut self.agent_cycle_reverse, other.agent_cycle_reverse);
        merge_option_replace(&mut self.variant_cycle, other.variant_cycle);
        merge_option_replace(&mut self.input_clear, other.input_clear);
        merge_option_replace(&mut self.input_paste, other.input_paste);
        merge_option_replace(&mut self.input_submit, other.input_submit);
        merge_option_replace(&mut self.input_newline, other.input_newline);
        merge_option_replace(&mut self.input_move_left, other.input_move_left);
        merge_option_replace(&mut self.input_move_right, other.input_move_right);
        merge_option_replace(&mut self.input_move_up, other.input_move_up);
        merge_option_replace(&mut self.input_move_down, other.input_move_down);
        merge_option_replace(&mut self.input_select_left, other.input_select_left);
        merge_option_replace(&mut self.input_select_right, other.input_select_right);
        merge_option_replace(&mut self.input_select_up, other.input_select_up);
        merge_option_replace(&mut self.input_select_down, other.input_select_down);
        merge_option_replace(&mut self.input_line_home, other.input_line_home);
        merge_option_replace(&mut self.input_line_end, other.input_line_end);
        merge_option_replace(
            &mut self.input_select_line_home,
            other.input_select_line_home,
        );
        merge_option_replace(&mut self.input_select_line_end, other.input_select_line_end);
        merge_option_replace(
            &mut self.input_visual_line_home,
            other.input_visual_line_home,
        );
        merge_option_replace(&mut self.input_visual_line_end, other.input_visual_line_end);
        merge_option_replace(
            &mut self.input_select_visual_line_home,
            other.input_select_visual_line_home,
        );
        merge_option_replace(
            &mut self.input_select_visual_line_end,
            other.input_select_visual_line_end,
        );
        merge_option_replace(&mut self.input_buffer_home, other.input_buffer_home);
        merge_option_replace(&mut self.input_buffer_end, other.input_buffer_end);
        merge_option_replace(
            &mut self.input_select_buffer_home,
            other.input_select_buffer_home,
        );
        merge_option_replace(
            &mut self.input_select_buffer_end,
            other.input_select_buffer_end,
        );
        merge_option_replace(&mut self.input_delete_line, other.input_delete_line);
        merge_option_replace(
            &mut self.input_delete_to_line_end,
            other.input_delete_to_line_end,
        );
        merge_option_replace(
            &mut self.input_delete_to_line_start,
            other.input_delete_to_line_start,
        );
        merge_option_replace(&mut self.input_backspace, other.input_backspace);
        merge_option_replace(&mut self.input_delete, other.input_delete);
        merge_option_replace(&mut self.input_undo, other.input_undo);
        merge_option_replace(&mut self.input_redo, other.input_redo);
        merge_option_replace(&mut self.input_word_forward, other.input_word_forward);
        merge_option_replace(&mut self.input_word_backward, other.input_word_backward);
        merge_option_replace(
            &mut self.input_select_word_forward,
            other.input_select_word_forward,
        );
        merge_option_replace(
            &mut self.input_select_word_backward,
            other.input_select_word_backward,
        );
        merge_option_replace(
            &mut self.input_delete_word_forward,
            other.input_delete_word_forward,
        );
        merge_option_replace(
            &mut self.input_delete_word_backward,
            other.input_delete_word_backward,
        );
        merge_option_replace(&mut self.history_previous, other.history_previous);
        merge_option_replace(&mut self.history_next, other.history_next);
        merge_option_replace(&mut self.session_child_cycle, other.session_child_cycle);
        merge_option_replace(
            &mut self.session_child_cycle_reverse,
            other.session_child_cycle_reverse,
        );
        merge_option_replace(&mut self.session_parent, other.session_parent);
        merge_option_replace(&mut self.terminal_suspend, other.terminal_suspend);
        merge_option_replace(&mut self.terminal_title_toggle, other.terminal_title_toggle);
        merge_option_replace(&mut self.tips_toggle, other.tips_toggle);
        merge_option_replace(&mut self.display_thinking, other.display_thinking);
        // Legacy fields
        merge_option_replace(&mut self.submit, other.submit);
        merge_option_replace(&mut self.cancel, other.cancel);
        merge_option_replace(&mut self.interrupt, other.interrupt);
    }
}

impl DeepMerge for TuiConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.mode, other.mode);
        merge_option_replace(&mut self.sidebar, other.sidebar);
        merge_option_replace(&mut self.scroll_speed, other.scroll_speed);
        merge_option_replace(&mut self.scroll_acceleration, other.scroll_acceleration);
        merge_option_replace(&mut self.diff_style, other.diff_style);
    }
}

impl DeepMerge for ServerConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.port, other.port);
        merge_option_replace(&mut self.hostname, other.hostname);
        merge_option_replace(&mut self.mdns, other.mdns);
        merge_option_replace(&mut self.mdns_domain, other.mdns_domain);
        merge_option_replace(&mut self.cors, other.cors);
    }
}

impl DeepMerge for CommandConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.name, other.name);
        merge_option_replace(&mut self.description, other.description);
        merge_option_replace(&mut self.template, other.template);
        merge_option_replace(&mut self.model, other.model);
        merge_option_replace(&mut self.agent, other.agent);
        merge_option_replace(&mut self.subtask, other.subtask);
    }
}

impl DeepMerge for SkillsConfig {
    fn deep_merge(&mut self, other: Self) {
        if !other.paths.is_empty() {
            self.paths = other.paths;
        }
        if !other.urls.is_empty() {
            self.urls = other.urls;
        }
    }
}

impl DeepMerge for WatcherConfig {
    fn deep_merge(&mut self, other: Self) {
        if !other.ignore.is_empty() {
            self.ignore = other.ignore;
        }
    }
}

impl DeepMerge for AgentConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.name, other.name);
        merge_option_replace(&mut self.model, other.model);
        merge_option_replace(&mut self.variant, other.variant);
        merge_option_replace(&mut self.temperature, other.temperature);
        merge_option_replace(&mut self.top_p, other.top_p);
        merge_option_replace(&mut self.prompt, other.prompt);
        merge_option_replace(&mut self.disable, other.disable);
        merge_option_replace(&mut self.description, other.description);
        merge_option_replace(&mut self.mode, other.mode);
        merge_option_replace(&mut self.hidden, other.hidden);
        merge_option_json_map(&mut self.options, other.options);
        merge_option_replace(&mut self.color, other.color);
        merge_option_replace(&mut self.steps, other.steps);
        merge_option_replace(&mut self.max_tokens, other.max_tokens);
        merge_option_replace(&mut self.max_steps, other.max_steps);
        merge_option_deep(&mut self.permission, other.permission);
        merge_option_map_overwrite_values(&mut self.tools, other.tools);
    }
}

impl DeepMerge for AgentConfigs {
    fn deep_merge(&mut self, other: Self) {
        merge_map_deep_values(&mut self.entries, other.entries);
    }
}

impl DeepMerge for ModelConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.name, other.name);
        merge_option_replace(&mut self.model, other.model);
        merge_option_replace(&mut self.api_key, other.api_key);
        merge_option_replace(&mut self.base_url, other.base_url);
        merge_option_map_deep_values(&mut self.variants, other.variants);
        merge_option_deep(&mut self.limit, other.limit);
        merge_option_deep(&mut self.capabilities, other.capabilities);
    }
}

impl DeepMerge for ModelLimitConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.context, other.context);
        merge_option_replace(&mut self.output, other.output);
    }
}

impl DeepMerge for ModelCapabilitiesConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.tools, other.tools);
        merge_option_replace(&mut self.reasoning, other.reasoning);
        merge_option_replace(&mut self.attachment, other.attachment);
        merge_option_replace(&mut self.input, other.input);
        merge_option_replace(&mut self.output, other.output);
    }
}

impl DeepMerge for ModelVariantConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.disabled, other.disabled);
        for (key, value) in other.extra {
            self.extra.insert(key, value);
        }
    }
}

impl DeepMerge for ProviderConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.name, other.name);
        merge_option_replace(&mut self.api_key, other.api_key);
        merge_option_replace(&mut self.base_url, other.base_url);
        merge_option_map_deep_values(&mut self.models, other.models);
        merge_option_json_map(&mut self.options, other.options);
        merge_option_replace(&mut self.npm, other.npm);
        merge_option_replace(&mut self.package, other.package);
        merge_option_json_map(&mut self.settings, other.settings);
        merge_option_json_map(&mut self.headers, other.headers);
        merge_option_json_map(&mut self.compatibility, other.compatibility);
        if !other.whitelist.is_empty() {
            self.whitelist = other.whitelist;
        }
        if !other.blacklist.is_empty() {
            self.blacklist = other.blacklist;
        }
    }
}

impl DeepMerge for McpServer {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.server_type, other.server_type);
        if !other.command.is_empty() {
            self.command = other.command;
        }
        merge_option_map_overwrite_values(&mut self.environment, other.environment);
        merge_option_replace(&mut self.cwd, other.cwd);
        merge_option_replace(&mut self.url, other.url);
        merge_option_replace(&mut self.enabled, other.enabled);
        merge_option_deep(&mut self.timeout, other.timeout);
        merge_option_replace(&mut self.protocol, other.protocol);
        merge_option_map_overwrite_values(&mut self.headers, other.headers);
        merge_option_replace(&mut self.oauth, other.oauth);
        // Legacy fields
        if !other.args.is_empty() {
            self.args = other.args;
        }
        merge_option_map_overwrite_values(&mut self.env, other.env);
        merge_option_replace(&mut self.client_id, other.client_id);
        merge_option_replace(&mut self.authorization_url, other.authorization_url);
    }
}

impl DeepMerge for McpTimeoutConfig {
    fn deep_merge(&mut self, other: Self) {
        // Merge per key so that a config that only overrides e.g. `execution`
        // keeps the `startup` / `catalog` values from a lower-precedence file.
        if let (McpTimeoutConfig::Millis(target), McpTimeoutConfig::Millis(source)) =
            (&mut *self, &other)
        {
            *target = *source;
            return;
        }

        let (mut startup, mut catalog, mut execution) = self.explicit();
        let (source_startup, source_catalog, source_execution) = other.explicit();
        if source_startup.is_some() {
            startup = source_startup;
        }
        if source_catalog.is_some() {
            catalog = source_catalog;
        }
        if source_execution.is_some() {
            execution = source_execution;
        }
        *self = McpTimeoutConfig::PerOperation(McpPerOperationTimeout {
            startup,
            catalog,
            execution,
        });
    }
}

impl DeepMerge for McpPerOperationTimeout {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.startup, other.startup);
        merge_option_replace(&mut self.catalog, other.catalog);
        merge_option_replace(&mut self.execution, other.execution);
    }
}

impl DeepMerge for McpServerConfig {
    fn deep_merge(&mut self, other: Self) {
        match other {
            McpServerConfig::Enabled { enabled } => match self {
                McpServerConfig::Enabled {
                    enabled: target_enabled,
                } => *target_enabled = enabled,
                McpServerConfig::Full(target_server) => target_server.enabled = Some(enabled),
            },
            McpServerConfig::Full(mut source_server) => match self {
                McpServerConfig::Full(target_server) => target_server.deep_merge(*source_server),
                McpServerConfig::Enabled { enabled } => {
                    if source_server.enabled.is_none() {
                        source_server.enabled = Some(*enabled);
                    }
                    *self = McpServerConfig::Full(source_server);
                }
            },
        }
    }
}

impl DeepMerge for FormatterEntry {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.disabled, other.disabled);
        if !other.command.is_empty() {
            self.command = other.command;
        }
        merge_option_map_overwrite_values(&mut self.environment, other.environment);
        if !other.extensions.is_empty() {
            self.extensions = other.extensions;
        }
    }
}

impl DeepMerge for FormatterConfig {
    fn deep_merge(&mut self, other: Self) {
        match other {
            FormatterConfig::Disabled(value) => *self = FormatterConfig::Disabled(value),
            FormatterConfig::Enabled(source_map) => match self {
                FormatterConfig::Disabled(_) => *self = FormatterConfig::Enabled(source_map),
                FormatterConfig::Enabled(target_map) => {
                    merge_map_deep_values(target_map, source_map);
                }
            },
        }
    }
}

impl DeepMerge for LspServerConfig {
    fn deep_merge(&mut self, other: Self) {
        if !other.command.is_empty() {
            self.command = other.command;
        }
        if !other.extensions.is_empty() {
            self.extensions = other.extensions;
        }
        merge_option_replace(&mut self.disabled, other.disabled);
        merge_option_map_overwrite_values(&mut self.env, other.env);
        merge_option_json_map(&mut self.initialization, other.initialization);
    }
}

impl DeepMerge for LspConfig {
    fn deep_merge(&mut self, other: Self) {
        match other {
            LspConfig::Disabled(value) => *self = LspConfig::Disabled(value),
            LspConfig::Enabled(source_map) => match self {
                LspConfig::Disabled(_) => *self = LspConfig::Enabled(source_map),
                LspConfig::Enabled(target_map) => {
                    merge_map_deep_values(target_map, source_map);
                }
            },
        }
    }
}

impl DeepMerge for PermissionConfig {
    fn deep_merge(&mut self, other: Self) {
        for (key, value) in other.rules() {
            self.rules_mut().insert(key, value);
        }
    }
}

impl DeepMerge for EnterpriseConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.url, other.url);
        merge_option_replace(&mut self.managed_config_dir, other.managed_config_dir);
    }
}

impl DeepMerge for CompactionConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.auto, other.auto);
        merge_option_replace(&mut self.prune, other.prune);
        merge_option_replace(&mut self.reserved, other.reserved);
    }
}

impl DeepMerge for ExperimentalConfig {
    fn deep_merge(&mut self, other: Self) {
        merge_option_replace(&mut self.disable_paste_summary, other.disable_paste_summary);
        merge_option_replace(&mut self.batch_tool, other.batch_tool);
        merge_option_replace(&mut self.open_telemetry, other.open_telemetry);
        if !other.primary_tools.is_empty() {
            self.primary_tools = other.primary_tools;
        }
        merge_option_replace(&mut self.continue_loop_on_deny, other.continue_loop_on_deny);
        merge_option_replace(&mut self.mcp_timeout, other.mcp_timeout);
    }
}

impl Config {
    pub fn merge(&mut self, other: Config) {
        merge_option_replace(&mut self.schema, other.schema);
        merge_option_replace(&mut self.theme, other.theme);
        merge_option_deep(&mut self.keybinds, other.keybinds);
        merge_option_replace(&mut self.log_level, other.log_level);
        merge_option_deep(&mut self.tui, other.tui);
        merge_option_deep(&mut self.server, other.server);
        merge_option_map_deep_values(&mut self.command, other.command);
        merge_option_deep(&mut self.skills, other.skills);
        merge_option_deep(&mut self.watcher, other.watcher);
        merge_option_replace(&mut self.snapshot, other.snapshot);
        merge_option_replace(&mut self.share, other.share);
        merge_option_replace(&mut self.autoshare, other.autoshare);
        merge_option_replace(&mut self.autoupdate, other.autoupdate);
        merge_option_replace(&mut self.model, other.model);
        merge_option_replace(&mut self.small_model, other.small_model);
        merge_option_replace(&mut self.default_agent, other.default_agent);
        merge_option_replace(&mut self.username, other.username);
        merge_option_replace(&mut self.timezone, other.timezone);
        merge_option_replace(&mut self.permissions, other.permissions);
        merge_option_map_deep_values(&mut self.agents, other.agents);
        merge_option_deep(&mut self.mode, other.mode);
        merge_option_deep(&mut self.agent, other.agent);
        merge_provider_maps(
            &mut self.provider,
            &mut self.providers,
            other.provider,
            other.providers,
        );
        merge_option_deep(&mut self.mcp, other.mcp);
        merge_option_deep(&mut self.formatter, other.formatter);
        merge_option_deep(&mut self.lsp, other.lsp);
        merge_option_replace(&mut self.layout, other.layout);
        merge_option_deep(&mut self.permission, other.permission);
        merge_option_map_overwrite_values(&mut self.tools, other.tools);
        merge_option_deep(&mut self.enterprise, other.enterprise);
        merge_option_deep(&mut self.compaction, other.compaction);
        merge_option_deep(&mut self.experimental, other.experimental);
        merge_option_map_overwrite_values(&mut self.env, other.env);

        merge_explicit_string_list(&mut self.plugin, other.plugin);
        merge_explicit_string_list(&mut self.instructions, other.instructions);

        if !other.disabled_providers.is_empty() {
            self.disabled_providers = other.disabled_providers;
        }
        if !other.enabled_providers.is_empty() {
            self.enabled_providers = other.enabled_providers;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_protocol_revision_legacy_and_auto_share_legacy_revision() {
        assert_eq!(McpProtocol::Legacy.revision(), "2025-11-25");
        assert_eq!(McpProtocol::Auto.revision(), "2025-11-25");
        assert_eq!(McpProtocol::V20260728.revision(), "2026-07-28");
    }

    #[test]
    fn merges_nested_structs_without_losing_existing_fields() {
        let mut base = Config {
            keybinds: Some(KeybindsConfig {
                submit: Some("enter".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let overlay = Config {
            keybinds: Some(KeybindsConfig {
                interrupt: Some("esc".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        base.merge(overlay);

        let merged = base.keybinds.unwrap();
        assert_eq!(merged.submit, Some("enter".to_string()));
        assert_eq!(merged.interrupt, Some("esc".to_string()));
    }

    #[test]
    fn merges_maps_recursively_for_same_keys() {
        let mut base = Config {
            provider: Some(HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    base_url: Some("https://old".to_string()),
                    models: Some(HashMap::from([(
                        "gpt-4o".to_string(),
                        ModelConfig {
                            api_key: Some("old-key".to_string()),
                            ..Default::default()
                        },
                    )])),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };

        let overlay = Config {
            provider: Some(HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    api_key: Some("new-provider-key".to_string()),
                    models: Some(HashMap::from([(
                        "gpt-4o".to_string(),
                        ModelConfig {
                            model: Some("gpt-4o-2026".to_string()),
                            ..Default::default()
                        },
                    )])),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };

        base.merge(overlay);

        let provider = base.provider.unwrap().remove("openai").unwrap();
        assert_eq!(provider.base_url, Some("https://old".to_string()));
        assert_eq!(provider.api_key, Some("new-provider-key".to_string()));

        let model = provider.models.unwrap().remove("gpt-4o").unwrap();
        assert_eq!(model.api_key, Some("old-key".to_string()));
        assert_eq!(model.model, Some("gpt-4o-2026".to_string()));
    }

    #[test]
    fn plugin_and_instruction_arrays_append_unique_keep_order() {
        let mut base = Config {
            plugin: vec!["a".to_string(), "b".to_string()].into(),
            instructions: vec!["one".to_string(), "two".to_string()].into(),
            ..Default::default()
        };

        let overlay = Config {
            plugin: vec!["b".to_string(), "c".to_string()].into(),
            instructions: vec!["two".to_string(), "three".to_string()].into(),
            ..Default::default()
        };

        base.merge(overlay);

        assert_eq!(
            base.plugin,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            base.instructions,
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
    }

    #[test]
    fn explicit_empty_array_clears_accumulated_plugins_and_instructions() {
        let mut base = Config {
            plugin: vec!["a".to_string(), "b".to_string()].into(),
            instructions: vec!["one.md".to_string(), "two.md".to_string()].into(),
            ..Default::default()
        };

        let clear: Config = serde_json::from_str(r#"{"plugin": [], "instructions": []}"#).unwrap();
        assert!(clear.plugin.is_explicit());
        assert!(clear.instructions.is_explicit());

        base.merge(clear);

        assert!(
            base.plugin.is_empty(),
            "plugin: [] must remove existing plugins"
        );
        assert!(
            base.instructions.is_empty(),
            "instructions: [] must remove existing instructions"
        );
    }

    #[test]
    fn omitted_array_key_keeps_accumulated_plugins() {
        let mut base = Config {
            plugin: vec!["a".to_string()].into(),
            instructions: vec!["one.md".to_string()].into(),
            ..Default::default()
        };

        let theme_only: Config = serde_json::from_str(r#"{"theme": "dark"}"#).unwrap();
        assert!(!theme_only.plugin.is_explicit());
        assert!(!theme_only.instructions.is_explicit());

        base.merge(theme_only);

        assert_eq!(base.plugin, vec!["a".to_string()]);
        assert_eq!(base.instructions, vec!["one.md".to_string()]);
    }

    #[test]
    fn provider_lists_follow_replace_semantics_instead_of_union() {
        let mut base = Config {
            disabled_providers: vec!["anthropic".to_string()],
            enabled_providers: vec!["openai".to_string()],
            ..Default::default()
        };

        let overlay = Config {
            disabled_providers: vec!["google".to_string()],
            ..Default::default()
        };

        base.merge(overlay);

        assert_eq!(base.disabled_providers, vec!["google".to_string()]);
        assert_eq!(base.enabled_providers, vec!["openai".to_string()]);
    }

    #[test]
    fn mcp_enabled_flag_overlay_keeps_existing_full_server_fields() {
        let mut base = Config {
            mcp: Some(McpConfig {
                timeout: None,
                servers: Some(HashMap::from([(
                    "repo".to_string(),
                    McpServerConfig::Full(Box::new(McpServer {
                        command: vec!["node".to_string(), "mcp.js".to_string()],
                        timeout: Some(McpTimeoutConfig::Millis(3000)),
                        ..Default::default()
                    })),
                )])),
            }),
            ..Default::default()
        };

        let overlay = Config {
            mcp: Some(McpConfig {
                timeout: None,
                servers: Some(HashMap::from([(
                    "repo".to_string(),
                    McpServerConfig::Enabled { enabled: false },
                )])),
            }),
            ..Default::default()
        };

        base.merge(overlay);

        let server = base.mcp.unwrap().servers.unwrap().remove("repo").unwrap();
        match server {
            McpServerConfig::Full(server) => {
                assert_eq!(
                    server.command,
                    vec!["node".to_string(), "mcp.js".to_string()]
                );
                assert_eq!(
                    server.timeout.expect("timeout").per_operation(),
                    (3000, 3000, 3000)
                );
                assert_eq!(server.enabled, Some(false));
            }
            McpServerConfig::Enabled { .. } => panic!("expected full MCP server config"),
        }
    }

    #[test]
    fn agent_configs_support_dynamic_keys_and_deep_merge() {
        let mut base = Config {
            agent: Some(AgentConfigs {
                entries: HashMap::from([(
                    "reviewer".to_string(),
                    AgentConfig {
                        prompt: Some("old prompt".to_string()),
                        options: Some(HashMap::from([("a".to_string(), serde_json::json!(1))])),
                        ..Default::default()
                    },
                )]),
            }),
            ..Default::default()
        };

        let overlay = Config {
            agent: Some(AgentConfigs {
                entries: HashMap::from([
                    (
                        "reviewer".to_string(),
                        AgentConfig {
                            prompt: Some("new prompt".to_string()),
                            options: Some(HashMap::from([("b".to_string(), serde_json::json!(2))])),
                            ..Default::default()
                        },
                    ),
                    (
                        "research".to_string(),
                        AgentConfig {
                            mode: Some(AgentMode::Subagent),
                            ..Default::default()
                        },
                    ),
                ]),
            }),
            ..Default::default()
        };

        base.merge(overlay);

        let agents = base.agent.unwrap().entries;
        let reviewer = agents.get("reviewer").unwrap();
        assert_eq!(reviewer.prompt.as_deref(), Some("new prompt"));
        let options = reviewer.options.as_ref().unwrap();
        assert_eq!(options.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(options.get("b"), Some(&serde_json::json!(2)));
        assert!(agents.contains_key("research"));
    }

    #[test]
    fn parses_legacy_permission_name_list() {
        let config: Config = serde_json::from_str(r#"{"permissions":["read","edit"]}"#).unwrap();
        match config.permissions.unwrap() {
            PermissionsConfig::Names(names) => assert_eq!(names, vec!["read", "edit"]),
            other => panic!("expected Names, got {other:?}"),
        }
    }

    #[test]
    fn parses_rule_list_permissions() {
        let config: Config = serde_json::from_str(
            r#"{
                "permissions": [
                    {"action": "read", "resource": ".env", "effect": "deny"},
                    {"action": "edit", "resource": "secrets/**", "effect": "deny"}
                ]
            }"#,
        )
        .unwrap();
        match config.permissions.unwrap() {
            PermissionsConfig::Rules(rules) => {
                assert_eq!(rules.len(), 2);
                assert_eq!(rules[0].action, "read");
                assert_eq!(rules[0].resource, ".env");
                assert_eq!(rules[0].effect, "deny");
            }
            other => panic!("expected Rules, got {other:?}"),
        }
    }

    #[test]
    fn parses_permissions_with_unknown_fields() {
        // Future entries may carry extra fields; parsing must stay tolerant.
        let config: Config = serde_json::from_str(
            r#"{"permissions":[{"action":"read","resource":"*","effect":"allow","priority":1}]}"#,
        )
        .unwrap();
        assert_eq!(config.permissions.unwrap().len(), 1);
    }

    #[test]
    fn merges_permissions_by_replacement() {
        let mut base: Config = serde_json::from_str(r#"{"permissions":["read"]}"#).unwrap();
        let overlay: Config = serde_json::from_str(
            r#"{"permissions":[{"action":"edit","resource":"*","effect":"deny"}]}"#,
        )
        .unwrap();
        base.merge(overlay);
        match base.permissions.unwrap() {
            PermissionsConfig::Rules(rules) => assert_eq!(rules[0].action, "edit"),
            other => panic!("expected Rules, got {other:?}"),
        }
    }

    #[test]
    fn parses_current_schema_mcp_servers_wrapper() {
        let config: Config = serde_json::from_str(
            r#"{
                "mcp": {
                    "servers": {
                        "remote": {
                            "type": "remote",
                            "url": "https://example.com/mcp",
                            "headers": {"Authorization": "Bearer x"},
                            "disabled": true,
                            "oauth": false
                        },
                        "local": {
                            "type": "local",
                            "command": ["npx", "server"],
                            "environment": {"KEY": "value"},
                            "cwd": "/tmp/project"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let mcp = config.mcp.expect("mcp").servers.expect("servers");
        assert_eq!(mcp.len(), 2);
        assert!(!mcp.contains_key("servers"), "wrapper must be unwrapped");

        match mcp.get("remote").unwrap() {
            McpServerConfig::Full(server) => {
                assert_eq!(server.server_type.as_deref(), Some("remote"));
                assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
                assert_eq!(server.enabled, Some(false));
                assert_eq!(
                    server
                        .headers
                        .as_ref()
                        .and_then(|headers| headers.get("Authorization"))
                        .map(String::as_str),
                    Some("Bearer x")
                );
                assert!(matches!(
                    server.oauth,
                    Some(McpOAuthConfig::Disabled(false))
                ));
            }
            other => panic!("expected Full, got {other:?}"),
        }

        match mcp.get("local").unwrap() {
            McpServerConfig::Full(server) => {
                assert_eq!(server.command, vec!["npx", "server"]);
                assert_eq!(
                    server
                        .environment
                        .as_ref()
                        .and_then(|environment| environment.get("KEY"))
                        .map(String::as_str),
                    Some("value")
                );
                assert_eq!(server.cwd.as_deref(), Some("/tmp/project"));
                assert_eq!(server.enabled, None);
            }
            other => panic!("expected Full, got {other:?}"),
        }
    }

    #[test]
    fn keeps_legacy_mcp_map_shape() {
        let config: Config =
            serde_json::from_str(r#"{"mcp":{"x":{"command":["echo","hi"]}}}"#).unwrap();
        let mcp = config.mcp.expect("mcp").servers.expect("servers");
        match mcp.get("x").unwrap() {
            McpServerConfig::Full(server) => {
                assert_eq!(server.command, vec!["echo", "hi"]);
            }
            other => panic!("expected Full, got {other:?}"),
        }
    }

    #[test]
    fn full_mcp_config_with_enabled_flag_parses_as_full() {
        let config: Config = serde_json::from_str(
            r#"{"mcp":{"devtools":{"type":"local","command":["npx","server"],"enabled":true}}}"#,
        )
        .unwrap();
        match config
            .mcp
            .unwrap()
            .servers
            .unwrap()
            .get("devtools")
            .unwrap()
        {
            McpServerConfig::Full(server) => {
                assert_eq!(server.command, vec!["npx", "server"]);
                assert_eq!(server.enabled, Some(true));
            }
            other => panic!("expected Full, got {other:?}"),
        }
    }

    #[test]
    fn does_not_unwrap_legacy_server_named_servers() {
        let config: Config =
            serde_json::from_str(r#"{"mcp":{"servers":{"command":["echo"]}}}"#).unwrap();
        let mcp = config.mcp.expect("mcp").servers.expect("servers");
        assert!(mcp.contains_key("servers"));
    }
    #[test]
    fn parses_current_schema_model_limit_and_capabilities() {
        let config: Config = serde_json::from_str(
            r#"{
                "providers": {
                    "custom": {
                        "models": {
                            "m": {
                                "name": "M",
                                "limit": {"context": 1048576, "output": 131072},
                                "capabilities": {
                                    "tools": true,
                                    "reasoning": {"supported": true},
                                    "input": ["text", "image"],
                                    "output": ["text"]
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let provider = config.providers.unwrap().remove("custom").unwrap();
        let model = provider.models.unwrap().remove("m").unwrap();
        let limit = model.limit.expect("limit");
        assert_eq!(limit.context, Some(1048576));
        assert_eq!(limit.output, Some(131072));
        let capabilities = model.capabilities.expect("capabilities");
        assert_eq!(capabilities.tools, Some(true));
        assert_eq!(capabilities.reasoning_supported(), Some(true));
        assert_eq!(
            capabilities.input,
            Some(vec!["text".to_string(), "image".to_string()])
        );
        assert_eq!(capabilities.output, Some(vec!["text".to_string()]));
    }

    #[test]
    fn reasoning_capability_accepts_boolean_shape() {
        let capabilities: ModelCapabilitiesConfig =
            serde_json::from_str(r#"{"reasoning": true}"#).unwrap();
        assert_eq!(capabilities.reasoning_supported(), Some(true));
    }
    #[test]
    fn resolves_timeouts_from_global_defaults_and_server_overrides() {
        use super::{DEFAULT_MCP_CATALOG_MS, DEFAULT_MCP_EXECUTION_MS, DEFAULT_MCP_STARTUP_MS};

        let global: McpTimeoutConfig =
            serde_json::from_str(r#"{"startup":45000,"catalog":30000,"execution":600000}"#)
                .unwrap();
        let server: McpTimeoutConfig = serde_json::from_str(r#"{"catalog":60000}"#).unwrap();

        // No config at all -> documented defaults.
        assert_eq!(
            McpTimeoutConfig::resolve(None, None),
            (
                DEFAULT_MCP_STARTUP_MS,
                DEFAULT_MCP_CATALOG_MS,
                DEFAULT_MCP_EXECUTION_MS
            )
        );
        // Global only.
        assert_eq!(
            McpTimeoutConfig::resolve(Some(&global), None),
            (45000, 30000, 600000)
        );
        // Server override replaces only the matching key.
        assert_eq!(
            McpTimeoutConfig::resolve(Some(&global), Some(&server)),
            (45000, 60000, 600000)
        );
        // Server-only config still defaults the untouched keys.
        assert_eq!(
            McpTimeoutConfig::resolve(None, Some(&server)),
            (DEFAULT_MCP_STARTUP_MS, 60000, DEFAULT_MCP_EXECUTION_MS)
        );
    }

    #[test]
    fn merges_timeouts_per_key_across_files() {
        let mut base: Config = serde_json::from_str(
            r#"{"mcp":{"servers":{"x":{"command":["a"],"timeout":{"startup":1000,"catalog":2000}}}}}"#,
        )
        .unwrap();
        let overlay: Config = serde_json::from_str(
            r#"{"mcp":{"servers":{"x":{"command":["a"],"timeout":{"catalog":9000}}}}}"#,
        )
        .unwrap();
        base.merge(overlay);

        let server = match base.mcp.unwrap().servers.unwrap().remove("x").unwrap() {
            McpServerConfig::Full(server) => server,
            other => panic!("expected Full, got {other:?}"),
        };
        let timeout = server.timeout.expect("timeout");
        assert_eq!(
            timeout.per_operation(),
            (1000, 9000, DEFAULT_MCP_EXECUTION_MS)
        );
    }

    #[test]
    fn parses_mcp_timeout_integer_and_per_operation_forms() {
        let config: Config =
            serde_json::from_str(r#"{"mcp":{"a":{"command":["x"],"timeout":2500}}}"#).unwrap();
        let mut mcp = config.mcp.expect("mcp").servers.expect("servers");
        let server = match mcp.remove("a").expect("server") {
            McpServerConfig::Full(server) => server,
            other => panic!("expected Full, got {other:?}"),
        };
        assert_eq!(
            server.timeout.expect("timeout").per_operation(),
            (2500, 2500, 2500)
        );

        let config: Config = serde_json::from_str(
            r#"{"mcp":{"servers":{"a":{"command":["x"],"timeout":{"startup":100,"execution":900}}}}}"#,
        )
        .unwrap();
        let mut mcp = config.mcp.expect("mcp").servers.expect("servers");
        let server = match mcp.remove("a").expect("server") {
            McpServerConfig::Full(server) => server,
            other => panic!("expected Full, got {other:?}"),
        };
        assert_eq!(
            server.timeout.expect("timeout").per_operation(),
            (100, DEFAULT_MCP_CATALOG_MS, 900)
        );
    }

    #[test]
    fn legacy_server_named_timeout_is_kept_as_server() {
        let config: Config =
            serde_json::from_str(r#"{"mcp":{"timeout":{"command":["run-timeout-server"]}}}"#)
                .unwrap();
        let section = config.mcp.expect("mcp");
        assert!(
            section.timeout.is_none(),
            "server is not the global timeout"
        );
        let servers = section.servers.expect("servers");
        assert!(servers.contains_key("timeout"));
    }

    #[test]
    fn global_timeout_only_section_still_parses() {
        let config: Config =
            serde_json::from_str(r#"{"mcp":{"timeout":{"startup":45000}}}"#).unwrap();
        let section = config.mcp.expect("mcp");
        assert_eq!(section.timeout.expect("timeout").per_operation().0, 45000);
    }

    #[test]
    fn mcp_config_round_trips_through_serde() {
        let config: Config = serde_json::from_str(
            r#"{"mcp":{"timeout":{"startup":45000,"execution":600000},"servers":{"a":{"type":"local","command":["x"]}}}}"#,
        )
        .unwrap();
        let json = serde_json::to_value(&config).unwrap();
        assert!(json["mcp"].get("timeout").is_some());
        assert!(json["mcp"].get("servers").is_some());

        let reloaded: Config = serde_json::from_value(json).unwrap();
        let section = reloaded.mcp.expect("mcp");
        assert_eq!(
            section.timeout.expect("timeout").per_operation(),
            (45000, DEFAULT_MCP_CATALOG_MS, 600000)
        );
        assert!(section.servers.expect("servers").contains_key("a"));
    }

    #[test]
    fn mcp_wrapper_with_global_timeout_sibling_is_unwrapped() {
        let config: Config = serde_json::from_str(
            r#"{"mcp":{"timeout":{"startup":1000},"servers":{"a":{"command":["x"]}}}}"#,
        )
        .unwrap();
        let mcp = config.mcp.expect("mcp").servers.expect("servers");
        assert!(mcp.contains_key("a"));
        assert!(!mcp.contains_key("servers"));
        assert!(!mcp.contains_key("timeout"));
    }

    #[test]
    fn looks_like_mcp_server_detects_known_keys() {
        assert!(looks_like_mcp_server(
            &serde_json::json!({ "command": ["x"] })
        ));
        assert!(looks_like_mcp_server(
            &serde_json::json!({ "url": "http://x" })
        ));
        assert!(!looks_like_mcp_server(
            &serde_json::json!({ "startup": 1000 })
        ));
        assert!(!looks_like_mcp_server(&serde_json::json!("text")));
    }

    #[test]
    fn string_map_keeps_only_string_values() {
        let mut values = HashMap::new();
        values.insert("a".to_owned(), serde_json::json!("x"));
        values.insert("b".to_owned(), serde_json::json!(42));
        let map = string_map(values);
        assert_eq!(map.get("a").map(String::as_str), Some("x"));
        assert!(!map.contains_key("b"));
    }

    #[test]
    fn merge_option_replace_only_overwrites_when_source_present() {
        let mut target = Some("old".to_owned());
        merge_option_replace(&mut target, Some("new".to_owned()));
        assert_eq!(target.as_deref(), Some("new"));

        merge_option_replace(&mut target, None);
        assert_eq!(target.as_deref(), Some("new"));
    }

    #[test]
    fn append_unique_keep_order_deduplicates() {
        let mut target = vec!["a".to_owned(), "b".to_owned()];
        append_unique_keep_order(&mut target, vec!["b".to_owned(), "c".to_owned()]);
        assert_eq!(target, vec!["a", "b", "c"]);
    }

    #[test]
    fn explicit_empty_plugin_list_serializes_but_absent_key_is_omitted() {
        let explicit: Config = serde_json::from_str(r#"{"plugin":[]}"#).unwrap();
        assert!(explicit.plugin.is_explicit());
        let json = serde_json::to_value(&explicit).unwrap();
        assert_eq!(json.get("plugin"), Some(&serde_json::json!([])));

        let absent = Config::default();
        assert!(!absent.plugin.is_explicit());
        let json = serde_json::to_value(&absent).unwrap();
        assert!(
            json.get("plugin").is_none(),
            "an omitted plugin key must stay omitted"
        );
    }

    #[test]
    fn explicit_empty_instructions_list_survives_serialization_round_trip() {
        let explicit: Config = serde_json::from_str(r#"{"instructions":[]}"#).unwrap();
        assert!(explicit.instructions.is_explicit());

        let json = serde_json::to_value(&explicit).unwrap();
        assert_eq!(json.get("instructions"), Some(&serde_json::json!([])));

        let reloaded: Config = serde_json::from_value(json).unwrap();
        assert!(
            reloaded.instructions.is_explicit() && reloaded.instructions.is_empty(),
            "instructions: [] must persist as an explicit clear directive"
        );

        let absent = Config::default();
        assert!(!absent.instructions.is_explicit());
        let json = serde_json::to_value(&absent).unwrap();
        assert!(
            json.get("instructions").is_none(),
            "an omitted instructions key must stay omitted"
        );
    }

    /// Runtime proof for P-06: `timezone` is parsed; upstream-only keys are
    /// silently discarded (no `deny_unknown_fields`) and still parse fine.
    #[test]
    fn parses_timezone_and_discards_unmapped_upstream_keys() {
        let config: Config = serde_json::from_str(
            r#"{
                "timezone": "America/Los_Angeles",
                "policies": [{"action": "tool.use", "resource": "shell:git *", "effect": "deny"}],
                "worktree": {"directory": "../worktrees"},
                "update": "notify",
                "commands": {"hello": {"template": "echo hi"}},
                "plugins": ["some-plugin"]
            }"#,
        )
        .unwrap();
        assert_eq!(config.timezone.as_deref(), Some("America/Los_Angeles"));
        assert!(config.command.is_none());
        assert!(config.plugin.is_empty());
        assert!(config.autoupdate.is_none());

        let known = known_config_keys();
        assert!(known.contains(&"timezone"));
        for key in ["policies", "worktree", "update", "commands", "plugins"] {
            assert!(!known.contains(&key), "{key} must stay unmapped");
        }
    }

    /// Regression: `timezone` and `agents` used to be absent from `Config::merge`,
    /// so every layer above the first silently dropped them.
    #[test]
    fn merges_timezone_and_agents_across_multiple_layers() {
        let mut base: Config = serde_json::from_str(
            r#"{
                "timezone": "UTC",
                "agents": {
                    "reviewer": {"prompt": "base prompt", "model": "base-model"}
                }
            }"#,
        )
        .unwrap();

        let middle: Config = serde_json::from_str(
            r#"{
                "agents": {
                    "reviewer": {"description": "middle description"},
                    "research": {"mode": "subagent"}
                }
            }"#,
        )
        .unwrap();
        base.merge(middle);

        let top: Config = serde_json::from_str(
            r#"{
                "timezone": "Asia/Tokyo",
                "agents": {
                    "reviewer": {"prompt": "top prompt"}
                }
            }"#,
        )
        .unwrap();
        base.merge(top);

        assert_eq!(base.timezone.as_deref(), Some("Asia/Tokyo"));
        let agents = base.agents.expect("agents");
        let reviewer = agents.get("reviewer").expect("reviewer");
        assert_eq!(reviewer.prompt.as_deref(), Some("top prompt"));
        assert_eq!(reviewer.model.as_deref(), Some("base-model"));
        assert_eq!(reviewer.description.as_deref(), Some("middle description"));
        let research = agents.get("research").expect("research");
        assert!(matches!(research.mode, Some(AgentMode::Subagent)));
    }

    /// Regression: a global legacy `provider.x` must not shadow a project
    /// current-schema `providers.x` once normalization runs later in the loader.
    #[test]
    fn higher_layer_current_spelling_wins_over_lower_layer_legacy_spelling() {
        let mut base: Config = serde_json::from_str(
            r#"{"provider":{"x":{"name":"global-name","api_key":"global-value"}}}"#,
        )
        .unwrap();
        let overlay: Config = serde_json::from_str(
            r#"{"providers":{"x":{"name":"project-name","npm":"project-pkg"}}}"#,
        )
        .unwrap();

        base.merge(overlay);

        assert!(
            base.provider
                .as_ref()
                .is_none_or(|map| !map.contains_key("x")),
            "lower-precedence legacy entry must be removed so it cannot shadow"
        );
        let winner = base
            .providers
            .as_ref()
            .and_then(|map| map.get("x"))
            .expect("project providers.x");
        assert_eq!(winner.name.as_deref(), Some("project-name"));
        assert_eq!(winner.npm.as_deref(), Some("project-pkg"));
    }

    /// Symmetric case: global `providers.x` vs project `provider.x`.
    #[test]
    fn higher_layer_legacy_spelling_wins_over_lower_layer_current_spelling() {
        let mut base: Config =
            serde_json::from_str(r#"{"providers":{"x":{"name":"global-name"}}}"#).unwrap();
        let overlay: Config = serde_json::from_str(
            r#"{"provider":{"x":{"name":"project-name","base_url":"https://project"}}}"#,
        )
        .unwrap();

        base.merge(overlay);

        let winner = base
            .provider
            .as_ref()
            .and_then(|map| map.get("x"))
            .expect("project provider.x");
        assert_eq!(winner.name.as_deref(), Some("project-name"));
        assert_eq!(winner.base_url.as_deref(), Some("https://project"));
        assert!(
            base.providers
                .as_ref()
                .is_none_or(|map| !map.contains_key("x")),
            "lower-precedence current-schema entry must not linger"
        );
    }

    /// Same-spelling entries still deep-merge (no conflict is reported).
    #[test]
    fn same_spelling_provider_entries_still_deep_merge() {
        let mut base: Config =
            serde_json::from_str(r#"{"provider":{"x":{"base_url":"https://old"}}}"#).unwrap();
        let overlay: Config =
            serde_json::from_str(r#"{"provider":{"x":{"api_key":"new-value"}}}"#).unwrap();

        base.merge(overlay);

        let provider = base.provider.unwrap().remove("x").unwrap();
        assert_eq!(provider.base_url.as_deref(), Some("https://old"));
        assert_eq!(provider.api_key.as_deref(), Some("new-value"));
        assert!(base.providers.is_none());
    }
}
