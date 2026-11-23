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
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, RwLock};

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

/// Maximum lifetime of an in-flight authorization flow.
///
/// Flows abandoned by a client (or left behind by a restart) must not keep
/// their PKCE verifier / CSRF state resident in memory and on disk forever.
const FLOW_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// OAuth フローの有効期限。環境変数 `OPENCODE_MCP_OAUTH_FLOW_TTL_SECS` に
/// 正の整数を設定した場合のみ上書きし、不正値・0 は既定値へフォールバックする
/// （AGENTS.md §4 の設定外部化方針）。
fn flow_ttl() -> std::time::Duration {
    std::env::var("OPENCODE_MCP_OAUTH_FLOW_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(FLOW_TTL)
}

/// Transient state kept during an in-progress authorization flow.
#[derive(Debug)]
struct PendingAuth {
    pkce_verifier: String,
    csrf_state: String,
    /// When the flow was issued. Used by [`McpOAuthManager::reclaim_expired_pending`]
    /// to bound how long a stale flow may occupy memory and disk.
    created_at: std::time::Instant,
}

/// Outcome of discovery plus dynamic client registration for one server.
///
/// Shared between manager instances that target the same server so concurrent
/// callers never register a second client (RFC 7591).
#[derive(Clone)]
struct PreparedClient {
    auth_url: String,
    token_url: String,
    client_id: String,
    client_secret: Option<String>,
    scopes: Vec<String>,
    resource: Option<String>,
}

/// Identifies one OAuth server across every manager instance in the process.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PrepareKey {
    mcp_name: String,
    server_url: String,
}

/// Process-wide slots for [`McpOAuthManager::prepare`].
///
/// Callers such as `oauth_client_for` construct a fresh manager per request,
/// so an instance-local lock cannot prevent duplicate registration. The
/// short-lived `std::sync::Mutex` only guards the slot map; the long-lived
/// per-server `tokio::sync::Mutex` serializes the network work itself.
type PreparedSlot = Arc<Mutex<Option<PreparedClient>>>;

static PREPARE_SLOTS: OnceLock<std::sync::Mutex<HashMap<PrepareKey, PreparedSlot>>> =
    OnceLock::new();

fn prepare_slot(key: &PrepareKey) -> PreparedSlot {
    let slots = PREPARE_SLOTS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut slots = slots
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    slots
        .entry(key.clone())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Drop a cached preparation when credentials for the server are removed.
fn forget_prepare_slot(key: &PrepareKey) {
    if let Some(slots) = PREPARE_SLOTS.get() {
        let mut slots = slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slots.remove(key);
    }
}

/// Identifies one OAuth server's refresh critical section across every manager
/// instance in the process. Mirrors [`PrepareKey`] (the same `(mcp_name,
/// server_url)` pair that keys the auth store): callers such as
/// `oauth_client_for` build a fresh manager per request, so an instance-local
/// lock cannot serialize two refreshes that target the very same stored token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefreshKey {
    mcp_name: String,
    server_url: String,
}

/// Process-wide refresh locks, keyed by [`RefreshKey`].
///
/// The short-lived `std::sync::Mutex` only guards the slot map; the long-lived
/// `tokio::sync::Mutex` serializes the network exchange itself and is shared by
/// every manager instance for that server. Poisoned map locks are recovered via
/// `into_inner`, matching [`prepare_slot`].
type SharedRefreshLock = Arc<Mutex<()>>;

static REFRESH_LOCKS: OnceLock<std::sync::Mutex<HashMap<RefreshKey, SharedRefreshLock>>> =
    OnceLock::new();

fn refresh_lock_for(mcp_name: &str, server_url: &str) -> SharedRefreshLock {
    let key = RefreshKey {
        mcp_name: mcp_name.to_string(),
        server_url: server_url.to_string(),
    };
    let locks = REFRESH_LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Drop a process-wide refresh lock when credentials for the server are
/// removed, mirroring [`forget_prepare_slot`]. In-flight holders keep their
/// `Arc`, so a waiting exchange is unaffected (R4).
fn forget_refresh_lock(mcp_name: &str, server_url: &str) {
    if let Some(locks) = REFRESH_LOCKS.get() {
        let key = RefreshKey {
            mcp_name: mcp_name.to_string(),
            server_url: server_url.to_string(),
        };
        let mut locks = locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.remove(&key);
    }
}

/// Manages the OAuth lifecycle for a single MCP server.
pub struct McpOAuthManager {
    mcp_name: String,
    server_url: String,
    config: RwLock<McpOAuthConfig>,
    /// In-flight flows keyed by their CSRF state. Keying (instead of a single
    /// `Option`) keeps every concurrently issued state resolvable by
    /// `finish_auth`; a plain overwrite would silently drop one flow.
    pending: RwLock<HashMap<String, PendingAuth>>,
    /// RFC 8707 resource indicator discovered during endpoint discovery.
    /// Kept on the manager (not `McpOAuthConfig`) so existing external config
    /// struct literals remain source-compatible. `None` omits the parameter.
    resource: RwLock<Option<String>>,
    /// Serializes the HTTP refresh exchange for this server (RETRY-03, R4).
    /// The guard comes from the process-wide [`REFRESH_LOCKS`] map keyed by
    /// `(mcp_name, server_url)`, so distinct manager instances targeting the
    /// same stored token share one critical section. `get_token` observes
    /// expiry without this lock, then takes it only in the expired branch and
    /// re-checks the store inside the critical section so two concurrent
    /// callers cannot exchange the same refresh token twice. The public
    /// `refresh_token()` also takes this lock, covering the 401 retry path.
    /// This is the `tokio::sync::Mutex` imported above (not `std::sync`).
    refresh_lock: Arc<Mutex<()>>,
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
    /// An expired flow was addressed by `finish_auth`. The message is fixed:
    /// it never carries the state, token, URL or storage path.
    #[error("OAuth authorization flow has expired")]
    FlowExpired,
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
        let refresh_lock = refresh_lock_for(&mcp_name, &server_url);
        Self {
            mcp_name,
            server_url,
            config: RwLock::new(config),
            pending: RwLock::new(HashMap::new()),
            resource: RwLock::new(None),
            refresh_lock,
        }
    }

    /// Disk key for the transient record of one in-flight authorization flow.
    ///
    /// The PKCE verifier / CSRF state are persisted under a key derived from
    /// the flow's CSRF `state` instead of the fixed `mcp_name`. The auth store
    /// is a flat `HashMap<String, AuthEntry>`, so this gives each concurrent
    /// flow its own record and prevents a later `start_auth` from overwriting
    /// an earlier flow (or `finish_auth` from wiping another flow's record).
    /// Tokens and client info remain under `mcp_name`, so token lookup and
    /// refresh are unaffected.
    ///
    /// The returned key contains the raw state but is only ever used as a
    /// storage key — it is never logged or included in an error message.
    fn flow_disk_key(&self, state: &str) -> String {
        format!("{}-oauth-flow-{}", self.mcp_name, state)
    }

    /// Reclaim in-memory flows older than [`flow_ttl`], together with their
    /// disk records.
    ///
    /// Called at the flow entry points so a client that abandons the flow (or a
    /// process that is later restarted while a flow is still fresh in memory)
    /// cannot accumulate stale state. Only entries carrying an in-memory
    /// `created_at` can be aged out; a restart clears that memory, so records
    /// whose flow was already abandoned before the restart remain on disk until
    /// `remove_auth` is called.
    ///
    /// Returns the states that were reclaimed so `finish_auth` can distinguish
    /// an expired flow from one that never existed. The returned values are
    /// internal only — they are never logged or embedded in an error message.
    async fn reclaim_expired_pending(&self) -> Result<Vec<String>, OAuthError> {
        let expired: Vec<String> = {
            let pending = self.pending.read().await;
            pending
                .iter()
                .filter(|(_, auth)| auth.created_at.elapsed() >= flow_ttl())
                .map(|(state, _)| state.clone())
                .collect()
        };
        if expired.is_empty() {
            return Ok(expired);
        }

        // Remove the disk record first: if persistence fails we keep the
        // in-memory entry (still marked expired) and surface the error, so a
        // later call retries instead of silently accepting a stale flow.
        for state in &expired {
            auth::try_remove(&self.flow_disk_key(state)).await?;
        }
        {
            let mut pending = self.pending.write().await;
            for state in &expired {
                pending.remove(state);
            }
        }
        Ok(expired)
    }

    /// Records the RFC 8707 resource indicator from discovered endpoints.
    /// A missing value leaves any previously discovered indicator untouched.
    async fn remember_resource(&self, resource: Option<String>) {
        if let Some(resource) = resource {
            *self.resource.write().await = Some(resource);
        }
    }

    /// Fill only missing fields from a result produced by another manager
    /// instance, so explicitly configured values keep precedence.
    async fn apply_prepared(&self, prepared: &PreparedClient) {
        {
            let mut config = self.config.write().await;
            if config.auth_url.is_empty() {
                config.auth_url = prepared.auth_url.clone();
            }
            if config.token_url.is_empty() {
                config.token_url = prepared.token_url.clone();
            }
            if config.scopes.is_empty() {
                config.scopes = prepared.scopes.clone();
            }
            if config.client_id.is_none() {
                config.client_id = Some(prepared.client_id.clone());
            }
            if config.client_secret.is_none() {
                config.client_secret = prepared.client_secret.clone();
            }
        }
        self.remember_resource(prepared.resource.clone()).await;
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
        // Serialize discovery + dynamic registration per (mcp_name, server_url)
        // across every manager instance in this process. Callers that build a
        // fresh manager per request would otherwise register the same server
        // twice when two requests race here.
        let key = PrepareKey {
            mcp_name: self.mcp_name.clone(),
            server_url: self.server_url.clone(),
        };
        let slot = prepare_slot(&key);
        let mut prepared_guard = slot.lock().await;
        if let Some(prepared) = prepared_guard.as_ref() {
            self.apply_prepared(prepared).await;
            return Ok(());
        }

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

        // Publish the outcome for later (or previously blocked) managers of
        // the same server. Only a fully prepared client is reusable.
        {
            let config = self.config.read().await;
            if let Some(client_id) = config.client_id.clone().filter(|id| !id.is_empty()) {
                let resource = self.resource.read().await.clone();
                *prepared_guard = Some(PreparedClient {
                    auth_url: config.auth_url.clone(),
                    token_url: config.token_url.clone(),
                    client_id,
                    client_secret: config.client_secret.clone(),
                    scopes: config.scopes.clone(),
                    resource,
                });
            }
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
        // Age out abandoned flows before issuing a new state. Called before
        // taking the write lock below, so it cannot deadlock on `pending`.
        self.reclaim_expired_pending().await?;

        // One write lock spans state issuance, disk persistence and the in-memory
        // update. Concurrent calls can no longer interleave their PKCE/CSRF
        // values or overwrite each other's pending flow.
        let mut pending = self.pending.write().await;

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
        // Store under a state-derived flow key so a concurrent second flow does
        // not overwrite this one's verifier/state on disk.
        let flow_key = self.flow_disk_key(csrf_state.secret());
        // 2 回の try_update_* に分けると並行フローで read-modify-write が競合し、
        // 片方の verifier が消えるため 1 回の try_set で両方を保存する。
        let mut flow_entry = auth::try_get(&flow_key).await?.unwrap_or_default();
        flow_entry.code_verifier = Some(pkce_verifier.secret().clone());
        flow_entry.oauth_state = Some(csrf_state.secret().clone());
        auth::try_set(&flow_key, flow_entry, None).await?;

        // Also keep in memory for the fast path, keyed by the issued state so a
        // second concurrent flow stays resolvable by `finish_auth`.
        pending.insert(
            csrf_state.secret().clone(),
            PendingAuth {
                pkce_verifier: pkce_verifier.secret().clone(),
                csrf_state: csrf_state.secret().clone(),
                created_at: std::time::Instant::now(),
            },
        );
        drop(pending);

        Ok(url.to_string())
    }
    /// Complete the authorization flow by exchanging the authorization code
    /// for tokens.
    pub async fn finish_auth(&self, code: &str, state: &str) -> Result<(), OAuthError> {
        // Age out stale flows first. If the requested `state` was among them,
        // reject it with a fixed error before any token exchange; the state
        // value itself is never echoed back.
        let reclaimed = self.reclaim_expired_pending().await?;
        if reclaimed.iter().any(|expired| expired == state) {
            return Err(OAuthError::FlowExpired);
        }

        // Recover verifier + expected state.
        let (verifier_secret, expected_state) = {
            let pending = self.pending.read().await;
            match pending.get(state) {
                Some(p) => (p.pkce_verifier.clone(), p.csrf_state.clone()),
                None => {
                    // No in-memory pending flow: recover this flow's
                    // verifier/state from its own state-keyed disk record. A
                    // broken store is an error, never "no pending auth".
                    let entry = auth::try_get(&self.flow_disk_key(state))
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

        // Clean up transient state for THIS flow only. Removing the whole
        // flow record keeps a concurrent flow's verifier/state intact on disk.
        // The removal is a write too; a failing store must surface instead of
        // leaving stale PKCE/CSRF state.
        auth::try_remove(&self.flow_disk_key(state)).await?;
        {
            let mut pending = self.pending.write().await;
            // Only the completed flow is removed; other in-flight flows stay.
            pending.remove(state);
        }

        Ok(())
    }
    /// Refresh an expired access token using the stored refresh token.
    ///
    /// This public entry point always acquires the process-wide per-server
    /// [`Self::refresh_lock`] before doing any work. Callers that bypass
    /// `get_token` — notably the 401 retry path in `client.rs` — are therefore
    /// serialized with expiry-driven refreshes and with each other, even when
    /// they hold a different `McpOAuthManager` instance for the same server.
    /// A forced refresh needs no double-check; taking the lock is enough.
    ///
    /// The actual exchange lives in [`Self::refresh_token_unlocked`] so that
    /// [`Self::refresh_if_needed`], which already holds the lock, can reuse it
    /// without deadlocking on a non-reentrant `tokio::sync::Mutex`.
    pub async fn refresh_token(&self) -> Result<(), OAuthError> {
        let _guard = self.refresh_lock.lock().await;
        self.refresh_token_unlocked().await
    }

    /// Refresh implementation that assumes the caller already holds
    /// `refresh_lock`. Do not call directly from outside: use
    /// [`Self::refresh_token`] (or [`Self::refresh_if_needed`] from `get_token`).
    async fn refresh_token_unlocked(&self) -> Result<(), OAuthError> {
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

        // Check expiry (with a 60-second buffer). This fast path deliberately
        // does NOT take `refresh_lock`: a still-valid token is returned exactly
        // as before, with no serialization overhead (RETRY-03).
        if let Some(exp) = tokens.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            if exp < now + 60.0 {
                // The token is expired. Serialize the refresh and re-check the
                // freshest store state inside the lock so a concurrent caller
                // that already refreshed is reused instead of exchanging the
                // same refresh token a second time.
                return self.refresh_if_needed().await;
            }
        }

        Ok(Some(tokens.access_token.clone()))
    }

    /// Refresh an expired token under `McpOAuthManager::refresh_lock` using a
    /// double-checked read of the auth store (RETRY-03).
    ///
    /// The caller has already observed expiry *before* taking the lock. Inside
    /// the lock the newest store state is re-read and re-checked: when another
    /// concurrent caller refreshed in the meantime, that fresh token is
    /// returned and no second HTTP exchange occurs. `refresh_token()` runs
    /// only while the token is still expired. Neither token values nor the
    /// store path are logged or embedded in errors.
    async fn refresh_if_needed(&self) -> Result<Option<String>, OAuthError> {
        let _guard = self.refresh_lock.lock().await;

        // Re-read the latest token; another task may have updated the store
        // while we waited for the lock.
        let entry = match auth::try_get_for_url(&self.mcp_name, &self.server_url).await? {
            Some(entry) => entry,
            None => return Ok(None),
        };
        let tokens = match entry.tokens {
            Some(tokens) => tokens,
            None => return Ok(None),
        };

        // Second check under the lock: still expired (same 60s buffer)?
        let still_expired = match tokens.expires_at {
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                exp < now + 60.0
            }
            None => false,
        };

        if !still_expired {
            // Someone else already refreshed the token while we waited.
            return Ok(Some(tokens.access_token));
        }

        if tokens.refresh_token.is_none() {
            return Ok(None);
        }

        // The lock is already held, so call the unlocked implementation:
        // `refresh_token()` would try to re-acquire this non-reentrant mutex.
        self.refresh_token_unlocked().await?;

        // Read back the token the winning caller just stored.
        if let Some(refreshed) = auth::try_get_for_url(&self.mcp_name, &self.server_url).await? {
            if let Some(tokens) = refreshed.tokens {
                return Ok(Some(tokens.access_token));
            }
        }
        Ok(None)
    }

    /// Remove all stored OAuth credentials for this server.
    pub async fn remove_auth(&self) -> Result<(), OAuthError> {
        auth::try_remove(&self.mcp_name).await?;
        // Also drop any in-flight flow records keyed by their CSRF state, so
        // logout leaves no verifier/state behind on disk.
        let states: Vec<String> = {
            let pending = self.pending.read().await;
            pending.keys().cloned().collect()
        };
        for state in &states {
            auth::try_remove(&self.flow_disk_key(state)).await?;
        }
        self.pending.write().await.clear();
        forget_prepare_slot(&PrepareKey {
            mcp_name: self.mcp_name.clone(),
            server_url: self.server_url.clone(),
        });
        forget_refresh_lock(&self.mcp_name, &self.server_url);
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
                manager.pending.read().await.is_empty(),
                "no transient state may be installed when persistence fails"
            );
        }

        // (b) Callback path: pending is empty, so finish_auth recovers the
        // verifier/state from disk. Corruption must fail the callback.
        {
            let store = TestAuthFile::new("corrupt-callback", true);
            let manager = manager_with(dummy_config());
            assert!(manager.pending.read().await.is_empty());
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
                    let pending = pending.values().next().expect("in-memory pending auth");
                    (pending.pkce_verifier.clone(), pending.csrf_state.clone())
                };
                let flow_key = manager.flow_disk_key(&state);
                let stored_state = auth::try_get_oauth_state(&flow_key).await?;
                let stored_verifier = auth::try_get(&flow_key)
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

    /// RES-04 regression: a flow older than `FLOW_TTL` is reclaimed from both
    /// memory and disk, and `finish_auth` rejects it with a fixed error that
    /// carries no state, token, URL or storage path.
    #[tokio::test]
    async fn expired_pending_is_reclaimed_and_rejected() {
        let store = TestAuthFile::new("ttl-expired", false);
        let manager = manager_with(dummy_config());

        with_isolated_store(&store, || {
            let manager = &manager;
            async move {
                let url = manager.start_auth().await?;
                let parsed = url::Url::parse(&url).expect("authorization URL must parse");
                let state = parsed
                    .query_pairs()
                    .find(|(key, _)| key.as_ref() == "state")
                    .map(|(_, value)| value.into_owned())
                    .expect("authorization URL must carry a state");
                let flow_key = manager.flow_disk_key(&state);

                // Fresh flow: present in memory and persisted on disk.
                assert!(manager.pending.read().await.contains_key(&state));
                assert!(auth::try_get(&flow_key).await?.is_some());

                // Age the entry past the TTL.
                {
                    let mut pending = manager.pending.write().await;
                    pending.get_mut(&state).expect("pending flow").created_at =
                        std::time::Instant::now() - FLOW_TTL - std::time::Duration::from_secs(1);
                }

                let err = manager
                    .finish_auth("dummy-code", &state)
                    .await
                    .expect_err("an expired state must never reach token exchange");
                assert!(matches!(err, OAuthError::FlowExpired));
                // Fixed wording: no state value, URL or path is disclosed.
                assert_eq!(err.to_string(), "OAuth authorization flow has expired");
                assert!(!err.to_string().contains(state.as_str()));
                assert!(!err.to_string().contains(DUMMY_AUTH_URL));

                assert!(manager.pending.read().await.get(&state).is_none());
                assert!(auth::try_get(&flow_key).await?.is_none());
                Ok::<_, OAuthError>(())
            }
        })
        .await
        .expect("expired-flow scenario must run");
    }

    /// RES-04 regression: reclamation ages out only expired flows. A fresh
    /// flow keeps both its in-memory entry and its state-keyed disk record.
    #[tokio::test]
    async fn reclaim_only_removes_expired_entries() {
        let store = TestAuthFile::new("ttl-mixed", false);
        let manager = manager_with(dummy_config());

        with_isolated_store(&store, || {
            let manager = &manager;
            async move {
                // Two concurrent flows, each with its own in-memory entry and
                // state-keyed disk record.
                let (first, second) = tokio::join!(manager.start_auth(), manager.start_auth());
                let _ = (first?, second?);

                let mut states: Vec<String> =
                    manager.pending.read().await.keys().cloned().collect();
                states.sort();
                assert_eq!(states.len(), 2, "both flows must be pending");
                let expired_state = states[0].clone();
                let fresh_state = states[1].clone();
                let expired_key = manager.flow_disk_key(&expired_state);
                let fresh_key = manager.flow_disk_key(&fresh_state);

                {
                    let mut pending = manager.pending.write().await;
                    pending
                        .get_mut(&expired_state)
                        .expect("first pending flow")
                        .created_at =
                        std::time::Instant::now() - FLOW_TTL - std::time::Duration::from_secs(1);
                }

                let reclaimed = manager.reclaim_expired_pending().await?;
                assert_eq!(reclaimed, vec![expired_state.clone()]);

                assert!(manager.pending.read().await.get(&expired_state).is_none());
                assert!(manager.pending.read().await.get(&fresh_state).is_some());
                assert!(auth::try_get(&expired_key).await?.is_none());
                assert!(auth::try_get(&fresh_key).await?.is_some());
                Ok::<_, OAuthError>(())
            }
        })
        .await
        .expect("mixed reclaim scenario must run");
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

    // --- concurrency regression tests -------------------------------------
    //
    // No real credentials and no external network: only loopback HTTP mocks
    // and the isolated auth store are used.

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex as StdMutex, Once};
    use std::thread;
    use std::time::{Duration, Instant};

    static PREPARE_TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn ensure_no_proxy_for_loopback() {
        static SET: Once = Once::new();
        SET.call_once(|| {
            std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
            std::env::set_var("no_proxy", "127.0.0.1,localhost");
        });
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("mock must read request");
            assert!(read > 0, "unexpected EOF while reading request headers");
            buf.extend_from_slice(&chunk[..read]);
            if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]);
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buf.len() < header_end + content_length {
            let read = stream
                .read(&mut chunk)
                .expect("mock must read request body");
            assert!(read > 0, "unexpected EOF while reading request body");
            buf.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Minimal loopback OAuth server: resource metadata, AS metadata, register.
    struct PrepareMock {
        base_url: String,
        seen: Arc<StdMutex<Vec<String>>>,
        thread: thread::JoinHandle<()>,
    }

    impl PrepareMock {
        fn spawn() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
            let addr = listener.local_addr().expect("loopback addr must succeed");
            let base_url = format!("http://{addr}");
            let base_for_thread = base_url.clone();
            let seen = Arc::new(StdMutex::new(Vec::new()));
            let seen_for_thread = Arc::clone(&seen);
            let thread = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("mock listener must be nonblocking");
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut last_request: Option<Instant> = None;
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(5)))
                                .expect("mock stream read timeout");
                            let request = read_http_request(&mut stream);
                            let (status, body) = if request.starts_with("POST /register ") {
                                ("200 OK", r#"{"client_id":"registered-client"}"#.to_string())
                            } else if request.contains("/.well-known/oauth-protected-resource") {
                                (
                                    "200 OK",
                                    format!(
                                        r#"{{"resource":"{b}/mcp","authorization_servers":["{b}"],"scopes_supported":["mcp:read"]}}"#,
                                        b = base_for_thread
                                    ),
                                )
                            } else if request.contains("/.well-known/oauth-authorization-server") {
                                (
                                    "200 OK",
                                    format!(
                                        r#"{{"issuer":"{b}","authorization_endpoint":"{b}/authorize","token_endpoint":"{b}/token","registration_endpoint":"{b}/register","scopes_supported":["mcp:read"]}}"#,
                                        b = base_for_thread
                                    ),
                                )
                            } else {
                                ("404 Not Found", "not found".to_string())
                            };
                            seen_for_thread
                                .lock()
                                .expect("mock seen lock")
                                .push(request);
                            let headers = format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            stream
                                .write_all(headers.as_bytes())
                                .expect("mock must write headers");
                            stream
                                .write_all(body.as_bytes())
                                .expect("mock must write body");
                            last_request = Some(Instant::now());
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            let quiet = last_request
                                .is_some_and(|at| at.elapsed() > Duration::from_millis(300));
                            if quiet || Instant::now() >= deadline {
                                break;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock accept failed: {error}"),
                    }
                }
            });
            Self {
                base_url,
                seen,
                thread,
            }
        }

        fn requests(self) -> Vec<String> {
            self.thread.join().expect("mock thread must not panic");
            self.seen.lock().expect("mock seen lock").clone()
        }
    }

    /// Loopback OAuth token endpoint that counts exchange requests. Every
    /// request receives a valid token response so `exchange_refresh_token`
    /// succeeds. `hits` reveals how many refreshes actually happened, while
    /// `peak` records the maximum number of exchanges in flight at once — this
    /// is what distinguishes "serialized" from "deduplicated" (a forced
    /// refresh legitimately runs twice, but never concurrently). Each accepted
    /// connection is served on its own thread so overlapping requests are
    /// observable. No credentials or external network are involved.
    struct TokenMock {
        token_url: String,
        hits: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl TokenMock {
        fn spawn() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
            let addr = listener.local_addr().expect("loopback addr must succeed");
            let token_url = format!("http://{addr}/token");
            let hits = Arc::new(AtomicUsize::new(0));
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let hits_for_thread = Arc::clone(&hits);
            let active_for_thread = Arc::clone(&active);
            let peak_for_thread = Arc::clone(&peak);
            let stop_for_thread = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("mock listener must be nonblocking");
                let mut handlers = Vec::new();
                while !stop_for_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let hits = Arc::clone(&hits_for_thread);
                            let active = Arc::clone(&active_for_thread);
                            let peak = Arc::clone(&peak_for_thread);
                            handlers.push(thread::spawn(move || {
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(5)))
                                    .expect("mock stream read timeout");
                                let _request = read_http_request(&mut stream);
                                let in_flight = active.fetch_add(1, Ordering::SeqCst) + 1;
                                peak.fetch_max(in_flight, Ordering::SeqCst);
                                // Widen the overlap window: without the shared
                                // lock, two forced refreshes would be in flight
                                // at the same time and `peak` would reach 2.
                                thread::sleep(Duration::from_millis(30));
                                hits.fetch_add(1, Ordering::SeqCst);
                                let body = r#"{"access_token":"refreshed-access-token","token_type":"Bearer","expires_in":3600,"refresh_token":"refreshed-refresh-token"}"#;
                                let headers = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                );
                                stream
                                    .write_all(headers.as_bytes())
                                    .expect("mock must write headers");
                                stream
                                    .write_all(body.as_bytes())
                                    .expect("mock must write body");
                                active.fetch_sub(1, Ordering::SeqCst);
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock accept failed: {error}"),
                    }
                }
                for handler in handlers {
                    let _ = handler.join();
                }
            });
            Self {
                token_url,
                hits,
                peak,
                stop,
                thread: Some(thread),
            }
        }

        /// Stop the accept loop and join the threads, then return the final
        /// (exchange count, peak in-flight count). Mirrors
        /// `PrepareMock::requests()` so each isolated-store attempt owns its
        /// own counters.
        fn finish_with_peak(&mut self) -> (usize, usize) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            (
                self.hits.load(Ordering::SeqCst),
                self.peak.load(Ordering::SeqCst),
            )
        }

        /// Stop the accept loop and return only the exchange count.
        fn finish(&mut self) -> usize {
            self.finish_with_peak().0
        }
    }

    impl Drop for TokenMock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[tokio::test]
    async fn concurrent_start_auth_keeps_both_states() {
        let store = TestAuthFile::new("concurrent-start", false);
        let manager = manager_with(dummy_config());

        let states = with_isolated_store(&store, || {
            let manager = &manager;
            async move {
                manager.pending.write().await.clear();
                let (first, second) = tokio::join!(manager.start_auth(), manager.start_auth());
                let urls = [first?, second?];
                let mut states = Vec::new();
                for url in &urls {
                    let parsed = url::Url::parse(url).expect("authorization URL must parse");
                    let state = parsed
                        .query_pairs()
                        .find(|(key, _)| key.as_ref() == "state")
                        .map(|(_, value)| value.into_owned())
                        .expect("authorization URL must carry a state");
                    states.push(state);
                }
                Ok::<_, OAuthError>(states)
            }
        })
        .await
        .expect("both concurrent flows must start");

        assert_eq!(states.len(), 2);
        assert_ne!(states[0], states[1], "each flow needs a distinct state");
        let pending = manager.pending.read().await;
        assert_eq!(pending.len(), 2, "neither concurrent flow may be dropped");
        for state in &states {
            let flow = pending
                .get(state)
                .expect("every issued state must remain resolvable");
            assert!(!flow.pkce_verifier.is_empty());
            assert_eq!(&flow.csrf_state, state);
        }
    }

    /// Regression (C): two concurrent `start_auth` calls must produce two
    /// distinct disk records. The first flow stays recoverable from disk (no
    /// overwrite), and removing one flow's record leaves the other intact —
    /// this is exactly what `finish_auth` now does via `try_remove(flow_key)`.
    #[tokio::test]
    async fn concurrent_start_auth_keeps_both_flows_on_disk() {
        let store = TestAuthFile::new("concurrent-start-disk", false);
        let manager = manager_with(dummy_config());

        let states = with_isolated_store(&store, || {
            let manager = &manager;
            async move {
                manager.pending.write().await.clear();
                let (first, second) = tokio::join!(manager.start_auth(), manager.start_auth());
                let urls = [first?, second?];
                let mut states = Vec::new();
                for url in &urls {
                    let parsed = url::Url::parse(url).expect("authorization URL must parse");
                    let state = parsed
                        .query_pairs()
                        .find(|(key, _)| key.as_ref() == "state")
                        .map(|(_, value)| value.into_owned())
                        .expect("authorization URL must carry a state");
                    states.push(state);
                }
                Ok::<_, OAuthError>(states)
            }
        })
        .await
        .expect("both concurrent flows must start");

        assert_eq!(states.len(), 2);
        assert_ne!(states[0], states[1]);

        // with_isolated_store を再度呼ぶと activate() が store を "{}" で
        // 上書きするため、ここでは env が自 store を指していることだけ確認し、
        // 書き込まれたデータをそのまま読む。
        assert!(
            env_points_to(store.path.as_path()),
            "test store must still be active after start_auth"
        );
        {
            let manager = &manager;
            let states = &states;
            async move {
                let mut flow_keys = Vec::new();
                for state in states {
                    let flow_key = manager.flow_disk_key(state);
                    let disk_state = auth::try_get_oauth_state(&flow_key).await?;
                    let disk_verifier = auth::try_get(&flow_key)
                        .await?
                        .and_then(|entry| entry.code_verifier);
                    let pending_verifier = manager
                        .pending
                        .read()
                        .await
                        .get(state)
                        .expect("issued state must remain in memory")
                        .pkce_verifier
                        .clone();
                    assert_eq!(disk_state.as_deref(), Some(state.as_str()));
                    assert_eq!(disk_verifier.as_deref(), Some(pending_verifier.as_str()));
                    flow_keys.push(flow_key);
                }
                assert_ne!(
                    flow_keys[0], flow_keys[1],
                    "each concurrent flow must own a distinct disk record"
                );

                // Finishing one flow removes only that flow's record.
                auth::try_remove(&flow_keys[0]).await?;
                assert!(auth::try_get(&flow_keys[0]).await?.is_none());
                assert!(
                    auth::try_get(&flow_keys[1]).await?.is_some(),
                    "the other flow must survive the first flow's cleanup"
                );
                Ok::<_, OAuthError>(())
            }
            .await
            .expect("both flows must be recoverable and independently removable");
        }
    }

    #[tokio::test]
    async fn concurrent_prepare_registers_client_once() {
        ensure_no_proxy_for_loopback();
        let store = TestAuthFile::new("concurrent-prepare", false);

        let (results, client_ids, requests) = with_isolated_store(&store, || async {
            let mock = PrepareMock::spawn();
            let server_url = format!("{}/mcp", mock.base_url);
            let mcp_name = format!(
                "prepare-once-{}",
                PREPARE_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let make_manager = || {
                McpOAuthManager::new(
                    mcp_name.clone(),
                    server_url.clone(),
                    McpOAuthConfig {
                        client_id: None,
                        client_secret: None,
                        auth_url: String::new(),
                        token_url: String::new(),
                        scopes: Vec::new(),
                        redirect_uri: DUMMY_REDIRECT_URI.to_string(),
                    },
                )
            };
            let first = make_manager();
            let second = make_manager();
            let (result_a, result_b) = tokio::join!(first.prepare(), second.prepare());
            let client_ids = (
                first.config.read().await.client_id.clone(),
                second.config.read().await.client_id.clone(),
            );
            let requests = mock.requests();
            ([result_a, result_b], client_ids, requests)
        })
        .await;

        for result in &results {
            assert!(result.is_ok(), "prepare must succeed: {result:?}");
        }
        assert_eq!(client_ids.0.as_deref(), Some("registered-client"));
        assert_eq!(client_ids.1.as_deref(), Some("registered-client"));
        let registrations: Vec<&String> = requests
            .iter()
            .filter(|request| request.starts_with("POST /register "))
            .collect();
        assert_eq!(
            registrations.len(),
            1,
            "concurrent prepare must register exactly once: {requests:?}"
        );
    }

    /// RETRY-03 regression: two concurrent `get_token` calls that both observe
    /// an expired token must perform exactly one HTTP refresh exchange. The
    /// lock holder refreshes; the waiter re-reads the store inside the lock,
    /// sees the fresh token and skips its own exchange.
    #[tokio::test]
    async fn concurrent_get_token_refreshes_expired_token_once() {
        ensure_no_proxy_for_loopback();
        let store = TestAuthFile::new("concurrent-refresh", false);

        let (tokens, hits) = with_isolated_store(&store, || async {
            let mut mock = TokenMock::spawn();
            let config = McpOAuthConfig {
                client_id: Some("dummy-client".to_string()),
                client_secret: Some("dummy-secret".to_string()),
                auth_url: DUMMY_AUTH_URL.to_string(),
                token_url: mock.token_url.clone(),
                scopes: Vec::new(),
                redirect_uri: DUMMY_REDIRECT_URI.to_string(),
            };
            let manager = McpOAuthManager::new(
                "refresh-once".to_string(),
                DUMMY_SERVER_URL.to_string(),
                config,
            );

            // Seed an already-expired token (one hour in the past).
            let expired_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                - 3600.0;
            auth::try_update_tokens(
                "refresh-once",
                auth::OAuthTokens {
                    access_token: "expired-access-token".to_string(),
                    refresh_token: Some("dummy-refresh-token".to_string()),
                    expires_at: Some(expired_at),
                    scope: None,
                },
                Some(DUMMY_SERVER_URL),
            )
            .await?;

            let (first, second) = tokio::join!(manager.get_token(), manager.get_token());
            let tokens = (first?, second?);
            let hits = mock.finish();
            Ok::<_, OAuthError>((tokens, hits))
        })
        .await
        .expect("concurrent refresh scenario must run");

        assert_eq!(tokens.0.as_deref(), Some("refreshed-access-token"));
        assert_eq!(tokens.1.as_deref(), Some("refreshed-access-token"));
        assert_eq!(
            hits, 1,
            "two concurrent get_token calls must exchange the refresh token exactly once"
        );
    }

    /// R4 (a) regression: `oauth_client_for` builds a fresh manager per request,
    /// so two distinct instances for the same (mcp_name, server_url) must share
    /// one process-wide refresh lock. Both observe an expired token, but only
    /// one HTTP exchange happens: the waiter re-reads the freshly stored token
    /// inside the shared critical section and skips its own exchange.
    #[tokio::test]
    async fn concurrent_get_token_across_instances_refreshes_once() {
        ensure_no_proxy_for_loopback();
        let store = TestAuthFile::new("concurrent-refresh-cross-instance", false);

        let (tokens, hits) = with_isolated_store(&store, || async {
            let mut mock = TokenMock::spawn();
            let mcp_name = format!(
                "refresh-once-cross-{}",
                PREPARE_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let config = McpOAuthConfig {
                client_id: Some("dummy-client".to_string()),
                client_secret: Some("dummy-secret".to_string()),
                auth_url: DUMMY_AUTH_URL.to_string(),
                token_url: mock.token_url.clone(),
                scopes: Vec::new(),
                redirect_uri: DUMMY_REDIRECT_URI.to_string(),
            };
            let make_manager = || {
                McpOAuthManager::new(
                    mcp_name.clone(),
                    DUMMY_SERVER_URL.to_string(),
                    config.clone(),
                )
            };

            let expired_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                - 3600.0;
            auth::try_update_tokens(
                &mcp_name,
                auth::OAuthTokens {
                    access_token: "expired-access-token".to_string(),
                    refresh_token: Some("dummy-refresh-token".to_string()),
                    expires_at: Some(expired_at),
                    scope: None,
                },
                Some(DUMMY_SERVER_URL),
            )
            .await?;

            let first = make_manager();
            let second = make_manager();
            let (a, b) = tokio::join!(first.get_token(), second.get_token());
            let tokens = (a?, b?);
            let hits = mock.finish();
            Ok::<_, OAuthError>((tokens, hits))
        })
        .await
        .expect("cross-instance refresh scenario must run");

        assert_eq!(tokens.0.as_deref(), Some("refreshed-access-token"));
        assert_eq!(tokens.1.as_deref(), Some("refreshed-access-token"));
        assert_eq!(
            hits, 1,
            "two instances of one server must exchange the refresh token exactly once"
        );
    }

    /// R4 (b) regression: the public `refresh_token()` (called directly by the
    /// 401 retry path) now takes the same process-wide per-server lock as
    /// `get_token`. When a forced refresh races an expiry-driven refresh from a
    /// different manager instance, the exchanges must never be in flight
    /// together. `hits` may be 1 or 2 depending on which caller wins the lock
    /// (a forced refresh does not double-check), but `peak` must stay at 1.
    #[tokio::test]
    async fn public_refresh_token_serializes_with_get_token() {
        ensure_no_proxy_for_loopback();
        let store = TestAuthFile::new("public-refresh-vs-get-token", false);

        let (get_token_result, refresh_result, hits, peak) =
            with_isolated_store(&store, || async {
                let mut mock = TokenMock::spawn();
                let mcp_name = format!(
                    "refresh-vs-get-{}",
                    PREPARE_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
                );
                let config = McpOAuthConfig {
                    client_id: Some("dummy-client".to_string()),
                    client_secret: Some("dummy-secret".to_string()),
                    auth_url: DUMMY_AUTH_URL.to_string(),
                    token_url: mock.token_url.clone(),
                    scopes: Vec::new(),
                    redirect_uri: DUMMY_REDIRECT_URI.to_string(),
                };
                let make_manager = || {
                    McpOAuthManager::new(
                        mcp_name.clone(),
                        DUMMY_SERVER_URL.to_string(),
                        config.clone(),
                    )
                };

                let expired_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    - 3600.0;
                auth::try_update_tokens(
                    &mcp_name,
                    auth::OAuthTokens {
                        access_token: "expired-access-token".to_string(),
                        refresh_token: Some("dummy-refresh-token".to_string()),
                        expires_at: Some(expired_at),
                        scope: None,
                    },
                    Some(DUMMY_SERVER_URL),
                )
                .await?;

                let expiry_manager = make_manager();
                let forced_manager = make_manager();
                let (get_token_result, refresh_result) =
                    tokio::join!(expiry_manager.get_token(), forced_manager.refresh_token());
                let (hits, peak) = mock.finish_with_peak();
                Ok::<_, OAuthError>((get_token_result, refresh_result, hits, peak))
            })
            .await
            .expect("public-refresh vs get_token scenario must run");

        assert!(
            get_token_result.is_ok(),
            "get_token must succeed: {get_token_result:?}"
        );
        assert_eq!(
            get_token_result.unwrap().as_deref(),
            Some("refreshed-access-token")
        );
        assert!(
            refresh_result.is_ok(),
            "public refresh_token must succeed: {refresh_result:?}"
        );
        assert!(hits >= 1, "at least one exchange must have happened");
        assert_eq!(
            peak, 1,
            "the public refresh path must share the per-server lock with get_token"
        );
    }

    /// R4 (b) regression: two concurrent public `refresh_token()` calls from
    /// distinct instances both perform a forced exchange (no double-check by
    /// design), but the exchanges must be serialized by the shared per-server
    /// lock instead of overlapping — the case two simultaneous 401 retries
    /// would produce through `client.rs`.
    #[tokio::test]
    async fn concurrent_public_refresh_token_is_serialized() {
        ensure_no_proxy_for_loopback();
        let store = TestAuthFile::new("concurrent-public-refresh", false);

        let (results, hits, peak) = with_isolated_store(&store, || async {
            let mut mock = TokenMock::spawn();
            let mcp_name = format!(
                "public-refresh-{}",
                PREPARE_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let config = McpOAuthConfig {
                client_id: Some("dummy-client".to_string()),
                client_secret: Some("dummy-secret".to_string()),
                auth_url: DUMMY_AUTH_URL.to_string(),
                token_url: mock.token_url.clone(),
                scopes: Vec::new(),
                redirect_uri: DUMMY_REDIRECT_URI.to_string(),
            };
            let make_manager = || {
                McpOAuthManager::new(
                    mcp_name.clone(),
                    DUMMY_SERVER_URL.to_string(),
                    config.clone(),
                )
            };

            auth::try_update_tokens(
                &mcp_name,
                auth::OAuthTokens {
                    access_token: "seeded-access-token".to_string(),
                    refresh_token: Some("seeded-refresh-token".to_string()),
                    expires_at: Some(0.0),
                    scope: None,
                },
                Some(DUMMY_SERVER_URL),
            )
            .await?;

            let first = make_manager();
            let second = make_manager();
            let (a, b) = tokio::join!(first.refresh_token(), second.refresh_token());
            let (hits, peak) = mock.finish_with_peak();
            Ok::<_, OAuthError>(([a, b], hits, peak))
        })
        .await
        .expect("concurrent public refresh scenario must run");

        for result in &results {
            assert!(result.is_ok(), "public refresh must succeed: {result:?}");
        }
        assert_eq!(hits, 2, "each forced refresh performs its own exchange");
        assert_eq!(peak, 1, "the two exchanges must not overlap");
    }
}
