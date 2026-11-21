use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::Config;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes
/// リモート `.well-known/opencode` レスポンスの最大バイト数（8 MiB）。
const MAX_WELLKNOWN_JSON_BYTES: u64 = 8 * 1024 * 1024;
/// `CACHE` が保持する URL エントリ数の上限。
/// URL が際限なく増えても HashMap が無制限に伸びないようにする。
const MAX_CACHE_ENTRIES: usize = 256;

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
/// key name is ever logged — the URL and token value are never written to logs.
///
/// Returns `true` when the token was injected, `false` when the key was
/// rejected as unsafe.
fn inject_auth_token(_url: &str, auth: &WellKnownAuth) -> bool {
    if !is_safe_env_key(&auth.key) {
        // URL は内部エンドポイントを含み得るためログへ出さない（R4）。
        tracing::warn!(key = %auth.key, "refusing unsafe env key from auth.json");
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
            Err(_) => {
                // URL と生エラー（レスポンス本文を含む）はログへ出さない（R4）。
                tracing::warn!("failed to fetch wellknown config, skipping");
            }
        }
    }

    merged
}

async fn fetch_wellknown_config(client: &reqwest::Client, endpoint: &str) -> Result<Config> {
    let resp = client.get(endpoint).send().await?;

    if !resp.status().is_success() {
        // エンドポイント URL は内部情報を露し得るためエラーへ埋め込まない（R4）。
        anyhow::bail!("well-known config fetch returned an unsuccessful status");
    }

    let body = resp.bytes().await?;
    if body.len() as u64 > MAX_WELLKNOWN_JSON_BYTES {
        anyhow::bail!("well-known response exceeds size limit");
    }
    // serde の生エラーはレスポンス値を含むため、固定文言へ変換する（R4）。
    let wk: WellKnownResponse = serde_json::from_slice(&body)
        .map_err(|_| anyhow::anyhow!("well-known config has an invalid envelope"))?;
    let config_value = wk
        .config
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let config: Config = config_from_wellknown_value(config_value)?;
    Ok(config)
}

/// Deserialize a remote `.well-known/opencode` `config` object through the same
/// validation pipeline as local config files. Unknown keys are warned (key
/// names only) and type errors report a sanitized field path; values, tokens
/// and URLs never appear in warnings or errors.
fn config_from_wellknown_value(config_value: serde_json::Value) -> Result<Config> {
    crate::loader::validate_config_value(config_value)
}

fn get_cached(url: &str) -> Option<Config> {
    let mut guard = CACHE.lock().ok()?;
    let map = guard.as_mut()?;
    let is_fresh = map.get(url)?.fetched_at.elapsed() < CACHE_TTL;
    if is_fresh {
        map.get(url).map(|entry| entry.config.clone())
    } else {
        // TTL 切れのエントリはここで削除する。放置すると URL 数だけ
        // HashMap が伸び続けるため、ミス時にも確実に回収する。
        map.remove(url);
        None
    }
}

fn set_cached(url: String, config: Config) {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let map = guard.get_or_insert_with(HashMap::new);
    // 新規 URL で上限に達している場合は最古のエントリを追い出してから挿入する。
    // 既存 URL の更新ではエントリ数は増えないので追い出さない。
    if !map.contains_key(&url) && map.len() >= MAX_CACHE_ENTRIES {
        evict_oldest_cached(map);
    }
    map.insert(
        url,
        CacheEntry {
            config,
            fetched_at: Instant::now(),
        },
    );
}

/// Removes the least-recently-fetched entry to make room under
/// [`MAX_CACHE_ENTRIES`]. The URL itself is never logged.
fn evict_oldest_cached(map: &mut HashMap<String, CacheEntry>) {
    let oldest = map
        .iter()
        .min_by_key(|(_, entry)| entry.fetched_at)
        .map(|(oldest_url, _)| oldest_url.clone());
    if let Some(oldest) = oldest {
        map.remove(&oldest);
    }
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
    use std::fmt::Write as _;
    use std::sync::Arc;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// Minimal `tracing` capture; `tracing-subscriber` is not a dev-dependency.
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
    fn wellknown_byte_limit_is_eight_mib() {
        assert_eq!(MAX_WELLKNOWN_JSON_BYTES, 8 * 1024 * 1024);
    }

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
        let config: Config = config_from_wellknown_value(config_val).unwrap();
        assert_eq!(config.model.as_deref(), Some("claude-3-opus"));
    }

    /// 回帰 (CFG2-05): wellknown の config もローカル設定と同じ検証パイプラインを
    /// 通り、未知キーはキー名のみで警告され、値（トークン風文字列）は出ない。
    #[test]
    fn wellknown_unknown_keys_warn_without_values() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "model": "claude-3-opus",
                "secretive_typo": "TOKEN_SENTINEL_NEVER_LOG",
                "server": {"port": 4096, "prots": "URL_SENTINEL_NEVER_LOG"}
            }"#,
        )
        .unwrap();

        let buffer = WarningBuffer::default();
        let subscriber = WarningSubscriber {
            buffer: buffer.clone(),
        };
        tracing::subscriber::with_default(subscriber, || {
            let _config =
                config_from_wellknown_value(value).expect("unknown keys must not block startup");
        });

        let text = buffer.text();
        for expected in ["secretive_typo", "server.prots"] {
            assert!(
                text.contains(expected),
                "missing unknown-key warning: {text}"
            );
        }
        for forbidden in ["TOKEN_SENTINEL_NEVER_LOG", "URL_SENTINEL_NEVER_LOG"] {
            assert!(
                !text.contains(forbidden),
                "value leaked into warning: {text}"
            );
        }
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

    // Regression: a TTL-expired entry must be removed from the map on lookup,
    // not merely reported as a miss.
    #[test]
    fn expired_cache_entry_is_removed() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        clear_cache();

        let url = "https://expired.example.com".to_string();
        {
            let mut guard = CACHE.lock().unwrap();
            let map = guard.get_or_insert_with(HashMap::new);
            map.insert(
                url.clone(),
                CacheEntry {
                    config: Config::default(),
                    fetched_at: Instant::now() - CACHE_TTL - Duration::from_secs(1),
                },
            );
        }

        assert!(get_cached(&url).is_none());
        {
            let guard = CACHE.lock().unwrap();
            let map = guard.as_ref().expect("cache map must still exist");
            assert!(!map.contains_key(&url), "expired entry must be removed");
        }

        clear_cache();
    }

    // Regression: the total number of cached URLs must stay bounded.
    #[test]
    fn cache_entry_count_is_bounded_by_max_cache_entries() {
        let _guard = TEST_ENV_MUTEX.lock().unwrap();
        clear_cache();

        for i in 0..(MAX_CACHE_ENTRIES + 8) {
            set_cached(format!("https://cap-{i}.example.com"), Config::default());
        }

        {
            let guard = CACHE.lock().unwrap();
            let map = guard.as_ref().expect("cache map must exist");
            assert!(
                map.len() <= MAX_CACHE_ENTRIES,
                "cache must stay within MAX_CACHE_ENTRIES"
            );
        }

        clear_cache();
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
