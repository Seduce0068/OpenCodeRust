//! Plugin discovery, npm installation, and subprocess lifecycle management.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::RwLock;
use url::Url;

use super::auth::PluginAuthBridge;
use super::client::{PluginContext, PluginSubprocess, PluginSubprocessError};
use super::runtime::{detect_runtime, JsRuntime};
use crate::{Hook, HookContext, HookError, HookEvent, HookOutput, PluginSystem};

// ---------------------------------------------------------------------------
// Embedded host script
// ---------------------------------------------------------------------------

const HOST_SCRIPT: &str = include_str!("../../host/plugin-host.ts");
const BUILTIN_CODEX_AUTH: &str = include_str!("../../builtin/codex-auth.ts");
const BUILTIN_COPILOT_AUTH: &str = include_str!("../../builtin/copilot-auth.ts");

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PluginLoaderError {
    #[error("no JS runtime found (install bun, deno, or node)")]
    NoRuntime,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("subprocess error: {0}")]
    Subprocess(#[from] PluginSubprocessError),

    #[error("npm install failed: {0}")]
    NpmInstall(String),

    #[error("invalid npm plugin spec `{0}`: expected a lowercase npm name with an explicit fixed version (e.g. @scope/pkg@1.2.3)")]
    InvalidNpmSpec(String),

    #[error("plugin path is not absolute and cannot be converted to a file URL: {0:?}")]
    InvalidPluginPath(PathBuf),
}

// ---------------------------------------------------------------------------
// PluginLoader
// ---------------------------------------------------------------------------

pub struct PluginLoader {
    clients: RwLock<Vec<Arc<PluginSubprocess>>>,
    /// Auth bridges for plugins that declare auth, keyed by provider ID.
    auth_bridges: RwLock<HashMap<String, Arc<PluginAuthBridge>>>,
    hook_system: Arc<PluginSystem>,
    runtime: JsRuntime,
    host_script_path: PathBuf,
}

impl PluginLoader {
    /// Create a new loader. Detects the JS runtime and writes the host script
    /// to `~/.cache/opencode/plugin-host.ts`.
    pub fn new() -> Result<Self, PluginLoaderError> {
        let runtime = detect_runtime().ok_or(PluginLoaderError::NoRuntime)?;

        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("opencode");
        std::fs::create_dir_all(&cache_dir)?;

        let host_script_path = cache_dir.join("plugin-host.ts");
        std::fs::write(&host_script_path, HOST_SCRIPT)?;

        Ok(Self {
            clients: RwLock::new(Vec::new()),
            auth_bridges: RwLock::new(HashMap::new()),
            hook_system: Arc::new(PluginSystem::new()),
            runtime,
            host_script_path,
        })
    }

    /// Load all plugins from the given spec list.
    ///
    /// Each spec is either:
    /// - `file:///path/to/plugin.ts` — loaded directly
    /// - An npm package name (e.g. `opencode-anthropic-auth@0.0.13`)
    pub async fn load_all(
        &self,
        specs: &[String],
        context: &PluginContext,
    ) -> Result<(), PluginLoaderError> {
        // Collect npm packages that need installing
        let npm_specs: Vec<&str> = specs
            .iter()
            .filter(|s| !s.starts_with("file://"))
            .map(|s| s.as_str())
            .collect();

        if !npm_specs.is_empty() {
            self.install_npm_packages(&npm_specs).await?;
        }

        // Spawn each plugin
        for spec in specs {
            let plugin_path = self.resolve_plugin(spec)?;
            // For npm packages (non file:// specs), set cwd to the npm dir
            // so bare-specifier imports resolve against node_modules/.
            let cwd = if !spec.starts_with("file://") {
                Some(self.npm_dir())
            } else {
                None
            };
            match PluginSubprocess::spawn(
                self.runtime,
                self.host_script_path.to_str().unwrap_or("plugin-host.ts"),
                &plugin_path,
                context.clone(),
                cwd.as_deref(),
            )
            .await
            {
                Ok(client) => {
                    tracing::info!(
                        plugin = client.name(),
                        hooks = ?client.hooks(),
                        has_auth = client.auth_meta().is_some(),
                        "loaded TS plugin"
                    );
                    let client = Arc::new(client);

                    // If the plugin provides auth, create an auth bridge
                    if let Some(auth_meta) = client.auth_meta().cloned() {
                        let provider = auth_meta.provider.clone();
                        let bridge =
                            Arc::new(PluginAuthBridge::new(Arc::clone(&client), auth_meta));
                        tracing::info!(
                            plugin = client.name(),
                            provider = provider.as_str(),
                            methods = ?bridge.methods(),
                            "registered plugin auth bridge"
                        );
                        let mut bridges = self.auth_bridges.write().await;
                        bridges.insert(provider, bridge);
                    }
                    self.register_client_hooks(Arc::clone(&client)).await;

                    let mut clients = self.clients.write().await;
                    clients.push(client);
                }
                Err(e) => {
                    tracing::error!(spec = spec.as_str(), error = %e, "failed to load TS plugin");
                }
            }
        }

        Ok(())
    }

    /// Load bundled auth plugins shipped with the Rust runtime.
    pub async fn load_builtins(&self, context: &PluginContext) -> Result<(), PluginLoaderError> {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("opencode")
            .join("plugins")
            .join("builtin");
        std::fs::create_dir_all(&cache_dir)?;

        let codex_path = cache_dir.join("builtin-codex-auth.ts");
        std::fs::write(&codex_path, BUILTIN_CODEX_AUTH)?;

        let copilot_path = cache_dir.join("builtin-copilot-auth.ts");
        std::fs::write(&copilot_path, BUILTIN_COPILOT_AUTH)?;

        let specs = vec![file_url(&codex_path)?, file_url(&copilot_path)?];
        self.load_all(&specs, context).await
    }

    /// Get all loaded plugin clients.
    pub async fn clients(&self) -> Vec<Arc<PluginSubprocess>> {
        self.clients.read().await.clone()
    }

    /// Get the hook system this loader registers bridge hooks into.
    pub fn hook_system(&self) -> Arc<PluginSystem> {
        Arc::clone(&self.hook_system)
    }

    /// Shut down all plugin subprocesses.
    pub async fn shutdown_all(&self) {
        let clients = self.clients.read().await;
        for client in clients.iter() {
            if let Err(e) = client.shutdown().await {
                tracing::warn!(plugin = client.name(), error = %e, "error shutting down plugin");
            }
        }
    }

    /// Get the auth bridge for a given provider ID, if any plugin provides it.
    pub async fn auth_bridge(&self, provider: &str) -> Option<Arc<PluginAuthBridge>> {
        self.auth_bridges.read().await.get(provider).cloned()
    }

    /// Get all registered auth bridges, keyed by provider ID.
    pub async fn auth_bridges(&self) -> HashMap<String, Arc<PluginAuthBridge>> {
        self.auth_bridges.read().await.clone()
    }

    // -- Private helpers ----------------------------------------------------

    async fn register_client_hooks(&self, client: Arc<PluginSubprocess>) {
        for hook_name in client.hooks() {
            let Some(event) = super::hook_name_to_event(hook_name) else {
                tracing::debug!(
                    plugin = client.name(),
                    hook = hook_name.as_str(),
                    "skipping unsupported TS hook"
                );
                continue;
            };

            let hook_id = format!("ts:{}:{}", client.name(), hook_name);
            let plugin_name = client.name().to_string();
            let hook_name_owned = hook_name.clone();
            let hook_client = Arc::clone(&client);

            // Avoid duplicate registrations when load_all() is called more than once.
            let _ = self.hook_system.remove(&event, &hook_id).await;
            self.hook_system
                .register(Hook::new(&hook_id, event, move |context: HookContext| {
                    let hook_client = Arc::clone(&hook_client);
                    let hook_name_owned = hook_name_owned.clone();
                    let plugin_name = plugin_name.clone();
                    async move {
                        let (input, output) = hook_io_from_context(&context);
                        let value = hook_client
                            .invoke_hook(&hook_name_owned, input, output)
                            .await
                            .map_err(|err| {
                                HookError::ExecutionError(format!(
                                    "TS plugin `{}` hook `{}` failed: {}",
                                    plugin_name, hook_name_owned, err
                                ))
                            })?;
                        Ok(HookOutput::with_payload(value))
                    }
                }))
                .await;
        }
    }

    /// Resolve a plugin spec to a path that the host can `import()`.
    ///
    /// For npm packages, we return the bare package name (not a `file://` URL)
    /// because `import("file:///path/to/dir")` doesn't resolve `package.json`
    /// exports — only bare specifiers trigger full module resolution.
    /// The subprocess working directory is set to `npm_dir()` so the runtime
    /// finds the package in `node_modules/`.
    fn resolve_plugin(&self, spec: &str) -> Result<String, PluginLoaderError> {
        if spec.starts_with("file://") {
            return Ok(spec.to_string());
        }

        // npm package — return bare package name for proper module resolution.
        // Reuse parse_npm_spec so scoped names (e.g. @scope/pkg@1.0) keep their
        // leading `@scope/` instead of being truncated at the first `@`.
        let (name, _version) = parse_npm_spec(spec)?;
        Ok(name.to_string())
    }

    /// Install npm packages into the shared cache directory.
    async fn install_npm_packages(&self, specs: &[&str]) -> Result<(), PluginLoaderError> {
        let npm_dir = self.npm_dir();
        std::fs::create_dir_all(&npm_dir)?;

        // Write/update package.json
        let pkg_json = npm_dir.join("package.json");
        let mut deps = serde_json::Map::new();

        // Read existing deps if present
        if pkg_json.exists() {
            if let Ok(content) = std::fs::read_to_string(&pkg_json) {
                if let Ok(existing) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(obj) = existing.get("dependencies").and_then(|d| d.as_object()) {
                        deps = obj.clone();
                    }
                }
            }
        }

        // Add new packages
        for spec in specs {
            let (name, version) = parse_npm_spec(spec)?;
            deps.insert(
                name.to_string(),
                serde_json::Value::String(version.to_string()),
            );
        }

        let pkg = serde_json::json!({
            "name": "opencode-plugins",
            "private": true,
            "dependencies": deps,
        });
        std::fs::write(&pkg_json, serde_json::to_string_pretty(&pkg).unwrap())?;

        // Run install
        let install_cmd = self.runtime.install_command();
        let install_args = self.runtime.install_args();

        let status = tokio::process::Command::new(install_cmd)
            .args(&install_args)
            .current_dir(&npm_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .status()
            .await?;

        if !status.success() {
            return Err(PluginLoaderError::NpmInstall(format!(
                "{} install exited with {}",
                install_cmd, status
            )));
        }

        Ok(())
    }

    fn npm_dir(&self) -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("opencode")
            .join("plugins")
    }
}

/// Build an encoded, local `file://` URL for a plugin path.
///
/// `Url::from_file_path` percent-encodes URL-significant characters (`#`, `?`,
/// `%`, spaces) and yields a localhost/empty-host URL on every platform.
/// `format!("file://{}", path.display())` instead breaks on Windows (`file://C:\...`,
/// rejected by the host's local-host check) and for paths containing those
/// characters.
fn file_url(path: &Path) -> Result<String, PluginLoaderError> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| PluginLoaderError::InvalidPluginPath(path.to_path_buf()))
}

/// Validate an npm package name using the same lowercase-only grammar as the
/// host (`[a-z0-9][a-z0-9._-]*`) plus npm's ban on leading/trailing/repeated
/// dots. Scoped names (`@scope/pkg`) validate both the scope and the leaf.
fn is_valid_npm_name(name: &str) -> bool {
    let leaf = match name.strip_prefix('@') {
        Some(rest) => match rest.split_once('/') {
            Some((scope, leaf)) if is_valid_npm_name_part(scope) => leaf,
            _ => return false,
        },
        None => name,
    };
    is_valid_npm_name_part(leaf)
}

/// One name component: `[a-z0-9][a-z0-9._-]*`, no leading/trailing or repeated
/// dot, within npm's 214-character limit.
fn is_valid_npm_name_part(part: &str) -> bool {
    if part.is_empty() || part.len() > 214 {
        return false;
    }
    let mut bytes = part.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if part.starts_with('.') || part.ends_with('.') || part.contains("..") {
        return false;
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Accept only fixed versions (`1.2.3`, `1.2.3-beta.1+build.5`), never dist-tags
/// (`latest`) or ranges (`^1.0.0`, `1.x`, `>=1.0.0`).
fn is_fixed_version(version: &str) -> bool {
    let mut bytes = version.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    if !bytes
        .all(|b| b.is_ascii_digit() || b.is_ascii_alphabetic() || matches!(b, b'.' | b'+' | b'-'))
    {
        return false;
    }
    // `1.x` / `1.X` select a range, so they are not pinned versions.
    !version.split('.').any(|part| part == "x" || part == "X")
}

/// Parse `"pkg@version"` into `(name, version)`. Handles scoped packages like
/// `"@scope/pkg@1.0"`.
///
/// An explicit fixed version and a valid lowercase npm name are required;
/// dist-tags, ranges, wildcards and malformed names are rejected so that
/// repeated installs cannot silently resolve to a different release.
fn parse_npm_spec(spec: &str) -> Result<(&str, &str), PluginLoaderError> {
    // Handle scoped packages: @scope/pkg@version
    let (name, version) = if let Some(stripped) = spec.strip_prefix('@') {
        if let Some(idx) = stripped.find('@') {
            let split = idx + 1;
            (&spec[..split], &spec[split + 1..])
        } else {
            (spec, "")
        }
    } else if let Some(idx) = spec.find('@') {
        (&spec[..idx], &spec[idx + 1..])
    } else {
        (spec, "")
    };

    if !is_valid_npm_name(name) || !is_fixed_version(version) {
        return Err(PluginLoaderError::InvalidNpmSpec(spec.to_string()));
    }

    Ok((name, version))
}

fn hook_io_from_context(context: &HookContext) -> (Value, Value) {
    let source = context_values(context);
    let mut input = Map::new();
    let mut output = Map::new();

    match context.event {
        HookEvent::ToolDefinition => {
            copy_first(&source, &mut input, "toolID", &["toolID"]);
            copy_first(&source, &mut output, "description", &["description"]);
            copy_first(&source, &mut output, "parameters", &["parameters"]);
        }
        HookEvent::ToolExecuteBefore => {
            copy_first(&source, &mut input, "tool", &["tool"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "callID", &["callID"]);
            copy_first(&source, &mut output, "args", &["args"]);
        }
        HookEvent::ToolExecuteAfter => {
            copy_first(&source, &mut input, "tool", &["tool"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "callID", &["callID"]);
            copy_first(&source, &mut input, "args", &["args"]);
            copy_first(&source, &mut input, "error", &["error"]);
            copy_first(&source, &mut output, "title", &["title"]);
            copy_first(&source, &mut output, "output", &["output"]);
            copy_first(&source, &mut output, "metadata", &["metadata"]);
        }
        HookEvent::ChatSystemTransform => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut output, "system", &["system"]);
        }
        HookEvent::ChatMessagesTransform => {
            copy_first(&source, &mut output, "messages", &["messages"]);
        }
        HookEvent::ChatParams => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "agent", &["agent"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut input, "provider", &["provider"]);
            if !input.contains_key("provider") {
                if let Some(provider) = synthesize_provider(&source) {
                    input.insert("provider".to_string(), provider);
                }
            }
            copy_first(&source, &mut input, "message", &["message"]);
            copy_first(&source, &mut output, "temperature", &["temperature"]);
            copy_first(&source, &mut output, "topP", &["topP"]);
            copy_first(&source, &mut output, "topK", &["topK"]);
            copy_first(&source, &mut output, "options", &["options"]);
            copy_first(&source, &mut output, "maxTokens", &["maxTokens"]);
        }
        HookEvent::ChatHeaders => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "agent", &["agent"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut input, "provider", &["provider"]);
            if !input.contains_key("provider") {
                if let Some(provider) = synthesize_provider(&source) {
                    input.insert("provider".to_string(), provider);
                }
            }
            copy_first(&source, &mut input, "message", &["message"]);
            copy_first(&source, &mut output, "headers", &["headers"]);
        }
        HookEvent::ChatMessage => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "agent", &["agent"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut input, "messageID", &["messageID"]);
            copy_first(&source, &mut input, "variant", &["variant"]);
            copy_first(&source, &mut input, "has_tool_calls", &["has_tool_calls"]);
            copy_first(&source, &mut output, "message", &["message"]);
            copy_first(&source, &mut output, "parts", &["parts"]);
        }
        HookEvent::SessionCompacting => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "auto", &["auto"]);
            copy_first(&source, &mut input, "completed", &["completed"]);
            copy_first(&source, &mut output, "context", &["context"]);
            copy_first(&source, &mut output, "prompt", &["prompt"]);
        }
        HookEvent::TextComplete => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "messageID", &["messageID"]);
            copy_first(&source, &mut input, "partID", &["partID"]);
            copy_first(&source, &mut output, "text", &["text"]);
        }
        HookEvent::ShellEnv => {
            copy_first(&source, &mut input, "cwd", &["cwd"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "callID", &["callID"]);
            copy_first(&source, &mut output, "env", &["env"]);
        }
        HookEvent::CommandExecuteBefore => {
            copy_first(&source, &mut input, "command", &["command"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "arguments", &["arguments"]);
            copy_first(&source, &mut input, "source", &["source"]);
            copy_first(&source, &mut output, "parts", &["parts"]);
        }
        HookEvent::PermissionAsk => {
            copy_first(&source, &mut input, "permission", &["permission"]);
            copy_first(
                &source,
                &mut input,
                "permission_type",
                &["permission_type", "permissionType"],
            );
            copy_first(
                &source,
                &mut input,
                "permission_id",
                &["permission_id", "permissionID"],
            );
            copy_first(&source, &mut output, "status", &["status"]);
        }
        _ => {
            input = source.clone();
            output = source;
        }
    }

    seed_hook_output(context.event.clone(), &mut output);

    (Value::Object(input), Value::Object(output))
}

fn context_values(context: &HookContext) -> Map<String, Value> {
    let mut values = Map::new();
    for (key, value) in &context.data {
        values.insert(key.clone(), value.clone());
        let normalized = normalize_hook_key(key);
        if normalized != *key {
            values.entry(normalized).or_insert_with(|| value.clone());
        }
    }
    if let Some(session_id) = &context.session_id {
        values
            .entry("sessionID".to_string())
            .or_insert_with(|| Value::String(session_id.clone()));
    }
    values
}

fn first_value(source: &Map<String, Value>, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|key| source.get(*key).cloned())
}

fn copy_first(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    target_key: &str,
    candidate_keys: &[&str],
) {
    if let Some(value) = first_value(source, candidate_keys) {
        target.insert(target_key.to_string(), value);
    }
}

fn synthesize_model(source: &Map<String, Value>) -> Option<Value> {
    let model_id = first_value(source, &["modelID", "model_id"])?;
    let mut model = Map::new();
    model.insert("modelID".to_string(), model_id.clone());
    model.insert("id".to_string(), model_id);
    if let Some(provider_id) = first_value(source, &["providerID", "provider_id"]) {
        model.insert("providerID".to_string(), provider_id);
    }
    Some(Value::Object(model))
}

fn synthesize_provider(source: &Map<String, Value>) -> Option<Value> {
    let provider_id = first_value(source, &["providerID", "provider_id"])?;
    let mut provider = Map::new();
    provider.insert("id".to_string(), provider_id.clone());
    provider.insert(
        "info".to_string(),
        Value::Object(Map::from_iter([("id".to_string(), provider_id)])),
    );
    Some(Value::Object(provider))
}

fn normalize_hook_key(key: &str) -> String {
    match key {
        "tool_id" => "toolID".to_string(),
        "call_id" => "callID".to_string(),
        "model_id" => "modelID".to_string(),
        "provider_id" => "providerID".to_string(),
        "message_id" => "messageID".to_string(),
        "part_id" => "partID".to_string(),
        "max_tokens" => "maxTokens".to_string(),
        _ => key.to_string(),
    }
}

fn ensure_default(map: &mut Map<String, Value>, key: &str, value: Value) {
    map.entry(key.to_string()).or_insert(value);
}

fn ensure_object(map: &mut Map<String, Value>, key: &str) {
    ensure_default(map, key, Value::Object(Map::new()));
}

fn ensure_array(map: &mut Map<String, Value>, key: &str) {
    ensure_default(map, key, Value::Array(Vec::new()));
}

fn seed_hook_output(event: HookEvent, output: &mut Map<String, Value>) {
    match event {
        HookEvent::ToolDefinition => {
            ensure_default(output, "description", Value::String(String::new()));
            ensure_object(output, "parameters");
        }
        HookEvent::ToolExecuteBefore => {
            ensure_object(output, "args");
        }
        HookEvent::ToolExecuteAfter => {
            ensure_default(output, "title", Value::String(String::new()));
            ensure_default(output, "output", Value::String(String::new()));
            ensure_object(output, "metadata");
        }
        HookEvent::ChatHeaders => {
            ensure_object(output, "headers");
        }
        HookEvent::ChatParams => {
            ensure_default(output, "temperature", Value::Null);
            ensure_default(output, "topP", Value::Null);
            ensure_default(output, "topK", Value::Null);
            ensure_object(output, "options");
        }
        HookEvent::ChatMessage => {
            ensure_default(output, "message", Value::Null);
            ensure_array(output, "parts");
        }
        HookEvent::ChatMessagesTransform => {
            ensure_array(output, "messages");
        }
        HookEvent::ChatSystemTransform => {
            ensure_array(output, "system");
        }
        HookEvent::SessionCompacting => {
            ensure_array(output, "context");
        }
        HookEvent::TextComplete => {
            ensure_default(output, "text", Value::String(String::new()));
        }
        HookEvent::ShellEnv => {
            ensure_object(output, "env");
        }
        HookEvent::CommandExecuteBefore => {
            ensure_array(output, "parts");
        }
        HookEvent::PermissionAsk => {
            ensure_default(output, "status", Value::String("ask".to_string()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_npm_spec() {
        assert_eq!(parse_npm_spec("foo@1.0.0").unwrap(), ("foo", "1.0.0"));
        assert_eq!(
            parse_npm_spec("@scope/foo@2.0").unwrap(),
            ("@scope/foo", "2.0")
        );
        assert_eq!(
            parse_npm_spec("foo-bar_baz.qux@1.2.3-beta.1+build.5").unwrap(),
            ("foo-bar_baz.qux", "1.2.3-beta.1+build.5")
        );
        assert_eq!(
            parse_npm_spec("@scope/pkg-name@0.0.13").unwrap(),
            ("@scope/pkg-name", "0.0.13")
        );

        // 未指定・空・ワイルドカードの version はエラー（install 毎の最新解釈を防ぐ）
        for spec in ["foo", "@scope/foo", "foo@", "foo@*", "@scope/foo@"] {
            assert!(
                parse_npm_spec(spec).is_err(),
                "未指定 spec はエラーになるべき: {spec}"
            );
        }
    }

    #[test]
    fn parse_npm_spec_rejects_invalid_names_and_non_fixed_versions() {
        for spec in [
            "Foo@1.0.0",
            "foo..bar@1.0.0",
            ".foo@1.0.0",
            "foo.@1.0.0",
            "@Scope/pkg@1.0.0",
            "@scope/pkg..x@1.0.0",
            "@scope@1.0.0",
            "foo@latest",
            "foo@^1.0.0",
            "foo@~1.2.3",
            "foo@1.x",
            "foo@>=1.0.0",
        ] {
            assert!(
                parse_npm_spec(spec).is_err(),
                "不正 spec は拒否されるべき: {spec}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_url_keeps_plain_absolute_path_shape() {
        assert_eq!(
            file_url(Path::new("/tmp/plugin.ts")).unwrap(),
            "file:///tmp/plugin.ts"
        );
    }

    #[test]
    fn file_url_percent_encodes_url_significant_characters() {
        let path = std::env::temp_dir().join("plugin #1?100%.ts");
        let url = file_url(&path).expect("絶対パスは file URL へ変換できる");
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.contains("%20"), "スペース未エンコード: {url}");
        assert!(url.contains("%23"), "# 未エンコード: {url}");
        assert!(url.contains("%3F"), "? 未エンコード: {url}");
        assert!(url.contains("%25"), "% 未エンコード: {url}");
        assert!(url.ends_with(".ts"), "{url}");
    }

    #[test]
    fn file_url_rejects_relative_paths() {
        assert!(file_url(Path::new("relative/plugin.ts")).is_err());
    }

    #[test]
    fn first_value_returns_first_matching_key() {
        let mut source = Map::new();
        source.insert("modelID".to_string(), serde_json::json!("m1"));
        source.insert("model_id".to_string(), serde_json::json!("m2"));
        assert_eq!(
            first_value(&source, &["modelID", "model_id"]),
            Some(serde_json::json!("m1"))
        );
        assert_eq!(first_value(&source, &["missing"]), None);
    }

    #[test]
    fn synthesize_model_uses_snake_case_fallback() {
        let mut source = Map::new();
        source.insert("model_id".to_string(), serde_json::json!("gpt-5"));
        source.insert("provider_id".to_string(), serde_json::json!("openai"));

        let model = synthesize_model(&source).unwrap();
        assert_eq!(model["modelID"], serde_json::json!("gpt-5"));
        assert_eq!(model["id"], serde_json::json!("gpt-5"));
        assert_eq!(model["providerID"], serde_json::json!("openai"));
    }

    #[test]
    fn synthesize_model_without_id_is_none() {
        assert!(synthesize_model(&Map::new()).is_none());
    }

    #[test]
    fn normalize_hook_key_converts_snake_to_camel() {
        assert_eq!(normalize_hook_key("tool_id"), "toolID");
        assert_eq!(normalize_hook_key("max_tokens"), "maxTokens");
        assert_eq!(normalize_hook_key("custom"), "custom");
    }
}
