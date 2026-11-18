use std::collections::HashMap;
use std::sync::Arc;

use opencode_plugin::subprocess::PluginLoader;
use opencode_provider::{AuthError, AuthInfo, AuthManager, AuthMethodType, Authorization};
use serde::{Deserialize, Serialize};

/// Errors surfaced by provider authentication operations.
///
/// Persistence errors are kept distinct from plugin/AuthError failures so callers can map them to
/// an appropriate HTTP status. Their messages never contain credential values.
#[derive(Debug, thiserror::Error)]
pub enum ProviderAuthError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("failed to persist provider credentials: {0}")]
    Persist(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethodInfo {
    #[serde(rename = "type")]
    pub method_type: String,
    pub label: String,
}

pub struct ProviderAuth {
    auth_manager: Arc<AuthManager>,
}

impl ProviderAuth {
    pub fn new(auth_manager: Arc<AuthManager>) -> Self {
        Self { auth_manager }
    }

    pub async fn methods(loader: &PluginLoader) -> HashMap<String, Vec<AuthMethodInfo>> {
        let bridges = loader.auth_bridges().await;
        bridges
            .iter()
            .map(|(provider, bridge)| {
                let methods = bridge
                    .methods()
                    .iter()
                    .map(|method| AuthMethodInfo {
                        method_type: method.method_type.clone(),
                        label: method.label.clone(),
                    })
                    .collect::<Vec<_>>();
                (provider.clone(), methods)
            })
            .collect()
    }

    pub async fn authorize(
        loader: &PluginLoader,
        provider_id: &str,
        method: usize,
        inputs: Option<HashMap<String, String>>,
    ) -> Result<Authorization, AuthError> {
        let bridge = loader
            .auth_bridge(provider_id)
            .await
            .ok_or_else(|| AuthError::OauthMissing(provider_id.to_string()))?;
        let result = bridge
            .authorize(method, inputs)
            .await
            .map_err(|_| AuthError::OauthCallbackFailed)?;

        let method_type = match result.method.as_deref() {
            Some("code") => AuthMethodType::Code,
            _ => AuthMethodType::Auto,
        };

        Ok(Authorization {
            url: result.url.unwrap_or_default(),
            method: method_type,
            instructions: result.instructions.unwrap_or_default(),
        })
    }

    pub async fn callback(
        &self,
        loader: &PluginLoader,
        provider_id: &str,
        code: Option<&str>,
    ) -> Result<(), ProviderAuthError> {
        let bridge = loader
            .auth_bridge(provider_id)
            .await
            .ok_or_else(|| AuthError::OauthMissing(provider_id.to_string()))?;
        let result = bridge
            .callback(code)
            .await
            .map_err(|_| AuthError::OauthCallbackFailed)?;

        let auth_type = result.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if auth_type != "success" {
            return Err(AuthError::OauthCallbackFailed.into());
        }

        // Plugin callback can override target provider (e.g. copilot enterprise).
        let target_provider = result
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or(provider_id);

        if let Some(key) = result
            .get("key")
            .and_then(|v| v.as_str())
            .or_else(|| result.get("apiKey").and_then(|v| v.as_str()))
            .or_else(|| result.get("token").and_then(|v| v.as_str()))
        {
            self.auth_manager
                .try_set(
                    target_provider,
                    AuthInfo::Api {
                        key: key.to_string(),
                    },
                )
                .await?;
            return Ok(());
        }

        let access = result
            .get("access")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let refresh = result
            .get("refresh")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if access.is_empty() && refresh.is_empty() {
            return Err(AuthError::OauthCallbackFailed.into());
        }

        self.auth_manager
            .try_set(
                target_provider,
                AuthInfo::OAuth {
                    access,
                    refresh,
                    expires: result.get("expires").and_then(|v| v.as_i64()),
                    account_id: result
                        .get("accountId")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    enterprise_url: result
                        .get("enterpriseUrl")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
            )
            .await?;

        Ok(())
    }

    /// Set an API key and persist it, returning any persistence error to the caller.
    pub async fn set_api_key(&self, provider_id: &str, key: String) -> anyhow::Result<()> {
        self.auth_manager
            .try_set(provider_id, AuthInfo::Api { key })
            .await
    }

    /// Remove credentials and persist the deletion, returning any persistence error.
    pub async fn remove(&self, provider_id: &str) -> anyhow::Result<()> {
        self.auth_manager.try_remove(provider_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ダミー文字列のみを使用する。実際のトークン値・認証情報は一切使わない。
    const DUMMY_PROVIDER: &str = "dummy-provider";
    const DUMMY_PERSIST_MESSAGE: &str = "dummy persistence failure";

    #[test]
    fn auth_error_from_conversion_creates_auth_variant() {
        // Given: 源となる AuthError（source を持たない葉バリアント）
        let auth_error = AuthError::OauthMissing(DUMMY_PROVIDER.to_string());

        // When: From<AuthError> で ProviderAuthError に変換する
        let err: ProviderAuthError = auth_error.into();

        // Then: Auth バリアントになり、元の値が保持される
        assert!(matches!(
            &err,
            ProviderAuthError::Auth(AuthError::OauthMissing(provider)) if provider == DUMMY_PROVIDER
        ));
    }

    #[test]
    fn auth_variant_display_forwards_inner_message() {
        let err = ProviderAuthError::from(AuthError::OauthMissing(DUMMY_PROVIDER.to_string()));

        assert_eq!(
            err.to_string(),
            format!("OAuth pending request not found for provider: {DUMMY_PROVIDER}")
        );
    }

    #[test]
    fn auth_variant_source_forwards_transparent_source() {
        let err = ProviderAuthError::from(AuthError::OauthMissing(DUMMY_PROVIDER.to_string()));

        // #[error(transparent)] は内側の source を委譲する。
        // AuthError::OauthMissing は source を持たないため None になる。
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn persist_error_from_conversion_creates_persist_variant() {
        let cause = anyhow::anyhow!(DUMMY_PERSIST_MESSAGE);

        let err: ProviderAuthError = cause.into();

        assert!(matches!(&err, ProviderAuthError::Persist(_)));
    }

    #[test]
    fn persist_variant_display_prefixes_inner_message() {
        let err = ProviderAuthError::from(anyhow::anyhow!(DUMMY_PERSIST_MESSAGE));

        assert_eq!(
            err.to_string(),
            format!("failed to persist provider credentials: {DUMMY_PERSIST_MESSAGE}")
        );
    }

    #[test]
    fn persist_variant_source_chain_exposes_inner_error() {
        let err = ProviderAuthError::from(anyhow::anyhow!(DUMMY_PERSIST_MESSAGE));

        let source = std::error::Error::source(&err)
            .expect("Persist バリアントは内側の anyhow::Error を source として保持する");
        assert_eq!(source.to_string(), DUMMY_PERSIST_MESSAGE);
    }

    #[test]
    fn auth_and_persist_variants_are_distinct() {
        let auth_err = ProviderAuthError::from(AuthError::OauthCallbackFailed);
        let persist_err = ProviderAuthError::from(anyhow::anyhow!(DUMMY_PERSIST_MESSAGE));

        assert!(matches!(&auth_err, ProviderAuthError::Auth(_)));
        assert!(matches!(&persist_err, ProviderAuthError::Persist(_)));
        assert_ne!(auth_err.to_string(), persist_err.to_string());
    }
}
