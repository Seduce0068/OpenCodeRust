use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::Config;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// The JSON shape returned by `{url}/.well-known/opencode`.
#[derive(Debug, Deserialize)]
struct WellKnownResponse {
    #[serde(default)]
    config: Option<serde_json::Value>,
}

/// A single auth entry stored in `auth.json` with `type: "wellknown"`.
#[derive(Debug, Clone, Deserialize)]
struct WellKnownAuth {
    /// Environment variable name to expose the token as.
    key: String,
    /// The token value.
    token: String,
}

/// Wrapper for the full auth.json — we only care about wellknown entries.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum AuthEntry {
    #[serde(rename = "wellknown")]
    WellKnown { key: String, token: String },
    #[serde(other)]
    Other,
}

struct CacheEntry {
    config: Config,
    fetched_at: Instant,
}

static CACHE: Mutex<Option<HashMap<String, CacheEntry>>> = Mutex::new(None);
/// Returns the path to `auth.json` inside the opencode data directory.
fn auth_json_path() -> PathBuf {
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("opencode");
    data_dir.join("auth.json")
}

/// Reads `auth.json` and returns only the wellknown entries (url -> WellKnownAuth).
fn read_wellknown_entries() -> HashMap<String, WellKnownAuth> {
    let path = auth_json_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let raw: HashMap<String, serde_json::Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let mut result = HashMap::new();
    for (url, value) in raw {
        if let Ok(AuthEntry::WellKnown { key, token }) = serde_json::from_value::<AuthEntry>(value)
        {
            result.insert(url, WellKnownAuth { key, token });
        }
    }
    result
}

/// Returns true when an auth.json key is safe to expose as a process
/// environment variable. Blocks names that would alter program loading or
/// lookup behaviour for every later subprocess.
fn is_safe_env_key(key: &str) -> bool {
    const BLOCKED: &[&str] = &[
        "PATH",
        "HOME",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "SHELL",
        "USER",
        "PWD",
        "IFS",
    ];
    let valid = !key.is_empty()
        && key
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_uppercase() || (i > 0 && c.is_ascii_digit()));
    valid && !BLOCKED.contains(&key)
}

/// Injects an auth token into the process environment when its key is safe.
///
/// Shared by both the fetch and the cache-hit paths so a rotated token is
/// always re-injected, even while the config cache is still valid. Only the
/// key and URL are ever logged — the token value is never written to logs.
///
/// Returns `true` when the token was injected, `false` when the key was
/// rejected as unsafe.
fn inject_auth_token(url: &str, auth: &WellKnownAuth) -> bool {
    if !is_safe_env_key(&auth.key) {
        tracing::warn!(url = %url, key = %auth.key, "refusing unsafe env key from auth.json");
        return false;
    }
    std::env::set_var(&auth.key, &auth.token);
    true
}

/// Fetches remote config from all wellknown auth entries and returns a merged
/// `Config` representing the combined remote configuration.
///
/// This is the lowest-priority config source — it will be merged first so that
/// global and project configs override it.
///
/// Network failures are logged as warnings and never prevent startup.
pub async fn load_wellknown() -> Config {
    let entries = read_wellknown_entries();
    if entries.is_empty() {
        return Config::default();
    }
    load_wellknown_from_entries(&entries).await
}

/// Core implementation of [`load_wellknown`], split out so tests can supply
/// entries directly without depending on an on-disk `auth.json`.
async fn load_wellknown_from_entries(entries: &HashMap<String, WellKnownAuth>) -> Config {
    let mut merged = Config::default();
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .unwrap_or_default();

    for (url, auth) in entries {
        // Always (re)inject the token, including on cache hits. Tokens can be
        // rotated while the cached config is still inside its TTL; skipping
        // injection on a hit leaves the stale value in the environment and
        // downstream provider auth keeps using the old token.
        if !inject_auth_token(url, auth) {
            continue;
        }

        // A cache hit still merges the cached config, but only after the
        // token has been refreshed above.
        if let Some(cached) = get_cached(url) {
            tracing::debug!(url = %url, "using cached wellknown config");
            merged.merge(cached);
            continue;
        }

        let endpoint = format!("{}/.well-known/opencode", url.trim_end_matches('/'));
        tracing::debug!(url = %endpoint, "fetching remote wellknown config");

        match fetch_wellknown_config(&client, &endpoint).await {
            Ok(config) => {
                set_cached(url.clone(), config.clone());
                tracing::debug!(url = %url, "loaded remote config from well-known");
                merged.merge(config);
            }
            Err(e) => {
                tracing::warn!(url = %endpoint, error = %e, "failed to fetch wellknown config, skipping");
            }
        }
    }

    merged
}

async fn fetch_wellknown_config(client: &reqwest::Client, endpoint: &str) -> Result<Config> {
    let resp = client.get(endpoint).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} from {}", resp.status(), endpoint);
    }

    let wk: WellKnownResponse = resp.json().await?;
    let config_value = wk
        .config
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let config: Config = serde_json::from_value(config_value)?;
    Ok(config)
}

fn get_cached(url: &str) -> Option<Config> {
    let guard = CACHE.lock().ok()?;
    let map = guard.as_ref()?;
    let entry = map.get(url)?;
    if entry.fetched_at.elapsed() < CACHE_TTL {
        Some(entry.config.clone())
    } else {
        None
    }
}

fn set_cached(url: String, config: Config) {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(
        url,
        CacheEntry {
            config,
            fetched_at: Instant::now(),
        },
    );
}

/// Clears the wellknown config cache. Useful for testing or forced refresh.
pub fn clear_cache() {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env vars and the wellknown cache are process-global: serialize every
    /// test that touches either so they cannot interfere with one another.
    static TEST_ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Restores an environment variable to its original value on drop.
    struct EnvRestoreGuard {
        key: String,
        original: Option<std::ffi::OsString>,
    }

    impl Drop for EnvRestoreGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    #[test]
    fn parses_wellknown_auth_entries_from_json() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        let json = r#"{
            "https://corp.example.com": {
                "type": "wellknown",
                "key": "CORP_TOKEN",
                "token": "secret-123"
            },
            "anthropic": {
                "type": "api",
                "key": "sk-ant-xxx"
            }
        }"#;

        let raw: HashMap<String, serde_json::Value> = serde_json::from_str(json).unwrap();
        let mut result = HashMap::new();
        for (url, value) in raw {
            if let Ok(AuthEntry::WellKnown { key, token }) =
                serde_json::from_value::<AuthEntry>(value)
            {
                result.insert(url, WellKnownAuth { key, token });
            }
        }

        assert_eq!(result.len(), 1);
        let entry = result.get("https://corp.example.com").unwrap();
        assert_eq!(entry.key, "CORP_TOKEN");
        assert_eq!(entry.token, "secret-123");
    }

    #[test]
    fn wellknown_response_parses_config_field() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        let json = r#"{"config": {"model": "claude-3-opus"}}"#;
        let wk: WellKnownResponse = serde_json::from_str(json).unwrap();
        let config_val = wk.config.unwrap();
        let config: Config = serde_json::from_value(config_val).unwrap();
        assert_eq!(config.model.as_deref(), Some("claude-3-opus"));
    }

    #[test]
    fn wellknown_response_handles_missing_config() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        let json = r#"{"auth": {"command": ["login"]}}"#;
        let wk: WellKnownResponse = serde_json::from_str(json).unwrap();
        assert!(wk.config.is_none());
    }

    #[test]
    fn cache_stores_and_retrieves() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        clear_cache();
        let config = Config {
            model: Some("cached-model".to_string()),
            ..Default::default()
        };
        set_cached("https://test.example.com".to_string(), config);
        let cached = get_cached("https://test.example.com");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().model.as_deref(), Some("cached-model"));
        clear_cache();
    }

    #[test]
    fn cache_miss_for_unknown_url() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        clear_cache();
        assert!(get_cached("https://unknown.example.com").is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn load_wellknown_returns_default_when_no_auth_file() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        // With no auth.json, should return default config without error
        let config = load_wellknown().await;
        assert!(config.model.is_none());
    }

    #[test]
    fn safe_env_keys_accept_auth_vars() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        assert!(is_safe_env_key("OPENAI_API_KEY"));
        assert!(is_safe_env_key("GH_TOKEN"));
        assert!(is_safe_env_key("MY_TOKEN_1"));
    }

    #[test]
    fn safe_env_keys_reject_hijacking_names() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        assert!(!is_safe_env_key("PATH"));
        assert!(!is_safe_env_key("LD_PRELOAD"));
        assert!(!is_safe_env_key("DYLD_INSERT_LIBRARIES"));
        assert!(!is_safe_env_key("lowerCase"));
        assert!(!is_safe_env_key("1BAD"));
        assert!(!is_safe_env_key("BAD-KEY"));
    }

    // std Mutex はプロセスグローバルな env の直列化にのみ使用し、await 中は
    // 他スレッドが env を触らないため保持しても安全（テスト専用）。
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn cache_hit_reinjects_rotated_token_into_env() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();

        let key = "OPENCODE_WK_ROTATION_TEST";
        let url = "https://cache-hit.example.com";
        let _env_guard = EnvRestoreGuard {
            key: key.to_string(),
            original: std::env::var_os(key),
        };

        // Simulate a still-valid cache entry (inside the TTL) and a stale
        // token left in the process environment from an earlier load.
        set_cached(
            url.to_string(),
            Config {
                model: Some("cached-model".to_string()),
                ..Default::default()
            },
        );
        std::env::set_var(key, "stale-token");
        assert_eq!(std::env::var(key).unwrap(), "stale-token");

        let mut entries = HashMap::new();
        entries.insert(
            url.to_string(),
            WellKnownAuth {
                key: key.to_string(),
                token: "rotated-token".to_string(),
            },
        );

        // Cache hit: the cached config must be merged AND the token refreshed.
        let config = load_wellknown_from_entries(&entries).await;
        assert_eq!(config.model.as_deref(), Some("cached-model"));

        assert_eq!(
            std::env::var(key).unwrap(),
            "rotated-token",
            "cache hit must re-inject the current auth token"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn cache_hit_rejects_unsafe_env_key() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();

        // "IFS" is on the blocklist but harmless to touch inside a Rust test
        // process; the guard restores its original value on drop.
        let key = "IFS";
        let url = "https://unsafe-key.example.com";
        let _env_guard = EnvRestoreGuard {
            key: key.to_string(),
            original: std::env::var_os(key),
        };
        std::env::remove_var(key);

        set_cached(url.to_string(), Config::default());

        let mut entries = HashMap::new();
        entries.insert(
            url.to_string(),
            WellKnownAuth {
                key: key.to_string(),
                token: "must-not-be-injected".to_string(),
            },
        );

        let _config = load_wellknown_from_entries(&entries).await;

        assert!(
            std::env::var_os(key).is_none(),
            "unsafe env key must be rejected even on a cache hit"
        );
    }
}
