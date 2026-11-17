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

    pub async fn get_oauth_token(&self, provider_id: &str) -> Option<String> {
        let creds = self.credentials.read().await;
        match creds.get(provider_id) {
            Some(AuthInfo::OAuth { access, .. }) => Some(access.clone()),
            _ => None,
        }
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
}
