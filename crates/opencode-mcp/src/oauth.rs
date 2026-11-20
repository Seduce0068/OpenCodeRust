//! OAuth 2.0 manager for MCP remote servers.
//!
//! Handles the full authorization-code + PKCE flow, token refresh, and
//! credential storage.  Uses the `oauth2` crate for the heavy lifting.

use crate::auth;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, Scope, TokenResponse, TokenUrl,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Static configuration for an OAuth-enabled MCP server.
#[derive(Debug, Clone)]
pub struct McpOAuthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
}

impl Default for McpOAuthConfig {
    fn default() -> Self {
        Self {
            client_id: None,
            client_secret: None,
            auth_url: String::new(),
            token_url: String::new(),
            scopes: Vec::new(),
            redirect_uri: "http://127.0.0.1:19876/mcp/oauth/callback".to_string(),
        }
    }
}

/// Transient state kept during an in-progress authorization flow.
#[derive(Debug)]
struct PendingAuth {
    pkce_verifier: String,
    csrf_state: String,
}

/// Manages the OAuth lifecycle for a single MCP server.
pub struct McpOAuthManager {
    mcp_name: String,
    server_url: String,
    config: RwLock<McpOAuthConfig>,
    pending: RwLock<Option<PendingAuth>>,
    /// RFC 8707 resource indicator discovered during endpoint discovery.
    /// Kept on the manager (not `McpOAuthConfig`) so existing external config
    /// struct literals remain source-compatible. `None` omits the parameter.
    resource: RwLock<Option<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("OAuth configuration error: {0}")]
    Config(String),
    #[error("Token exchange failed: {0}")]
    TokenExchange(String),
    #[error("Token refresh failed: {0}")]
    TokenRefresh(String),
    #[error("No pending auth flow")]
    NoPendingAuth,
    #[error("CSRF state mismatch")]
    StateMismatch,
    #[error("No refresh token available")]
    NoRefreshToken,
    #[error("Storage error: {0}")]
    Storage(#[from] std::io::Error),
    /// auth store の読み書き失敗を型として伝播する。
    ///
    /// Display は種別のみ（トークン値・PKCE verifier・CSRF state は出さない）。
    /// 詳細な原因は `std::error::Error::source()` から参照できる。
    #[error("MCP auth store failure")]
    AuthStore(#[from] auth::AuthStoreError),
}

/// Appends the RFC 8707 `resource` parameter to an authorization request when
/// a resource indicator is available. `None` leaves the request untouched.
fn with_resource_param<'a>(
    mut request: oauth2::AuthorizationRequest<'a>,
    resource: Option<String>,
) -> oauth2::AuthorizationRequest<'a> {
    if let Some(resource) = resource {
        request = request.add_extra_param("resource", resource);
    }
    request
}

impl McpOAuthManager {
    pub fn new(mcp_name: String, server_url: String, config: McpOAuthConfig) -> Self {
        Self {
            mcp_name,
            server_url,
            config: RwLock::new(config),
            pending: RwLock::new(None),
            resource: RwLock::new(None),
        }
    }

    /// Records the RFC 8707 resource indicator from discovered endpoints.
    /// A missing value leaves any previously discovered indicator untouched.
    async fn remember_resource(&self, resource: Option<String>) {
        if let Some(resource) = resource {
            *self.resource.write().await = Some(resource);
        }
    }

    /// Validate config and return parsed URL components.
    /// Resolves the endpoints, preferring the effective ones discovered at
    /// startup over the static configuration.
    pub async fn set_endpoints(&self, auth_url: &str, token_url: &str) {
        let mut config = self.config.write().await;
        config.auth_url = auth_url.to_string();
        config.token_url = token_url.to_string();
    }

    /// Ensures `auth_url` / `token_url` are populated, discovering them when
    /// they are not configured.
    pub async fn ensure_endpoints(&self) -> Result<(), OAuthError> {
        {
            let config = self.config.read().await;
            if !config.auth_url.is_empty() && !config.token_url.is_empty() {
                return Ok(());
            }
        }

        let (explicit_auth, explicit_token) = {
            let config = self.config.read().await;
            (
                (!config.auth_url.is_empty()).then(|| config.auth_url.clone()),
                (!config.token_url.is_empty()).then(|| config.token_url.clone()),
            )
        };

        let endpoints = crate::discovery::discover_endpoints(
            &self.server_url,
            explicit_auth.as_deref(),
            explicit_token.as_deref(),
        )
        .await
        .map_err(|e| OAuthError::Config(format!("endpoint discovery failed: {e}")))?;

        {
            let mut config = self.config.write().await;
            if config.auth_url.is_empty() {
                config.auth_url = endpoints.authorization_endpoint;
            }
            if config.token_url.is_empty() {
                config.token_url = endpoints.token_endpoint;
            }
            if config.scopes.is_empty() {
                config.scopes = endpoints.scopes;
            }
        }

        self.remember_resource(endpoints.resource).await;
        Ok(())
    }

    /// Prepares the client for an authorization-code flow: discovers endpoints
    /// and, when no `client_id` is configured, registers a client dynamically
    /// (RFC 7591).
    ///
    /// The MCP authorization spec requires a pre-registered client, a Client ID
    /// Metadata Document, or dynamic registration. Only the first and last are
    /// implemented here; without either, the flow fails rather than guessing.
    pub async fn prepare(&self) -> Result<(), OAuthError> {
        let (needs_endpoints, needs_client, redirect_uri, scopes) = {
            let config = self.config.read().await;
            (
                config.auth_url.is_empty() || config.token_url.is_empty(),
                config.client_id.is_none(),
                config.redirect_uri.clone(),
                config.scopes.clone(),
            )
        };

        let mut registration_endpoint = None;
        if needs_endpoints || needs_client {
            let (explicit_auth, explicit_token) = {
                let config = self.config.read().await;
                (
                    (!config.auth_url.is_empty()).then(|| config.auth_url.clone()),
                    (!config.token_url.is_empty()).then(|| config.token_url.clone()),
                )
            };

            let endpoints = crate::discovery::discover_endpoints(
                &self.server_url,
                explicit_auth.as_deref(),
                explicit_token.as_deref(),
            )
            .await
            .map_err(|e| OAuthError::Config(format!("endpoint discovery failed: {e}")))?;

            {
                let mut config = self.config.write().await;
                if config.auth_url.is_empty() {
                    config.auth_url = endpoints.authorization_endpoint.clone();
                }
                if config.token_url.is_empty() {
                    config.token_url = endpoints.token_endpoint.clone();
                }
                if config.scopes.is_empty() {
                    config.scopes = endpoints.scopes.clone();
                }
            }
            registration_endpoint = endpoints.registration_endpoint;
            self.remember_resource(endpoints.resource).await;

            // Persist endpoints so a restart can resume the flow.
            let (auth_url, token_url) = {
                let config = self.config.read().await;
                (config.auth_url.clone(), config.token_url.clone())
            };
            crate::auth::try_update_endpoints(
                &self.mcp_name,
                &auth_url,
                &token_url,
                Some(&self.server_url),
            )
            .await?;
        }

        if needs_client {
            let registration_endpoint = registration_endpoint.ok_or_else(|| {
                OAuthError::Config(
                    "no client_id configured and the authorization server does not support \
                     dynamic client registration"
                        .into(),
                )
            })?;

            let registered =
                crate::discovery::register_client(&registration_endpoint, &redirect_uri, &scopes)
                    .await
                    .map_err(|e| {
                        OAuthError::Config(format!("dynamic client registration failed: {e}"))
                    })?;

            {
                let mut config = self.config.write().await;
                config.client_id = Some(registered.client_id.clone());
                if registered.client_secret.is_some() {
                    config.client_secret = registered.client_secret.clone();
                }
            }

            crate::auth::try_update_client_info(
                &self.mcp_name,
                crate::auth::OAuthClientInfo {
                    client_id: registered.client_id,
                    client_secret: registered.client_secret,
                    client_id_issued_at: None,
                    client_secret_expires_at: None,
                },
                Some(&self.server_url),
            )
            .await?;
        }

        Ok(())
    }

    fn parsed_config(
        &self,
        config: &McpOAuthConfig,
    ) -> Result<
        (
            ClientId,
            Option<ClientSecret>,
            AuthUrl,
            TokenUrl,
            RedirectUrl,
        ),
        OAuthError,
    > {
        let client_id = config
            .client_id
            .as_deref()
            .ok_or_else(|| OAuthError::Config("client_id is required".into()))?;

        let auth_url = AuthUrl::new(config.auth_url.clone())
            .map_err(|e| OAuthError::Config(format!("invalid auth_url: {e}")))?;
        let token_url = TokenUrl::new(config.token_url.clone())
            .map_err(|e| OAuthError::Config(format!("invalid token_url: {e}")))?;
        let redirect_url = RedirectUrl::new(config.redirect_uri.clone())
            .map_err(|e| OAuthError::Config(format!("invalid redirect_uri: {e}")))?;

        let secret = config
            .client_secret
            .as_ref()
            .map(|s| ClientSecret::new(s.clone()));

        Ok((
            ClientId::new(client_id.to_string()),
            secret,
            auth_url,
            token_url,
            redirect_url,
        ))
    }
    /// Start the authorization flow.
    ///
    /// Returns the URL the user should open in a browser.  The PKCE verifier
    /// and CSRF state are persisted both in-memory and on disk so that
    /// `finish_auth` can complete the exchange even after a process restart.
    pub async fn start_auth(&self) -> Result<String, OAuthError> {
        let config = self.config.read().await.clone();
        let (client_id, client_secret, auth_url, token_url, redirect_url) =
            self.parsed_config(&config)?;

        let mut client = oauth2::basic::BasicClient::new(client_id)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);

        if let Some(secret) = client_secret {
            client = client.set_client_secret(secret);
        }

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let mut auth_request = client.authorize_url(CsrfToken::new_random);
        for scope in &config.scopes {
            auth_request = auth_request.add_scope(Scope::new(scope.clone()));
        }
        // RFC 8707: bind the authorization request to the resource advertised
        // by the MCP server. When absent, behavior is unchanged.
        let resource = self.resource.read().await.as_ref().cloned();
        let auth_request = with_resource_param(auth_request, resource);

        let (url, csrf_state) = auth_request.set_pkce_challenge(pkce_challenge).url();

        // Persist to disk for cross-restart resilience. A corrupt or unreadable
        // store must abort the flow instead of silently dropping PKCE/CSRF state.
        auth::try_update_code_verifier(&self.mcp_name, pkce_verifier.secret()).await?;
        auth::try_update_oauth_state(&self.mcp_name, csrf_state.secret()).await?;

        // Also keep in memory for the fast path.
        {
            let mut pending = self.pending.write().await;
            *pending = Some(PendingAuth {
                pkce_verifier: pkce_verifier.secret().clone(),
                csrf_state: csrf_state.secret().clone(),
            });
        }

        Ok(url.to_string())
    }
    /// Complete the authorization flow by exchanging the authorization code
    /// for tokens.
    pub async fn finish_auth(&self, code: &str, state: &str) -> Result<(), OAuthError> {
        // Recover verifier + expected state.
        let (verifier_secret, expected_state) = {
            let pending = self.pending.read().await;
            match pending.as_ref() {
                Some(p) => (p.pkce_verifier.clone(), p.csrf_state.clone()),
                None => {
                    // No in-memory pending flow: recover verifier/state from disk.
                    // A broken store is an error, never "no pending auth".
                    let entry = auth::try_get(&self.mcp_name)
                        .await?
                        .ok_or(OAuthError::NoPendingAuth)?;
                    let v = entry.code_verifier.ok_or(OAuthError::NoPendingAuth)?;
                    let s = entry.oauth_state.ok_or(OAuthError::NoPendingAuth)?;
                    (v, s)
                }
            }
        };

        if state != expected_state {
            return Err(OAuthError::StateMismatch);
        }

        let config = self.config.read().await.clone();
        let (client_id, client_secret, auth_url, token_url, redirect_url) =
            self.parsed_config(&config)?;

        let mut client = oauth2::basic::BasicClient::new(client_id)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);

        if let Some(secret) = client_secret {
            client = client.set_client_secret(secret);
        }

        let http_client = reqwest::Client::new();

        let resource = self.resource.read().await.as_ref().cloned();
        let mut token_request = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier_secret));
        // RFC 8707: the token request must carry the same resource indicator
        // that was sent in the authorization request.
        if let Some(resource) = resource {
            token_request = token_request.add_extra_param("resource", resource);
        }

        let token_result = token_request
            .request_async(&http_client)
            .await
            .map_err(|e| OAuthError::TokenExchange(e.to_string()))?;

        self.save_token_result(&token_result).await?;

        // Clean up transient state. The clear operations are writes too; a
        // failing store must surface instead of leaving stale PKCE/CSRF state.
        auth::try_clear_code_verifier(&self.mcp_name).await?;
        auth::try_clear_oauth_state(&self.mcp_name).await?;
        {
            let mut pending = self.pending.write().await;
            *pending = None;
        }

        Ok(())
    }
    /// Refresh an expired access token using the stored refresh token.
    pub async fn refresh_token(&self) -> Result<(), OAuthError> {
        let entry = auth::try_get_for_url(&self.mcp_name, &self.server_url)
            .await?
            .ok_or(OAuthError::NoRefreshToken)?;

        let tokens = entry.tokens.ok_or(OAuthError::NoRefreshToken)?;
        let refresh = tokens.refresh_token.ok_or(OAuthError::NoRefreshToken)?;

        let config = self.config.read().await.clone();
        let (client_id, client_secret, auth_url, token_url, redirect_url) =
            self.parsed_config(&config)?;

        let mut client = oauth2::basic::BasicClient::new(client_id)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);

        if let Some(secret) = client_secret {
            client = client.set_client_secret(secret);
        }

        let http_client = reqwest::Client::new();

        let token_result = client
            .exchange_refresh_token(&RefreshToken::new(refresh))
            .request_async(&http_client)
            .await
            .map_err(|e| OAuthError::TokenRefresh(e.to_string()))?;

        self.save_token_result(&token_result).await?;

        Ok(())
    }

    /// Helper: persist a token response to the auth store.
    async fn save_token_result<EF: oauth2::ExtraTokenFields>(
        &self,
        token_result: &oauth2::StandardTokenResponse<EF, oauth2::basic::BasicTokenType>,
    ) -> Result<(), OAuthError> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let tokens = auth::OAuthTokens {
            access_token: token_result.access_token().secret().clone(),
            refresh_token: token_result.refresh_token().map(|t| t.secret().clone()),
            expires_at: token_result
                .expires_in()
                .map(|d| now_secs + d.as_secs_f64()),
            scope: token_result.scopes().map(|s| {
                s.iter()
                    .map(|sc| sc.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        };

        auth::try_update_tokens(&self.mcp_name, tokens, Some(&self.server_url)).await?;

        Ok(())
    }
    /// Return a valid access token, refreshing if necessary.
    ///
    /// Returns `None` if no tokens are stored (the caller should initiate the
    /// auth flow).
    pub async fn get_token(&self) -> Result<Option<String>, OAuthError> {
        let entry = match auth::try_get_for_url(&self.mcp_name, &self.server_url).await? {
            Some(e) => e,
            None => return Ok(None),
        };

        let tokens = match &entry.tokens {
            Some(t) => t,
            None => return Ok(None),
        };

        // Check expiry (with a 60-second buffer).
        if let Some(exp) = tokens.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            if exp < now + 60.0 {
                if tokens.refresh_token.is_some() {
                    self.refresh_token().await?;
                    if let Some(refreshed) =
                        auth::try_get_for_url(&self.mcp_name, &self.server_url).await?
                    {
                        if let Some(t) = refreshed.tokens {
                            return Ok(Some(t.access_token));
                        }
                    }
                }
                return Ok(None);
            }
        }

        Ok(Some(tokens.access_token.clone()))
    }

    /// Remove all stored OAuth credentials for this server.
    pub async fn remove_auth(&self) -> Result<(), OAuthError> {
        auth::try_remove(&self.mcp_name).await?;
        let mut pending = self.pending.write().await;
        *pending = None;
        Ok(())
    }

    /// Whether tokens exist on disk for this server + URL combination.
    ///
    /// Fail-loud variant: a corrupt or unreadable store is reported as
    /// [`OAuthError::AuthStore`] rather than being mistaken for "no tokens".
    pub async fn try_has_stored_tokens(&self) -> Result<bool, OAuthError> {
        Ok(auth::try_get_for_url(&self.mcp_name, &self.server_url)
            .await?
            .and_then(|entry| entry.tokens)
            .is_some())
    }

    /// Compatibility wrapper for [`Self::try_has_stored_tokens`].
    ///
    /// 読み込み失敗はログへ残し `false` を返す（互換のための fail-open）。
    /// 認可フロー判断には [`Self::try_has_stored_tokens`] を使うこと。
    pub async fn has_stored_tokens(&self) -> bool {
        match self.try_has_stored_tokens().await {
            Ok(has_tokens) => has_tokens,
            Err(error) => {
                tracing::error!(%error, mcp_name = %self.mcp_name, "MCP stored-token check failed");
                false
            }
        }
    }

    /// Authentication status, propagating auth store failures.
    pub async fn try_auth_status(&self) -> Result<AuthStatus, OAuthError> {
        let entry = auth::try_get_for_url(&self.mcp_name, &self.server_url).await?;
        Ok(match entry {
            Some(entry) => match auth::is_token_expired(&entry) {
                Some(true) => AuthStatus::Expired,
                Some(false) => AuthStatus::Authenticated,
                None => AuthStatus::NotAuthenticated,
            },
            None => AuthStatus::NotAuthenticated,
        })
    }

    /// Compatibility wrapper for [`Self::try_auth_status`].
    ///
    /// 読み込み失敗はログへ残し [`AuthStatus::NotAuthenticated`] を返す。
    /// 認可状態の判定には [`Self::try_auth_status`] を使うこと。
    pub async fn auth_status(&self) -> AuthStatus {
        match self.try_auth_status().await {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(%error, mcp_name = %self.mcp_name, "MCP auth status read failed");
                AuthStatus::NotAuthenticated
            }
        }
    }

    pub fn mcp_name(&self) -> &str {
        &self.mcp_name
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }
}

/// High-level authentication status for an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatus {
    Authenticated,
    Expired,
    NotAuthenticated,
}

/// Registry of OAuth managers, keyed by MCP server name.
pub struct OAuthRegistry {
    managers: RwLock<HashMap<String, Arc<McpOAuthManager>>>,
}

impl OAuthRegistry {
    pub fn new() -> Self {
        Self {
            managers: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, manager: McpOAuthManager) -> Arc<McpOAuthManager> {
        let name = manager.mcp_name().to_string();
        let arc = Arc::new(manager);
        self.managers.write().await.insert(name, arc.clone());
        arc
    }

    pub async fn get(&self, mcp_name: &str) -> Option<Arc<McpOAuthManager>> {
        self.managers.read().await.get(mcp_name).cloned()
    }

    pub async fn remove(&self, mcp_name: &str) {
        self.managers.write().await.remove(mcp_name);
    }
}

impl Default for OAuthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Test basis: M-11 (this module previously had zero tests).
    // Risk tier: T1 (authentication / CSRF state). Design techniques: uniqueness
    // property, valid/invalid equivalence classes, and in-memory state transitions
    // (ISO/IEC/IEEE 29119-4). All values below are dummy or freshly generated
    // non-secret randomness; no real token, credential, or network I/O is used.

    const DUMMY_AUTH_URL: &str = "https://example.invalid/authorize";
    const DUMMY_TOKEN_URL: &str = "https://example.invalid/token";
    const DUMMY_SERVER_URL: &str = "https://example.invalid";
    const DUMMY_REDIRECT_URI: &str = "http://127.0.0.1:19876/mcp/oauth/callback";

    fn dummy_config() -> McpOAuthConfig {
        McpOAuthConfig {
            client_id: Some("dummy-client".to_string()),
            client_secret: Some("dummy-secret".to_string()),
            auth_url: DUMMY_AUTH_URL.to_string(),
            token_url: DUMMY_TOKEN_URL.to_string(),
            scopes: vec!["dummy:read".to_string(), "dummy:write".to_string()],
            redirect_uri: DUMMY_REDIRECT_URI.to_string(),
        }
    }

    fn manager_with(config: McpOAuthConfig) -> McpOAuthManager {
        McpOAuthManager::new(
            "dummy-server".to_string(),
            DUMMY_SERVER_URL.to_string(),
            config,
        )
    }

    /// Builds an authorization URL and asserts which `resource` params it has.
    /// The oauth2 client type changes to the `EndpointSet` variant after the
    /// `set_*_uri` calls, so all of those types stay local to this helper.
    fn assert_authorize_url_resource(config: &McpOAuthConfig, resource: Option<&str>) {
        let manager = manager_with(config.clone());
        let (client_id, client_secret, auth_url, token_url, redirect_url) = manager
            .parsed_config(config)
            .expect("dummy config must parse");
        let mut client = oauth2::basic::BasicClient::new(client_id)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);
        if let Some(secret) = client_secret {
            client = client.set_client_secret(secret);
        }

        let (challenge, _verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client.authorize_url(CsrfToken::new_random);
        for scope in &config.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let request = with_resource_param(request, resource.map(str::to_string));
        let (url, _state) = request.set_pkce_challenge(challenge).url();

        let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let resources: Vec<&str> = params
            .iter()
            .filter(|(key, _)| key == "resource")
            .map(|(_, value)| value.as_str())
            .collect();
        let expected: Vec<&str> = resource.into_iter().collect();
        assert_eq!(resources, expected);
    }

    #[test]
    fn csrf_state_is_unique_and_non_empty() {
        const N: usize = 32;
        let states: Vec<String> = (0..N)
            .map(|_| CsrfToken::new_random().secret().clone())
            .collect();

        assert!(
            states.iter().all(|state| !state.is_empty()),
            "generated CSRF state must never be empty"
        );
        let unique: HashSet<&str> = states.iter().map(String::as_str).collect();
        assert_eq!(
            unique.len(),
            N,
            "each authorization request must receive a distinct CSRF state"
        );
    }

    #[test]
    fn default_config_is_safe_and_uses_loopback_redirect() {
        let config = McpOAuthConfig::default();
        assert!(config.client_id.is_none());
        assert!(config.client_secret.is_none());
        assert!(config.auth_url.is_empty());
        assert!(config.token_url.is_empty());
        assert!(config.scopes.is_empty());
        assert_eq!(config.redirect_uri, DUMMY_REDIRECT_URI);
    }

    #[test]
    fn parsed_config_requires_client_id() {
        let manager = manager_with(McpOAuthConfig::default());
        let err = manager
            .parsed_config(&McpOAuthConfig::default())
            .expect_err("a missing client_id must be rejected");
        assert!(matches!(err, OAuthError::Config(_)));
    }

    #[test]
    fn parsed_config_rejects_invalid_urls() {
        let manager = manager_with(dummy_config());
        let cases = [
            (
                "auth_url",
                McpOAuthConfig {
                    auth_url: "not a url".to_string(),
                    ..dummy_config()
                },
            ),
            (
                "token_url",
                McpOAuthConfig {
                    token_url: "not a url".to_string(),
                    ..dummy_config()
                },
            ),
            (
                "redirect_uri",
                McpOAuthConfig {
                    redirect_uri: "not a url".to_string(),
                    ..dummy_config()
                },
            ),
        ];

        for (field, config) in cases {
            let err = manager
                .parsed_config(&config)
                .expect_err("an invalid URL must be rejected");
            assert!(matches!(err, OAuthError::Config(_)), "{field} should fail");
        }
    }

    #[test]
    fn parsed_config_builds_expected_authorize_url() {
        // Mirrors start_auth's pure URL construction without its disk writes.
        let config = dummy_config();
        let manager = manager_with(config.clone());
        let (client_id, client_secret, auth_url, token_url, redirect_url) = manager
            .parsed_config(&config)
            .expect("dummy config must parse");

        let mut client = oauth2::basic::BasicClient::new(client_id)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);
        if let Some(secret) = client_secret {
            client = client.set_client_secret(secret);
        }

        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client.authorize_url(CsrfToken::new_random);
        for scope in &config.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, state) = request.set_pkce_challenge(challenge).url();

        assert!(!state.secret().is_empty());
        assert!(!verifier.secret().is_empty());
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("example.invalid"));

        let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let has = |key: &str, value: &str| params.iter().any(|(k, v)| k == key && v == value);
        assert!(has("client_id", "dummy-client"));
        assert!(has("redirect_uri", DUMMY_REDIRECT_URI));
        assert!(has("response_type", "code"));
        assert!(has("code_challenge_method", "S256"));
        assert!(has("state", state.secret()));
        assert!(params
            .iter()
            .any(|(k, v)| k == "code_challenge" && !v.is_empty()));
        // oauth2 encodes all scopes as one space-separated `scope` parameter.
        let scopes: Vec<&str> = params
            .iter()
            .filter(|(k, _)| k == "scope")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(scopes, vec!["dummy:read dummy:write"]);
    }

    #[tokio::test]
    async fn set_endpoints_updates_only_in_memory_config() {
        let manager = manager_with(McpOAuthConfig::default());
        manager
            .set_endpoints(
                "https://example.invalid/auth2",
                "https://example.invalid/token2",
            )
            .await;

        let config = manager.config.read().await;
        assert_eq!(config.auth_url, "https://example.invalid/auth2");
        assert_eq!(config.token_url, "https://example.invalid/token2");
    }

    #[tokio::test]
    async fn ensure_endpoints_is_noop_when_configured() {
        let manager = manager_with(dummy_config());
        let scopes_before = manager.config.read().await.scopes.clone();

        manager
            .ensure_endpoints()
            .await
            .expect("configured endpoints must not trigger discovery");

        let config = manager.config.read().await;
        assert_eq!(config.auth_url, DUMMY_AUTH_URL);
        assert_eq!(config.token_url, DUMMY_TOKEN_URL);
        assert_eq!(config.scopes, scopes_before);
    }

    #[tokio::test]
    async fn registry_register_get_remove_uses_memory_only() {
        let registry = OAuthRegistry::new();
        let manager = manager_with(dummy_config());
        let registered = registry.register(manager).await;
        assert_eq!(registered.mcp_name(), "dummy-server");

        let fetched = registry
            .get("dummy-server")
            .await
            .expect("registered manager must be retrievable");
        assert_eq!(fetched.server_url(), DUMMY_SERVER_URL);

        registry.remove("dummy-server").await;
        assert!(registry.get("dummy-server").await.is_none());
    }

    #[test]
    fn authorize_url_includes_resource_param_when_discovered() {
        // Pure URL construction; no disk writes and no real OAuth server.
        assert_authorize_url_resource(&dummy_config(), Some("https://mcp.example.invalid/mcp"));
    }

    #[test]
    fn authorize_url_omits_resource_param_when_absent() {
        assert_authorize_url_resource(&dummy_config(), None);
    }

    #[tokio::test]
    async fn remember_resource_keeps_discovered_indicator() {
        let manager = manager_with(dummy_config());
        manager
            .remember_resource(Some("https://mcp.example.invalid/mcp".to_string()))
            .await;
        // A later discovery without the field must not erase a known value.
        manager.remember_resource(None).await;

        let resource = manager.resource.read().await;
        assert_eq!(resource.as_deref(), Some("https://mcp.example.invalid/mcp"));
    }

    // --- fail-loud auth store regression tests -------------------------------
    //
    // These scenarios use no real credentials and no network I/O: `start_auth`
    // and the callback's disk-recovery path fail before any token request.

    use std::path::PathBuf;

    const CORRUPT_STORE: &str = "{not valid json";

    fn env_points_to(path: &std::path::Path) -> bool {
        match std::env::var_os("OPENCODE_MCP_AUTH_FILE") {
            Some(value) => std::path::Path::new(&value) == path,
            None => false,
        }
    }

    /// Redirects the auth store to an isolated temp file. `corrupt` seeds
    /// unparsable JSON; otherwise an empty store is seeded. This mirrors the
    /// isolation pattern used by `auth`'s own store tests.
    struct TestAuthFile {
        dir: PathBuf,
        path: PathBuf,
        corrupt: bool,
    }

    impl TestAuthFile {
        fn new(label: &str, corrupt: bool) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "opencode-mcp-oauth-{pid}-{label}",
                pid = std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let path = dir.join("mcp-auth.json");
            Self { dir, path, corrupt }
        }

        /// (Re)installs the env var and the file contents before every attempt.
        fn activate(&self) {
            let contents = if self.corrupt { CORRUPT_STORE } else { "{}" };
            std::fs::write(&self.path, contents).expect("seed auth store");
            std::env::set_var("OPENCODE_MCP_AUTH_FILE", &self.path);
        }
    }

    impl Drop for TestAuthFile {
        fn drop(&mut self) {
            // Never unset a value another test installed.
            if env_points_to(self.path.as_path()) {
                std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// `OPENCODE_MCP_AUTH_FILE` is process-global and `auth`'s tests use it as
    /// well. Wait while it points elsewhere, and retry a scenario if another
    /// test clobbers it mid-flight, so this regression test stays deterministic.
    async fn wait_for_store_slot(store: &TestAuthFile) {
        for _ in 0..200 {
            match std::env::var_os("OPENCODE_MCP_AUTH_FILE") {
                Some(value) if std::path::Path::new(&value) != store.path.as_path() => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                _ => return,
            }
        }
    }

    async fn with_isolated_store<T, F, Fut>(store: &TestAuthFile, scenario: F) -> T
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        for _ in 0..20 {
            wait_for_store_slot(store).await;
            store.activate();
            let result = scenario().await;
            if env_points_to(store.path.as_path()) {
                return result;
            }
        }
        store.activate();
        scenario().await
    }

    #[tokio::test]
    async fn corrupt_auth_store_is_fail_loud() {
        // (a) start_auth must not silently discard PKCE verifier / CSRF state.
        {
            let store = TestAuthFile::new("corrupt-start", true);
            let manager = manager_with(dummy_config());
            let err = with_isolated_store(&store, || manager.start_auth())
                .await
                .expect_err("start_auth must fail on a corrupt auth store");
            assert!(
                matches!(&err, OAuthError::AuthStore(_)),
                "unexpected: {err}"
            );
            assert!(
                manager.pending.read().await.is_none(),
                "no transient state may be installed when persistence fails"
            );
        }

        // (b) Callback path: pending is empty, so finish_auth recovers the
        // verifier/state from disk. Corruption must fail the callback.
        {
            let store = TestAuthFile::new("corrupt-callback", true);
            let manager = manager_with(dummy_config());
            assert!(manager.pending.read().await.is_none());
            let err =
                with_isolated_store(&store, || manager.finish_auth("dummy-code", "dummy-state"))
                    .await
                    .expect_err("callback must fail on a corrupt auth store");
            assert!(
                matches!(&err, OAuthError::AuthStore(_)),
                "unexpected: {err}"
            );
        }

        // (c) Token reads and status queries must not round corruption to
        // "no credentials".
        {
            let store = TestAuthFile::new("corrupt-queries", true);
            let manager = manager_with(dummy_config());
            let err = with_isolated_store(&store, || manager.get_token())
                .await
                .expect_err("get_token must fail on a corrupt auth store");
            assert!(matches!(&err, OAuthError::AuthStore(_)));

            let err = with_isolated_store(&store, || manager.try_has_stored_tokens())
                .await
                .expect_err("token check must fail on a corrupt auth store");
            assert!(matches!(&err, OAuthError::AuthStore(_)));

            let err = with_isolated_store(&store, || manager.try_auth_status())
                .await
                .expect_err("status check must fail on a corrupt auth store");
            assert!(matches!(&err, OAuthError::AuthStore(_)));
        }
    }

    #[tokio::test]
    async fn healthy_auth_store_preserves_oauth_flow() {
        let store = TestAuthFile::new("healthy", false);
        let manager = manager_with(dummy_config());

        let (url, verifier, state, stored_state, stored_verifier) =
            with_isolated_store(&store, || async {
                let url = manager.start_auth().await?;
                let (verifier, state) = {
                    let pending = manager.pending.read().await;
                    let pending = pending.as_ref().expect("in-memory pending auth");
                    (pending.pkce_verifier.clone(), pending.csrf_state.clone())
                };
                let stored_state = auth::try_get_oauth_state("dummy-server").await?;
                let stored_verifier = auth::try_get("dummy-server")
                    .await?
                    .and_then(|entry| entry.code_verifier);
                Ok::<_, OAuthError>((url, verifier, state, stored_state, stored_verifier))
            })
            .await
            .expect("a healthy store must support the normal flow");

        assert!(url.starts_with(DUMMY_AUTH_URL));
        assert!(!verifier.is_empty());
        assert!(!state.is_empty());
        assert_eq!(stored_state.as_deref(), Some(state.as_str()));
        assert_eq!(stored_verifier.as_deref(), Some(verifier.as_str()));
    }

    #[test]
    fn auth_store_error_display_never_exposes_values() {
        const SENTINEL: &str = "SENTINEL_DO_NOT_LEAK";
        let io_error = auth::AuthStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            SENTINEL,
        ));
        let json_error = auth::AuthStoreError::Json(
            serde_json::from_str::<serde_json::Value>(SENTINEL)
                .expect_err("the sentinel is not valid JSON"),
        );

        for source in [io_error, json_error] {
            let err = OAuthError::from(source);
            assert!(matches!(&err, OAuthError::AuthStore(_)));
            assert_eq!(
                err.to_string(),
                "MCP auth store failure",
                "error text must carry the failure type only"
            );
            assert!(!err.to_string().contains(SENTINEL));
        }
    }
}
