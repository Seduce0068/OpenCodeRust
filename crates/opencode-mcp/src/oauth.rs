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
}

impl McpOAuthManager {
    pub fn new(mcp_name: String, server_url: String, config: McpOAuthConfig) -> Self {
        Self {
            mcp_name,
            server_url,
            config: RwLock::new(config),
            pending: RwLock::new(None),
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

            // Persist endpoints so a restart can resume the flow.
            let (auth_url, token_url) = {
                let config = self.config.read().await;
                (config.auth_url.clone(), config.token_url.clone())
            };
            crate::auth::update_endpoints(
                &self.mcp_name,
                &auth_url,
                &token_url,
                Some(&self.server_url),
            )
            .await
            .map_err(OAuthError::Storage)?;
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

            crate::auth::update_client_info(
                &self.mcp_name,
                crate::auth::OAuthClientInfo {
                    client_id: registered.client_id,
                    client_secret: registered.client_secret,
                    client_id_issued_at: None,
                    client_secret_expires_at: None,
                },
                Some(&self.server_url),
            )
            .await
            .map_err(OAuthError::Storage)?;
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

        let (url, csrf_state) = auth_request.set_pkce_challenge(pkce_challenge).url();

        // Persist to disk for cross-restart resilience.
        auth::update_code_verifier(&self.mcp_name, pkce_verifier.secret())
            .await
            .map_err(OAuthError::Storage)?;
        auth::update_oauth_state(&self.mcp_name, csrf_state.secret())
            .await
            .map_err(OAuthError::Storage)?;

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
                    let entry = auth::get(&self.mcp_name)
                        .await
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

        let token_result = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier_secret))
            .request_async(&http_client)
            .await
            .map_err(|e| OAuthError::TokenExchange(e.to_string()))?;

        self.save_token_result(&token_result).await?;

        // Clean up transient state.
        auth::clear_code_verifier(&self.mcp_name).await.ok();
        auth::clear_oauth_state(&self.mcp_name).await.ok();
        {
            let mut pending = self.pending.write().await;
            *pending = None;
        }

        Ok(())
    }
    /// Refresh an expired access token using the stored refresh token.
    pub async fn refresh_token(&self) -> Result<(), OAuthError> {
        let entry = auth::get_for_url(&self.mcp_name, &self.server_url)
            .await
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

        auth::update_tokens(&self.mcp_name, tokens, Some(&self.server_url))
            .await
            .map_err(OAuthError::Storage)?;

        Ok(())
    }
    /// Return a valid access token, refreshing if necessary.
    ///
    /// Returns `None` if no tokens are stored (the caller should initiate the
    /// auth flow).
    pub async fn get_token(&self) -> Result<Option<String>, OAuthError> {
        let entry = match auth::get_for_url(&self.mcp_name, &self.server_url).await {
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
                        auth::get_for_url(&self.mcp_name, &self.server_url).await
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
        auth::remove(&self.mcp_name)
            .await
            .map_err(OAuthError::Storage)?;
        let mut pending = self.pending.write().await;
        *pending = None;
        Ok(())
    }

    /// Whether tokens exist on disk for this server + URL combination.
    pub async fn has_stored_tokens(&self) -> bool {
        auth::get_for_url(&self.mcp_name, &self.server_url)
            .await
            .and_then(|e| e.tokens)
            .is_some()
    }

    /// Authentication status.
    pub async fn auth_status(&self) -> AuthStatus {
        match auth::get_for_url(&self.mcp_name, &self.server_url).await {
            Some(entry) => match auth::is_token_expired(&entry) {
                Some(true) => AuthStatus::Expired,
                Some(false) => AuthStatus::Authenticated,
                None => AuthStatus::NotAuthenticated,
            },
            None => AuthStatus::NotAuthenticated,
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
