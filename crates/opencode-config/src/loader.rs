use crate::schema::{
    known_config_keys, nested_root_child, nested_schema, AgentConfig, AgentConfigs, AgentMode,
    CommandConfig, McpConfig, McpTimeoutConfig, NestedValueSchema, PermissionAction,
    PermissionConfig, PermissionRule, ProviderConfig,
};
use crate::Config;
use anyhow::{Context, Result};
use jsonc_parser::{parse_to_serde_value, ParseOptions};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct ConfigLoader {
    config: Config,
    config_paths: Vec<PathBuf>,
    /// When true, `load_global` is skipped so callers are independent of the
    /// user's `~/.config/opencode` (used by tests and isolated runs).
    ignore_global_config: bool,
}

impl ConfigLoader {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            config_paths: Vec::new(),
            ignore_global_config: false,
        }
    }

    /// Returns a loader that does not read the user's global config.
    ///
    /// Use this for tests and isolated runs so results do not depend on the
    /// contents of `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode`.
    pub fn without_global_config(mut self) -> Self {
        self.ignore_global_config = true;
        self
    }

    /// Current merged config (for tests and inspection).
    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    pub fn load_from_str(&mut self, content: &str) -> Result<()> {
        // Inline config (OPENCODE_CONFIG_CONTENT) must go through the same
        // placeholder expansion as file-based config. Relative `{file:...}`
        // references resolve against the current directory. No value or path
        // is logged here; only names are surfaced by the helpers on failure.
        let mut env_lookup = self.config.env.clone().unwrap_or_default();
        if let Some(local_env) = extract_local_env(content) {
            // This inline layer is higher priority than the accumulated map.
            env_lookup.extend(local_env);
        }
        let content = substitute_env_vars_for_load(content, Some(&env_lookup))?;
        let content = resolve_file_references(&content, Path::new("."))
            .with_context(|| "Failed to resolve file references in inline config")?;

        let config: Config =
            parse_jsonc(&content).with_context(|| "Failed to parse config content")?;
        self.config.merge(config);
        Ok(())
    }

    pub fn load_from_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(());
        }

        // パスにはユーザー識別子が含まれ得るためエラーへ埋め込まない。
        let content =
            fs::read_to_string(path).with_context(|| "Failed to read config file".to_string())?;

        // `{env:VAR}` resolves from the merged top-level `env` map first
        // (config-scoped values, including this file's own `env` object), then
        // from the process environment. This is text substitution only: no
        // value is injected into the process or any child process.
        let mut env_lookup = self.config.env.clone().unwrap_or_default();
        if let Some(local_env) = extract_local_env(&content) {
            // This file is a higher-priority layer than the accumulated map.
            env_lookup.extend(local_env);
        }
        let content = substitute_env_vars_for_load(&content, Some(&env_lookup))?;

        // Apply {file:path} substitution
        let base_dir = path.parent().unwrap_or(Path::new("."));
        let content = resolve_file_references(&content, base_dir)
            .with_context(|| "Failed to resolve file references".to_string())?;

        // Never include the config path here: it can contain user identifiers.
        // `parse_jsonc` carries only a sanitized field path and never the serde
        // error body, which may embed the offending value.
        let config: Config = parse_jsonc(&content)
            .map_err(|error| anyhow::anyhow!("Failed to parse config file: {error}"))?;

        self.config.merge(config);
        self.config_paths.push(path.to_path_buf());
        Ok(())
    }

    /// Loads a config file explicitly selected by the caller (for example via
    /// `OPENCODE_CONFIG`).
    ///
    /// Unlike the incidental candidates of the project find-up scan, an
    /// explicit path that does not exist is a likely user mistake and must not
    /// disappear silently. Only the file name is logged: the full path can
    /// contain user identifiers and is deliberately never emitted. No file
    /// content, token value or path is included in the warning payload.
    pub fn load_from_explicit_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            tracing::warn!(
                file_name = %file_name,
                "explicitly specified config file does not exist; ignoring (full path not logged)"
            );
            return Ok(());
        }
        self.load_from_file(path)
    }

    pub fn load_global(&mut self) -> Result<()> {
        if self.ignore_global_config {
            return Ok(());
        }

        let global_config_path = get_global_config_path();
        let Some(config_dir) = global_config_path.parent() else {
            return Ok(());
        };
        self.load_global_from(config_dir)
    }

    /// Read the global config from an explicit directory.
    ///
    /// Kept separate from `load_global` so the read-only invariants of this
    /// path can be tested without touching the real user config directory.
    fn load_global_from(&mut self, config_dir: &Path) -> Result<()> {
        let global_config_path = config_dir.join("opencode");

        // Match project/managed precedence: read both formats in order so
        // plain JSON is merged last and wins when both files exist. Breaking
        // at the first match would invert this layer's priority.
        for ext in &["jsonc", "json"] {
            let path = global_config_path.with_extension(ext);
            if path.exists() {
                self.load_from_file(&path)?;
            }
        }

        // Read-only: legacy TOML migration is a destructive, one-time startup
        // step and is now performed explicitly by the CLI via
        // `migrate_global_config()`, never as a side effect of loading config.

        Ok(())
    }

    /// Load project-level config files, including `.opencode/opencode.json{,c}`.
    ///
    /// This is the single reader of `.opencode` config JSON. `load_all`'s
    /// `.opencode` directory scan only handles commands, agents, modes and
    /// plugins; loading the same config files there as well would merge them
    /// twice, duplicate `config_paths`, and emit duplicate warnings.
    pub fn load_project<P: AsRef<Path>>(&mut self, project_dir: P) -> Result<()> {
        let input = project_dir.as_ref();
        let start_dir = if input.is_dir() {
            input.to_path_buf()
        } else {
            input
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| input.to_path_buf())
        };
        let start_dir = normalize_existing_path(&start_dir);
        let stop_dir = detect_worktree_stop(&start_dir);

        // TS parity: findUp per target, then load from ancestor -> descendant.
        for target in [
            "opencode.jsonc",
            "opencode.json",
            ".opencode/opencode.jsonc",
            ".opencode/opencode.json",
        ] {
            let found = find_up(target, &start_dir, &stop_dir);
            for path in found.into_iter().rev() {
                self.load_from_file(path)?;
            }
        }

        // find-up は worktree ルートで止まるため、HOME/.opencode と
        // OPENCODE_CONFIG_DIR は明示的に読み込む（旧 load_all 走査の分を吸収）。
        let mut extra_dirs = Vec::new();
        if let Some(home) = dirs::home_dir() {
            extra_dirs.push(home.join(".opencode"));
        }
        if let Ok(config_dir) = env::var("OPENCODE_CONFIG_DIR") {
            extra_dirs.push(PathBuf::from(config_dir));
        }
        for dir in extra_dirs {
            for target in ["opencode.jsonc", "opencode.json"] {
                let path = dir.join(target);
                if path.exists() && !self.config_paths.contains(&path) {
                    self.load_from_file(path)?;
                }
            }
        }

        Ok(())
    }

    pub fn load_from_env(&mut self) -> Result<()> {
        if let Ok(config_path) = env::var("OPENCODE_CONFIG") {
            // Explicitly requested paths get a missing-file warning; the
            // find-up candidates in `load_project` stay silent by design.
            self.load_from_explicit_file(&config_path)?;
        }

        Ok(())
    }

    /// Load inline config content from OPENCODE_CONFIG_CONTENT env var.
    /// Per TS parity, this is applied after project config but before managed config.
    pub fn load_from_env_content(&mut self) -> Result<()> {
        if let Ok(config_content) = env::var("OPENCODE_CONFIG_CONTENT") {
            self.load_from_str(&config_content)?;
        }

        Ok(())
    }

    /// Loads all config sources synchronously (without remote wellknown).
    /// Merge order (TS parity):
    /// 1. Global config (~/.config/opencode/opencode.json{,c})
    /// 2. Custom config (OPENCODE_CONFIG)
    /// 3. Project config (opencode.json{,c} and .opencode/opencode.json{,c},
    ///    both read exclusively by `load_project`'s find-up scan)
    /// 4. .opencode directories (agents, commands, plugins, modes only; config
    ///    JSON is already owned by `load_project` and is not reloaded here)
    /// 5. Inline config (OPENCODE_CONFIG_CONTENT)
    /// 6. OPENCODE_* environment overrides (OPENCODE_PERMISSION,
    ///    OPENCODE_DISABLE_*). Applied before managed config so that managed
    ///    config keeps the documented highest priority.
    /// 7. Managed config directory (enterprise, highest priority)
    ///    Then: legacy field migrations, plugin dedup, provider normalization
    pub fn load_all<P: AsRef<Path>>(&mut self, project_dir: P) -> Result<Config> {
        let project_dir = project_dir.as_ref();

        self.load_global()?;
        self.load_from_env()?;
        self.load_project(project_dir)?;

        // Scan .opencode directories for commands, agents, modes and plugins.
        // `.opencode/opencode.json{,c}` are intentionally NOT loaded here:
        // `load_project` already read them from every directory in the project
        // hierarchy. Re-reading them would merge twice, duplicate `config_paths`
        // and emit duplicate warnings (including `{env:}` warnings).
        let directories = collect_opencode_directories(project_dir);
        for dir in &directories {
            // Load commands, agents, modes from markdown files
            let commands = load_commands_from_dir(dir);
            if !commands.is_empty() {
                let mut cmd_map = self.config.command.take().unwrap_or_default();
                for (name, cmd) in commands {
                    cmd_map.insert(name, cmd);
                }
                self.config.command = Some(cmd_map);
            }

            let agents = load_agents_from_dir(dir);
            if !agents.is_empty() {
                let mut agent_configs = self.config.agent.take().unwrap_or_default();
                for (name, agent) in agents {
                    if let Some(existing) = agent_configs.entries.get_mut(&name) {
                        // Deep merge
                        merge_agent_config(existing, agent);
                    } else {
                        agent_configs.entries.insert(name, agent);
                    }
                }
                self.config.agent = Some(agent_configs);
            }

            let modes = load_modes_from_dir(dir);
            if !modes.is_empty() {
                let mut agent_configs = self.config.agent.take().unwrap_or_default();
                for (name, agent) in modes {
                    if let Some(existing) = agent_configs.entries.get_mut(&name) {
                        merge_agent_config(existing, agent);
                    } else {
                        agent_configs.entries.insert(name, agent);
                    }
                }
                self.config.agent = Some(agent_configs);
            }

            // Load plugins from .ts/.js files. An explicitly empty `plugin`
            // array (from global or project config) is a clear directive:
            // directory plugins must not be re-added, or the user's "disable
            // all plugins" choice would silently resurrect them on restart.
            // Distinguish `is_empty` from `is_explicit`: an omitted key still
            // allows the additive directory scan.
            let plugin_explicitly_cleared =
                self.config.plugin.is_explicit() && self.config.plugin.is_empty();
            if !plugin_explicitly_cleared {
                let plugins = load_plugins_from_dir(dir);
                for plugin in plugins {
                    if !self.config.plugin.contains(&plugin) {
                        self.config.plugin.push(plugin);
                    }
                }
            }
        }

        // Inline config content overrides all non-managed config sources
        self.load_from_env_content()?;

        // Environment overrides are intentionally applied *before* managed
        // config so the enterprise managed config remains the highest-priority
        // source (matching the documented merge order above).
        apply_env_overrides(&mut self.config);

        // Load managed config (enterprise, highest priority)
        self.load_managed_config()?;

        // Apply legacy field migrations, plugin dedup, provider normalization.
        apply_post_load_transforms(&mut self.config);

        // Log only the count: config paths can contain user identifiers and
        // must not be emitted to logs.
        eprintln!(
            "[OpenCode Config] Loaded {} config files",
            self.config_paths.len()
        );

        Ok(self.config.clone())
    }

    /// Loads all config sources including remote `.well-known/opencode` endpoints.
    /// Merge order (low -> high precedence):
    /// 1. Remote .well-known/opencode (org defaults) -- lowest priority
    /// 2. Global config (~/.config/opencode/opencode.json{,c})
    /// 3. Custom config (OPENCODE_CONFIG)
    /// 4. Project config (opencode.json{,c} and .opencode/opencode.json{,c},
    ///    read only by `load_project`)
    /// 5. .opencode directories (agents, commands, plugins, modes only)
    /// 6. Inline config (OPENCODE_CONFIG_CONTENT)
    /// 7. OPENCODE_* environment overrides (below managed config)
    /// 8. Managed config directory (enterprise, highest priority)
    pub async fn load_all_with_remote<P: AsRef<Path>>(&mut self, project_dir: P) -> Result<Config> {
        let wellknown_config = crate::wellknown::load_wellknown().await;
        self.config.merge(wellknown_config);

        // Delegate to load_all which handles everything else
        self.load_all(project_dir)
    }

    /// Load managed config files from enterprise directory (highest priority).
    fn load_managed_config(&mut self) -> Result<()> {
        let managed_dir = get_managed_config_dir();
        if managed_dir.exists() {
            for ext in &["opencode.jsonc", "opencode.json"] {
                let path = managed_dir.join(ext);
                self.load_from_file(&path)?;
            }
        }
        Ok(())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn config_paths(&self) -> &[PathBuf] {
        &self.config_paths
    }
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

fn get_global_config_path() -> PathBuf {
    let config_dir = if cfg!(target_os = "macos") {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"))
    } else if cfg!(target_os = "windows") {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from("%APPDATA%"))
    } else {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"))
    };

    config_dir.join("opencode/opencode")
}

/// Explicitly migrate the legacy global TOML config into `opencode.json`.
///
/// This is the mutating counterpart of the file readers such as
/// `ConfigLoader::load_from_file`: on success it creates `config.bak`, writes
/// `opencode.json` and removes the original `config` file. Keeping this
/// behavior in a dedicated public API makes the destructive side effects
/// visible to callers instead of hiding them inside `load_global`.
///
/// The operation is guarded by `migrate_global_config_if_absent`, so it is a
/// no-op when a current JSON(C) config already exists. This makes it safe to
/// call once during CLI startup.
pub fn migrate_global_config() -> Result<Option<PathBuf>> {
    let global_config_path = get_global_config_path();
    let Some(config_dir) = global_config_path.parent() else {
        return Ok(None);
    };
    let mut config = Config::default();
    Ok(migrate_global_config_if_absent(config_dir, &mut config))
}

/// Guarded compatibility wrapper for the legacy TOML migration.
///
/// Called only by the explicit [`migrate_global_config`] startup path; the
/// config readers (`load_global` / `load_config`) never invoke it. Because the
/// raw migration is destructive (it rewrites `opencode.json` and removes the
/// source `config`), it is only run when neither `opencode.json` nor
/// `opencode.jsonc` exists. Once a JSON config exists the guard short-circuits,
/// so an existing user config is never overwritten and the destructive
/// migration effectively happens at most once.
fn migrate_global_config_if_absent(config_dir: &Path, config: &mut Config) -> Option<PathBuf> {
    if config_dir.join("opencode.json").exists() || config_dir.join("opencode.jsonc").exists() {
        tracing::debug!(
            dir = %config_dir.display(),
            "skipping legacy TOML migration: a current JSON config already exists"
        );
        return None;
    }
    migrate_legacy_toml_config(config_dir, config)
}

/// Migrate legacy global TOML config (`~/.config/opencode/config`) into
/// `opencode.json` and merge it into the currently loaded config.
///
/// The original file is copied to `config.bak` before any JSON is written; it
/// is removed only after both the backup and `opencode.json` exist. If the
/// backup cannot be created the migration aborts without writing JSON, leaving
/// the legacy TOML as the single source of truth. The in-memory config is
/// committed once the backup and `opencode.json` both exist; a failure to
/// remove the legacy TOML is warned about but does not abort the migration
/// (the JSON remains the active source).
fn migrate_legacy_toml_config(config_dir: &Path, config: &mut Config) -> Option<PathBuf> {
    let legacy_path = config_dir.join("config");
    if !legacy_path.exists() {
        return None;
    }

    let content = match fs::read_to_string(&legacy_path) {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!(
                path = %legacy_path.display(),
                %error,
                "failed to read legacy TOML config"
            );
            return None;
        }
    };

    let legacy_toml: toml::Value = match toml::from_str(&content) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                path = %legacy_path.display(),
                %error,
                "failed to parse legacy TOML config"
            );
            return None;
        }
    };
    let mut legacy_json = match serde_json::to_value(legacy_toml) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                path = %legacy_path.display(),
                %error,
                "failed to convert legacy TOML config to JSON value"
            );
            return None;
        }
    };

    let mut migrated = Config::default();
    if let Some(table) = legacy_json.as_object_mut() {
        let provider = table
            .remove("provider")
            .and_then(|value| value.as_str().map(str::to_owned));
        let model = table
            .remove("model")
            .and_then(|value| value.as_str().map(str::to_owned));
        if let (Some(provider), Some(model)) = (provider, model) {
            migrated.model = Some(format!("{provider}/{model}"));
        }
    }

    match serde_json::from_value::<Config>(legacy_json) {
        Ok(rest) => migrated.merge(rest),
        Err(error) => {
            tracing::warn!(
                path = %legacy_path.display(),
                %error,
                "failed to deserialize legacy TOML config into schema"
            );
        }
    }

    if migrated.schema.is_none() {
        migrated.schema = Some("https://opencode.ai/config.json".to_string());
    }

    // Build the value to persist on a clone so `config` stays untouched until
    // every filesystem step has succeeded. A failed migration is therefore a
    // true no-op for the caller, matching the `None` return value.
    let mut migrated_config = config.clone();
    migrated_config.merge(migrated);

    let json_path = config_dir.join("opencode.json");
    if let Some(parent) = json_path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                %error,
                "failed to create config directory during TOML migration"
            );
            return None;
        }
    }

    let serialized = match serde_json::to_string_pretty(&migrated_config) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(
                path = %json_path.display(),
                %error,
                "failed to serialize migrated config"
            );
            return None;
        }
    };

    // Back up the original before writing any JSON. If the backup fails, return
    // without creating `opencode.json` so the legacy file stays the single
    // source of truth and no unrecoverable half-migrated state is left behind.
    let backup_path = config_dir.join("config.bak");
    if let Err(error) = fs::copy(&legacy_path, &backup_path) {
        // Deliberately no config paths in this warning: directory names may be
        // sensitive. Only the I/O error category is reported.
        tracing::warn!(
            %error,
            "failed to back up legacy TOML config; migration aborted before writing JSON"
        );
        return None;
    }

    if let Err(error) = write_config_file(&json_path, serialized.as_bytes()) {
        tracing::warn!(%error, "failed to write migrated JSON config");
        return None;
    }

    if let Err(error) = fs::remove_file(&legacy_path) {
        tracing::warn!(
            path = %legacy_path.display(),
            backup = %backup_path.display(),
            %error,
            "failed to remove legacy TOML config after migration; backup was preserved"
        );
    }

    // Commit the in-memory change only after every on-disk step succeeded.
    *config = migrated_config;

    tracing::info!(
        legacy = %legacy_path.display(),
        migrated = %json_path.display(),
        "migrated legacy TOML config"
    );

    Some(json_path)
}

/// Return the escaped contents of a JSON string literal for `value`.
///
/// `serde_json::to_string` escapes every control character, including `\b`,
/// `\f` and NUL. The first and last bytes of its result are always ASCII double
/// quotes, so slicing them off yields the body to splice into an existing
/// string literal. Shared by `{env:VAR}` and `{file:...}` expansions.
fn json_string_body(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    quoted[1..quoted.len() - 1].to_string()
}

/// Neutralize bare `{env:VAR}` tokens in raw config text so it can be parsed
/// for the read-only extraction of the top-level `env` map. A bare token is
/// not valid JSON; replacing it with an empty JSON string keeps the probe
/// input parseable. Tokens inside string literals are already valid JSON and
/// are left verbatim. No value is resolved or logged here.
fn sanitize_bare_env_tokens(text: &str) -> String {
    let re = regex::Regex::new(r"\{env:([^}]+)\}").unwrap();
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for caps in re.captures_iter(text) {
        let whole = caps.get(0).expect("regex capture");
        result.push_str(&text[cursor..whole.start()]);
        if inside_json_string_literal(text, whole.start()) {
            result.push_str(whole.as_str());
        } else {
            result.push_str("\"\"");
        }
        cursor = whole.end();
    }
    result.push_str(&text[cursor..]);
    result
}

/// Read this file's own top-level `env` object from raw JSONC text.
///
/// This read-only probe makes `{env:VAR}` placeholders in the same file
/// resolvable from the config's own `env` map. Only string values are used;
/// a missing, malformed, or non-object `env` is simply ignored and left to the
/// normal schema parser. Values are never included in logs or errors.
fn extract_local_env(text: &str) -> Option<HashMap<String, String>> {
    let sanitized = sanitize_bare_env_tokens(text);
    let options = ParseOptions {
        allow_trailing_commas: true,
        ..Default::default()
    };
    let value = parse_to_serde_value::<Option<serde_json::Value>>(&sanitized, &options).ok()??;
    let env = value.as_object()?.get("env")?.as_object()?;
    Some(
        env.iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
            .collect(),
    )
}

/// Hard upper bound for iterative `{env:VAR}` expansion.
///
/// Each pass resolves one level of chained references between `config.env`
/// values. The cap guarantees termination even if cycle detection is defeated
/// by a pathological configuration.
const MAX_ENV_SUBSTITUTION_PASSES: usize = 16;

/// Fixed error text for opt-in strict mode. It deliberately contains no
/// variable name, value, file path or config content, so it is safe to surface
/// to callers and to logs.
const STRICT_ENV_UNDEFINED_ERROR: &str =
    "config contains an undefined environment variable reference (strict mode enabled)";

/// Whether opt-in strict env handling is enabled for this process.
///
/// Only `1`, `true` and `yes` (case-insensitive) enable it. Unset, malformed or
/// non-UTF-8 values keep the default warn-and-continue behavior. The value is
/// never logged, so a secret accidentally placed here cannot leak into logs.
fn strict_env_mode_enabled() -> bool {
    match env::var("OPENCODE_CONFIG_STRICT_ENV") {
        Ok(value) => matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => false,
    }
}

/// Loader entry point: reads the opt-in switch once and converts a strict
/// failure into the fixed, content-free error message above.
fn substitute_env_vars_for_load(
    text: &str,
    config_env: Option<&HashMap<String, String>>,
) -> Result<String> {
    substitute_env_vars_inner(text, config_env, strict_env_mode_enabled())
        .map_err(|_| anyhow::anyhow!("{}", STRICT_ENV_UNDEFINED_ERROR))
}

/// Non-strict helper kept for the substitution unit tests. Undefined names
/// still expand to empty and warn by name only, as in the default behavior.
#[cfg(test)]
fn substitute_env_vars(text: &str, config_env: Option<&HashMap<String, String>>) -> String {
    substitute_env_vars_inner(text, config_env, false)
        .expect("non-strict env substitution cannot fail")
}

/// Substitute `{env:VAR}` patterns with environment variable values.
///
/// Names are resolved from `config_env` (the merged top-level `env` map) first
/// and only then from the process environment. `config_env` is a substitution
/// source only: no value is injected into the process or child processes.
///
/// A value in `config_env` may itself contain `{env:VAR}` (for example
/// `B = "{env:A}/suffix"`). Resolution is iterative: each pass expands one
/// level until a fixpoint is reached, so such chains are fully resolved before
/// the config text is substituted. A repeating set of pending references
/// signals a cycle (e.g. `A = "{env:B}"`, `B = "{env:A}"`) and stops expansion;
/// [`MAX_ENV_SUBSTITUTION_PASSES`] is the hard termination bound.
///
/// Values are inserted as JSON string data, never as raw source text, so a
/// value containing quotes, backslashes or newlines cannot terminate a string
/// early or inject additional config keys. The insertion depends on context:
///
/// * Inside an existing JSON string literal (the documented `"{env:VAR}"`
///   form), only the escaped contents are inserted. The surrounding quotes are
///   already present, so the value keeps becoming the string's content as
///   before -- values without special characters produce byte-identical
///   output to the previous implementation.
/// * As a bare token (e.g. `"key": {env:VAR}`), a complete JSON string literal
///   is inserted, including quotes. This keeps such configs parseable instead
///   of emitting invalid JSON or letting the value alter the object shape.
///
/// Works on the raw JSONC text before parsing so `{file:...}` references can
/// still be resolved afterwards. In the default mode no value is written to
/// logs: only variable names appear in undefined-variable warnings. With
/// `strict` enabled, one undefined reference fails the whole substitution with
/// a fixed error that contains no names, values or paths.
///
/// Core substitution shared by the default (fail-open) and opt-in strict paths.
/// `strict` is passed in rather than read here so tests can exercise both modes
/// without mutating process-global environment variables.
fn substitute_env_vars_inner(
    text: &str,
    config_env: Option<&HashMap<String, String>>,
    strict: bool,
) -> Result<String> {
    let re = regex::Regex::new(r"\{env:([^}]+)\}").unwrap();
    let missing_in_strict = std::cell::Cell::new(false);
    let mut resolved_env = config_env.cloned().unwrap_or_default();

    // Resolve chained references between config.env values iteratively. Every
    // pass expands one level using the previous pass as the lookup source, so
    // a chain A -> B -> ... -> value is fully collapsed at the fixpoint.
    let mut previous_refs: Option<std::collections::HashSet<String>> = None;
    for _ in 0..MAX_ENV_SUBSTITUTION_PASSES {
        let snapshot = resolved_env.clone();
        let mut changed = false;
        for value in resolved_env.values_mut() {
            let expanded = re
                .replace_all(value.as_str(), |caps: &regex::Captures| {
                    let name = &caps[1];
                    match snapshot.get(name).cloned().or_else(|| env::var(name).ok()) {
                        Some(value) => value,
                        None => {
                            if strict {
                                // Strict mode fails the whole load. The caller
                                // emits a fixed error with no name or value.
                                missing_in_strict.set(true);
                            } else {
                                // Undefined names expand to empty without
                                // blocking startup. Log the name only, never
                                // the value.
                                tracing::warn!(
                                    variable = %name,
                                    "undefined environment variable in config.env expanded to an empty string; startup continues"
                                );
                            }
                            String::new()
                        }
                    }
                })
                .to_string();
            if expanded != *value {
                changed = true;
                *value = expanded;
            }
        }

        // A stable set of pending references means the remaining tokens form a
        // cycle: cut expansion off instead of looping until the pass cap.
        let pending: std::collections::HashSet<String> = resolved_env
            .values()
            .flat_map(|value| re.captures_iter(value).map(|caps| caps[1].to_string()))
            .collect();
        if !changed || previous_refs.as_ref() == Some(&pending) {
            break;
        }
        previous_refs = Some(pending);
    }

    // Substitute the config text with the fully resolved map. Values stay
    // escaped JSON string data and are inserted per the surrounding context.
    let result = re.replace_all(text, |caps: &regex::Captures| {
        // コメント内の例示・メモは置換せず、strict の未定義検出からも除外する（R4）。
        // {file:...} の解決と挙動を揃える。
        let match_start = caps.get(0).map_or(0, |m| m.start());
        if is_inside_comment(text, match_start) {
            return caps.get(0).map_or(String::new(), |m| m.as_str().to_string());
        }
        let var_name = &caps[1];
        let value = match resolved_env
            .get(var_name)
            .cloned()
            .or_else(|| env::var(var_name).ok())
        {
            Some(value) => value,
            None => {
                if strict {
                    // Strict mode fails the whole load. The caller emits a
                    // fixed error with no name, value or path.
                    missing_in_strict.set(true);
                } else {
                    // Keep the existing empty-string expansion so startup is
                    // not blocked, but surface the missing variable by name
                    // only. Never log the value (it may be a secret).
                    tracing::warn!(
                        variable = %var_name,
                        "undefined environment variable in config expanded to an empty string; startup continues"
                    );
                }
                String::new()
            }
        };

        let body = json_string_body(&value);
        if inside_json_string_literal(text, match_start) {
            body
        } else {
            format!("\"{body}\"")
        }
    })
    .to_string();

    if strict && missing_in_strict.get() {
        return Err(anyhow::anyhow!("{}", STRICT_ENV_UNDEFINED_ERROR));
    }
    Ok(result)
}

/// Returns `true` when the byte offset `pos` in `text` is inside a JSON string
/// literal.
///
/// Scans the preceding text and tracks string, escape and JSONC comment state.
/// This keeps quotes in comments from misclassifying a later placeholder and
/// prevents escaped quotes (`\"`) from toggling the string state. Checking
/// only the character directly before the match would misclassify placeholders
/// in the middle of a string such as `"prefix-{env:VAR}-suffix"`.
fn inside_json_string_literal(text: &str, pos: usize) -> bool {
    let bytes = text.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut i = 0;

    while i < pos {
        let byte = bytes[i];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && i + 1 < pos && bytes[i + 1] == b'/' {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'/' if i + 1 < pos && bytes[i + 1] == b'/' => {
                line_comment = true;
                i += 2;
                continue;
            }
            b'/' if i + 1 < pos && bytes[i + 1] == b'*' => {
                block_comment = true;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    in_string
}

/// Returns `true` when byte offset `pos` in `text` is inside a JSONC comment.
///
/// Both `//` line comments and `/* ... */` block comments are recognised. The
/// scan also tracks string literals so comment markers inside a string (for
/// example `"https://example.com"`) are not mistaken for real comments, and
/// so quotes inside a comment do not disturb the string state.
fn is_inside_comment(text: &str, pos: usize) -> bool {
    let bytes = text.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut i = 0;

    while i < pos {
        let byte = bytes[i];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                line_comment = true;
                i += 2;
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                block_comment = true;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    line_comment || block_comment
}

/// Resolve `{file:path}` patterns by reading file contents.
///
/// Every match is classified at its own byte offset: references inside `//`
/// or `/* ... */` comments are left verbatim, all others are expanded. The
/// output is rebuilt by splicing only the matched ranges, so an identical
/// reference at another position is never affected by this decision.
///
/// File contents are escaped with `serde_json::to_string`, the same path used
/// by `substitute_env_vars`, so every control character (including `\b`, `\f`
/// and NUL) is escaped and the expanded config stays parseable.
fn resolve_file_references(text: &str, base_dir: &Path) -> Result<String> {
    let re = regex::Regex::new(r"\{file:([^}]+)\}").unwrap();

    // Collect ranges first to avoid borrowing `text` while rebuilding it.
    let matches: Vec<(usize, usize, String, String)> = re
        .captures_iter(text)
        .map(|caps| {
            let whole = caps.get(0).unwrap();
            (
                whole.start(),
                whole.end(),
                whole.as_str().to_string(),
                caps[1].to_string(),
            )
        })
        .collect();

    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;

    for (start, end, full_match, file_path_str) in matches {
        result.push_str(&text[cursor..start]);
        cursor = end;

        if is_inside_comment(text, start) {
            result.push_str(&full_match);
            continue;
        }

        let resolved = if let Some(stripped) = file_path_str.strip_prefix("~/") {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(stripped)
        } else if Path::new(&file_path_str).is_absolute() {
            PathBuf::from(&file_path_str)
        } else {
            base_dir.join(&file_path_str)
        };

        // 参照トークンにも解決先パスにもユーザー識別子が含まれ得るため、
        // エラーへは埋め込まず固定文言のみを返す。ファイル本文も出さない。
        let content =
            fs::read_to_string(&resolved).with_context(|| "bad file reference".to_string())?;
        let content = content.trim();

        // Use the shared serde_json escaper instead of the old hand-written
        // five-case replacement: it escapes every control character (\b, \f,
        // NUL, ...), not only backslash, quote, newline, carriage return and
        // tab.
        let body = json_string_body(content);
        if inside_json_string_literal(text, start) {
            // The surrounding quotes are already present in the config text.
            result.push_str(&body);
        } else {
            result.push('"');
            result.push_str(&body);
            result.push('"');
        }
    }

    result.push_str(&text[cursor..]);
    Ok(result)
}

fn parse_jsonc(content: &str) -> Result<Config> {
    let parse_options = ParseOptions {
        allow_trailing_commas: true,
        ..Default::default()
    };
    // JSONC syntax errors can echo invalid source text. Surface only a fixed
    // category so no config value reaches an error chain or a log.
    let parsed = match parse_to_serde_value::<Option<serde_json::Value>>(content, &parse_options) {
        Ok(value) => value.context("Config content is empty")?,
        Err(_parse_error) => return Err(anyhow::anyhow!("Failed to parse JSONC")),
    };

    validate_config_value(parsed)
}

/// Shared validation pipeline for an already-parsed config JSON value.
///
/// Both local config parsing ([`parse_jsonc`]) and the remote
/// `.well-known/opencode` loader call this helper, so unknown keys, nested
/// typos and type errors are handled identically for every config source.
/// Previously the wellknown path deserialized its `config` value directly with
/// `serde_json::from_value`, bypassing the unknown-key/nested-key warnings and
/// the sanitized error path.
///
/// Only key names and key paths are emitted in warnings; values, tokens and
/// URLs are never included in warnings or errors. Unknown keys are reported but
/// never rejected, preserving the documented "never block startup" policy.
pub(crate) fn validate_config_value(parsed: serde_json::Value) -> Result<Config> {
    // Warn about (but never reject) unknown keys. Only key names / key paths
    // are logged -- never a value, which may hold secrets. Ignoring unknown
    // keys keeps the documented "never block startup" compatibility policy.
    for key in unknown_config_keys(&parsed) {
        tracing::warn!(
            key = %key,
            "unknown configuration key ignored; startup continues"
        );
    }
    // Nested typos (e.g. `server.prots`) used to be dropped silently. The
    // walker inspects key names only, skips user-defined name levels and has a
    // fixed depth bound; it never logs the values behind those keys.
    for key_path in unknown_nested_config_keys(&parsed) {
        tracing::warn!(
            key_path = %key_path,
            "unknown nested configuration key ignored; startup continues"
        );
    }

    match serde_json::from_value::<Config>(parsed.clone()) {
        Ok(config) => Ok(config),
        Err(_serde_error) => {
            // `_serde_error` is deliberately dropped: serde embeds the offending
            // value in its Display text (for example an api_key with the wrong
            // JSON type). Only a sanitized field path is allowed through.
            match failing_config_field_path(&parsed) {
                Some(path) => Err(anyhow::anyhow!(
                    "Failed to parse config JSON at field: {path}"
                )),
                None => Err(anyhow::anyhow!("Failed to parse config JSON")),
            }
        }
    }
}

/// Locate the field responsible for a `Config` deserialization failure.
///
/// Serde's error text is never used: it can embed the offending value (for
/// example an API key with the wrong JSON type), which must not reach logs or
/// errors. Instead the parsed value is probed: removing a key that makes the
/// whole config deserialize identifies the failing field by name only. Nested
/// objects are examined first so the most specific path (`server.port`) wins.
/// Returns `None` when no safe, unambiguous path can be found.
fn failing_config_field_path(value: &serde_json::Value) -> Option<String> {
    if serde_json::from_value::<Config>(value.clone()).is_ok() {
        return None;
    }
    let mut path: Vec<String> = Vec::new();
    find_failing_field_path::<Config>(value, &mut path)
}

/// Recursive probe behind [`failing_config_field_path`].
///
/// `path` addresses the object currently being inspected in `root`. Each
/// candidate is applied to a fresh clone of `root` and validated against the
/// real deserializer, so only key names -- never values -- influence the
/// result.
fn find_failing_field_path<T: serde::de::DeserializeOwned>(
    root: &serde_json::Value,
    path: &mut Vec<String>,
) -> Option<String> {
    let mut current = root;
    for segment in path.iter() {
        current = current.get(segment.as_str())?;
    }
    let object = current.as_object()?;

    // Prefer the deepest culprit so nested paths such as `server.port` are
    // reported instead of just the containing object.
    for (key, child) in object {
        if !child.is_object() || !is_safe_field_segment(key) {
            continue;
        }
        path.push(key.clone());
        let found = find_failing_field_path::<T>(root, path);
        path.pop();
        if found.is_some() {
            return found;
        }
    }

    // No deeper field explains the failure: test key removal at this level.
    for key in object.keys() {
        if !is_safe_field_segment(key) {
            continue;
        }
        let mut candidate = root.clone();
        let Some(target) = value_at_path_mut(&mut candidate, path) else {
            continue;
        };
        let Some(map) = target.as_object_mut() else {
            continue;
        };
        map.remove(key.as_str());
        if serde_json::from_value::<T>(candidate).is_err() {
            continue;
        }
        let mut segments = path.clone();
        segments.push(key.clone());
        let joined = segments.join(".");
        if is_safe_field_path(&joined) {
            return Some(joined);
        }
    }

    None
}

/// Navigate to a mutable value addressed by object keys only.
fn value_at_path_mut<'a>(
    root: &'a mut serde_json::Value,
    path: &[String],
) -> Option<&'a mut serde_json::Value> {
    let mut current = root;
    for segment in path {
        current = current.get_mut(segment.as_str())?;
    }
    Some(current)
}

/// A single key is loggable only in identifier-like form. Dots are permitted
/// because they separate nested segments.
fn is_safe_field_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 64
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Complete paths stay within the same safe alphabet and length bound.
fn is_safe_field_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 64
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '*'))
}

/// Returns the names of top-level keys in a parsed config that `Config` does
/// not recognize. Values are deliberately discarded: callers log only the
/// key names. Only top-level keys are inspected here so the M-08 findings
/// (`security.strictPermissionChecks`, `storage.gcBufferMb`,
/// `references.gemini`) stay a single top-level warning; nested sections are
/// handled by [`unknown_nested_config_keys`].
fn unknown_config_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let known: std::collections::HashSet<&str> = known_config_keys().iter().copied().collect();
    let mut unknown: Vec<String> = object
        .keys()
        .filter(|key| !known.contains(key.as_str()))
        // リモート設定由来のキー名もあり得るため、入れ子警告と同じ安全基準で
        // サニタイズしてからログへ渡す（R4）。
        .filter(|key| is_safe_field_segment(key))
        .cloned()
        .collect();
    unknown.sort();
    unknown
}

/// 入れ子キーの誤記を検出するときの深さ上限。トップレベル直下を 1 と数え、
/// ユーザー定義名（エージェント名・provider id・MCP server id）の階層は
/// カウントしない。これにより `provider.<id>.<key>` や
/// `mcp.servers.<id>.<key>` も上限内で検査できる。
const MAX_NESTED_KEY_DEPTH: usize = 3;

/// 既知の入れ子キーに対して、誤記と思われるキーパスだけを返す。
///
/// 値の型や内容は一切見ない（値依存の判定をしないため）。返る文字列は安全な
/// 識別子と `.` だけで構成され、呼び出し側はこれをそのまま警告へ渡す。
/// ユーザー定義キー自体は警告の対象外で、パス区切りとして使う場合も
/// [`is_safe_field_segment`] を通ったものだけを使用する。
fn unknown_nested_config_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut unknown = Vec::new();
    for (key, child) in object {
        let Some(schema) = nested_root_child(key) else {
            // このセクションの入れ子キー集合は未定義。誤検知を避けるため対象外。
            continue;
        };
        if !is_safe_field_segment(key) {
            continue;
        }
        collect_unknown_nested_keys(child, schema, 1, key, &mut unknown);
    }
    unknown.sort();
    unknown.dedup();
    unknown
}

/// `value` を `schema` に従って辿り、未知のキー名を `path` 配下のキーパスとして集める。
fn collect_unknown_nested_keys(
    value: &serde_json::Value,
    schema: NestedValueSchema,
    depth: usize,
    path: &str,
    out: &mut Vec<String>,
) {
    if depth > MAX_NESTED_KEY_DEPTH {
        return;
    }

    match schema {
        NestedValueSchema::Opaque | NestedValueSchema::UserMapOpaque => {}
        NestedValueSchema::UserMapStruct(name) => {
            let Some(object) = value.as_object() else {
                return;
            };
            for (user_key, child) in object {
                // ユーザー定義名（provider id / agent 名 / MCP server id）は
                // 誤記判定せず、警告パスへも出さず固定プレースホルダ `*` に置換する。
                if !is_safe_field_segment(user_key) {
                    continue;
                }
                let child_path = format!("{path}.*");
                if !is_safe_field_path(&child_path) {
                    continue;
                }
                collect_unknown_nested_keys(
                    child,
                    NestedValueSchema::Struct(name),
                    depth,
                    &child_path,
                    out,
                );
            }
        }
        NestedValueSchema::ListOfStruct(name) => {
            let Some(items) = value.as_array() else {
                return;
            };
            for (index, item) in items.iter().enumerate() {
                let child_path = format!("{path}.entry{index}");
                if !is_safe_field_path(&child_path) {
                    continue;
                }
                collect_unknown_nested_keys(
                    item,
                    NestedValueSchema::Struct(name),
                    depth,
                    &child_path,
                    out,
                );
            }
        }
        NestedValueSchema::Struct(name) => {
            let Some(object) = value.as_object() else {
                return;
            };
            let Some(spec) = nested_schema(name) else {
                return;
            };
            for (key, child) in object {
                let declared = spec
                    .children
                    .iter()
                    .find(|(candidate, _)| *candidate == key.as_str())
                    .map(|(_, child_schema)| *child_schema);
                let is_known = spec.known.contains(&key.as_str());

                // 値は見ず、キー名だけを既知集合と照合する。
                if !is_known && declared.is_none() && spec.unknown_child.is_none() {
                    if !is_safe_field_segment(key) {
                        continue;
                    }
                    let key_path = format!("{path}.{key}");
                    if is_safe_field_path(&key_path) {
                        out.push(key_path);
                    }
                }

                let child_schema = declared
                    .or(if is_known {
                        Some(NestedValueSchema::Opaque)
                    } else {
                        spec.unknown_child
                    })
                    .unwrap_or(NestedValueSchema::Opaque);
                if matches!(child_schema, NestedValueSchema::Opaque) || !is_safe_field_segment(key)
                {
                    continue;
                }
                let child_path = format!("{path}.{key}");
                if !is_safe_field_path(&child_path) {
                    continue;
                }
                collect_unknown_nested_keys(child, child_schema, depth + 1, &child_path, out);
            }
        }
    }
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn detect_worktree_stop(start: &Path) -> PathBuf {
    let mut current = normalize_existing_path(start);
    let mut topmost = current.clone();
    loop {
        if current.join(".git").exists() {
            return current;
        }
        let Some(parent) = current.parent() else {
            return topmost;
        };
        if parent == current {
            return topmost;
        }
        topmost = parent.to_path_buf();
        current = parent.to_path_buf();
    }
}

fn find_up(target: &str, start: &Path, stop: &Path) -> Vec<PathBuf> {
    let mut current = normalize_existing_path(start);
    let stop = normalize_existing_path(stop);
    let mut result = Vec::new();

    loop {
        let candidate = current.join(target);
        if candidate.exists() {
            result.push(candidate);
        }
        if current == stop {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    result
}

/// Get the managed config directory for enterprise deployments.
fn get_managed_config_dir() -> PathBuf {
    if let Ok(test_dir) = env::var("OPENCODE_TEST_MANAGED_CONFIG_DIR") {
        return PathBuf::from(test_dir);
    }
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/opencode")
    } else if cfg!(target_os = "windows") {
        let program_data =
            env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        PathBuf::from(program_data).join("opencode")
    } else {
        PathBuf::from("/etc/opencode")
    }
}

/// Collect .opencode directories from project hierarchy.
/// Global config (~/.config/opencode) is loaded separately via load_global() and should not be included here.
fn collect_opencode_directories(project_dir: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();

    // Project .opencode directories (walk up from project_dir to worktree root)
    let start_dir = normalize_existing_path(project_dir);
    let stop_dir = detect_worktree_stop(&start_dir);
    let found = find_up(".opencode", &start_dir, &stop_dir);
    // Reverse so ancestor dirs come first (lower priority)
    for path in found.into_iter().rev() {
        directories.push(path);
    }

    // Home directory .opencode
    if let Some(home) = dirs::home_dir() {
        let home_opencode = home.join(".opencode");
        if home_opencode.exists() && !directories.contains(&home_opencode) {
            directories.push(home_opencode);
        }
    }

    // OPENCODE_CONFIG_DIR override
    if let Ok(config_dir) = env::var("OPENCODE_CONFIG_DIR") {
        let dir = PathBuf::from(config_dir);
        if !directories.contains(&dir) {
            directories.push(dir);
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    directories.retain(|d| seen.insert(d.clone()));

    directories
}

/// Load command definitions from markdown files in {command,commands}/**/*.md
fn load_commands_from_dir(dir: &Path) -> HashMap<String, CommandConfig> {
    let mut result = HashMap::new();

    for subdir_name in &["command", "commands"] {
        let subdir = dir.join(subdir_name);
        if !subdir.is_dir() {
            continue;
        }
        if let Ok(entries) = glob_md_files(&subdir) {
            for entry in entries {
                if let Some((name, content)) = parse_markdown_command(&entry, dir) {
                    result.insert(name, content);
                }
            }
        }
    }

    result
}

/// Load agent definitions from markdown files in {agent,agents}/**/*.md
fn load_agents_from_dir(dir: &Path) -> HashMap<String, AgentConfig> {
    let mut result = HashMap::new();

    for subdir_name in &["agent", "agents"] {
        let subdir = dir.join(subdir_name);
        if !subdir.is_dir() {
            continue;
        }
        if let Ok(entries) = glob_md_files(&subdir) {
            for entry in entries {
                if let Some((name, config)) = parse_markdown_agent(&entry, dir) {
                    result.insert(name, config);
                }
            }
        }
    }

    result
}

/// Load mode definitions from markdown files in {mode,modes}/*.md
fn load_modes_from_dir(dir: &Path) -> HashMap<String, AgentConfig> {
    let mut result = HashMap::new();

    for subdir_name in &["mode", "modes"] {
        let subdir = dir.join(subdir_name);
        if !subdir.is_dir() {
            continue;
        }
        if let Ok(entries) = glob_md_files(&subdir) {
            for entry in entries {
                if let Some((name, mut config)) = parse_markdown_agent(&entry, dir) {
                    // Modes are always primary agents
                    config.mode = Some(AgentMode::Primary);
                    result.insert(name, config);
                }
            }
        }
    }

    result
}

/// Load plugin paths from .ts/.js files in {plugin,plugins}/*.{ts,js}
fn load_plugins_from_dir(dir: &Path) -> Vec<String> {
    let mut plugins = Vec::new();

    for subdir_name in &["plugin", "plugins"] {
        let subdir = dir.join(subdir_name);
        if !subdir.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&subdir) {
            // `fs::read_dir` order is unspecified. Sort candidate paths so the
            // later name-based deduplication (deduplicate_plugins keeps the
            // last entry) always retains the same file, e.g. `foo.ts` rather
            // than whichever extension the filesystem enumerated last.
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .map(|ext| ext == "ts" || ext == "js")
                        .unwrap_or(false)
                })
                .collect();
            paths.sort();
            for path in paths {
                // Convert to file:// URL like TS does
                plugins.push(format!("file://{}", path.display()));
            }
        }
    }

    plugins
}

/// Recursively find all .md files in a directory.
///
/// Results are returned sorted by path. `fs::read_dir` yields entries in an
/// unspecified, filesystem-dependent order; downstream loaders deduplicate by
/// derived name (keeping one entry), so they must see a stable order to pick a
/// deterministic winner regardless of the environment (CFG2-10).
fn glob_md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();
    glob_md_files_recursive(dir, &mut results)?;
    results.sort();
    Ok(results)
}

fn glob_md_files_recursive(dir: &Path, results: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            glob_md_files_recursive(&path, results)?;
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            results.push(path);
        }
    }
    Ok(())
}

/// Recover the frontmatter key responsible for a deserialization failure.
///
/// Serde error text can embed the offending value (`invalid type: string
/// "..."`), so it is never logged. Instead each key is removed in turn and the
/// config is re-parsed; a key whose removal makes parsing succeed identifies
/// the failing field. Only identifier-shaped keys are eligible for logging.
fn failing_frontmatter_field<T: serde::de::DeserializeOwned>(
    parsed: &serde_json::Value,
) -> Option<String> {
    let object = parsed.as_object()?;
    for key in object.keys() {
        if !is_loggable_field_name(key) {
            continue;
        }
        let mut reduced = object.clone();
        reduced.remove(key);
        if serde_json::from_value::<T>(serde_json::Value::Object(reduced)).is_ok() {
            return Some(key.clone());
        }
    }
    None
}

fn is_loggable_field_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Warn about a frontmatter metadata parse failure using only the file name
/// and the failing field name. Values and the markdown body are never logged.
fn warn_frontmatter_parse_failure<T: serde::de::DeserializeOwned>(
    path: &Path,
    parsed: &serde_json::Value,
) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .unwrap_or("<unnamed>");
    let field = failing_frontmatter_field::<T>(parsed).unwrap_or_else(|| "<unknown>".to_string());
    tracing::warn!(
        file = %file_name,
        field = %field,
        "frontmatter metadata parse failed; using defaults for metadata"
    );
}

/// Parse a markdown file as a command definition.
/// Extracts YAML frontmatter and body content.
fn parse_markdown_command(path: &Path, base_dir: &Path) -> Option<(String, CommandConfig)> {
    let content = fs::read_to_string(path).ok()?;
    let (frontmatter, body) = split_frontmatter(&content);

    // Derive name from relative path
    let name = derive_name_from_path(path, base_dir, &["command", "commands"]);

    let mut config = if let Some(fm) = frontmatter {
        let parsed = serde_yaml_frontmatter_to_json(&fm);
        match serde_json::from_value::<CommandConfig>(parsed.clone()) {
            Ok(parsed_config) => parsed_config,
            Err(_) => {
                warn_frontmatter_parse_failure::<CommandConfig>(path, &parsed);
                CommandConfig::default()
            }
        }
    } else {
        CommandConfig::default()
    };

    config.template = Some(body.trim().to_string());

    Some((name, config))
}

/// Parse a markdown file as an agent definition.
fn parse_markdown_agent(path: &Path, base_dir: &Path) -> Option<(String, AgentConfig)> {
    let content = fs::read_to_string(path).ok()?;
    let (frontmatter, body) = split_frontmatter(&content);

    let name = derive_name_from_path(path, base_dir, &["agent", "agents", "mode", "modes"]);

    let mut config = if let Some(fm) = frontmatter {
        let parsed = serde_yaml_frontmatter_to_json(&fm);
        match serde_json::from_value::<AgentConfig>(parsed.clone()) {
            Ok(parsed_config) => parsed_config,
            Err(_) => {
                warn_frontmatter_parse_failure::<AgentConfig>(path, &parsed);
                AgentConfig::default()
            }
        }
    } else {
        AgentConfig::default()
    };

    config.prompt = Some(body.trim().to_string());

    Some((name, config))
}

/// Split markdown content into optional YAML frontmatter and body.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, content.to_string());
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    if let Some(end_idx) = after_first.find("\n---") {
        let fm = after_first[..end_idx].trim().to_string();
        let body_start = end_idx + 4; // skip \n---
        let body = if body_start < after_first.len() {
            after_first[body_start..].to_string()
        } else {
            String::new()
        };
        (Some(fm), body)
    } else {
        (None, content.to_string())
    }
}

/// Fallback sanitization for invalid YAML frontmatter.
/// Matches TS `ConfigMarkdown.fallbackSanitization`: if a top-level value
/// contains a colon (which confuses simple YAML parsers), convert it to a
/// block scalar so the value is preserved verbatim.
fn fallback_sanitize_yaml(yaml: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let kv_re = regex::Regex::new(r"^([a-zA-Z_][a-zA-Z0-9_]*)\s*:\s*(.*)$").unwrap();
    for line in yaml.lines() {
        let trimmed = line.trim();
        // Pass through comments and empty lines
        if trimmed.starts_with('#') || trimmed.is_empty() {
            result.push(line.to_string());
            continue;
        }
        // Pass through continuation/indented lines
        if line.starts_with(char::is_whitespace) {
            result.push(line.to_string());
            continue;
        }
        // Match top-level key: value
        let Some(caps) = kv_re.captures(line) else {
            result.push(line.to_string());
            continue;
        };
        let key = &caps[1];
        let value = caps[2].trim();
        // Skip if value is empty, already quoted, or uses block scalar indicator
        if value.is_empty()
            || value == ">"
            || value == "|"
            || value == "|-"
            || value == ">-"
            || value.starts_with('"')
            || value.starts_with('\'')
        {
            result.push(line.to_string());
            continue;
        }
        // If value contains a colon, convert to block scalar
        if value.contains(':') {
            result.push(format!("{}: |-", key));
            result.push(format!("  {}", value));
            continue;
        }
        result.push(line.to_string());
    }
    result.join("\n")
}

/// Parse a YAML scalar value string into a JSON value.
fn yaml_scalar_to_json(value: &str) -> serde_json::Value {
    let value = value.trim();
    if value.is_empty() {
        return serde_json::Value::Null;
    }
    // Booleans
    if value == "true" || value == "True" || value == "TRUE" {
        return serde_json::Value::Bool(true);
    }
    if value == "false" || value == "False" || value == "FALSE" {
        return serde_json::Value::Bool(false);
    }
    if value == "null" || value == "Null" || value == "NULL" || value == "~" {
        return serde_json::Value::Null;
    }
    // Numbers
    if let Ok(n) = value.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    if let Ok(n) = value.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(num);
        }
    }
    // Strip surrounding quotes. A lone quote character (`"` or `'`) has length 1;
    // slicing it as `1..len - 1` yields the invalid range `1..0` and panics.
    // Requiring at least two bytes keeps the slice valid, and such malformed
    // input is simply returned unchanged as a string.
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return serde_json::Value::String(value[1..value.len() - 1].to_string());
    }
    serde_json::Value::String(value.to_string())
}

/// Parse an inline YAML flow sequence like `[a, b, c]` into a JSON array.
fn parse_inline_list(value: &str) -> Option<serde_json::Value> {
    let trimmed = value.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Some(serde_json::Value::Array(Vec::new()));
    }
    let items: Vec<serde_json::Value> = inner
        .split(',')
        .map(|item| yaml_scalar_to_json(item.trim()))
        .collect();
    Some(serde_json::Value::Array(items))
}

/// Parse an inline YAML flow mapping like `{a: 1, b: true}` into a JSON object.
fn parse_inline_map(value: &str) -> Option<serde_json::Value> {
    let trimmed = value.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Some(serde_json::Value::Object(serde_json::Map::new()));
    }
    let mut map = serde_json::Map::new();
    for pair in inner.split(',') {
        if let Some((k, v)) = pair.split_once(':') {
            map.insert(k.trim().to_string(), yaml_scalar_to_json(v));
        }
    }
    Some(serde_json::Value::Object(map))
}

/// Compute the indentation level (number of leading spaces) of a line.
fn indent_level(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// YAML frontmatter to JSON conversion.
/// Handles: flat key-value, inline lists/maps, multi-line dash lists,
/// nested objects (indentation-based), and block scalars (| and >).
/// Falls back to sanitized re-parse on failure.
fn serde_yaml_frontmatter_to_json(yaml: &str) -> serde_json::Value {
    match parse_yaml_mapping(yaml) {
        Some(value) => value,
        None => {
            // Fallback: sanitize and retry
            let sanitized = fallback_sanitize_yaml(yaml);
            parse_yaml_mapping(&sanitized)
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
        }
    }
}

/// Parse a YAML mapping (object) from a string. Returns None on structural failure.
fn parse_yaml_mapping(yaml: &str) -> Option<serde_json::Value> {
    let lines: Vec<&str> = yaml.lines().collect();
    let (map, _) = parse_yaml_mapping_lines(&lines, 0, 0)?;
    Some(serde_json::Value::Object(map))
}

/// Parse YAML mapping lines starting at `start` index with expected `base_indent`.
/// Returns the parsed map and the index of the next unconsumed line.
fn parse_yaml_mapping_lines(
    lines: &[&str],
    start: usize,
    base_indent: usize,
) -> Option<(serde_json::Map<String, serde_json::Value>, usize)> {
    let mut map = serde_json::Map::new();
    let mut i = start;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        let current_indent = indent_level(line);

        // If we've dedented past our base, we're done with this mapping
        if current_indent < base_indent {
            break;
        }

        // Handle list items at this level (shouldn't appear in a mapping context at same indent
        // unless it's a top-level list, which we don't support as root)
        if trimmed.starts_with("- ") && current_indent == base_indent {
            // This is a list at the mapping level -- not valid for our use case, skip
            break;
        }

        // Parse key: value
        if let Some((key_part, value_part)) = trimmed.split_once(':') {
            let key = key_part.trim().to_string();
            let value_str = value_part.trim();

            if value_str.is_empty() {
                // Value is on subsequent indented lines -- could be a nested map, list, or block scalar
                i += 1;
                if i < lines.len() {
                    let next_trimmed = lines[i].trim();
                    let next_indent = indent_level(lines[i]);
                    if next_indent > current_indent && next_trimmed.starts_with("- ") {
                        // It's a dash-list
                        let (list, next_i) = parse_yaml_list_lines(lines, i, next_indent);
                        map.insert(key, serde_json::Value::Array(list));
                        i = next_i;
                    } else if next_indent > current_indent {
                        // It's a nested mapping
                        if let Some((nested_map, next_i)) =
                            parse_yaml_mapping_lines(lines, i, next_indent)
                        {
                            map.insert(key, serde_json::Value::Object(nested_map));
                            i = next_i;
                        } else {
                            // Treat as string value from remaining indented lines
                            let (text, next_i) = collect_block_text(lines, i, current_indent);
                            map.insert(key, serde_json::Value::String(text));
                            i = next_i;
                        }
                    } else {
                        // Empty value, next line is at same or lower indent
                        map.insert(key, serde_json::Value::Null);
                    }
                } else {
                    map.insert(key, serde_json::Value::Null);
                }
            } else if value_str == "|" || value_str == "|-" || value_str == "|+" {
                // Block scalar (literal)
                i += 1;
                let (text, next_i) = collect_block_text(lines, i, current_indent);
                let text = if value_str == "|-" {
                    text.trim_end().to_string()
                } else {
                    text
                };
                map.insert(key, serde_json::Value::String(text));
                i = next_i;
            } else if value_str == ">" || value_str == ">-" || value_str == ">+" {
                // Block scalar (folded)
                i += 1;
                let (text, next_i) = collect_block_text(lines, i, current_indent);
                // Folded: join lines with spaces
                let folded = text.lines().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
                let folded = if value_str == ">-" {
                    folded.trim_end().to_string()
                } else {
                    folded
                };
                map.insert(key, serde_json::Value::String(folded));
                i = next_i;
            } else if let Some(list) = parse_inline_list(value_str) {
                map.insert(key, list);
                i += 1;
            } else if let Some(obj) = parse_inline_map(value_str) {
                map.insert(key, obj);
                i += 1;
            } else {
                map.insert(key, yaml_scalar_to_json(value_str));
                i += 1;
            }
        } else {
            // Line doesn't match key: value pattern, skip
            i += 1;
        }
    }

    Some((map, i))
}

/// Parse YAML list lines (lines starting with "- ") at the given indent level.
/// Returns the list of values and the next unconsumed line index.
fn parse_yaml_list_lines(
    lines: &[&str],
    start: usize,
    base_indent: usize,
) -> (Vec<serde_json::Value>, usize) {
    let mut list = Vec::new();
    let mut i = start;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        let current_indent = indent_level(line);
        if current_indent < base_indent {
            break;
        }
        if current_indent > base_indent {
            // Continuation of previous item, skip
            i += 1;
            continue;
        }

        if let Some(item_str) = trimmed.strip_prefix("- ") {
            let item_str = item_str.trim();
            // Check if item itself is a key: value (nested object in list)
            if item_str.contains(": ") {
                // Could be an inline object item like "- key: value"
                // For simplicity, treat as scalar string
                list.push(yaml_scalar_to_json(item_str));
            } else if let Some(inline_list) = parse_inline_list(item_str) {
                list.push(inline_list);
            } else {
                list.push(yaml_scalar_to_json(item_str));
            }
            i += 1;
        } else {
            break;
        }
    }

    (list, i)
}

/// Collect indented block text lines (for block scalars or multi-line values).
/// Stops when a line at or below `parent_indent` is encountered.
fn collect_block_text(lines: &[&str], start: usize, parent_indent: usize) -> (String, usize) {
    let mut text_lines = Vec::new();
    let mut i = start;
    let mut block_indent: Option<usize> = None;

    while i < lines.len() {
        let line = lines[i];
        // Empty lines are part of the block
        if line.trim().is_empty() {
            text_lines.push("");
            i += 1;
            continue;
        }
        let current_indent = indent_level(line);
        if current_indent <= parent_indent {
            break;
        }
        // Determine the block's base indent from the first non-empty line
        let bi = *block_indent.get_or_insert(current_indent);
        if current_indent >= bi {
            text_lines.push(&line[bi..]);
        } else {
            text_lines.push(line.trim());
        }
        i += 1;
    }

    // Trim trailing empty lines
    while text_lines.last() == Some(&"") {
        text_lines.pop();
    }

    (text_lines.join("\n"), i)
}

/// Derive a name from a file path relative to the base directory.
fn derive_name_from_path(path: &Path, base_dir: &Path, strip_prefixes: &[&str]) -> String {
    let rel = path
        .strip_prefix(base_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    // Strip known directory prefixes
    let mut name = rel.as_str();
    for prefix in strip_prefixes {
        let with_sep = format!("{}/", prefix);
        if let Some(stripped) = name.strip_prefix(&with_sep) {
            name = stripped;
            break;
        }
        // Also try .opencode/ prefix
        let with_opencode = format!(".opencode/{}/", prefix);
        if let Some(stripped) = name.strip_prefix(&with_opencode) {
            name = stripped;
            break;
        }
    }

    // Remove .md extension
    name.strip_suffix(".md").unwrap_or(name).to_string()
}

/// Merge one AgentConfig into another (simple field-level merge).
fn merge_agent_config(target: &mut AgentConfig, source: AgentConfig) {
    if source.name.is_some() {
        target.name = source.name;
    }
    if source.model.is_some() {
        target.model = source.model;
    }
    if source.variant.is_some() {
        target.variant = source.variant;
    }
    if source.temperature.is_some() {
        target.temperature = source.temperature;
    }
    if source.top_p.is_some() {
        target.top_p = source.top_p;
    }
    if source.prompt.is_some() {
        target.prompt = source.prompt;
    }
    if source.disable.is_some() {
        target.disable = source.disable;
    }
    if source.description.is_some() {
        target.description = source.description;
    }
    if source.mode.is_some() {
        target.mode = source.mode;
    }
    if source.hidden.is_some() {
        target.hidden = source.hidden;
    }
    if source.color.is_some() {
        target.color = source.color;
    }
    if source.steps.is_some() {
        target.steps = source.steps;
    }
    if source.max_steps.is_some() {
        target.max_steps = source.max_steps;
    }
    if source.max_tokens.is_some() {
        target.max_tokens = source.max_tokens;
    }
    if let Some(source_opts) = source.options {
        let target_opts = target.options.get_or_insert_with(HashMap::new);
        for (k, v) in source_opts {
            target_opts.insert(k, v);
        }
    }
    if let Some(source_perm) = source.permission {
        if let Some(target_perm) = &mut target.permission {
            for (k, v) in source_perm.rules() {
                target_perm.rules_mut().insert(k, v);
            }
        } else {
            target.permission = Some(source_perm);
        }
    }
    if let Some(source_tools) = source.tools {
        let target_tools = target.tools.get_or_insert_with(HashMap::new);
        for (k, v) in source_tools {
            target_tools.insert(k, v);
        }
    }
}

/// Extract canonical plugin name from a specifier.
/// - For file:// URLs: extracts filename without extension
/// - For npm packages: extracts package name without version
pub fn get_plugin_name(plugin: &str) -> String {
    if let Some(path) = plugin.strip_prefix("file://") {
        // A traversal or percent-escaped spec is not decoded here, so deriving
        // a file_stem from it could collide with a legitimate spec's dedup key.
        let has_dot_component = path.split(['/', '\\']).any(|c| c == "." || c == "..");
        if has_dot_component || path.contains('%') {
            return plugin.to_string();
        }
        return Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| plugin.to_string());
    }
    // For npm packages: strip version after last @
    if let Some(last_at) = plugin.rfind('@') {
        if last_at > 0 {
            return plugin[..last_at].to_string();
        }
    }
    plugin.to_string()
}

/// Deduplicate plugins by name, with later entries (higher priority) winning.
/// Since plugins are added in low-to-high priority order,
/// we reverse, deduplicate (keeping first occurrence), then restore order.
pub fn deduplicate_plugins(plugins: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut unique: Vec<String> = Vec::new();

    for specifier in plugins.iter().rev() {
        let name = get_plugin_name(specifier);
        if seen.insert(name) {
            unique.push(specifier.clone());
        }
    }

    unique.reverse();
    unique
}

/// Parses a boolean-like environment value with fail-closed semantics.
///
/// Only `true`, `1`, and `yes` (case-insensitive, surrounding whitespace
/// ignored) enable a flag. `false`, `0`, `no`, empty strings, and any malformed
/// value leave the flag disabled. This stops an accidentally-set
/// `OPENCODE_DISABLE_*=false` (or `=0`, or an empty value) from silently
/// disabling functionality.
fn parse_env_flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}

/// 有効な非真値（`false` / `0` / `no` / 空）かどうか。これらは警告しない。
fn is_valid_false_env_flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "0" | "no" | ""
    )
}

/// Reads a boolean-like environment variable with fail-closed semantics.
///
/// Unset, non-UTF-8, or non-truthy values all yield `false`. A set but invalid
/// value is warned about by variable name only; the value itself is never
/// logged (it may be a token accidentally pasted into the variable).
fn env_flag_enabled(name: &str) -> bool {
    match env::var(name) {
        Ok(value) if parse_env_flag(&value) => true,
        // 明示的な false / 0 / no / 空は有効値なので警告しない。
        Ok(value) if is_valid_false_env_flag(&value) => false,
        Ok(_) => {
            tracing::warn!(
                env_var = %name,
                "ignoring invalid OPENCODE_DISABLE_* value; flag stays disabled"
            );
            false
        }
        Err(env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                env_var = %name,
                "ignoring non-UTF-8 OPENCODE_DISABLE_* value; flag stays disabled"
            );
            false
        }
        Err(env::VarError::NotPresent) => false,
    }
}

/// Applies the `OPENCODE_PERMISSION` environment override.
///
/// A malformed value is ignored and only the variable name plus the JSON error
/// category are logged. The raw value is never included in the warning, so a
/// secret accidentally placed in the variable cannot leak into logs.
fn apply_permission_env_override(config: &mut Config) {
    let Ok(perm_json) = env::var("OPENCODE_PERMISSION") else {
        return;
    };

    match serde_json::from_str::<PermissionConfig>(&perm_json) {
        Ok(perm) => {
            let target = config
                .permission
                .get_or_insert_with(PermissionConfig::default);
            for (k, v) in perm.rules() {
                target.rules_mut().insert(k, v);
            }
        }
        Err(err) => {
            // Log only the variable name and error category — never the value.
            tracing::warn!(
                env_var = "OPENCODE_PERMISSION",
                error_category = ?err.classify(),
                "ignoring invalid OPENCODE_PERMISSION value"
            );
        }
    }
}

/// Applies all `OPENCODE_*` environment overrides.
///
/// Called before managed config loading so that managed (enterprise) config
/// stays the highest-priority source, as documented on `load_all`.
fn apply_env_overrides(config: &mut Config) {
    apply_permission_env_override(config);

    if env_flag_enabled("OPENCODE_DISABLE_AUTOCOMPACT") {
        config.compaction.get_or_insert_with(Default::default).auto = Some(false);
    }
    if env_flag_enabled("OPENCODE_DISABLE_PRUNE") {
        config.compaction.get_or_insert_with(Default::default).prune = Some(false);
    }
}

/// Resolve legacy `tools` entries into permission actions.
///
/// Several legacy names (`write`, `edit`, `patch`, `multiedit`) collapse onto
/// the single `"edit"` permission key. Iterating the input `HashMap` has no
/// defined order, so a plain "last insert wins" made `{"write": true,
/// "edit": false}` resolve differently between runs. Resolution is now
/// deterministic and fail-safe: **Deny wins**. If any alias for a key disables
/// the tool, the merged action is `Deny` regardless of the visitation order.
/// The enable flag is never logged, only used to choose the action.
fn resolve_legacy_tools_actions(
    tools: impl IntoIterator<Item = (String, bool)>,
) -> HashMap<String, PermissionAction> {
    let mut resolved: HashMap<String, PermissionAction> = HashMap::new();
    for (tool, enabled) in tools {
        let action = if enabled {
            PermissionAction::Allow
        } else {
            PermissionAction::Deny
        };
        // write, edit, patch, multiedit all map to "edit" permission
        let key = if tool == "write" || tool == "edit" || tool == "patch" || tool == "multiedit" {
            "edit".to_string()
        } else {
            tool
        };
        resolved
            .entry(key)
            .and_modify(|current| {
                if action == PermissionAction::Deny {
                    *current = PermissionAction::Deny;
                }
            })
            .or_insert(action);
    }
    resolved
}

/// Apply post-load transforms: legacy migrations, plugin dedup, provider normalization.
///
/// Public so the server can normalize PATCH-merged config the same way the
/// startup loader does. Only key names are logged, never values.
pub fn apply_post_load_transforms(config: &mut Config) {
    // Migrate deprecated `mode` field to `agent` field. The warning names only
    // the keys — never the mode names or any other config value.
    if let Some(mode_configs) = config.mode.take() {
        tracing::warn!("mode is deprecated; use agent instead (value not logged)");
        let agent_configs = config.agent.get_or_insert_with(AgentConfigs::default);
        for (name, mut mode_agent) in mode_configs.entries {
            // A legacy `mode` entry implies Primary, but must not clobber an
            // explicit `agent.<name>.mode`. Apply the legacy default only when
            // the same-named agent has no explicit mode of its own.
            let existing_has_mode = agent_configs
                .entries
                .get(&name)
                .is_some_and(|existing| existing.mode.is_some());
            if existing_has_mode {
                // 既存の明示 mode を守るため、legacy 側の mode は渡さない（R4）。
                // mode 項目自身が mode を持っていても上書きされない。
                mode_agent.mode = None;
            } else {
                mode_agent.mode = Some(AgentMode::Primary);
            }
            if let Some(existing) = agent_configs.entries.get_mut(&name) {
                merge_agent_config(existing, mode_agent);
            } else {
                agent_configs.entries.insert(name, mode_agent);
            }
        }
    }

    // Backwards compatibility: legacy top-level `tools` config -> permission.
    // Log only the key names; tool names and enable flags are never included.
    if let Some(tools) = config.tools.take() {
        tracing::warn!("tools is deprecated; use permission instead (value not logged)");
        let mut perms = HashMap::new();
        // Resolve aliased tool names with a fixed Deny-wins rule so the result
        // cannot depend on HashMap iteration order.
        for (key, action) in resolve_legacy_tools_actions(tools) {
            perms.insert(key, PermissionRule::Action(action));
        }
        // Legacy tools have lower priority than explicit permission config
        if let Some(existing) = &config.permission {
            for (k, v) in existing.rules() {
                perms.insert(k, v);
            }
        }
        config.permission = Some(PermissionConfig::Map(perms));
    }

    // Set default username from system
    if config.username.is_none() {
        config.username = env::var("USER").or_else(|_| env::var("USERNAME")).ok();
    }

    // Handle migration from autoshare to share field. Warn whenever the
    // deprecated key was supplied, even when its value does not trigger the
    // migration; the boolean value itself is never logged.
    if config.autoshare.is_some() {
        tracing::warn!("autoshare is deprecated; use share instead (value not logged)");
    }
    if config.autoshare == Some(true) && config.share.is_none() {
        config.share = Some(crate::schema::ShareMode::Auto);
    }

    // The legacy `experimental.mcp_timeout` was merged but never consumed:
    // MCP timeout resolution only reads `mcp.timeout` and a server's own
    // `timeout`. Fold it into the canonical location so it takes effect, and
    // emit a deprecation warning that never includes the numeric value.
    // Explicit `mcp.timeout` wins; the legacy value is only a fallback and is
    // cleared afterwards so a single source of truth remains.
    if let Some(legacy_timeout) = config
        .experimental
        .as_ref()
        .and_then(|experimental| experimental.mcp_timeout)
    {
        tracing::warn!(
            "experimental.mcp_timeout is deprecated; use mcp.timeout instead (value not logged)"
        );
        let mcp = config.mcp.get_or_insert_with(McpConfig::default);
        if mcp.timeout.is_none() {
            mcp.timeout = Some(McpTimeoutConfig::Millis(legacy_timeout));
        }
        if let Some(experimental) = config.experimental.as_mut() {
            experimental.mcp_timeout = None;
        }
    }

    // Deduplicate plugins, preserving whether the source explicitly set the key.
    config.plugin = std::mem::take(&mut config.plugin).map_values(deduplicate_plugins);

    // `steps` / `max_steps` の 0 は未指定として扱い、executor の既定値を適用する。
    normalize_agent_steps(config);

    // Fold the current-schema `providers` map into the runtime `provider` map.
    normalize_provider_configs(config);
}

/// `steps` / `max_steps` が `0` のエージェント設定を未指定（`None`）へ正規化する。
///
/// executor は `max_steps` が `None` のとき既定値（100 steps）を用いるが、
/// `Some(0)` のまま渡ると実行ループが一回も回らず空応答になる。
/// 正の値は変更しない。警告は正規化の事実のみを示し、設定値やエージェント ID は
/// ログへ出さない。
fn normalize_agent_steps(config: &mut Config) {
    let mut normalized = false;
    for configs in [config.agent.as_mut(), config.mode.as_mut()]
        .into_iter()
        .flatten()
    {
        for agent in configs.entries.values_mut() {
            for steps in [&mut agent.steps, &mut agent.max_steps] {
                if *steps == Some(0) {
                    *steps = None;
                    normalized = true;
                }
            }
        }
    }
    if normalized {
        tracing::warn!(
            "agent steps/max_steps of zero is treated as unspecified; the default step limit applies (value not logged)"
        );
    }
}

/// Folds the current-schema `providers` map into the runtime `provider` map.
///
/// Cross-layer spelling conflicts are resolved earlier by `Config::merge`,
/// where the higher-precedence layer wins regardless of which spelling it
/// used. What reaches this function is a leftover from a single source layer
/// that defined both spellings for one id; for that case the legacy `provider`
/// map keeps its historical priority. Connection settings that the current
/// schema nests under `settings` are flattened into the fields consumed by the
/// provider bootstrap (`npm`, `base_url`, `api_key`, `options`).
fn normalize_provider_configs(config: &mut Config) {
    let Some(current_schema_providers) = config.providers.take() else {
        return;
    };
    let providers = config.provider.get_or_insert_with(HashMap::new);
    for (id, provider) in current_schema_providers {
        if providers.contains_key(&id) {
            continue;
        }
        providers.insert(id, normalize_provider_config(provider));
    }
}

fn normalize_provider_config(mut provider: ProviderConfig) -> ProviderConfig {
    if provider.npm.is_none() {
        provider.npm = provider.package.take();
    } else {
        provider.package = None;
    }

    if let Some(settings) = provider.settings.take() {
        for (key, value) in settings {
            let target = match key.as_str() {
                "baseURL" | "baseUrl" | "base_url" | "api" => &mut provider.base_url,
                "apiKey" | "api_key" => &mut provider.api_key,
                _ => {
                    provider
                        .options
                        .get_or_insert_with(HashMap::new)
                        .insert(key, value);
                    continue;
                }
            };
            if target.is_none() {
                *target = value.as_str().map(ToString::to_string);
            }
        }
    }

    for (key, value) in [
        ("headers", provider.headers.take()),
        ("compatibility", provider.compatibility.take()),
    ] {
        if let Some(values) = value {
            provider.options.get_or_insert_with(HashMap::new).insert(
                key.to_string(),
                serde_json::Value::Object(values.into_iter().collect()),
            );
        }
    }

    provider
}

/// Loads config synchronously (without remote wellknown fetching).
pub fn load_config<P: AsRef<Path>>(project_dir: P) -> Result<Config> {
    let mut loader = ConfigLoader::new();
    loader.load_all(project_dir)
}

/// Loads config including remote `.well-known/opencode` endpoints.
/// Use this in async contexts where you want the full config with remote sources.
pub async fn load_config_with_remote<P: AsRef<Path>>(project_dir: P) -> Result<Config> {
    let mut loader = ConfigLoader::new();
    loader.load_all_with_remote(project_dir).await
}

/// Select the project-level config file that an update must write back to.
///
/// This mirrors the precedence of [`ConfigLoader::load_project`], which loads
/// `opencode.jsonc` before `opencode.json` so that plain JSON wins when both
/// files are present. The write target must be that same effective winner,
/// otherwise a patch written to the shadowed JSONC file would be overwritten
/// again by `opencode.json` on the next load. Concretely:
///
/// * both `opencode.jsonc` and `opencode.json` exist -> `opencode.json`;
/// * only `opencode.jsonc` exists -> `opencode.jsonc` (never create a shadowing
///   `opencode.json` in a JSONC-only project);
/// * neither exists -> `opencode.json` (historical behaviour).
///
/// # JSONC write-back trade-off
///
/// When `opencode.jsonc` is the selected target the config is re-serialized
/// with `serde_json::to_string_pretty`. Comments and trailing commas from the
/// original file are **not** preserved and are lost after the write. This is
/// an accepted trade-off that keeps a single, safe serialization path for all
/// targets; callers must be aware that a JSONC write-back rewrites the file as
/// plain JSON syntax.
fn project_config_write_path(project_dir: &Path) -> PathBuf {
    let json = project_dir.join("opencode.json");
    let jsonc = project_dir.join("opencode.jsonc");

    // `load_project` reads jsonc first, then json, so json is the last writer
    // and wins. When both exist we must write to json for the patch to survive
    // a reload; jsonc is only chosen when it is the sole existing file.
    if json.exists() {
        json
    } else if jsonc.exists() {
        jsonc
    } else {
        json
    }
}

/// Persist serialized config with an atomic replace.
///
/// On Unix the bytes go to a fresh temporary file created with mode `0600`,
/// are `fsync`ed, and are then `rename`d over the destination. The umask cannot
/// widen the permissions, an interrupted write can never truncate the
/// destination, and errors carry only an I/O error category -- never a value
/// or a path. The rename also tightens an existing file's permissions to 0600.
#[cfg(unix)]
fn write_config_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".{file_name}.tmp-{}-{nanos}-{seq}", std::process::id());
    let temp_path = path.with_file_name(temp_name);

    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)?;
        // `mode(0o600)` is still subject to the umask; pin the exact mode so
        // a restrictive umask cannot make the file unreadable on the next run.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result.map_err(|error| anyhow::anyhow!("failed to persist config ({:?})", error.kind()))
}

/// Windows keeps the historical direct write: `rename` cannot atomically
/// replace an existing file there, so a temp-file dance would be less safe.
#[cfg(not(unix))]
fn write_config_file(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents)
        .map_err(|error| anyhow::anyhow!("failed to persist config ({:?})", error.kind()))
}

/// Parse the on-disk config immediately before a write-back update.
///
/// An unparseable existing file must abort the update: falling back to
/// `Config::default()` and re-serializing would silently erase the user's
/// `api_key`, `permission` and other settings. The returned error is generic
/// on purpose -- it carries neither the file path nor the raw contents (which
/// may contain secrets), only the fact that parsing failed. A warning is
/// emitted at the same time so operators can see why the update was refused.
fn parse_existing_config_for_update(content: &str) -> Result<Config> {
    parse_jsonc(content).map_err(|_| {
        tracing::warn!("existing config could not be parsed; refusing to overwrite it");
        anyhow::anyhow!("refusing to overwrite existing config: current content failed to parse")
    })
}

/// プロセス共有の設定書き込み直列化ロック。
///
/// `read_to_string → merge → apply_post_load_transforms → write_config_file`
/// は不可分でなければならない。2 並行の PATCH が同じ旧ディスク内容を読むと、
/// 後から書いた側が先のパッチを上書きして消してしまう（再起動後も復元しない）。
/// `update_config` / `update_global_config` は同期 fn なので、auth.rs の
/// tokio::Mutex パターンを `std::sync::Mutex` に置き換えて採用する。
static CONFIG_WRITE_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();

/// 書き込み直列化ロックを取得する（初回呼び出しで初期化）。
fn config_write_lock() -> &'static std::sync::Mutex<()> {
    CONFIG_WRITE_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Update project-level config by merging a patch.
///
/// Writes to the file chosen by [`project_config_write_path`]. If that file is
/// `opencode.jsonc`, re-serialization drops its comments and trailing commas
/// (see the note on [`project_config_write_path`]). An existing file that fails
/// to parse aborts the update rather than being replaced with a default
/// config, so secrets and permission rules are never silently discarded.
pub fn update_config(project_dir: &Path, patch: &Config) -> Result<()> {
    // read-modify-write 全体を直列化する。poisoned でもパニックせずガードを回収する。
    let _guard = config_write_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let config_path = project_config_write_path(project_dir);

    let existing = if config_path.exists() {
        let content = fs::read_to_string(&config_path).map_err(|error| {
            anyhow::anyhow!("failed to read current config ({:?})", error.kind())
        })?;
        parse_existing_config_for_update(&content)?
    } else {
        Config::default()
    };

    let mut merged = existing;
    merged.merge(patch.clone());
    // PATCH 経由のマージ結果も起動時ロードと同じ正規化を適用する。
    apply_post_load_transforms(&mut merged);

    let json =
        serde_json::to_string_pretty(&merged).with_context(|| "Failed to serialize config")?;
    write_config_file(&config_path, json.as_bytes())?;

    Ok(())
}

/// Update global config by merging a patch.
///
/// The target file follows the precedence of [`ConfigLoader::load_global`]:
/// both `opencode.jsonc` and `opencode.json` are read, in that order, so plain
/// JSON wins when both exist. When they coexist the patch must land in JSON,
/// otherwise it would be shadowed on the next load. If a JSONC file is the
/// sole target, re-serialization drops its comments and trailing commas, just
/// as for [`project_config_write_path`]. An existing file that fails to parse
/// aborts the update instead of being replaced with a default config,
/// preserving the user's secrets and permission rules.
pub fn update_global_config(patch: &Config) -> Result<()> {
    // read-modify-write 全体を直列化する。poisoned でもパニックせずガードを回収する。
    let _guard = config_write_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let global_path = get_global_config_path();

    // `load_global_from` reads jsonc first and json second, so json is the
    // last writer and wins when both exist. Mirror that precedence here.
    let config_path = if global_path.with_extension("json").exists() {
        global_path.with_extension("json")
    } else if global_path.with_extension("jsonc").exists() {
        global_path.with_extension("jsonc")
    } else {
        global_path.with_extension("json")
    };

    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = if config_path.exists() {
        let content = fs::read_to_string(&config_path).map_err(|error| {
            anyhow::anyhow!("failed to read current config ({:?})", error.kind())
        })?;
        parse_existing_config_for_update(&content)?
    } else {
        Config::default()
    };

    let mut merged = existing;
    merged.merge(patch.clone());
    // PATCH 経由のマージ結果も起動時ロードと同じ正規化を適用する。
    apply_post_load_transforms(&mut merged);

    let json =
        serde_json::to_string_pretty(&merged).with_context(|| "Failed to serialize config")?;
    write_config_file(&config_path, json.as_bytes())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(prefix: &str) -> Self {
            let unique = format!(
                "{}_{}_{}",
                prefix,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock error")
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("failed to create test temp dir");
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_parse_jsonc_simple() {
        let content = r#"{"model": "claude-3-opus"}"#;
        let config: Config = parse_jsonc(content).unwrap();
        assert_eq!(config.model, Some("claude-3-opus".to_string()));
    }

    #[test]
    fn unknown_config_keys_detects_m08_sections_without_values() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "model": "known-key",
                "security": {"strictPermissionChecks": true},
                "storage": {"gcBufferMb": 4096},
                "references": {"gemini": {"secret-looking": "DO-NOT-LOG-ME"}}
            }"#,
        )
        .unwrap();
        let unknown = unknown_config_keys(&value);
        assert_eq!(unknown, vec!["references", "security", "storage"]);
        // No config value must ever be included in the warning payload.
        assert!(unknown.iter().all(|key| !key.contains("DO-NOT-LOG-ME")));
    }

    #[test]
    fn unknown_config_keys_accepts_renames_and_aliases() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "$schema": "https://opencode.ai/config.json",
                "logLevel": "INFO",
                "log_level": "DEBUG",
                "providers": {}
            }"#,
        )
        .unwrap();
        assert!(unknown_config_keys(&value).is_empty());
    }

    /// 回帰: `server.prots` のような入れ子誤記は従来サイレントに捨てられていた。
    /// キーパスだけが警告され、値（トークン風の文字列）は一切出ないこと。
    #[test]
    fn nested_unknown_keys_warn_by_path_without_values() {
        let content = r#"{
            "server": {"port": 4096, "prots": 4096},
            "provider": {"my-provider-id": {"name": "ok", "api_ky": "TOKEN_SENTINEL_NEVER_LOG"}},
            "agent": {"reviewer": {"model": "x", "promt": "PROMPT_SENTINEL_NEVER_LOG"}},
            "mcp": {"servers": {"local": {"command": ["x"], "environmnt": {"SECRET": "VALUE_SENTINEL_NEVER_LOG"}}}}
        }"#;

        let buffer = WarningBuffer::default();
        let subscriber = WarningSubscriber {
            buffer: buffer.clone(),
        };
        tracing::subscriber::with_default(subscriber, || {
            let _config = parse_jsonc(content).expect("unknown nested keys must not block startup");
        });

        let text = buffer.text();
        for expected in [
            "server.prots",
            "provider.*.api_ky",
            "agent.*.promt",
            "mcp.servers.*.environmnt",
        ] {
            assert!(text.contains(expected), "missing nested warning: {text}");
        }
        for forbidden in [
            "my-provider-id",
            "reviewer",
            "local",
            "TOKEN_SENTINEL_NEVER_LOG",
            "PROMPT_SENTINEL_NEVER_LOG",
            "VALUE_SENTINEL_NEVER_LOG",
        ] {
            assert!(
                !text.contains(forbidden),
                "user name or value leaked: {text}"
            );
        }
    }

    /// 正しいキーとユーザー定義名（agent 名・provider id・mcp server id）は
    /// 誤記として警告されない。深さ上限も明示的に確認する。
    #[test]
    fn nested_unknown_keys_ignore_valid_keys_user_names_and_overflow() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "server": {"port": 4096, "hostname": "localhost"},
                "provider": {"my-provider-id": {"name": "ok", "api_key": "not-logged"}},
                "agent": {"reviewer": {"model": "x", "mode": "primary"}},
                "mcp": {"servers": {"local": {"command": ["x"], "enabled": true}}}
            }"#,
        )
        .unwrap();
        assert!(
            unknown_nested_config_keys(&value).is_empty(),
            "valid keys and user-defined names must not warn"
        );

        let mut out = Vec::new();
        collect_unknown_nested_keys(
            &serde_json::json!({"prots": 4096}),
            NestedValueSchema::Struct("server"),
            MAX_NESTED_KEY_DEPTH + 1,
            "server",
            &mut out,
        );
        assert!(out.is_empty(), "depth-limit subtree must not be inspected");
    }

    #[test]
    fn test_parse_jsonc_with_comments() {
        let content = r#"{
            // This is a comment
            "model": "claude-3-opus",
            /* Multi-line
                comment */
            "theme": "dark"
        }"#;
        let config: Config = parse_jsonc(content).unwrap();
        assert_eq!(config.model, Some("claude-3-opus".to_string()));
        assert_eq!(config.theme, Some("dark".to_string()));
    }

    #[test]
    fn test_parse_jsonc_allows_trailing_comma_in_object() {
        let content = r#"{
            "model": "claude-3-opus",
            "theme": "dark",
        }"#;
        let config: Config = parse_jsonc(content).unwrap();
        assert_eq!(config.model, Some("claude-3-opus".to_string()));
        assert_eq!(config.theme, Some("dark".to_string()));
    }

    #[test]
    fn test_parse_jsonc_allows_trailing_comma_in_array() {
        let content = r#"{
            "instructions": ["a.md", "b.md",],
            "plugin": ["p1", "p2",],
        }"#;
        let config: Config = parse_jsonc(content).unwrap();
        assert_eq!(
            config.instructions,
            vec!["a.md".to_string(), "b.md".to_string()]
        );
        assert_eq!(config.plugin, vec!["p1".to_string(), "p2".to_string()]);
    }

    /// Regression: a type mismatch must surface the failing field path (keys
    /// only) and never the invalid value or the config file path.
    #[test]
    fn type_error_reports_field_path_without_value_or_file_path() {
        const INVALID_VALUE: &str = "DO-NOT-LOG-THIS-INVALID-TOKEN-VALUE";
        let temp = TestDir::new("opencode_config_type_error_redaction");
        let config_path = temp.path.join("opencode.json");
        fs::write(
            &config_path,
            format!(r#"{{"compaction": {{"auto": "{INVALID_VALUE}"}}}}"#),
        )
        .unwrap();

        let mut loader = ConfigLoader::new().without_global_config();
        let error = loader
            .load_from_file(&config_path)
            .expect_err("a wrong field type must fail to parse")
            .to_string();

        assert!(
            error.contains("compaction.auto"),
            "field path missing from error: {error}"
        );
        assert!(
            !error.contains(INVALID_VALUE),
            "invalid value leaked into error: {error}"
        );
        let full_path = temp.path.to_string_lossy().into_owned();
        assert!(!error.contains(&full_path), "config path leaked: {error}");
        assert!(
            !error.contains("opencode.json"),
            "config file name leaked: {error}"
        );
    }

    /// Field paths are restricted to a safe alphabet and length so a crafted
    /// key cannot inject newlines or unbounded text into an error message.
    #[test]
    fn field_path_extraction_rejects_unsafe_key_names() {
        assert!(is_safe_field_segment("server"));
        assert!(is_safe_field_segment("port"));
        assert!(is_safe_field_segment("some_key-1"));
        assert!(is_safe_field_segment("dot.ted"));
        assert!(!is_safe_field_segment(""));
        assert!(!is_safe_field_segment("has space"));
        assert!(!is_safe_field_segment("has\nnewline"));
        assert!(!is_safe_field_segment("has/slash"));
        assert!(!is_safe_field_segment(&"a".repeat(65)));

        assert!(is_safe_field_path("server.port"));
        assert!(!is_safe_field_path("server.po\nrt"));
        assert!(!is_safe_field_path(&"a".repeat(65)));
    }

    /// The generic fallback and the top-level path both stay value-free.
    #[test]
    fn type_error_never_includes_invalid_value() {
        let error = parse_jsonc(r#"{"theme": 12345}"#)
            .expect_err("wrong type must fail")
            .to_string();
        assert!(error.contains("theme"), "key should be reported: {error}");
        assert!(!error.contains("12345"), "invalid value leaked: {error}");
    }

    #[test]
    fn test_parse_jsonc_preserves_comment_markers_inside_strings() {
        let content = r#"{
            "provider": {
                "openai": {
                    "base_url": "https://example.com/path//not-comment",
                    "api_key": "abc/*not-comment*/def"
                }
            }
        }"#;
        let config: Config = parse_jsonc(content).unwrap();
        let provider = config.provider.unwrap();
        let openai = provider.get("openai").unwrap();
        assert_eq!(
            openai.base_url.as_deref(),
            Some("https://example.com/path//not-comment")
        );
        assert_eq!(openai.api_key.as_deref(), Some("abc/*not-comment*/def"));
    }

    #[test]
    fn test_config_merge() {
        let mut config1 = Config {
            model: Some("model1".to_string()),
            instructions: vec!["inst1".to_string()].into(),
            ..Default::default()
        };

        let config2 = Config {
            model: Some("model2".to_string()),
            instructions: vec!["inst2".to_string()].into(),
            ..Default::default()
        };

        config1.merge(config2);

        assert_eq!(config1.model, Some("model2".to_string()));
        assert_eq!(
            config1.instructions,
            vec!["inst1".to_string(), "inst2".to_string()]
        );
    }

    #[test]
    fn test_load_project_finds_and_merges_parent_configs() {
        let temp = TestDir::new("opencode_config_findup");
        let root = temp.path.join("repo");
        let child = root.join("apps/web");
        fs::create_dir_all(&child).unwrap();

        fs::write(
            root.join("opencode.jsonc"),
            r#"{ "model": "parent-model" }"#,
        )
        .unwrap();
        fs::write(
            root.join("apps/opencode.jsonc"),
            r#"{ "theme": "dark", "instructions": ["parent.md"] }"#,
        )
        .unwrap();
        fs::write(
            child.join("opencode.jsonc"),
            r#"{ "instructions": ["child.md"] }"#,
        )
        .unwrap();

        let mut loader = ConfigLoader::new();
        loader.load_project(&child).unwrap();
        let cfg = loader.config();

        assert_eq!(cfg.model.as_deref(), Some("parent-model"));
        assert_eq!(cfg.theme.as_deref(), Some("dark"));
        assert_eq!(
            cfg.instructions,
            vec!["parent.md".to_string(), "child.md".to_string()]
        );
    }

    #[test]
    fn test_load_project_stops_at_git_root() {
        let temp = TestDir::new("opencode_config_gitroot");
        let outer = temp.path.join("outer");
        let repo = outer.join("repo");
        let child = repo.join("sub");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();

        fs::write(
            outer.join("opencode.jsonc"),
            r#"{ "model": "outer-model" }"#,
        )
        .unwrap();
        fs::write(repo.join("opencode.jsonc"), r#"{ "model": "repo-model" }"#).unwrap();
        fs::write(
            child.join("opencode.jsonc"),
            r#"{ "theme": "child-theme" }"#,
        )
        .unwrap();

        let mut loader = ConfigLoader::new();
        loader.load_project(&child).unwrap();
        let cfg = loader.config();

        assert_eq!(cfg.model.as_deref(), Some("repo-model"));
        assert_eq!(cfg.theme.as_deref(), Some("child-theme"));
    }

    #[test]
    fn test_load_project_finds_up_dot_opencode_configs() {
        let temp = TestDir::new("opencode_config_dotdir");
        let root = temp.path.join("repo");
        let child = root.join("service");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".opencode")).unwrap();
        fs::create_dir_all(child.join(".opencode")).unwrap();

        fs::write(
            root.join(".opencode/opencode.jsonc"),
            r#"{ "default_agent": "build", "instructions": ["root.md"] }"#,
        )
        .unwrap();
        fs::write(
            child.join(".opencode/opencode.jsonc"),
            r#"{ "default_agent": "reviewer", "instructions": ["child.md"] }"#,
        )
        .unwrap();

        let mut loader = ConfigLoader::new();
        loader.load_project(&child).unwrap();
        let cfg = loader.config();

        assert_eq!(cfg.default_agent.as_deref(), Some("reviewer"));
        assert_eq!(
            cfg.instructions,
            vec!["root.md".to_string(), "child.md".to_string()]
        );
    }

    /// Regression: `.opencode/opencode.json{,c}` must be loaded exactly once by
    /// `load_all`. `load_project` is the sole reader of those files; the
    /// `.opencode` directory scan must not load them again, otherwise
    /// `config_paths` gets duplicate entries and warnings (including undefined
    /// `{env:}` warnings) fire twice. No token value or path appears in the
    /// assertion messages.
    #[test]
    fn load_all_records_dot_opencode_config_path_only_once() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = TestDir::new("opencode_config_no_dup_paths");
        let root = temp.path.join("repo");
        let dot = root.join(".opencode");
        fs::create_dir_all(&dot).unwrap();
        // Stop find-up here so the result cannot depend on an ancestor config.
        fs::create_dir_all(root.join(".git")).unwrap();

        let config_path = dot.join("opencode.jsonc");
        fs::write(&config_path, r#"{ "model": "dot-model", "theme": "dark" }"#).unwrap();

        let mut loader = ConfigLoader::new().without_global_config();
        loader.load_all(&root).unwrap();

        let hits = loader
            .config_paths()
            .iter()
            .filter(|path| path.as_path() == config_path.as_path())
            .count();
        assert_eq!(hits, 1, "the .opencode config must be loaded exactly once");

        let mut seen = std::collections::HashSet::new();
        for path in loader.config_paths() {
            assert!(
                seen.insert(path),
                "config_paths must not contain duplicate entries"
            );
        }

        // The single load still participates in the merged config as before.
        assert_eq!(loader.config().model.as_deref(), Some("dot-model"));
        assert_eq!(loader.config().theme.as_deref(), Some("dark"));
    }

    #[test]
    fn test_substitute_env_vars() {
        std::env::set_var("OPENCODE_TEST_VAR", "test_value");
        let input = r#"{"api_key": "{env:OPENCODE_TEST_VAR}"}"#;
        let result = substitute_env_vars(input, None);
        assert_eq!(result, r#"{"api_key": "test_value"}"#);
        std::env::remove_var("OPENCODE_TEST_VAR");
    }

    #[test]
    fn test_substitute_env_vars_missing() {
        let input = r#"{"api_key": "{env:NONEXISTENT_VAR_12345}"}"#;
        let result = substitute_env_vars(input, None);
        assert_eq!(result, r#"{"api_key": ""}"#);
    }

    // ERRPATH-01: env vars are process-global, so every test that mutates the
    // strict switch is serialized and restores the original value on drop.
    struct StrictEnvRestore(Option<std::ffi::OsString>);

    impl StrictEnvRestore {
        fn set(value: &str) -> Self {
            let original = env::var_os("OPENCODE_CONFIG_STRICT_ENV");
            env::set_var("OPENCODE_CONFIG_STRICT_ENV", value);
            Self(original)
        }

        fn unset() -> Self {
            let original = env::var_os("OPENCODE_CONFIG_STRICT_ENV");
            env::remove_var("OPENCODE_CONFIG_STRICT_ENV");
            Self(original)
        }
    }

    impl Drop for StrictEnvRestore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => env::set_var("OPENCODE_CONFIG_STRICT_ENV", value),
                None => env::remove_var("OPENCODE_CONFIG_STRICT_ENV"),
            }
        }
    }

    #[test]
    fn strict_env_flag_accepts_only_explicit_truthy_values() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = StrictEnvRestore::unset();
        for value in ["1", "true", "TRUE", "yes", "YeS"] {
            env::set_var("OPENCODE_CONFIG_STRICT_ENV", value);
            assert!(strict_env_mode_enabled(), "{value} must enable strict mode");
        }
        for value in ["0", "false", "no", "", "enabled", " true "] {
            env::set_var("OPENCODE_CONFIG_STRICT_ENV", value);
            assert!(!strict_env_mode_enabled(), "{value} must stay non-strict");
        }
    }

    #[test]
    fn strict_env_mode_rejects_undefined_reference_on_load() {
        const VAR: &str = "OPENCODE_STRICT_MISSING_AUTH_KEY";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = StrictEnvRestore::set("TRUE");

        let mut loader = ConfigLoader::new().without_global_config();
        let input = format!(r#"{{"model": "{{env:{VAR}}}"}}"#);
        let error = loader
            .load_from_str(&input)
            .expect_err("strict mode must reject an undefined reference");
        let message = error.to_string();
        assert_eq!(message, STRICT_ENV_UNDEFINED_ERROR);
        assert!(!message.contains(VAR), "variable name leaked into error");
        assert!(
            !message.contains("model"),
            "config content leaked into error"
        );
        assert!(!message.contains('/'), "path-like data leaked into error");
    }

    #[test]
    fn strict_env_mode_rejects_undefined_reference_from_file() {
        const VAR: &str = "OPENCODE_STRICT_MISSING_FILE_KEY";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = StrictEnvRestore::set("yes");
        let temp = TestDir::new("opencode_strict_env");
        let file = temp.path.join("opencode.jsonc");
        fs::write(&file, format!(r#"{{"model": "{{env:{VAR}}}"}}"#)).unwrap();

        let mut loader = ConfigLoader::new().without_global_config();
        let error = loader
            .load_from_file(&file)
            .expect_err("strict mode must reject an undefined file reference");
        let message = error.to_string();
        assert_eq!(message, STRICT_ENV_UNDEFINED_ERROR);
        assert!(!message.contains(VAR), "variable name leaked into error");
        assert!(
            !message.contains(&temp.path.to_string_lossy().into_owned()),
            "config path leaked into error"
        );
    }

    #[test]
    fn strict_env_mode_rejects_undefined_reference_inside_config_env() {
        let mut config_env = HashMap::new();
        config_env.insert(
            "OPENCODE_STRICT_CHAIN_ROOT".to_string(),
            "{env:OPENCODE_STRICT_MISSING_CHAIN}".to_string(),
        );
        let error = substitute_env_vars_inner(
            r#"{"model": "{env:OPENCODE_STRICT_CHAIN_ROOT}"}"#,
            Some(&config_env),
            true,
        )
        .expect_err("strict mode must reject an undefined chained reference");
        assert_eq!(error.to_string(), STRICT_ENV_UNDEFINED_ERROR);
        assert!(!error.to_string().contains("OPENCODE_STRICT_MISSING_CHAIN"));
    }

    #[test]
    fn default_env_mode_keeps_empty_expansion_and_startup_continuation() {
        const VAR: &str = "OPENCODE_DEFAULT_MISSING_AUTH_KEY";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = StrictEnvRestore::unset();
        assert!(!strict_env_mode_enabled());

        let mut loader = ConfigLoader::new().without_global_config();
        let input = format!(r#"{{"model": "{{env:{VAR}}}"}}"#);
        loader
            .load_from_str(&input)
            .expect("default mode must warn, expand empty and continue");
        assert_eq!(loader.config().model.as_deref(), Some(""));
        assert_eq!(substitute_env_vars(&input, None), r#"{"model": ""}"#);
    }

    /// R4: chained `{env:VAR}` references inside `config.env` must be resolved
    /// before they are substituted into the config text.
    #[test]
    fn substitute_env_vars_resolves_chained_config_env_values() {
        let mut env = HashMap::new();
        env.insert("OPENCODE_CHAIN_ROOT".to_string(), "root-value".to_string());
        env.insert(
            "OPENCODE_CHAIN_MID".to_string(),
            "{env:OPENCODE_CHAIN_ROOT}/mid".to_string(),
        );
        env.insert(
            "OPENCODE_CHAIN_LEAF".to_string(),
            "pre-{env:OPENCODE_CHAIN_MID}".to_string(),
        );

        let input = r#"{"model": "{env:OPENCODE_CHAIN_LEAF}"}"#;
        let result = substitute_env_vars(input, Some(&env));
        assert_eq!(result, r#"{"model": "pre-root-value/mid"}"#);
    }

    /// R4: a cycle between `config.env` values must be cut off, not loop until
    /// the pass cap or produce unbounded output. The result stays valid JSON.
    #[test]
    fn substitute_env_vars_cuts_off_cyclic_config_env() {
        let mut env = HashMap::new();
        env.insert(
            "OPENCODE_CYCLE_A".to_string(),
            "{env:OPENCODE_CYCLE_B}".to_string(),
        );
        env.insert(
            "OPENCODE_CYCLE_B".to_string(),
            "{env:OPENCODE_CYCLE_A}".to_string(),
        );

        let result = substitute_env_vars(r#"{"model": "{env:OPENCODE_CYCLE_A}"}"#, Some(&env));
        assert!(
            result.len() < 512,
            "cyclic env expansion must be cut off, got {result}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("cyclic expansion must remain valid JSON");
        assert!(parsed["model"].as_str().is_some());
    }

    #[test]
    fn substitute_env_vars_token_escapes_quote_backslash_and_controls() {
        const VAR: &str = "OPENCODE_H17_TOKEN_SPECIAL";
        let value = "a\"b\\c\nd\re\tf";
        std::env::set_var(VAR, value);
        let result = substitute_env_vars(r#"{"api_key": {env:OPENCODE_H17_TOKEN_SPECIAL}}"#, None);
        std::env::remove_var(VAR);

        // A bare token must become a complete JSON string literal.
        let expected = serde_json::to_string(value).unwrap();
        assert_eq!(result, format!(r#"{{"api_key": {expected}}}"#));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["api_key"].as_str(), Some(value));
    }

    #[test]
    fn substitute_env_vars_escapes_backspace_and_formfeed() {
        // An OS environment variable cannot hold NUL (set_var would panic), so
        // the env path is verified with \b and \f. NUL is covered through the
        // shared json_string_body helper used by this expansion.
        const VAR: &str = "OPENCODE_H17_CTRL_CHARS";
        let value = "a\u{0008}b\u{000c}d";
        std::env::set_var(VAR, value);

        let token = substitute_env_vars(r#"{"api_key": {env:OPENCODE_H17_CTRL_CHARS}}"#, None);
        let literal = substitute_env_vars(r#"{"api_key": "{env:OPENCODE_H17_CTRL_CHARS}"}"#, None);
        std::env::remove_var(VAR);

        for result in [&token, &literal] {
            let parsed: serde_json::Value = serde_json::from_str(result)
                .unwrap_or_else(|err| panic!("control chars must stay JSON-safe: {err}"));
            assert_eq!(parsed["api_key"].as_str(), Some(value));
        }
    }

    #[test]
    fn json_string_body_escapes_nul_and_other_control_characters() {
        let value = "a\u{0008}b\u{000c}c\u{0000}d";
        let body = json_string_body(value);
        let parsed: serde_json::Value = serde_json::from_str(&format!("\"{body}\""))
            .unwrap_or_else(|err| panic!("control chars must stay JSON-safe: {err}"));
        assert_eq!(parsed.as_str(), Some(value));
    }

    #[test]
    fn substitute_env_vars_string_literal_escapes_special_characters() {
        const VAR: &str = "OPENCODE_H17_LITERAL_SPECIAL";
        let value = "a\"b\\c\nd\re\tf";
        std::env::set_var(VAR, value);
        let result =
            substitute_env_vars(r#"{"api_key": "{env:OPENCODE_H17_LITERAL_SPECIAL}"}"#, None);
        std::env::remove_var(VAR);

        // The surrounding quotes are already present, so only escaped content
        // is inserted; the final text still parses to the original value.
        let expected = serde_json::to_string(value).unwrap();
        assert_eq!(result, format!(r#"{{"api_key": {expected}}}"#));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["api_key"].as_str(), Some(value));
    }

    #[test]
    fn substitute_env_vars_mid_string_placeholder_stays_a_string() {
        const VAR: &str = "OPENCODE_H17_MID_STRING";
        let value = "a\"b\\c\nd";
        std::env::set_var(VAR, value);
        let result = substitute_env_vars(
            r#"{"api_key": "pre-{env:OPENCODE_H17_MID_STRING}-post"}"#,
            None,
        );
        std::env::remove_var(VAR);

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let expected = format!("pre-{value}-post");
        assert_eq!(parsed["api_key"].as_str(), Some(expected.as_str()));
    }

    #[test]
    fn substitute_env_vars_cannot_inject_config_keys() {
        const VAR: &str = "OPENCODE_H17_INJECTION";
        // Classic injection payload: close the string, then add another key.
        let value = "\" , \"theme\": \"injected";
        std::env::set_var(VAR, value);
        let token = substitute_env_vars(r#"{"api_key": {env:OPENCODE_H17_INJECTION}}"#, None);
        let literal = substitute_env_vars(r#"{"api_key": "{env:OPENCODE_H17_INJECTION}"}"#, None);
        std::env::remove_var(VAR);

        for result in [&token, &literal] {
            let parsed: serde_json::Value = serde_json::from_str(result).unwrap();
            let object = parsed.as_object().expect("result must stay an object");
            assert_eq!(object.len(), 1, "injected a key into {result}");
            assert!(
                object.get("theme").is_none(),
                "injected theme into {result}"
            );
            assert_eq!(object.get("api_key").and_then(|v| v.as_str()), Some(value));
        }
    }

    #[test]
    fn inside_json_string_literal_ignores_quotes_in_comments() {
        let line = "// a \"quote\" {env:FIRST}\n{\"key\": \"pre-{env:SECOND}-post\"}";
        let first = line.find("{env:FIRST}").unwrap();
        let second = line.find("{env:SECOND}").unwrap();
        assert!(!inside_json_string_literal(line, first));
        assert!(inside_json_string_literal(line, second));

        let block = "/* \"quote\" {env:FIRST} */ {\"key\": \"{env:SECOND}\"}";
        let first = block.find("{env:FIRST}").unwrap();
        let second = block.find("{env:SECOND}").unwrap();
        assert!(!inside_json_string_literal(block, first));
        assert!(inside_json_string_literal(block, second));
    }

    #[test]
    fn load_from_file_parses_env_value_with_quotes_newlines_and_backslashes() {
        const VAR: &str = "OPENCODE_H17_E2E";
        let temp = TestDir::new("opencode_env_e2e");
        let value = "line1\n\"quoted\"\\path";
        fs::write(
            temp.path.join("opencode.jsonc"),
            r#"{"model": {env:OPENCODE_H17_E2E}}"#,
        )
        .unwrap();

        std::env::set_var(VAR, value);
        let mut loader = ConfigLoader::new().without_global_config();
        let loaded = loader.load_from_file(temp.path.join("opencode.jsonc"));
        std::env::remove_var(VAR);

        loaded.expect("config with special env value must parse");
        assert_eq!(loader.config().model.as_deref(), Some(value));
    }

    #[test]
    fn test_resolve_file_references() {
        let temp = TestDir::new("opencode_file_ref");
        let secret_path = temp.path.join("secret.txt");
        fs::write(&secret_path, "my-secret-key").unwrap();

        let input = r#"{"api_key": "{file:secret.txt}"}"#.to_string();
        let result = resolve_file_references(&input, &temp.path).unwrap();
        assert_eq!(result, r#"{"api_key": "my-secret-key"}"#);
    }

    #[test]
    fn test_resolve_file_references_skips_comments() {
        let temp = TestDir::new("opencode_file_ref_comment");
        let input = r#"{
            // "api_key": "{file:secret.txt}"
            "model": "claude"
        }"#;
        let result = resolve_file_references(input, &temp.path).unwrap();
        assert!(result.contains("{file:secret.txt}"));
    }

    #[test]
    fn test_resolve_file_references_absolute_path() {
        let temp = TestDir::new("opencode_file_ref_abs");
        let secret_path = temp.path.join("abs_secret.txt");
        fs::write(&secret_path, "absolute-secret").unwrap();

        let input = format!(r#"{{"api_key": "{{file:{}}}"}}"#, secret_path.display());
        let result = resolve_file_references(&input, &temp.path).unwrap();
        assert!(result.contains("absolute-secret"));
    }

    #[test]
    fn resolve_file_references_skips_block_comments() {
        let temp = TestDir::new("opencode_file_ref_block_comment");
        fs::write(temp.path.join("secret.txt"), "block-comment-secret").unwrap();

        let input = r#"{
            /* "api_key": "{file:secret.txt}" */
            "model": "claude"
        }"#;
        let result = resolve_file_references(input, &temp.path).unwrap();

        assert!(
            result.contains("{file:secret.txt}"),
            "a reference inside a block comment must stay verbatim"
        );
        assert!(
            !result.contains("block-comment-secret"),
            "a reference inside a block comment must not be expanded"
        );
    }

    #[test]
    fn resolve_file_references_expands_setting_despite_identical_comment() {
        let temp = TestDir::new("opencode_file_ref_duplicate");
        fs::write(temp.path.join("shared.txt"), "shared-secret").unwrap();

        // The same reference appears first in a comment and then in a real
        // setting; the first occurrence must not decide the fate of the rest.
        let input = r#"{
            // "api_key": "{file:shared.txt}"
            "instructions": ["{file:shared.txt}"]
        }"#;
        let result = resolve_file_references(input, &temp.path).unwrap();

        assert!(
            result.contains(r#""instructions": ["shared-secret"]"#),
            "the real setting must be expanded"
        );
        assert!(
            result.contains(r#"// "api_key": "{file:shared.txt}""#),
            "the identical comment occurrence must stay verbatim"
        );
    }

    #[test]
    fn resolve_file_references_escapes_backspace_formfeed_and_nul() {
        let temp = TestDir::new("opencode_file_ref_ctrl");
        let content = "a\u{0008}b\u{000c}c\u{0000}d";
        fs::write(temp.path.join("secret.txt"), content).unwrap();

        let input = r#"{"api_key": "{file:secret.txt}"}"#;
        let result = resolve_file_references(input, &temp.path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result)
            .unwrap_or_else(|err| panic!("file control chars must stay JSON-safe: {err}"));
        assert_eq!(parsed["api_key"].as_str(), Some(content));
    }

    #[test]
    fn is_inside_comment_ignores_comment_markers_inside_strings() {
        let text = r#"{"url": "https://example.com", "key": "{file:secret.txt}"}"#;
        let pos = text.find("{file:secret.txt}").unwrap();
        assert!(!is_inside_comment(text, pos));
    }

    #[test]
    fn update_config_round_trips_mcp_timeout_and_servers() {
        let temp = TestDir::new("opencode_config_mcp_roundtrip");

        let patch: Config = serde_json::from_str(
            r#"{
                "mcp": {
                    "timeout": {"startup": 45000, "execution": 600000},
                    "servers": {
                        "srv": {"type": "local", "command": ["node", "s.js"]}
                    }
                }
            }"#,
        )
        .unwrap();

        update_config(&temp.path, &patch).unwrap();

        let content = fs::read_to_string(temp.path.join("opencode.json")).unwrap();
        // The global timeout must be written as a sibling of `servers`, not as
        // a bare entry that would be re-read as a server named `timeout`.
        let raw: serde_json::Value = serde_json::from_str(&content).unwrap();
        let mcp = raw.get("mcp").expect("mcp object");
        assert!(mcp.get("timeout").is_some(), "mcp.timeout must be written");
        assert!(mcp.get("servers").is_some(), "mcp.servers must be written");
        assert!(mcp.get("srv").is_none(), "legacy shape must not be used");

        let reloaded: Config = serde_json::from_str(&content).unwrap();
        let mcp = reloaded.mcp.expect("mcp");
        assert_eq!(
            mcp.timeout.expect("timeout").per_operation(),
            (45000, 30000, 600000)
        );
        let servers = mcp.servers.expect("servers");
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("srv"));
        assert!(!servers.contains_key("timeout"));
    }

    #[test]
    fn test_update_config() {
        let temp = TestDir::new("opencode_update_config");

        let patch = Config {
            model: Some("claude-3-opus".to_string()),
            ..Default::default()
        };

        update_config(&temp.path, &patch).unwrap();

        let content = fs::read_to_string(temp.path.join("opencode.json")).unwrap();
        let config: Config = serde_json::from_str(&content).unwrap();
        assert_eq!(config.model, Some("claude-3-opus".to_string()));
    }

    #[test]
    fn concurrent_update_config_keeps_both_patches() {
        let temp = TestDir::new("opencode_concurrent_update");

        // 2 スレッドを同じタイミングで解放し、両者が同じ旧内容を読む競合を狙う。
        // 注: Barrier は呼び出し直前の同時解放に過ぎず、実際の read 開始は
        // スケジューリング次第。ロックが無くても両キーが残り得るため、この
        // テストはベストエフォートの回帰ガードであり、決定的な競合再現ではない（R4）。
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for json in [
            r#"{"model":"concurrent-model"}"#,
            r#"{"theme":"concurrent-theme"}"#,
        ] {
            let path = temp.path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let patch: Config = serde_json::from_str(json).expect("patch");
                barrier.wait();
                update_config(&path, &patch).expect("update config");
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread");
        }

        let written = fs::read_to_string(temp.path.join("opencode.json")).expect("written config");
        let raw: serde_json::Value = serde_json::from_str(&written).expect("parse written config");
        assert_eq!(raw["model"], serde_json::json!("concurrent-model"));
        assert_eq!(raw["theme"], serde_json::json!("concurrent-theme"));
    }

    #[test]
    fn update_config_writes_back_to_existing_jsonc_without_creating_json() {
        let temp = TestDir::new("opencode_update_jsonc_preferred");
        let jsonc = temp.path.join("opencode.jsonc");
        fs::write(
            &jsonc,
            r#"{
                // user-maintained JSONC file
                "model": "jsonc-model",
                "theme": "dark",
            }"#,
        )
        .unwrap();

        let patch: Config = serde_json::from_str(r#"{"model":"patched-model"}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        assert!(
            !temp.path.join("opencode.json").exists(),
            "updating a JSONC-only project must not create a shadowing opencode.json"
        );
        let written = fs::read_to_string(&jsonc).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(raw["model"], serde_json::json!("patched-model"));
        assert_eq!(
            raw["theme"],
            serde_json::json!("dark"),
            "keys already present in the JSONC file must survive the merge"
        );
    }

    #[test]
    fn update_config_updates_existing_json_in_place() {
        let temp = TestDir::new("opencode_update_existing_json");
        let json = temp.path.join("opencode.json");
        fs::write(&json, r#"{"model":"old-model","theme":"dark"}"#).unwrap();

        let patch: Config = serde_json::from_str(r#"{"model":"new-model"}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        assert!(!temp.path.join("opencode.jsonc").exists());
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(raw["model"], serde_json::json!("new-model"));
        assert_eq!(raw["theme"], serde_json::json!("dark"));
    }

    #[test]
    fn update_config_writes_to_json_when_jsonc_and_json_coexist() {
        let temp = TestDir::new("opencode_update_both_formats");
        // Stop `find_up` at this directory so the reload below cannot pick up
        // a config from an ancestor of the temp directory.
        fs::create_dir_all(temp.path.join(".git")).unwrap();

        let jsonc = temp.path.join("opencode.jsonc");
        let json = temp.path.join("opencode.json");
        // The loader reads jsonc first and json second, so json wins.
        let jsonc_original = r#"{ "model": "jsonc-model", "theme": "light" }"#;
        let json_original = r#"{ "model": "json-model", "theme": "dark" }"#;
        fs::write(&jsonc, jsonc_original).unwrap();
        fs::write(&json, json_original).unwrap();

        let patch: Config = serde_json::from_str(r#"{"model":"patched-model"}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        // The patch must land in opencode.json -- writing it to the shadowed
        // JSONC file would be invisible after the next load.
        let written = fs::read_to_string(&json).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(raw["model"], serde_json::json!("patched-model"));
        assert_eq!(raw["theme"], serde_json::json!("dark"));
        assert_eq!(
            fs::read_to_string(&jsonc).unwrap(),
            jsonc_original,
            "the shadowed JSONC file must be left untouched"
        );

        // Verify through the real loader merge order that the patch survives.
        let mut loader = ConfigLoader::new().without_global_config();
        loader.load_project(&temp.path).unwrap();
        assert_eq!(loader.config().model.as_deref(), Some("patched-model"));
        assert_eq!(loader.config().theme.as_deref(), Some("dark"));
    }

    #[test]
    fn update_config_does_not_overwrite_unparseable_existing_config() {
        let temp = TestDir::new("opencode_update_invalid_existing");
        let json = temp.path.join("opencode.json");
        let original = r#"{ "model": "kept-model", "theme": }"#; // invalid JSON
        fs::write(&json, original).unwrap();

        let patch: Config = serde_json::from_str(r#"{"model":"patched-model"}"#).unwrap();
        let result = update_config(&temp.path, &patch);
        assert!(
            result.is_err(),
            "an unparseable existing config must abort the update"
        );

        assert_eq!(
            fs::read_to_string(&json).unwrap(),
            original,
            "existing config must remain byte-identical"
        );
        let error = result.unwrap_err().to_string();
        for secret in ["kept-model", "patched-model"] {
            assert!(
                !error.contains(secret),
                "token value leaked into error: {secret}"
            );
        }
        assert!(
            !error.contains(&temp.path.to_string_lossy().into_owned()),
            "path leaked into error"
        );
    }

    #[test]
    fn parse_existing_config_for_update_rejects_invalid_content() {
        assert!(parse_existing_config_for_update(r#"{"theme": }"#).is_err());
        let valid = parse_existing_config_for_update(r#"{"model":"m"}"#).unwrap();
        assert_eq!(valid.model.as_deref(), Some("m"));
    }

    #[test]
    fn update_config_leaves_no_temporary_files() {
        let temp = TestDir::new("opencode_update_no_temp_residue");
        let patch: Config = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        let entries: Vec<String> = fs::read_dir(&temp.path)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["opencode.json".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn update_config_writes_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDir::new("opencode_update_0600");
        let patch: Config = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        let mode = fs::metadata(temp.path.join("opencode.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "config must not be umask-dependent or readable by other users"
        );
    }

    #[cfg(unix)]
    #[test]
    fn update_config_tightens_permissions_of_existing_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDir::new("opencode_update_tighten");
        let path = temp.path.join("opencode.json");
        fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let patch: Config = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "atomic replace must correct permissive modes");
    }

    #[test]
    fn update_config_errors_never_expose_values_or_paths() {
        let temp = TestDir::new("opencode_update_error_redaction");
        let blocker = temp.path.join("blocker");
        fs::write(&blocker, "not a directory").unwrap();

        let secret = "DO-NOT-LOG-THIS-TOKEN";
        let patch: Config = serde_json::from_str(&format!(
            r#"{{"provider":{{"p":{{"api_key":"{secret}"}}}}}}"#
        ))
        .unwrap();

        let error = update_config(&blocker, &patch).unwrap_err().to_string();
        assert!(!error.contains(secret), "token value leaked into error");
        assert!(!error.contains("blocker"), "path leaked into error");
        assert!(
            !error.contains(&temp.path.to_string_lossy().into_owned()),
            "temporary path leaked into error"
        );
    }

    // ── OPENCODE_* environment override regression tests ───────────

    // Env vars are process-global; serialize the tests that mutate them.
    static ENV_OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_env_flag_accepts_only_explicit_truthy_values() {
        for value in ["true", "TRUE", " True ", "1", "yes", "YES"] {
            assert!(parse_env_flag(value), "{value:?} must enable the flag");
        }
        for value in [
            "false", "FALSE", "0", "no", "NO", "", " ", "garbage", "enabled", "2",
        ] {
            assert!(
                !parse_env_flag(value),
                "{value:?} must leave the flag disabled"
            );
        }
    }

    /// 不正な `OPENCODE_DISABLE_*` の値は変数名だけを警告し、値は出さない。
    #[test]
    fn invalid_disable_env_flag_warns_with_variable_name_only() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const AUTO: &str = "OPENCODE_DISABLE_AUTOCOMPACT";
        std::env::set_var(AUTO, "SENTINEL_VALUE_NEVER_LOG");

        let buffer = WarningBuffer::default();
        let subscriber = WarningSubscriber {
            buffer: buffer.clone(),
        };
        let enabled = tracing::subscriber::with_default(subscriber, || env_flag_enabled(AUTO));
        std::env::remove_var(AUTO);

        assert!(!enabled, "invalid env value must not enable the flag");
        let text = buffer.text();
        assert!(text.contains(AUTO), "variable name must be logged: {text}");
        assert!(
            !text.contains("SENTINEL_VALUE_NEVER_LOG"),
            "env value leaked: {text}"
        );
    }

    #[test]
    fn disable_flags_ignore_falsy_and_invalid_env_values() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const AUTO: &str = "OPENCODE_DISABLE_AUTOCOMPACT";
        const PRUNE: &str = "OPENCODE_DISABLE_PRUNE";

        for value in ["false", "0", "no", "", "garbage"] {
            let mut config = Config {
                compaction: Some(crate::schema::CompactionConfig {
                    auto: Some(true),
                    prune: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            };

            std::env::set_var(AUTO, value);
            std::env::set_var(PRUNE, value);
            apply_env_overrides(&mut config);
            std::env::remove_var(AUTO);
            std::env::remove_var(PRUNE);

            let compaction = config.compaction.expect("compaction config kept");
            assert_eq!(compaction.auto, Some(true), "{value:?} disabled auto");
            assert_eq!(compaction.prune, Some(true), "{value:?} disabled prune");
        }
    }

    #[test]
    fn disable_flags_fire_for_truthy_env_values() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const AUTO: &str = "OPENCODE_DISABLE_AUTOCOMPACT";
        const PRUNE: &str = "OPENCODE_DISABLE_PRUNE";

        let mut config = Config::default();
        std::env::set_var(AUTO, "true");
        std::env::set_var(PRUNE, "1");
        apply_env_overrides(&mut config);
        std::env::remove_var(AUTO);
        std::env::remove_var(PRUNE);

        let compaction = config.compaction.expect("truthy flags must set compaction");
        assert_eq!(compaction.auto, Some(false));
        assert_eq!(compaction.prune, Some(false));
    }

    #[test]
    fn invalid_permission_env_json_is_ignored_without_crashing() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const VAR: &str = "OPENCODE_PERMISSION";
        // Contains a secret-looking token: the warning must never include it.
        let invalid = r#"{"secret-looking":"DO-NOT-LOG-ME",,,"#;

        let mut config = Config::default();
        std::env::set_var(VAR, invalid);
        apply_env_overrides(&mut config); // must not panic
        std::env::remove_var(VAR);

        assert!(
            config.permission.is_none(),
            "invalid JSON must keep defaults"
        );
    }

    #[test]
    fn permission_env_json_merges_rules() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const VAR: &str = "OPENCODE_PERMISSION";

        let mut config = Config::default();
        std::env::set_var(VAR, r#"{"read":"deny"}"#);
        apply_env_overrides(&mut config);
        std::env::remove_var(VAR);

        let rules = config.permission.expect("permission set").rules();
        assert!(matches!(
            rules.get("read"),
            Some(PermissionRule::Action(PermissionAction::Deny))
        ));
    }

    #[test]
    fn managed_config_overrides_environment_flags() {
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const MANAGED: &str = "OPENCODE_TEST_MANAGED_CONFIG_DIR";
        const AUTO: &str = "OPENCODE_DISABLE_AUTOCOMPACT";

        let temp = TestDir::new("opencode_managed_over_env");
        fs::write(
            temp.path.join("opencode.json"),
            r#"{"compaction":{"auto":true}}"#,
        )
        .unwrap();

        std::env::set_var(MANAGED, temp.path.as_os_str());
        std::env::set_var(AUTO, "true");
        let mut loader = ConfigLoader::new().without_global_config();
        let result = loader.load_all(&temp.path);
        std::env::remove_var(AUTO);
        std::env::remove_var(MANAGED);

        let config = result.expect("load_all must succeed");
        assert_eq!(
            config.compaction.and_then(|c| c.auto),
            Some(true),
            "managed config must win over OPENCODE_DISABLE_AUTOCOMPACT"
        );
    }

    // ── YAML frontmatter parsing tests ──────────────────────────────

    #[test]
    fn test_split_frontmatter_basic() {
        let content = "---\nname: test\ndescription: hello\n---\nBody content here.";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        let fm = fm.unwrap();
        assert!(fm.contains("name: test"));
        assert!(fm.contains("description: hello"));
        assert!(body.contains("Body content here."));
    }

    #[test]
    fn test_split_frontmatter_no_frontmatter() {
        let content = "Just a regular markdown file.";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_yaml_frontmatter_flat_key_values() {
        let yaml = "name: reviewer\ndescription: Review code\nmodel: claude-3-opus";
        let json = serde_yaml_frontmatter_to_json(yaml);
        assert_eq!(json["name"], "reviewer");
        assert_eq!(json["description"], "Review code");
        assert_eq!(json["model"], "claude-3-opus");
    }

    #[test]
    fn test_yaml_frontmatter_booleans_and_numbers() {
        let yaml = "disable: true\nhidden: false\nsteps: 100\ntemperature: 0.7";
        let json = serde_yaml_frontmatter_to_json(yaml);
        assert_eq!(json["disable"], true);
        assert_eq!(json["hidden"], false);
        assert_eq!(json["steps"], 100);
        assert_eq!(json["temperature"], 0.7);
    }

    #[test]
    fn test_yaml_frontmatter_inline_list() {
        let yaml = "tools: [bash, read, write]";
        let json = serde_yaml_frontmatter_to_json(yaml);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0], "bash");
        assert_eq!(tools[1], "read");
        assert_eq!(tools[2], "write");
    }

    #[test]
    fn test_yaml_frontmatter_dash_list() {
        let yaml = "tools:\n  - bash\n  - read\n  - write";
        let json = serde_yaml_frontmatter_to_json(yaml);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0], "bash");
        assert_eq!(tools[1], "read");
        assert_eq!(tools[2], "write");
    }

    #[test]
    fn test_yaml_frontmatter_nested_object() {
        let yaml = "tools:\n  bash: true\n  read: false";
        let json = serde_yaml_frontmatter_to_json(yaml);
        let tools = json["tools"].as_object().unwrap();
        assert_eq!(tools["bash"], true);
        assert_eq!(tools["read"], false);
    }

    #[test]
    fn test_yaml_frontmatter_block_scalar_literal() {
        let yaml = "prompt: |\n  Line one\n  Line two";
        let json = serde_yaml_frontmatter_to_json(yaml);
        let prompt = json["prompt"].as_str().unwrap();
        assert!(prompt.contains("Line one"));
        assert!(prompt.contains("Line two"));
    }

    #[test]
    fn test_yaml_frontmatter_block_scalar_strip() {
        let yaml = "prompt: |-\n  Line one\n  Line two";
        let json = serde_yaml_frontmatter_to_json(yaml);
        let prompt = json["prompt"].as_str().unwrap();
        assert!(prompt.contains("Line one"));
        assert!(!prompt.ends_with('\n'));
    }

    #[test]
    fn test_yaml_frontmatter_comments_skipped() {
        let yaml = "# This is a comment\nname: test\n# Another comment\ndescription: hello";
        let json = serde_yaml_frontmatter_to_json(yaml);
        assert_eq!(json["name"], "test");
        assert_eq!(json["description"], "hello");
    }

    #[test]
    fn test_yaml_frontmatter_quoted_values() {
        let yaml = "name: \"quoted value\"\ndescription: 'single quoted'";
        let json = serde_yaml_frontmatter_to_json(yaml);
        assert_eq!(json["name"], "quoted value");
        assert_eq!(json["description"], "single quoted");
    }

    #[test]
    fn test_yaml_frontmatter_lone_quote_value_does_not_panic() {
        // 回帰: `key: "` のように値が引用符 1 文字だけだと、長さ 1 に対して
        // `value[1..value.len() - 1]` = `1..0` を計算して panic していた。
        // 本番の frontmatter 経路と同じ serde_yaml_frontmatter_to_json で確認する。
        let json = serde_yaml_frontmatter_to_json("description: \"");
        assert_eq!(json["description"], "\"");
        let json = serde_yaml_frontmatter_to_json("description: '");
        assert_eq!(json["description"], "'");

        // スカラー変換でも入力がそのまま返り、通常の引用符は従来どおり剥がれる。
        assert_eq!(
            yaml_scalar_to_json("\""),
            serde_json::Value::String("\"".to_string())
        );
        assert_eq!(
            yaml_scalar_to_json("'"),
            serde_json::Value::String("'".to_string())
        );
        assert_eq!(
            yaml_scalar_to_json("\"ok\""),
            serde_json::Value::String("ok".to_string())
        );
    }

    #[test]
    fn test_fallback_sanitize_yaml_colon_in_value() {
        let yaml = "description: Use model: claude-3 for tasks\nname: test";
        let sanitized = fallback_sanitize_yaml(yaml);
        assert!(sanitized.contains("description: |-"));
        assert!(sanitized.contains("  Use model: claude-3 for tasks"));
        assert!(sanitized.contains("name: test"));
    }

    #[test]
    fn test_fallback_sanitize_yaml_preserves_quoted() {
        let yaml = "description: \"already: quoted\"\nname: test";
        let sanitized = fallback_sanitize_yaml(yaml);
        // Quoted values should not be converted to block scalars
        assert!(sanitized.contains("description: \"already: quoted\""));
    }

    #[test]
    fn test_fallback_sanitize_yaml_preserves_block_scalar() {
        let yaml = "description: |\n  block content\nname: test";
        let sanitized = fallback_sanitize_yaml(yaml);
        assert!(sanitized.contains("description: |"));
    }

    #[test]
    fn test_yaml_frontmatter_value_with_colon_via_fallback() {
        // This YAML has a value with a colon, which would confuse naive parsers.
        // The fallback sanitization should handle it.
        let yaml = "description: Use model: claude-3 for tasks\nname: test";
        let json = serde_yaml_frontmatter_to_json(yaml);
        // After fallback, description should be preserved
        assert_eq!(json["name"], "test");
        let desc = json["description"].as_str().unwrap();
        assert!(desc.contains("model: claude-3"));
    }

    #[test]
    fn test_yaml_frontmatter_inline_map() {
        let yaml = "options: {verbose: true, timeout: 30}";
        let json = serde_yaml_frontmatter_to_json(yaml);
        let options = json["options"].as_object().unwrap();
        assert_eq!(options["verbose"], true);
        assert_eq!(options["timeout"], 30);
    }

    #[test]
    fn test_yaml_frontmatter_empty_value() {
        let yaml = "name:\ndescription: hello";
        let json = serde_yaml_frontmatter_to_json(yaml);
        assert!(json["name"].is_null());
        assert_eq!(json["description"], "hello");
    }

    #[test]
    fn test_parse_markdown_agent_with_frontmatter() {
        let temp = TestDir::new("opencode_md_agent");
        let agent_dir = temp.path.join("agents");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("reviewer.md"),
            "---\ndescription: Reviews code changes\nmode: subagent\nmodel: claude-3-opus\n---\n\nYou are a code reviewer.\n",
        )
        .unwrap();

        let result = parse_markdown_agent(&agent_dir.join("reviewer.md"), &temp.path);
        assert!(result.is_some());
        let (name, config) = result.unwrap();
        assert_eq!(name, "reviewer");
        assert_eq!(config.description.as_deref(), Some("Reviews code changes"));
        assert_eq!(config.model.as_deref(), Some("claude-3-opus"));
        assert!(config.prompt.unwrap().contains("You are a code reviewer."));
    }

    #[test]
    fn test_parse_markdown_command_with_frontmatter() {
        let temp = TestDir::new("opencode_md_cmd");
        let cmd_dir = temp.path.join("commands");
        fs::create_dir_all(&cmd_dir).unwrap();
        fs::write(
            cmd_dir.join("review.md"),
            "---\ndescription: Run a code review\nagent: reviewer\n---\n\nPlease review the changes.\n",
        )
        .unwrap();

        let result = parse_markdown_command(&cmd_dir.join("review.md"), &temp.path);
        assert!(result.is_some());
        let (name, config) = result.unwrap();
        assert_eq!(name, "review");
        assert_eq!(config.description.as_deref(), Some("Run a code review"));
        assert_eq!(config.agent.as_deref(), Some("reviewer"));
        assert!(config
            .template
            .unwrap()
            .contains("Please review the changes."));
    }

    #[test]
    fn test_parse_markdown_agent_with_tools_map() {
        let temp = TestDir::new("opencode_md_agent_tools");
        let agent_dir = temp.path.join("agents");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("safe.md"),
            "---\ndescription: Safe agent\ntools:\n  bash: false\n  read: true\n---\n\nSafe prompt.\n",
        )
        .unwrap();

        let result = parse_markdown_agent(&agent_dir.join("safe.md"), &temp.path);
        assert!(result.is_some());
        let (_name, config) = result.unwrap();
        assert_eq!(config.description.as_deref(), Some("Safe agent"));
        let tools = config.tools.unwrap();
        assert_eq!(tools.get("bash"), Some(&false));
        assert_eq!(tools.get("read"), Some(&true));
    }

    #[test]
    fn test_parse_markdown_agent_colon_in_description_fallback() {
        let temp = TestDir::new("opencode_md_agent_colon");
        let agent_dir = temp.path.join("agents");
        fs::create_dir_all(&agent_dir).unwrap();
        // Description contains a colon -- this is the case the fallback handles
        fs::write(
            agent_dir.join("tricky.md"),
            "---\ndescription: Use model: claude for tasks\nmode: primary\n---\n\nTricky prompt.\n",
        )
        .unwrap();

        let result = parse_markdown_agent(&agent_dir.join("tricky.md"), &temp.path);
        assert!(result.is_some());
        let (_name, config) = result.unwrap();
        let desc = config.description.unwrap();
        assert!(desc.contains("model: claude"));
    }

    /// Regression: one malformed frontmatter field used to be swallowed by
    /// `unwrap_or_default()`, losing every metadata field silently. The warning
    /// must name only the file and the failing field; values/body must not leak.
    #[test]
    fn invalid_frontmatter_warns_with_filename_and_field_only() {
        let temp = TestDir::new("opencode_md_bad_frontmatter");
        let agent_dir = temp.path.join("agents");
        let cmd_dir = temp.path.join("commands");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::create_dir_all(&cmd_dir).unwrap();

        const BAD_VALUE: &str = "VALUE_SENTINEL_NEVER_LOG";
        const BODY_SENTINEL: &str = "BODY_SENTINEL_NEVER_LOG";

        fs::write(
            agent_dir.join("bad-agent.md"),
            format!("---\ndescription: {BAD_VALUE}\nmode: not-a-mode\n---\n\n{BODY_SENTINEL}\n"),
        )
        .unwrap();
        fs::write(
            cmd_dir.join("bad-command.md"),
            format!("---\ndescription: keep-me\nsubtask: {BAD_VALUE}\n---\n\n{BODY_SENTINEL}\n"),
        )
        .unwrap();

        let buffer = WarningBuffer::default();
        let agent = tracing::subscriber::with_default(
            WarningSubscriber {
                buffer: buffer.clone(),
            },
            || parse_markdown_agent(&agent_dir.join("bad-agent.md"), &temp.path),
        );
        let command = tracing::subscriber::with_default(
            WarningSubscriber {
                buffer: buffer.clone(),
            },
            || parse_markdown_command(&cmd_dir.join("bad-command.md"), &temp.path),
        );

        let text = buffer.text();
        assert!(
            text.contains("frontmatter metadata parse failed") && text.contains("using defaults"),
            "warning must announce the fallback: {text}"
        );
        assert!(text.contains("bad-agent.md"), "{text}");
        assert!(text.contains("bad-command.md"), "{text}");
        assert!(text.contains("field=mode"), "{text}");
        assert!(text.contains("field=subtask"), "{text}");
        let full_path = temp.path.to_string_lossy().into_owned();
        for forbidden in [BAD_VALUE, BODY_SENTINEL, full_path.as_str()] {
            assert!(!text.contains(forbidden), "frontmatter data leaked: {text}");
        }

        // Existing fallback behavior is preserved: metadata is defaulted while
        // the markdown body is still loaded into the prompt/template.
        let (_name, agent_config) = agent.expect("bad agent must still load");
        assert!(agent_config.description.is_none());
        assert!(agent_config.prompt.unwrap().contains(BODY_SENTINEL));
        let (_name, command_config) = command.expect("bad command must still load");
        assert!(command_config.description.is_none());
        assert!(command_config.template.unwrap().contains(BODY_SENTINEL));
    }

    #[test]
    fn legacy_toml_config_migrates_to_opencode_json() {
        let temp = TestDir::new("opencode_legacy_toml");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config"),
            r#"
provider = "anthropic"
model = "claude-3-5-sonnet"
theme = "dark"
"#,
        )
        .unwrap();

        let mut config = Config::default();
        let migrated = migrate_legacy_toml_config(&config_dir, &mut config);
        assert!(migrated.is_some());
        assert_eq!(config.model.as_deref(), Some("anthropic/claude-3-5-sonnet"));
        assert_eq!(config.theme.as_deref(), Some("dark"));
        assert_eq!(
            config.schema.as_deref(),
            Some("https://opencode.ai/config.json")
        );

        let json_path = config_dir.join("opencode.json");
        assert!(json_path.exists());
        assert!(!config_dir.join("config").exists());

        let content = fs::read_to_string(json_path).unwrap();
        let written: Config = serde_json::from_str(&content).unwrap();
        assert_eq!(
            written.model.as_deref(),
            Some("anthropic/claude-3-5-sonnet")
        );
        assert_eq!(written.theme.as_deref(), Some("dark"));
    }

    #[test]
    fn legacy_toml_migration_keeps_backup_of_original_file() {
        let temp = TestDir::new("opencode_legacy_toml_backup");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();
        let original =
            "provider = \"anthropic\"\nmodel = \"claude-3-5-sonnet\"\ntheme = \"dark\"\n";
        fs::write(config_dir.join("config"), original).unwrap();

        let mut config = Config::default();
        assert!(migrate_legacy_toml_config(&config_dir, &mut config).is_some());

        let backup_path = config_dir.join("config.bak");
        assert!(backup_path.exists(), "backup should exist for rollback");
        assert_eq!(fs::read_to_string(&backup_path).unwrap(), original);
        assert!(!config_dir.join("config").exists());
    }

    #[test]
    fn legacy_toml_migration_keeps_original_when_backup_fails() {
        let temp = TestDir::new("opencode_legacy_toml_backup_fail");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();
        // Force fs::copy to fail by making the backup destination a directory.
        fs::create_dir(config_dir.join("config.bak")).unwrap();
        let original = "provider = \"anthropic\"\nmodel = \"claude\"\n";
        fs::write(config_dir.join("config"), original).unwrap();

        let mut config = Config::default();
        // A failed backup must abort before any JSON is written.
        assert!(migrate_legacy_toml_config(&config_dir, &mut config).is_none());

        assert_eq!(
            fs::read_to_string(config_dir.join("config")).unwrap(),
            original
        );
        assert!(!config_dir.join("opencode.json").exists());
        assert!(
            config.model.is_none(),
            "in-memory config must stay untouched on a failed migration"
        );
    }

    #[test]
    fn legacy_toml_migration_backup_failure_leaves_no_partial_state() {
        // Regression for ERRPATH-07: a backup failure used to be downgraded to
        // a warning while the new JSON was still written and reported as a
        // successful migration, leaving JSON + TOML with no backup at all.
        let temp = TestDir::new("opencode_legacy_toml_backup_rollback");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();

        let original = "provider = \"anthropic\"\nmodel = \"claude\"\ntheme = \"dark\"\n";
        fs::write(config_dir.join("config"), original).unwrap();
        // Make fs::copy fail by occupying the backup destination with a dir.
        fs::create_dir(config_dir.join("config.bak")).unwrap();

        let mut config = Config::default();
        assert!(migrate_legacy_toml_config(&config_dir, &mut config).is_none());

        assert_eq!(
            fs::read_to_string(config_dir.join("config")).unwrap(),
            original,
            "legacy TOML must remain the only config source"
        );
        assert!(
            !config_dir.join("opencode.json").exists(),
            "no JSON may be created when its backup could not be made"
        );
        assert!(config.model.is_none());
        assert!(config.theme.is_none());

        // The normal path is unchanged: with a writable backup destination the
        // migration still produces JSON, a backup, and removes the source.
        let ok_dir = temp.path.join("opencode_ok");
        fs::create_dir_all(&ok_dir).unwrap();
        fs::write(ok_dir.join("config"), original).unwrap();

        let mut ok_config = Config::default();
        assert!(migrate_legacy_toml_config(&ok_dir, &mut ok_config).is_some());

        assert!(ok_dir.join("opencode.json").exists());
        assert_eq!(
            fs::read_to_string(ok_dir.join("config.bak")).unwrap(),
            original
        );
        assert!(!ok_dir.join("config").exists());
        assert_eq!(ok_config.model.as_deref(), Some("anthropic/claude"));
        assert_eq!(ok_config.theme.as_deref(), Some("dark"));
    }

    #[test]
    fn load_global_guard_does_not_rewrite_existing_json_config() {
        let temp = TestDir::new("opencode_guard_existing_json");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();

        let legacy = "model = \"legacy\"\n";
        fs::write(config_dir.join("config"), legacy).unwrap();
        let current_json = r#"{"model":"current","theme":"dark"}"#;
        fs::write(config_dir.join("opencode.json"), current_json).unwrap();

        // Only the explicit startup migration reaches this guard; config
        // readers never do.
        let mut config = Config::default();
        assert!(migrate_global_config_if_absent(&config_dir, &mut config).is_none());

        assert_eq!(
            fs::read_to_string(config_dir.join("config")).unwrap(),
            legacy,
            "legacy source must be left untouched when JSON already exists"
        );
        assert_eq!(
            fs::read_to_string(config_dir.join("opencode.json")).unwrap(),
            current_json,
            "existing opencode.json must not be rewritten"
        );
        assert!(
            !config_dir.join("config.bak").exists(),
            "no backup (and therefore no removal) should be created"
        );
        assert!(
            config.model.is_none(),
            "a skipped migration must not alter the in-memory config"
        );
    }

    #[test]
    fn load_global_guard_does_not_rewrite_existing_jsonc_config() {
        let temp = TestDir::new("opencode_guard_existing_jsonc");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();

        let legacy = "model = \"legacy\"\n";
        fs::write(config_dir.join("config"), legacy).unwrap();
        let current_jsonc = r#"{"model":"current-jsonc"}"#;
        fs::write(config_dir.join("opencode.jsonc"), current_jsonc).unwrap();

        let mut config = Config::default();
        assert!(migrate_global_config_if_absent(&config_dir, &mut config).is_none());

        assert_eq!(
            fs::read_to_string(config_dir.join("config")).unwrap(),
            legacy
        );
        assert_eq!(
            fs::read_to_string(config_dir.join("opencode.jsonc")).unwrap(),
            current_jsonc
        );
        assert!(!config_dir.join("opencode.json").exists());
    }

    #[test]
    fn load_global_path_never_rewrites_an_existing_config_file() {
        let temp = TestDir::new("opencode_load_global_readonly");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();

        let legacy = "model = \"legacy-model\"\n";
        fs::write(config_dir.join("config"), legacy).unwrap();
        let current = r#"{"model":"current-model","theme":"dark"}"#;
        fs::write(config_dir.join("opencode.json"), current).unwrap();

        let mut loader = ConfigLoader::new();
        loader.load_global_from(&config_dir).unwrap();

        assert_eq!(
            fs::read_to_string(config_dir.join("config")).unwrap(),
            legacy
        );
        assert_eq!(
            fs::read_to_string(config_dir.join("opencode.json")).unwrap(),
            current
        );
        assert!(!config_dir.join("config.bak").exists());
        assert_eq!(loader.config().model.as_deref(), Some("current-model"));
    }

    #[test]
    fn load_global_from_reads_both_formats_with_json_winning() {
        let temp = TestDir::new("opencode_global_both_formats");
        let config_dir = temp.path.join("opencode");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("opencode.jsonc"),
            r#"{ "model": "jsonc-model", "theme": "light" }"#,
        )
        .unwrap();
        fs::write(
            config_dir.join("opencode.json"),
            r#"{ "model": "json-model" }"#,
        )
        .unwrap();

        let mut loader = ConfigLoader::new();
        loader.load_global_from(&config_dir).unwrap();

        assert_eq!(
            loader.config().model.as_deref(),
            Some("json-model"),
            "json must be merged after jsonc so it wins"
        );
        assert_eq!(
            loader.config().theme.as_deref(),
            Some("light"),
            "keys present only in jsonc must survive"
        );
    }

    #[test]
    fn update_config_with_empty_plugin_array_clears_existing_plugins() {
        let temp = TestDir::new("opencode_update_clear_plugins");

        let initial: Config =
            serde_json::from_str(r#"{"plugin":["plugin-a","plugin-b"]}"#).unwrap();
        update_config(&temp.path, &initial).unwrap();

        let patch: Config = serde_json::from_str(r#"{"plugin":[]}"#).unwrap();
        assert!(patch.plugin.is_explicit());
        update_config(&temp.path, &patch).unwrap();

        let content = fs::read_to_string(temp.path.join("opencode.json")).unwrap();
        let reloaded: Config = serde_json::from_str(&content).unwrap();
        assert!(
            reloaded.plugin.is_empty(),
            "plugin: [] must remove previously persisted plugins"
        );
    }

    #[test]
    fn update_config_without_plugin_key_preserves_existing_plugins() {
        let temp = TestDir::new("opencode_update_keep_plugins");

        let initial: Config = serde_json::from_str(r#"{"plugin":["plugin-a"]}"#).unwrap();
        update_config(&temp.path, &initial).unwrap();

        // A patch that never mentions `plugin` must not clear it.
        let patch: Config = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(!patch.plugin.is_explicit());
        update_config(&temp.path, &patch).unwrap();

        let content = fs::read_to_string(temp.path.join("opencode.json")).unwrap();
        let reloaded: Config = serde_json::from_str(&content).unwrap();
        assert_eq!(reloaded.plugin, vec!["plugin-a".to_string()]);
    }

    #[test]
    fn explicit_empty_plugin_array_ignores_opencode_plugin_directory() {
        let temp = TestDir::new("opencode_explicit_empty_plugin_dir");
        let plugins_dir = temp.path.join(".opencode/plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::write(plugins_dir.join("local.ts"), "export default {}").unwrap();
        fs::write(temp.path.join("opencode.json"), r#"{"plugin":[]}"#).unwrap();

        let mut loader = ConfigLoader::new().without_global_config();
        let config = loader.load_all(&temp.path).unwrap();

        assert!(config.plugin.is_explicit());
        assert!(
            config.plugin.is_empty(),
            "explicit plugin: [] must suppress files found in .opencode/plugins"
        );
    }

    #[test]
    fn absent_plugin_key_still_discovers_opencode_plugin_directory() {
        let temp = TestDir::new("opencode_absent_plugin_dir");
        let plugins_dir = temp.path.join(".opencode/plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::write(plugins_dir.join("local.ts"), "export default {}").unwrap();
        fs::write(temp.path.join("opencode.json"), r#"{"theme":"dark"}"#).unwrap();

        let mut loader = ConfigLoader::new().without_global_config();
        let config = loader.load_all(&temp.path).unwrap();

        assert!(
            config
                .plugin
                .iter()
                .any(|plugin| plugin.contains("local.ts")),
            "an omitted plugin key must keep the additive .opencode/plugins scan"
        );
    }

    #[test]
    fn update_config_empty_plugin_array_survives_reload_without_revival() {
        let temp = TestDir::new("opencode_reload_empty_plugin");
        let plugins_dir = temp.path.join(".opencode/plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::write(plugins_dir.join("local.js"), "module.exports = {}").unwrap();

        let initial: Config = serde_json::from_str(r#"{"plugin":["plugin-a"]}"#).unwrap();
        update_config(&temp.path, &initial).unwrap();
        let patch: Config = serde_json::from_str(r#"{"plugin":[]}"#).unwrap();
        update_config(&temp.path, &patch).unwrap();

        let written = fs::read_to_string(temp.path.join("opencode.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(
            value.get("plugin"),
            Some(&serde_json::json!([])),
            "the explicit empty array must be persisted, not omitted"
        );

        let mut loader = ConfigLoader::new().without_global_config();
        let config = loader.load_all(&temp.path).unwrap();
        assert!(config.plugin.is_explicit());
        assert!(
            config.plugin.is_empty(),
            "plugins from .opencode/plugins must not revive after reload"
        );
    }

    // ── R2 merge regressions: timezone / agents / provider conflicts ──

    #[test]
    fn load_from_str_layers_preserve_timezone_and_agents() {
        let mut loader = ConfigLoader::new().without_global_config();
        loader
            .load_from_str(r#"{"timezone":"UTC","agents":{"reviewer":{"prompt":"base prompt"}}}"#)
            .unwrap();
        loader
            .load_from_str(
                r#"{"timezone":"Asia/Tokyo","agents":{"reviewer":{"model":"new-model"},"build":{}}}"#,
            )
            .unwrap();

        let config = loader.get_config();
        assert_eq!(config.timezone.as_deref(), Some("Asia/Tokyo"));
        let agents = config.agents.expect("agents kept across layers");
        let reviewer = agents.get("reviewer").expect("reviewer");
        assert_eq!(reviewer.prompt.as_deref(), Some("base prompt"));
        assert_eq!(reviewer.model.as_deref(), Some("new-model"));
        assert!(agents.contains_key("build"));
    }

    #[test]
    fn project_providers_override_global_provider_after_normalization() {
        let mut loader = ConfigLoader::new().without_global_config();
        // Lower precedence layer uses the legacy spelling.
        loader
            .load_from_str(r#"{"provider":{"x":{"name":"global-name","api_key":"global-value"}}}"#)
            .unwrap();
        // Higher precedence layer uses the current spelling.
        loader
            .load_from_str(r#"{"providers":{"x":{"name":"project-name","npm":"project-pkg"}}}"#)
            .unwrap();

        let mut config = loader.get_config();
        apply_post_load_transforms(&mut config);

        assert!(
            config.providers.is_none(),
            "current-schema map must be folded into provider"
        );
        let provider = config
            .provider
            .expect("provider map")
            .remove("x")
            .expect("project provider");
        assert_eq!(provider.name.as_deref(), Some("project-name"));
        assert_eq!(provider.npm.as_deref(), Some("project-pkg"));
        assert_eq!(provider.api_key, None, "global value must not survive");
    }

    #[test]
    fn project_legacy_provider_overrides_global_providers_after_normalization() {
        let mut loader = ConfigLoader::new().without_global_config();
        loader
            .load_from_str(r#"{"providers":{"x":{"name":"global-name"}}}"#)
            .unwrap();
        loader
            .load_from_str(r#"{"provider":{"x":{"name":"project-name"}}}"#)
            .unwrap();

        let mut config = loader.get_config();
        apply_post_load_transforms(&mut config);

        let provider = config
            .provider
            .expect("provider map")
            .remove("x")
            .expect("project provider");
        assert_eq!(provider.name.as_deref(), Some("project-name"));
    }

    // ── R2 regression tests: config.env / experimental.mcp_timeout ───

    #[test]
    fn config_env_wins_over_process_env_in_placeholder_expansion() {
        const VAR: &str = "OPENCODE_R2_CONFIG_ENV_SAME_FILE";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = TestDir::new("opencode_r2_env_same_file");
        fs::write(
            temp.path.join("opencode.json"),
            r#"{
                "env": {"OPENCODE_R2_CONFIG_ENV_SAME_FILE": "from-config"},
                "model": "{env:OPENCODE_R2_CONFIG_ENV_SAME_FILE}"
            }"#,
        )
        .unwrap();

        // A process value with the same name must not win over config.env.
        std::env::set_var(VAR, "from-process");
        let mut loader = ConfigLoader::new().without_global_config();
        let loaded = loader.load_all(&temp.path);
        std::env::remove_var(VAR);

        let config = loaded.expect("config with config.env placeholder must load");
        assert_eq!(config.model.as_deref(), Some("from-config"));
    }

    #[test]
    fn config_env_from_earlier_layer_is_used_for_placeholder() {
        const VAR: &str = "OPENCODE_R2_CONFIG_ENV_LAYER";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = TestDir::new("opencode_r2_env_layer");
        let file = temp.path.join("opencode.json");
        fs::write(&file, r#"{"model":"{env:OPENCODE_R2_CONFIG_ENV_LAYER}"}"#).unwrap();

        // Ensure the process environment cannot satisfy the placeholder.
        std::env::remove_var(VAR);
        let mut loader = ConfigLoader::new().without_global_config();
        loader
            .load_from_str(r#"{"env":{"OPENCODE_R2_CONFIG_ENV_LAYER":"layer-value"}}"#)
            .unwrap();
        loader.load_from_file(&file).unwrap();

        assert_eq!(loader.config().model.as_deref(), Some("layer-value"));
    }

    /// R4: inline config from OPENCODE_CONFIG_CONTENT containing `{env:}` and
    /// `{file:}` placeholders must expand like file-based config instead of
    /// failing to parse at startup. No value or path is written to logs.
    #[test]
    fn load_from_str_expands_env_and_file_references() {
        const ROOT_VAR: &str = "OPENCODE_INLINE_ROOT";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = TestDir::new("opencode_inline_placeholders");
        let model_file = temp.path.join("model.txt");
        fs::write(&model_file, "file-model").unwrap();
        std::env::set_var(ROOT_VAR, "env-model");

        // `env.BASE` chains through the process variable, `model` uses the
        // chained value, and `theme` reads a file via an absolute path so the
        // inline base directory (".") is irrelevant to this test.
        let inline = format!(
            r#"{{
                "env": {{ "OPENCODE_INLINE_BASE": "{{env:{ROOT_VAR}}}" }},
                "model": "{{env:OPENCODE_INLINE_BASE}}",
                "theme": "{{file:{}}}"
            }}"#,
            model_file.display()
        );

        let mut loader = ConfigLoader::new().without_global_config();
        let result = loader.load_from_str(&inline);
        std::env::remove_var(ROOT_VAR);

        result.expect("inline config with env/file placeholders must load");
        let config = loader.config();
        assert_eq!(config.model.as_deref(), Some("env-model"));
        assert_eq!(config.theme.as_deref(), Some("file-model"));
    }

    #[test]
    fn experimental_mcp_timeout_migrates_to_mcp_timeout() {
        let mut config: Config =
            serde_json::from_str(r#"{"experimental":{"mcp_timeout":1234}}"#).unwrap();
        apply_post_load_transforms(&mut config);

        let timeout = config
            .mcp
            .expect("mcp section created")
            .timeout
            .expect("mcp.timeout migrated");
        assert_eq!(timeout.per_operation(), (1234, 1234, 1234));
        assert!(
            config
                .experimental
                .as_ref()
                .and_then(|experimental| experimental.mcp_timeout)
                .is_none(),
            "deprecated key must be cleared after migration"
        );
    }

    #[test]
    fn explicit_mcp_timeout_wins_over_deprecated_experimental_value() {
        let mut config: Config = serde_json::from_str(
            r#"{
                "experimental":{"mcp_timeout":1234},
                "mcp":{"timeout":{"startup":9999}}
            }"#,
        )
        .unwrap();
        apply_post_load_transforms(&mut config);

        let timeout = config.mcp.expect("mcp").timeout.expect("timeout");
        assert_eq!(
            timeout.per_operation().0,
            9999,
            "explicit mcp.timeout must keep priority"
        );
        assert!(config
            .experimental
            .as_ref()
            .and_then(|experimental| experimental.mcp_timeout)
            .is_none());
    }

    // ── R2 regression tests: silent legacy migrations / explicit config ───

    /// Captures `tracing` events without adding a `tracing-subscriber` dev
    /// dependency (same minimal pattern used by other crates in this repo).
    #[derive(Clone, Default)]
    struct WarningBuffer(Arc<Mutex<String>>);

    impl WarningBuffer {
        fn text(&self) -> String {
            self.0.lock().expect("warning buffer lock").clone()
        }
    }

    struct WarningVisitor(WarningBuffer);

    impl Visit for WarningVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let mut guard = self.0 .0.lock().expect("warning buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let mut guard = self.0 .0.lock().expect("warning buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={} ", field.name(), value);
        }
    }

    struct WarningSubscriber {
        buffer: WarningBuffer,
    }

    impl Subscriber for WarningSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            event.record(&mut WarningVisitor(self.buffer.clone()));
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn deprecated_migration_keys_emit_value_independent_warnings() {
        let mut config: Config = serde_json::from_str(
            r#"{
                "mode": {"primary-agent": {"model": "legacy-primary"}},
                "tools": {"secret-tool-name": true},
                "autoshare": false
            }"#,
        )
        .expect("legacy config must parse");

        let buffer = WarningBuffer::default();
        let subscriber = WarningSubscriber {
            buffer: buffer.clone(),
        };
        tracing::subscriber::with_default(subscriber, || {
            apply_post_load_transforms(&mut config);
        });

        let text = buffer.text();
        assert!(
            text.contains("mode is deprecated; use agent instead"),
            "{text}"
        );
        assert!(
            text.contains("tools is deprecated; use permission instead"),
            "{text}"
        );
        assert!(
            text.contains("autoshare is deprecated; use share instead"),
            "{text}"
        );
        // Distinctive sentinel values must never appear in the warning payload.
        for forbidden in ["primary-agent", "legacy-primary", "secret-tool-name"] {
            assert!(!text.contains(forbidden), "config value leaked: {text}");
        }
    }

    /// 回帰 (CFG2-02): write/edit/patch/multiedit は同じ "edit" キーに畳み込まれる。
    /// 入力順を変えても解決結果は変わらず、いずれかの別名が false なら Deny が勝つ。
    #[test]
    fn legacy_tools_edit_aliases_resolve_deterministically() {
        let orders: [Vec<(&str, bool)>; 4] = [
            vec![("write", true), ("edit", false)],
            vec![("edit", false), ("write", true)],
            vec![("patch", true), ("write", false), ("multiedit", true)],
            vec![("multiedit", true), ("write", false), ("patch", true)],
        ];
        for order in orders {
            let entries = order
                .into_iter()
                .map(|(name, enabled)| (name.to_string(), enabled));
            let resolved = resolve_legacy_tools_actions(entries);
            assert!(
                matches!(resolved.get("edit"), Some(PermissionAction::Deny)),
                "Deny must win regardless of input order: {resolved:?}"
            );
        }

        // 別名でないツールは独自キーと値を保つ（マッピング回帰の確認）。
        let resolved = resolve_legacy_tools_actions(vec![
            ("bash".to_string(), true),
            ("read".to_string(), false),
        ]);
        assert!(matches!(
            resolved.get("bash"),
            Some(PermissionAction::Allow)
        ));
        assert!(matches!(resolved.get("read"), Some(PermissionAction::Deny)));
    }

    /// 回帰 (CFG2-02): 実際の変換パスでも競合する別名が Deny に解決すること。
    #[test]
    fn legacy_tools_conflicting_edit_aliases_end_to_end_is_deny() {
        let mut config: Config =
            serde_json::from_str(r#"{"tools": {"write": true, "edit": false}}"#)
                .expect("legacy tools config must parse");
        apply_post_load_transforms(&mut config);

        assert!(
            config.tools.is_none(),
            "legacy tools field must be consumed"
        );
        let rules = config.permission.expect("permission set").rules();
        assert!(
            matches!(
                rules.get("edit"),
                Some(PermissionRule::Action(PermissionAction::Deny))
            ),
            "conflicting edit aliases must resolve to Deny: {rules:?}"
        );
    }

    #[test]
    fn legacy_mode_migration_preserves_explicit_agent_mode() {
        let mut config: Config = serde_json::from_str(
            r#"{
                "mode": {"reviewer": {"model": "legacy-model"}},
                "agent": {"reviewer": {"mode": "subagent"}}
            }"#,
        )
        .expect("config with an explicit agent mode must parse");

        apply_post_load_transforms(&mut config);

        let agent = config
            .agent
            .as_ref()
            .and_then(|agents| agents.entries.get("reviewer"))
            .expect("merged reviewer agent must exist");
        assert!(
            matches!(agent.mode, Some(AgentMode::Subagent)),
            "an explicit agent mode must survive legacy mode migration"
        );
        assert!(
            config.mode.is_none(),
            "the deprecated mode field must be consumed"
        );
    }

    #[test]
    fn legacy_mode_migration_defaults_to_primary_without_explicit_mode() {
        let mut config: Config = serde_json::from_str(
            r#"{
                "mode": {"reviewer": {"model": "legacy-model"}},
                "agent": {"reviewer": {"model": "explicit-model"}}
            }"#,
        )
        .expect("config without an explicit agent mode must parse");

        apply_post_load_transforms(&mut config);

        let agent = config
            .agent
            .as_ref()
            .and_then(|agents| agents.entries.get("reviewer"))
            .expect("merged reviewer agent must exist");
        assert!(
            matches!(agent.mode, Some(AgentMode::Primary)),
            "legacy mode must supply Primary when no explicit mode exists"
        );
    }

    #[test]
    fn missing_explicit_config_warns_with_filename_only() {
        const ENV_VAR: &str = "OPENCODE_CONFIG";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = TestDir::new("opencode_missing_explicit_config");
        let missing = temp.path.join("my-opencode.jsonc");
        std::env::set_var(ENV_VAR, &missing);

        let buffer = WarningBuffer::default();
        let subscriber = WarningSubscriber {
            buffer: buffer.clone(),
        };
        let mut loader = ConfigLoader::new().without_global_config();
        tracing::subscriber::with_default(subscriber, || {
            loader
                .load_from_env()
                .expect("a missing explicit config must be ignored, not fatal");
        });
        std::env::remove_var(ENV_VAR);

        let text = buffer.text();
        assert!(text.contains("my-opencode.jsonc"), "{text}");
        assert!(text.contains("does not exist"), "{text}");
        // The absolute path (temp dir prefix) must never be logged.
        let full_path = temp.path.to_string_lossy().into_owned();
        assert!(
            !text.contains(full_path.as_str()),
            "full path leaked: {text}"
        );
    }

    #[test]
    fn missing_find_up_candidates_do_not_warn() {
        const ENV_VAR: &str = "OPENCODE_CONFIG_DIR";
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = TestDir::new("opencode_find_up_no_warning");
        let project = temp.path.join("repo");
        fs::create_dir_all(&project).unwrap();
        // Stop find-up here so no ancestor config is consulted.
        fs::create_dir_all(project.join(".git")).unwrap();
        std::env::remove_var(ENV_VAR);

        let buffer = WarningBuffer::default();
        let subscriber = WarningSubscriber {
            buffer: buffer.clone(),
        };
        let mut loader = ConfigLoader::new().without_global_config();
        tracing::subscriber::with_default(subscriber, || {
            loader
                .load_project(&project)
                .expect("project load must succeed without config files");
        });

        let text = buffer.text();
        assert!(
            !text.contains("does not exist") && !text.contains("explicitly specified config file"),
            "find-up candidates must not warn when absent: {text}"
        );
    }

    // ---- CFG2-10: ディレクトリ走査順に依存しない決定的な勝者 --------------

    fn write_test_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    /// glob_md_files は作成順（read_dir 順）によらず常にパス順で返す。
    #[test]
    fn glob_md_files_returns_path_sorted_results() {
        let temp = TestDir::new("opencode_glob_sorted");
        // わざと辞書順ではない作成順にする。
        write_test_file(&temp.path.join("commands/zeta.md"), "z");
        write_test_file(&temp.path.join("commands/alpha.md"), "a");
        write_test_file(&temp.path.join("commands/nested/omega.md"), "o");
        write_test_file(&temp.path.join("commands/nested/beta.md"), "b");

        let files = glob_md_files(&temp.path).expect("glob must succeed");
        let expected = vec![
            temp.path.join("commands/alpha.md"),
            temp.path.join("commands/nested/beta.md"),
            temp.path.join("commands/nested/omega.md"),
            temp.path.join("commands/zeta.md"),
        ];
        assert_eq!(files, expected, "glob_md_files must return sorted paths");
    }

    /// 同名プラグイン（foo.ts / foo.js）はファイルシステムの列挙順にかかわらず
    /// 常に同じものが勝つ。
    #[test]
    fn load_plugins_from_dir_winner_is_creation_order_independent() {
        fn build(order: &[&str]) -> TestDir {
            let temp = TestDir::new("opencode_plugin_order");
            let dir = temp.path.join("plugin");
            for name in order {
                write_test_file(&dir.join(name), "// plugin");
            }
            temp
        }

        let js_first = build(&["foo.js", "foo.ts"]);
        let ts_first = build(&["foo.ts", "foo.js"]);

        let a = load_plugins_from_dir(&js_first.path);
        let b = load_plugins_from_dir(&ts_first.path);

        // 生のリスト自体がソート済みで、作成順に依存しない。
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(a, sorted, "plugin paths must be returned sorted");
        // 一時ディレクトリ名が異なるため、ディレクトリ部分を除いて比較する。
        let tails = |paths: &Vec<String>| -> Vec<String> {
            paths
                .iter()
                .map(|p| {
                    p.rsplit('/')
                        .take(2)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .collect()
        };
        assert_eq!(
            tails(&a),
            tails(&b),
            "plugin scan must not depend on read_dir order"
        );

        // 名前による dedup 後も同じ勝者が残る（"foo.js" < "foo.ts" なので foo.ts）。
        let dedup_a = deduplicate_plugins(a);
        let dedup_b = deduplicate_plugins(b);
        assert_eq!(
            tails(&dedup_a),
            tails(&dedup_b),
            "dedup winner must be deterministic"
        );
        assert_eq!(dedup_a.len(), 1);
        assert!(
            dedup_a[0].ends_with("foo.ts"),
            "expected foo.ts to be the deterministic winner"
        );
    }

    /// 同名コマンド（command/foo.md と commands/foo.md）も作成順に依存しない。
    #[test]
    fn load_commands_from_dir_winner_is_creation_order_independent() {
        fn build(order: &[(&str, &str)]) -> TestDir {
            let temp = TestDir::new("opencode_command_order");
            for (rel, body) in order {
                write_test_file(&temp.path.join(rel), body);
            }
            temp
        }

        let a_dir = build(&[
            ("command/foo.md", "from command"),
            ("commands/foo.md", "from commands"),
        ]);
        let b_dir = build(&[
            ("commands/foo.md", "from commands"),
            ("command/foo.md", "from command"),
        ]);

        let a = load_commands_from_dir(&a_dir.path);
        let b = load_commands_from_dir(&b_dir.path);

        let a_foo = a.get("foo").expect("foo command");
        let b_foo = b.get("foo").expect("foo command");
        assert_eq!(a_foo.template.as_deref(), Some("from commands"));
        assert_eq!(b_foo.template.as_deref(), Some("from commands"));
    }
}

#[cfg(test)]
mod agent_steps_normalization_tests {
    use super::*;

    #[test]
    fn zero_steps_are_normalized_to_unspecified_and_positive_values_are_kept() {
        let mut config = Config {
            agent: Some(AgentConfigs {
                entries: HashMap::from([
                    (
                        "zero".to_string(),
                        AgentConfig {
                            steps: Some(0),
                            max_steps: Some(0),
                            ..Default::default()
                        },
                    ),
                    (
                        "positive".to_string(),
                        AgentConfig {
                            steps: Some(7),
                            max_steps: Some(12),
                            ..Default::default()
                        },
                    ),
                    (
                        "mixed".to_string(),
                        AgentConfig {
                            steps: Some(0),
                            max_steps: Some(9),
                            ..Default::default()
                        },
                    ),
                ]),
            }),
            ..Default::default()
        };

        normalize_agent_steps(&mut config);

        let agent_configs = config.agent.expect("agent configs");
        let entries = &agent_configs.entries;
        assert_eq!(entries["zero"].steps, None);
        assert_eq!(entries["zero"].max_steps, None);
        assert_eq!(entries["positive"].steps, Some(7));
        assert_eq!(entries["positive"].max_steps, Some(12));
        assert_eq!(entries["mixed"].steps, None);
        assert_eq!(entries["mixed"].max_steps, Some(9));
    }

    #[test]
    fn load_all_normalizes_zero_steps_from_config_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        // find-up をここで止め、祖先の設定へ依存しないようにする。
        fs::create_dir_all(temp.path().join(".git")).expect("git dir");
        fs::write(
            temp.path().join("opencode.json"),
            r#"{"agent":{"zero":{"steps":0,"max_steps":0},"positive":{"steps":3,"max_steps":5}}}"#,
        )
        .expect("config file");

        let mut loader = ConfigLoader::new().without_global_config();
        let config = loader.load_all(temp.path()).expect("load config");

        let agent_configs = config.agent.expect("agent configs");
        let entries = &agent_configs.entries;
        assert_eq!(entries["zero"].steps, None);
        assert_eq!(entries["zero"].max_steps, None);
        assert_eq!(entries["positive"].steps, Some(3));
        assert_eq!(entries["positive"].max_steps, Some(5));
    }

    // ── R2 regression: PATCH 経由の更新にも post-load transforms を適用 ──
    #[test]
    fn update_config_applies_post_load_transforms_like_startup() {
        let temp = tempfile::tempdir().expect("temp dir");
        // find-up を止め、祖先の設定へ依存しないようにする。
        fs::create_dir_all(temp.path().join(".git")).expect("git dir");
        fs::write(
            temp.path().join("opencode.json"),
            r#"{"agent":{"zero":{"steps":0,"max_steps":0}}}"#,
        )
        .expect("config file");

        let patch: Config = serde_json::from_str(
            r#"{
                "mode": {"legacy-agent": {"model": "legacy-model"}},
                "tools": {"write": true},
                "agent": {"zero": {"steps": 0, "max_steps": 0}}
            }"#,
        )
        .expect("patch");

        update_config(temp.path(), &patch).expect("update config");

        let written = fs::read_to_string(temp.path().join("opencode.json")).expect("written");
        let updated: Config = serde_json::from_str(&written).expect("parse written");

        // 起動時ロードと同じ正規化結果になること。
        let mut loader = ConfigLoader::new().without_global_config();
        let loaded = loader.load_all(temp.path()).expect("load config");

        for config in [&updated, &loaded] {
            assert!(config.mode.is_none(), "mode must be migrated to agent");
            assert!(
                config.tools.is_none(),
                "tools must be migrated to permission"
            );
            let entries = &config.agent.as_ref().expect("agent configs").entries;
            let zero = entries.get("zero").expect("zero agent");
            assert_eq!(zero.steps, None, "steps=0 must be normalized");
            assert_eq!(zero.max_steps, None, "max_steps=0 must be normalized");
            let legacy = entries.get("legacy-agent").expect("legacy agent");
            assert_eq!(legacy.model.as_deref(), Some("legacy-model"));
            assert!(config.permission.is_some(), "permission must be populated");
        }
    }
}

#[cfg(test)]
mod provider_normalization_tests {
    use super::*;

    // env はプロセスグローバルなため、このモジュールの override テストも
    // 直列化し、panic 時も元の値へ復元する（値は一切ログへ出さない）。
    static ENV_OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvRestoreGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvRestoreGuard {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                original: std::env::var_os(key),
            }
        }
    }

    impl Drop for EnvRestoreGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn current_schema_provider() -> ProviderConfig {
        ProviderConfig {
            name: Some("Custom".to_string()),
            package: Some("@opencode/ai/providers/openai-compatible".to_string()),
            settings: Some(HashMap::from([
                (
                    "baseURL".to_string(),
                    serde_json::json!("https://example.com/v1"),
                ),
                ("apiKey".to_string(), serde_json::json!("secret")),
                ("extra".to_string(), serde_json::json!(true)),
            ])),
            headers: Some(HashMap::from([(
                "X-Test".to_string(),
                serde_json::json!("1"),
            )])),
            compatibility: Some(HashMap::from([(
                "maxTokensField".to_string(),
                serde_json::json!("max_tokens"),
            )])),
            models: Some(HashMap::from([(
                "m".to_string(),
                crate::schema::ModelConfig {
                    name: Some("M".to_string()),
                    model: Some("underlying".to_string()),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
    }

    #[test]
    fn maps_current_schema_provider_into_runtime_shape() {
        let mut config = Config {
            providers: Some(HashMap::from([(
                "custom".to_string(),
                current_schema_provider(),
            )])),
            ..Default::default()
        };

        normalize_provider_configs(&mut config);

        assert!(config.providers.is_none(), "providers must be folded");
        let provider = config
            .provider
            .as_ref()
            .and_then(|map| map.get("custom"))
            .expect("custom provider");
        assert_eq!(
            provider.npm.as_deref(),
            Some("@opencode/ai/providers/openai-compatible")
        );
        assert_eq!(provider.base_url.as_deref(), Some("https://example.com/v1"));
        assert_eq!(provider.api_key.as_deref(), Some("secret"));
        let options = provider.options.as_ref().expect("options");
        assert_eq!(options.get("extra"), Some(&serde_json::json!(true)));
        assert_eq!(
            options.get("headers"),
            Some(&serde_json::json!({"X-Test": "1"}))
        );
        assert_eq!(
            options.get("compatibility"),
            Some(&serde_json::json!({"maxTokensField": "max_tokens"}))
        );
        let model = provider
            .models
            .as_ref()
            .and_then(|models| models.get("m"))
            .expect("model");
        assert_eq!(model.name.as_deref(), Some("M"));
        assert_eq!(model.model.as_deref(), Some("underlying"));
    }

    #[test]
    fn legacy_provider_map_wins_over_current_schema() {
        let mut config = Config {
            provider: Some(HashMap::from([(
                "custom".to_string(),
                ProviderConfig {
                    name: Some("Legacy".to_string()),
                    ..Default::default()
                },
            )])),
            providers: Some(HashMap::from([(
                "custom".to_string(),
                current_schema_provider(),
            )])),
            ..Default::default()
        };

        normalize_provider_configs(&mut config);

        let provider = config
            .provider
            .as_ref()
            .and_then(|map| map.get("custom"))
            .expect("custom provider");
        assert_eq!(provider.name.as_deref(), Some("Legacy"));
    }

    #[test]
    fn parses_model_id_alias() {
        let config: Config = serde_json::from_str(
            r#"{"provider":{"openrouter":{"models":{"free":{"name":"Free","modelID":"free-model"}}}}}"#,
        )
        .unwrap();
        let model = config
            .provider
            .unwrap()
            .get("openrouter")
            .and_then(|p| p.models.as_ref())
            .and_then(|m| m.get("free"))
            .cloned()
            .expect("model");
        assert_eq!(model.model.as_deref(), Some("free-model"));
    }

    #[test]
    fn get_plugin_name_from_npm_and_file() {
        assert_eq!(get_plugin_name("@scope/pkg@1.2.3"), "@scope/pkg");
        assert_eq!(get_plugin_name("plain-plugin"), "plain-plugin");
        assert_eq!(get_plugin_name("file:///tmp/my-plugin.js"), "my-plugin");
    }

    #[test]
    fn get_plugin_name_keeps_traversal_and_escaped_specs_verbatim() {
        assert_eq!(get_plugin_name("file://../p.js"), "file://../p.js");
        assert_eq!(get_plugin_name("file://%2e%2e/p.js"), "file://%2e%2e/p.js");
        assert_eq!(get_plugin_name("file://p%2ejs"), "file://p%2ejs");
    }

    #[test]
    fn deduplicate_plugins_keeps_last_winning_order() {
        let plugins = vec!["pkg@1".to_owned(), "other@1".to_owned(), "pkg@2".to_owned()];
        assert_eq!(
            deduplicate_plugins(plugins),
            vec!["other@1".to_owned(), "pkg@2".to_owned()]
        );
    }

    #[test]
    fn deduplicate_plugins_does_not_merge_traversal_spec_with_file_spec() {
        let plugins = vec!["file://../p.js".to_owned(), "file:///tmp/p.js".to_owned()];
        assert_eq!(
            deduplicate_plugins(plugins),
            vec!["file://../p.js".to_owned(), "file:///tmp/p.js".to_owned()]
        );
    }

    #[test]
    fn split_frontmatter_with_delimiters() {
        let content = "---\nname: test\n---\nbody text";
        let (frontmatter, body) = split_frontmatter(content);
        assert_eq!(frontmatter.as_deref(), Some("name: test"));
        assert_eq!(body, "\nbody text");
    }

    #[test]
    fn split_frontmatter_without_delimiters() {
        let content = "just body";
        let (frontmatter, body) = split_frontmatter(content);
        assert!(frontmatter.is_none());
        assert_eq!(body, "just body");
    }

    #[test]
    fn substitute_env_vars_replaces_pattern() {
        // Env vars are process-global: serialize with the other override
        // tests and restore the original value even if this test panics.
        let _guard = ENV_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const VAR: &str = "OPENCODE_TEST_ENV_VAR";
        let _restore = EnvRestoreGuard::new(VAR);
        std::env::set_var(VAR, "resolved-value");
        // A placeholder outside a JSON string literal is emitted as a complete
        // JSON string literal (quotes included), not as raw source text.
        let result = substitute_env_vars("prefix:{env:OPENCODE_TEST_ENV_VAR}:suffix", None);
        assert_eq!(result, "prefix:\"resolved-value\":suffix");
    }
}
