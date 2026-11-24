#![allow(clippy::ptr_arg)]
//! Port of upstream packages/opencode/test/config/config.test.ts behaviour.
//! Tests config loading: JSON/JSONC, merge precedence, env substitution.

use opencode_config::{Config, ConfigLoader, PermissionRule};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

// Environment variables are process-global. Serialize every test that sets
// OPENCODE_PERMISSION and every test that reads it through load_all, so a
// concurrent reader cannot observe another test's override (R4).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_config(dir: &PathBuf, content: &str, name: &str) {
    let path = dir.join(name);
    fs::write(path, content).unwrap();
}

fn rule_for(config: &Config, key: &str) -> Option<PermissionRule> {
    config
        .permission
        .as_ref()
        .expect("runtime permission field")
        .rules()
        .remove(key)
}

struct EnvVarGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn loads_json_config_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{"$schema":"https://opencode.ai/config.json","model":"test/model","username":"testuser"}"#,
        "opencode.json",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();

    assert_eq!(config.model.as_deref(), Some("test/model"));
    assert_eq!(config.username.as_deref(), Some("testuser"));
}

#[test]
fn loads_jsonc_config_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{
        // This is a comment
        "$schema": "https://opencode.ai/config.json",
        "model": "test/model",
        "username": "testuser"
    }"#,
        "opencode.jsonc",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    loader.load_from_file(dir.join("opencode.jsonc")).unwrap();
    let config = loader.load_all(&dir).unwrap();

    assert_eq!(config.model.as_deref(), Some("test/model"));
    assert_eq!(config.username.as_deref(), Some("testuser"));
}

#[test]
fn merges_multiple_config_files_with_correct_precedence() {
    // Later-loaded override earlier. Project loads .jsonc then .json; .json wins for overlapping keys.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{"$schema":"https://opencode.ai/config.json","model":"base","username":"base"}"#,
        "opencode.jsonc",
    );
    write_config(
        &dir,
        r#"{"$schema":"https://opencode.ai/config.json","model":"override"}"#,
        "opencode.json",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();

    assert_eq!(config.model.as_deref(), Some("override"));
    assert_eq!(config.username.as_deref(), Some("base"));
}

#[test]
fn load_from_str_parses_and_merges() {
    let mut loader = ConfigLoader::new().without_global_config();
    loader
        .load_from_str(r#"{"model":"anthropic/claude-sonnet-4-20250514"}"#)
        .unwrap();
    let config = loader.get_config();
    assert_eq!(
        config.model.as_deref(),
        Some("anthropic/claude-sonnet-4-20250514")
    );
}

#[test]
fn tolerates_newer_schema_keys() {
    // Regression guard: the user's global config follows the current OpenCode
    // schema, which includes keys this crate does not map yet. Loading must
    // succeed (tolerate) so that an unknown key cannot block CLI startup.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{
            "$schema": "https://opencode.ai/config.json",
            "model": "deepai/deepseek-v4.1-flash",
            "snapshots": true,
            "websearch": true,
            "permissions": [
                {"action": "*", "resource": "*", "effect": "allow"},
                {"action": "read", "resource": ".env", "effect": "deny"}
            ],
            "providers": {
                "custom": {
                    "name": "Custom",
                    "package": "@opencode/ai/providers/openai-compatible",
                    "settings": {"baseURL": "https://example.com/v1", "apiKey": "test"},
                    "headers": {"X-Test": "1"},
                    "compatibility": {"maxTokensField": "max_tokens"},
                    "models": {"m": {"name": "M", "modelID": "underlying", "limit": {"context": 1000, "output": 100}}}
                }
            },
            "mcp": {
                "servers": {
                    "remote": {"type": "remote", "url": "https://example.com/mcp"}
                }
            },
            "agents": {"build": {"model": "test/model"}},
            "experimental": {"todo": true},
            "compaction": {"auto": true, "keep": {"tokens": 1000}},
            "watcher": {"ignore": ["target"]}
        }"#,
        "opencode.jsonc",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();

    assert_eq!(config.model.as_deref(), Some("deepai/deepseek-v4.1-flash"));
    // `permissions` is input-only and is folded into the runtime `permission`
    // map by apply_post_load_transforms; it must no longer remain as a second
    // source of truth.
    assert!(config.permissions.is_none());
    assert!(matches!(
        config.permission,
        Some(opencode_config::PermissionConfig::Map(ref rules)) if !rules.is_empty()
    ));

    // The current-schema `providers` map is folded into the runtime `provider`
    // map, and the `mcp.servers` wrapper is unwrapped into the runtime `mcp`
    // map.
    let custom = config
        .provider
        .as_ref()
        .and_then(|providers| providers.get("custom"))
        .expect("custom provider normalized into provider map");
    assert_eq!(
        custom.npm.as_deref(),
        Some("@opencode/ai/providers/openai-compatible")
    );
    assert_eq!(custom.base_url.as_deref(), Some("https://example.com/v1"));
    assert_eq!(custom.api_key.as_deref(), Some("test"));
    assert!(config.providers.is_none());

    let remote = config
        .mcp
        .as_ref()
        .and_then(|mcp| mcp.servers.as_ref())
        .and_then(|servers| servers.get("remote"))
        .expect("remote mcp server");
    match remote {
        opencode_config::McpServerConfig::Full(server) => {
            assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
        }
        other => panic!("expected Full, got {other:?}"),
    }
    assert!(!config
        .mcp
        .as_ref()
        .and_then(|mcp| mcp.servers.as_ref())
        .is_some_and(|servers| servers.contains_key("servers")));
}

#[test]
fn ignores_real_global_config_when_disabled() {
    // Sanity check for the isolation switch: a temp project config must load
    // even when the machine's global config is unusable.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(&dir, r#"{"model":"project/model"}"#, "opencode.json");

    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();
    assert_eq!(config.model.as_deref(), Some("project/model"));
}

#[test]
fn reads_global_mcp_timeout_and_server_overrides() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{
            "mcp": {
                "timeout": {"startup": 45000, "catalog": 30000, "execution": 600000},
                "servers": {
                    "slow": {
                        "type": "remote",
                        "url": "https://example.com/mcp",
                        "timeout": {"catalog": 60000}
                    },
                    "plain": {
                        "type": "local",
                        "command": ["npx", "server"]
                    }
                }
            }
        }"#,
        "opencode.jsonc",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();

    let section = config.mcp.expect("mcp");
    let global = section.timeout.as_ref().expect("global mcp.timeout");
    assert_eq!(global.per_operation(), (45000, 30000, 600000));

    let mcp = section.servers.expect("servers");
    let slow = match mcp.get("slow").unwrap() {
        opencode_config::McpServerConfig::Full(server) => server,
        other => panic!("expected Full, got {other:?}"),
    };
    let (startup, catalog, execution) =
        opencode_config::McpTimeoutConfig::resolve(Some(global), slow.timeout.as_ref());
    assert_eq!((startup, catalog, execution), (45000, 60000, 600000));

    let plain = match mcp.get("plain").unwrap() {
        opencode_config::McpServerConfig::Full(server) => server,
        other => panic!("expected Full, got {other:?}"),
    };
    assert!(plain.timeout.is_none());
    let (startup, catalog, execution) =
        opencode_config::McpTimeoutConfig::resolve(Some(global), plain.timeout.as_ref());
    assert_eq!((startup, catalog, execution), (45000, 30000, 600000));
    assert!(!mcp.contains_key("timeout"), "timeout is not a server");
}

#[test]
fn project_permissions_rules_override_lower_layer_permission() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // CFGMAP-03-R4: a lower layer's `permission` used to make the final
    // transform drop a higher layer's `permissions` entirely.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{"permission":{"read":"allow","bash":"deny"}}"#,
        "lower-opencode.json",
    );
    write_config(
        &dir,
        r#"{"permissions":[{"action":"read","resource":"","effect":"deny"}]}"#,
        "opencode.json",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    loader
        .load_from_file(dir.join("lower-opencode.json"))
        .unwrap();
    let config = loader.load_all(&dir).unwrap();

    assert!(
        config.permissions.is_none(),
        "input-only permissions field must be consumed"
    );
    assert!(
        matches!(
            rule_for(&config, "read"),
            Some(PermissionRule::Action(
                opencode_config::PermissionAction::Deny
            ))
        ),
        "higher-layer permissions rule must win"
    );
    assert!(
        matches!(
            rule_for(&config, "bash"),
            Some(PermissionRule::Action(
                opencode_config::PermissionAction::Deny
            ))
        ),
        "lower-layer keys not overridden must survive the key-wise merge"
    );
}

#[test]
fn project_permissions_names_override_lower_layer_permission() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{"permission":{"read":"deny"}}"#,
        "lower-opencode.json",
    );
    write_config(&dir, r#"{"permissions":["read"]}"#, "opencode.json");

    let mut loader = ConfigLoader::new().without_global_config();
    loader
        .load_from_file(dir.join("lower-opencode.json"))
        .unwrap();
    let config = loader.load_all(&dir).unwrap();

    assert!(
        matches!(
            rule_for(&config, "read"),
            Some(PermissionRule::Action(
                opencode_config::PermissionAction::Allow
            ))
        ),
        "higher-layer permissions name must win over lower-layer permission"
    );
}

#[test]
fn explicit_permission_wins_over_permissions_in_the_same_layer() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{
            "permission": {"read": "deny"},
            "permissions": [
                {"action":"read","effect":"allow"},
                {"action":"bash","effect":"allow"}
            ]
        }"#,
        "opencode.json",
    );

    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();

    assert!(config.permissions.is_none());
    assert!(
        matches!(
            rule_for(&config, "read"),
            Some(PermissionRule::Action(
                opencode_config::PermissionAction::Deny
            ))
        ),
        "explicit permission keeps priority within one layer"
    );
    assert!(
        rule_for(&config, "bash").is_none(),
        "the discarded permissions list must not leak extra rules"
    );
}

#[test]
fn opencode_permission_env_override_keeps_priority_over_project_permissions() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    write_config(
        &dir,
        r#"{"permissions":[{"action":"read","effect":"allow"}]}"#,
        "opencode.json",
    );

    let _env = EnvVarGuard::set("OPENCODE_PERMISSION", r#"{"read":"deny"}"#);
    let mut loader = ConfigLoader::new().without_global_config();
    let config = loader.load_all(&dir).unwrap();

    assert!(
        matches!(
            rule_for(&config, "read"),
            Some(PermissionRule::Action(
                opencode_config::PermissionAction::Deny
            ))
        ),
        "OPENCODE_PERMISSION must still override project-level permissions"
    );
}
