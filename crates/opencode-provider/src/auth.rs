use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuthInfo {
    #[serde(rename = "api")]
    Api { key: String },
    #[serde(rename = "oauth")]
    OAuth {
        access: String,
        #[serde(default)]
        refresh: String,
        expires: Option<i64>,
        #[serde(alias = "accountId")]
        account_id: Option<String>,
        #[serde(alias = "enterpriseUrl")]
        enterprise_url: Option<String>,
    },
    #[serde(rename = "wellknown")]
    WellKnown {
        /// Environment variable name to set with the token
        key: String,
        /// The authentication token value
        token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethod {
    pub auth_type: AuthType,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthType {
    OAuth,
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Authorization {
    pub url: String,
    pub method: AuthMethodType,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthMethodType {
    Auto,
    Code,
}

pub struct AuthManager {
    credentials: Arc<RwLock<HashMap<String, AuthInfo>>>,
    filepath: Option<PathBuf>,
    /// Reason the auth file could not be loaded, if any. Never contains file contents.
    load_error: Option<String>,
    /// Reason the most recent persistence attempt failed, if any. Never contains token values.
    persist_error: Arc<RwLock<Option<String>>>,
}

impl AuthManager {
    pub fn new() -> Self {
        Self {
            credentials: Arc::new(RwLock::new(HashMap::new())),
            filepath: None,
            load_error: None,
            persist_error: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_filepath(filepath: PathBuf) -> Self {
        Self {
            credentials: Arc::new(RwLock::new(HashMap::new())),
            filepath: Some(filepath),
            load_error: None,
            persist_error: Arc::new(RwLock::new(None)),
        }
    }

    /// Load the auth store, returning the load error instead of silently using empty credentials.
    ///
    /// Error messages intentionally contain only the file path and a short reason; file contents
    /// and token values are never included.
    pub async fn try_load_from_file(data_dir: &Path) -> anyhow::Result<Self> {
        let filepath = data_dir.join("auth.json");
        let manager = Self::with_filepath(filepath.clone());

        let content = tokio::fs::read_to_string(&filepath)
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to read auth file {}: {error}", filepath.display())
            })?;
        if content.trim().is_empty() {
            // An empty file means "no stored credentials", not a parse failure.
            return Ok(manager);
        }
        // Drop the parser's original error so that no fragment of the file (and therefore no
        // token value) can leak through the error chain.
        let data = serde_json::from_str::<HashMap<String, AuthInfo>>(&content)
            .map_err(|_| anyhow::anyhow!("failed to parse auth file {}", filepath.display()))?;
        *manager.credentials.write().await = data;
        Ok(manager)
    }

    /// Load the auth store, recording and logging any load error while keeping the legacy `Self`
    /// return type for API compatibility. Use [`Self::try_load_from_file`] to observe errors.
    pub async fn load_from_file(data_dir: &Path) -> Self {
        match Self::try_load_from_file(data_dir).await {
            Ok(manager) => manager,
            Err(error) => {
                tracing::warn!(error = %error, "failed to load auth store; using empty credentials");
                let filepath = data_dir.join("auth.json");
                let mut manager = Self::with_filepath(filepath);
                manager.load_error = Some(error.to_string());
                manager
            }
        }
    }

    /// Reason the auth file could not be loaded, if any.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Reason the most recent persistence attempt failed, if any.
    pub async fn last_persist_error(&self) -> Option<String> {
        self.persist_error.read().await.clone()
    }

    pub async fn get(&self, provider_id: &str) -> Option<AuthInfo> {
        let creds = self.credentials.read().await;
        creds.get(provider_id).cloned()
    }

    /// Insert credentials and persist them, returning a persistence error to the caller.
    pub async fn try_set(&self, provider_id: &str, auth: AuthInfo) -> anyhow::Result<()> {
        {
            let mut creds = self.credentials.write().await;
            creds.insert(provider_id.to_string(), auth);
        }
        self.persist().await
    }

    /// Insert credentials, logging a persistence error while keeping the legacy `()` return type
    /// for API compatibility. Use [`Self::try_set`] to observe errors.
    pub async fn set(&self, provider_id: &str, auth: AuthInfo) {
        if let Err(error) = self.try_set(provider_id, auth).await {
            tracing::warn!(%error, provider_id, "failed to persist auth store");
        }
    }

    /// Remove credentials and persist the deletion, returning a persistence error to the caller.
    pub async fn try_remove(&self, provider_id: &str) -> anyhow::Result<()> {
        {
            let mut creds = self.credentials.write().await;
            creds.remove(provider_id);
        }
        self.persist().await
    }

    /// Remove credentials, logging a persistence error while keeping the legacy `()` return type
    /// for API compatibility. Use [`Self::try_remove`] to observe errors.
    pub async fn remove(&self, provider_id: &str) {
        if let Err(error) = self.try_remove(provider_id).await {
            tracing::warn!(%error, provider_id, "failed to persist auth store");
        }
    }

    pub async fn has_auth(&self, provider_id: &str) -> bool {
        let creds = self.credentials.read().await;
        creds.contains_key(provider_id)
    }

    pub async fn get_api_key(&self, provider_id: &str) -> Option<String> {
        let creds = self.credentials.read().await;
        match creds.get(provider_id) {
            Some(AuthInfo::Api { key }) => Some(key.clone()),
            _ => None,
        }
    }

    /// OAuth アクセストークンを返す。期限切れ（skew ウィンドウ内を含む）の場合は
    /// トークンを返さず、理由（provider_id のみ）を warn ログに出す。
    ///
    /// 呼び出し側は既存の `Option<String>` シグネチャをそのまま使える。
    pub async fn get_oauth_token(&self, provider_id: &str) -> Option<String> {
        let creds = self.credentials.read().await;
        let auth = creds.get(provider_id)?;
        let AuthInfo::OAuth { access, .. } = auth else {
            return None;
        };
        if let Err(error) = ensure_oauth_not_expired(
            provider_id,
            auth,
            now_unix_ms(),
            DEFAULT_OAUTH_EXPIRY_SKEW_MS,
        ) {
            // エラーには provider_id のみが含まれ、トークン値は含まれない。
            tracing::warn!(%error, provider_id, "OAuth token rejected by expiry check");
            return None;
        }
        Some(access.clone())
    }

    pub async fn list(&self) -> HashMap<String, AuthInfo> {
        let creds = self.credentials.read().await;
        creds.clone()
    }

    async fn persist(&self) -> anyhow::Result<()> {
        let framed = self
            .persist_inner()
            .await
            .map_err(|error| match &self.filepath {
                Some(path) => {
                    anyhow::anyhow!(
                        "failed to persist auth store to {}: {error}",
                        path.display()
                    )
                }
                None => anyhow::anyhow!("failed to persist auth store: {error}"),
            });
        {
            let mut last_error = self.persist_error.write().await;
            *last_error = framed.as_ref().err().map(|error| error.to_string());
        }
        framed
    }

    async fn persist_inner(&self) -> anyhow::Result<()> {
        let Some(path) = self.filepath.as_ref() else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let creds = self.credentials.read().await;
        let json = serde_json::to_vec_pretty(&*creds)?;
        tokio::fs::write(path, json).await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
        }

        Ok(())
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("OAuth pending request not found for provider: {0}")]
    OauthMissing(String),

    #[error("OAuth authorization code missing for provider: {0}")]
    OauthCodeMissing(String),

    #[error("OAuth callback failed")]
    OauthCallbackFailed,

    #[error("API key not set for provider: {0}")]
    ApiKeyNotSet(String),

    #[error("OAuth token expired for provider: {provider_id}")]
    OAuthTokenExpired { provider_id: String },
}

/// OAuth トークンの期限に対して余裕を持たせる既定値（60 秒）。
pub const DEFAULT_OAUTH_EXPIRY_SKEW_MS: i64 = 60_000;

/// 現在時刻を epoch milliseconds で返す（`is_oauth_expired` と単位を揃える）。
/// システム時計が UNIX_EPOCH より前の場合は 0 として扱う。
pub fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// OAuth 資格情報の期限（epoch milliseconds）を取り出す。
///
/// `expires` の単位は epoch milliseconds である。根拠: bundled plugin
/// `crates/opencode-plugin/builtin/codex-auth.ts` が `Date.now() + 3600 * 1000`
/// （JS の `Date.now()` は epoch ms）で生成し、`server/oauth.rs` がその値を
/// `as_i64()` で保存するため。
/// なお bundled `copilot-auth.ts` は期限不明のセンチネルとして `expires: 0`
/// を書き込むため、0 以下は「期限情報なし」として扱う（期限切れにしない）。
pub fn oauth_expires_at_ms(auth: &AuthInfo) -> Option<i64> {
    match auth {
        AuthInfo::OAuth {
            expires: Some(expires),
            ..
        } if *expires > 0 => Some(*expires),
        _ => None,
    }
}

/// 期限が切れている、または `skew_ms` 以内に切れるかを判定する純粋ヘルパ。
pub fn is_oauth_expired(auth: &AuthInfo, now_ms: i64, skew_ms: i64) -> bool {
    match oauth_expires_at_ms(auth) {
        // 現在時刻への skew 加算でオーバーフローしないよう saturating_add を使う。
        Some(expires_ms) => now_ms.saturating_add(skew_ms.max(0)) >= expires_ms,
        None => false,
    }
}

/// 期限切れ（または skew ウィンドウ内）なら `AuthError` を返す純粋ヘルパ。
///
/// refresh による自動更新は外部 HTTP を伴うため本次では行わず、期限検知のみを提供する。
pub fn ensure_oauth_not_expired(
    provider_id: &str,
    auth: &AuthInfo,
    now_ms: i64,
    skew_ms: i64,
) -> Result<(), AuthError> {
    if is_oauth_expired(auth, now_ms, skew_ms) {
        Err(AuthError::OAuthTokenExpired {
            provider_id: provider_id.to_string(),
        })
    } else {
        Ok(())
    }
}

pub fn get_env_key(provider_id: &str) -> Option<String> {
    let env_var = format!("{}_API_KEY", provider_id.to_uppercase().replace("-", "_"));
    std::env::var(&env_var).ok()
}

pub fn get_env_key_or(provider_id: &str, default: &str) -> String {
    get_env_key(provider_id).unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_auth_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencode-auth-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp auth dir");
        dir
    }

    #[tokio::test]
    async fn auth_manager_persists_and_reloads_file() {
        let dir = temp_auth_dir();
        let manager = AuthManager::load_from_file(&dir).await;

        manager
            .set(
                "openai",
                AuthInfo::OAuth {
                    access: "access-token".to_string(),
                    refresh: "refresh-token".to_string(),
                    expires: Some(1234),
                    account_id: Some("acct_1".to_string()),
                    enterprise_url: Some("https://enterprise.example.com".to_string()),
                },
            )
            .await;
        manager
            .set(
                "github-copilot",
                AuthInfo::Api {
                    key: "copilot-key".to_string(),
                },
            )
            .await;

        let auth_path = dir.join("auth.json");
        assert!(auth_path.exists());

        let reloaded = AuthManager::load_from_file(&dir).await;
        match reloaded.get("openai").await {
            Some(AuthInfo::OAuth {
                access,
                refresh,
                account_id,
                enterprise_url,
                ..
            }) => {
                assert_eq!(access, "access-token");
                assert_eq!(refresh, "refresh-token");
                assert_eq!(account_id.as_deref(), Some("acct_1"));
                assert_eq!(
                    enterprise_url.as_deref(),
                    Some("https://enterprise.example.com")
                );
            }
            other => panic!("unexpected oauth entry: {other:?}"),
        }

        match reloaded.get("github-copilot").await {
            Some(AuthInfo::Api { key }) => assert_eq!(key, "copilot-key"),
            other => panic!("unexpected api entry: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auth_manager_remove_persists_deletion() {
        let dir = temp_auth_dir();
        let manager = AuthManager::load_from_file(&dir).await;
        manager
            .set(
                "openai",
                AuthInfo::Api {
                    key: "to-delete".to_string(),
                },
            )
            .await;

        manager.remove("openai").await;
        let reloaded = AuthManager::load_from_file(&dir).await;
        assert!(reloaded.get("openai").await.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn load_from_file_exposes_error_without_file_contents() {
        let dir = temp_auth_dir();
        let auth_path = dir.join("auth.json");
        // Malformed JSON containing a sentinel that must never appear in an error message.
        std::fs::write(&auth_path, b"{ \"invalid\": \"SENTINEL_SECRET_VALUE\" }")
            .expect("write malformed auth file");

        let manager = AuthManager::load_from_file(&dir).await;
        let load_error = manager.load_error().expect("load error must be recorded");
        assert!(load_error.contains("failed to parse auth file"));
        assert!(!load_error.contains("SENTINEL_SECRET_VALUE"));
        assert!(manager.get("invalid").await.is_none());

        assert!(AuthManager::try_load_from_file(&dir).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn try_set_and_try_remove_expose_persist_errors() {
        let dir = temp_auth_dir();
        let auth_path = dir.join("auth.json");
        // Make the target path a directory so writes fail deterministically.
        std::fs::create_dir(&auth_path).expect("create directory at auth file path");

        let manager = AuthManager::with_filepath(auth_path);

        let set_error = manager
            .try_set(
                "openai",
                AuthInfo::Api {
                    key: "SENTINEL_SECRET_VALUE".to_string(),
                },
            )
            .await
            .expect_err("persistence must fail");
        assert!(format!("{set_error}").contains("failed to persist auth store"));
        let last_error = manager
            .last_persist_error()
            .await
            .expect("persist error recorded");
        assert!(!last_error.contains("SENTINEL_SECRET_VALUE"));
        // The in-memory value is still updated, matching the legacy behaviour.
        assert!(manager.get("openai").await.is_some());

        let remove_error = manager
            .try_remove("openai")
            .await
            .expect_err("persistence must fail");
        assert!(format!("{remove_error}").contains("failed to persist auth store"));
        assert!(manager.last_persist_error().await.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn oauth_with_expiry(expires: Option<i64>) -> AuthInfo {
        AuthInfo::OAuth {
            access: "access-token".to_string(),
            refresh: "refresh-token".to_string(),
            expires,
            account_id: None,
            enterprise_url: None,
        }
    }

    #[test]
    fn oauth_expiry_detection_respects_skew() {
        let auth = oauth_with_expiry(Some(10_000));
        assert_eq!(oauth_expires_at_ms(&auth), Some(10_000));
        assert!(!is_oauth_expired(&auth, 8_000, 1_000));
        assert!(is_oauth_expired(&auth, 9_000, 1_000));
        assert!(is_oauth_expired(&auth, 10_000, 0));
        assert!(is_oauth_expired(&auth, 11_000, 0));
    }

    #[test]
    fn oauth_without_expiry_is_not_treated_as_expired() {
        let auth = oauth_with_expiry(None);
        assert_eq!(oauth_expires_at_ms(&auth), None);
        assert!(!is_oauth_expired(
            &auth,
            i64::MAX,
            DEFAULT_OAUTH_EXPIRY_SKEW_MS
        ));
        assert!(
            ensure_oauth_not_expired("openai", &auth, i64::MAX, DEFAULT_OAUTH_EXPIRY_SKEW_MS)
                .is_ok()
        );
    }

    #[test]
    fn ensure_oauth_not_expired_returns_auth_error_before_expiry() {
        let auth = oauth_with_expiry(Some(10_000));
        assert!(ensure_oauth_not_expired("openai", &auth, 8_000, 1_000).is_ok());

        let error = ensure_oauth_not_expired("openai", &auth, 9_500, 1_000)
            .expect_err("token inside skew window must be rejected");
        assert!(matches!(error, AuthError::OAuthTokenExpired { .. }));
        let message = error.to_string();
        assert!(message.contains("openai"));
        assert!(!message.contains("access-token"), "token must not leak");
        assert!(!message.contains("refresh-token"), "token must not leak");
    }

    #[test]
    fn non_oauth_auth_is_ignored_by_expiry_helper() {
        let auth = AuthInfo::Api {
            key: "key".to_string(),
        };
        assert_eq!(oauth_expires_at_ms(&auth), None);
        assert!(!is_oauth_expired(
            &auth,
            i64::MAX,
            DEFAULT_OAUTH_EXPIRY_SKEW_MS
        ));
        assert!(ensure_oauth_not_expired("openai", &auth, i64::MAX, 0).is_ok());
    }

    fn oauth_auth(access: &str, expires: Option<i64>) -> AuthInfo {
        AuthInfo::OAuth {
            access: access.to_string(),
            refresh: "dummy-refresh".to_string(),
            expires,
            account_id: None,
            enterprise_url: None,
        }
    }

    #[tokio::test]
    async fn get_oauth_token_returns_none_for_expired_token() {
        let manager = AuthManager::new();
        manager
            .set(
                "openai",
                oauth_auth("EXPIRED_DUMMY_TOKEN", Some(now_unix_ms() - 60_000)),
            )
            .await;

        assert_eq!(manager.get_oauth_token("openai").await, None);
    }

    #[tokio::test]
    async fn get_oauth_token_returns_none_inside_skew_window() {
        let manager = AuthManager::new();
        manager
            .set(
                "openai",
                oauth_auth("SKEW_DUMMY_TOKEN", Some(now_unix_ms() + 30_000)),
            )
            .await;

        assert_eq!(manager.get_oauth_token("openai").await, None);
    }

    #[tokio::test]
    async fn get_oauth_token_returns_unexpired_token() {
        let manager = AuthManager::new();
        manager
            .set(
                "openai",
                oauth_auth("FRESH_DUMMY_TOKEN", Some(now_unix_ms() + 3_600_000)),
            )
            .await;

        assert_eq!(
            manager.get_oauth_token("openai").await.as_deref(),
            Some("FRESH_DUMMY_TOKEN")
        );
    }

    #[tokio::test]
    async fn get_oauth_token_treats_zero_expires_as_no_expiry() {
        let manager = AuthManager::new();
        manager
            .set("github-copilot", oauth_auth("PASTE_DUMMY_TOKEN", Some(0)))
            .await;

        assert_eq!(
            manager.get_oauth_token("github-copilot").await.as_deref(),
            Some("PASTE_DUMMY_TOKEN")
        );
    }
}
