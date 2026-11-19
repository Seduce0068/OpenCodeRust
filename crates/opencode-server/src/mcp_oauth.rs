use chrono::Utc;
use opencode_mcp::client::{
    McpClientError, McpClientRegistry, McpServerConfig as McpClientConfig, McpTimeouts,
};
use opencode_mcp::oauth::{McpOAuthConfig as RealOAuthConfig, McpOAuthManager as RealOAuthManager};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct LocalMcpConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub cwd: Option<String>,
    pub timeouts: McpTimeouts,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteMcpConfig {
    pub url: String,
    pub oauth_enabled: bool,
    pub client_id: Option<String>,
    pub authorization_url: Option<String>,
    /// Static headers from the config (for API-key style remote servers).
    pub headers: Option<HashMap<String, String>>,
    pub timeouts: McpTimeouts,
    /// OAuth client settings from the config (`mcp.servers.<name>.oauth`).
    pub oauth: Option<RemoteOAuthSettings>,
    pub protocol: Option<String>,
}

/// OAuth client settings declared in the MCP server configuration.
#[derive(Debug, Clone, Default)]
pub struct RemoteOAuthSettings {
    pub client_secret: Option<String>,
    pub scope: Option<String>,
    pub callback_port: Option<u16>,
    pub redirect_uri: Option<String>,
    pub auth_server_metadata_url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum McpRuntimeConfig {
    Local(LocalMcpConfig),
    Remote(RemoteMcpConfig),
}

impl McpRuntimeConfig {
    fn oauth_required(&self) -> bool {
        matches!(
            self,
            McpRuntimeConfig::Remote(RemoteMcpConfig {
                oauth_enabled: true,
                ..
            })
        )
    }
}

/// Default loopback redirect used when the config does not set one.
pub const DEFAULT_MCP_OAUTH_REDIRECT: &str = "http://127.0.0.1:19876/mcp/oauth/callback";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthState {
    pub server_name: String,
    pub authorization_url: String,
    pub client_id: Option<String>,
    pub status: McpOAuthStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum McpOAuthStatus {
    Pending,
    Authorized,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub status: String,
    pub tools: usize,
    pub resources: usize,
    pub error: Option<String>,
    pub oauth_required: bool,
    pub oauth_status: Option<McpOAuthStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerLogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone)]
struct ManagedServer {
    config: McpRuntimeConfig,
    enabled: bool,
}

pub struct McpOAuthManager {
    oauth_states: Arc<RwLock<HashMap<String, McpOAuthState>>>,
    servers: Arc<RwLock<HashMap<String, ManagedServer>>>,
    statuses: Arc<RwLock<HashMap<String, McpServerInfo>>>,
    logs: Arc<RwLock<HashMap<String, Vec<McpServerLogEntry>>>>,
    clients: Arc<McpClientRegistry>,
    /// One real OAuth client per server, used for the authorization-code
    /// exchange and token refresh.
    oauth_clients: Arc<RwLock<HashMap<String, Arc<RealOAuthManager>>>>,
}

impl McpOAuthManager {
    pub fn new() -> Self {
        Self {
            oauth_states: Arc::new(RwLock::new(HashMap::new())),
            servers: Arc::new(RwLock::new(HashMap::new())),
            statuses: Arc::new(RwLock::new(HashMap::new())),
            logs: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(McpClientRegistry::new()),
            oauth_clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Returns (creating if needed) the real OAuth client for a server.
    async fn oauth_client_for(
        &self,
        server_name: &str,
    ) -> Result<Arc<RealOAuthManager>, McpOAuthError> {
        if let Some(existing) = self.oauth_clients.read().await.get(server_name) {
            return Ok(existing.clone());
        }

        let managed = self.managed_server(server_name).await?;
        let remote = match &managed.config {
            McpRuntimeConfig::Remote(remote) => remote,
            McpRuntimeConfig::Local(_) => {
                return Err(McpOAuthError::OAuthNotSupported(server_name.to_string()));
            }
        };

        let settings = remote.oauth.clone().unwrap_or_default();
        let redirect_uri = settings
            .redirect_uri
            .clone()
            .or_else(|| {
                settings
                    .callback_port
                    .map(|port| format!("http://127.0.0.1:{port}/mcp/oauth/callback"))
            })
            .unwrap_or_else(|| DEFAULT_MCP_OAUTH_REDIRECT.to_string());

        let config = RealOAuthConfig {
            client_id: remote.client_id.clone(),
            client_secret: settings.client_secret.clone(),
            auth_url: remote.authorization_url.clone().unwrap_or_default(),
            token_url: String::new(),
            scopes: settings
                .scope
                .as_deref()
                .map(|scope| scope.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
            redirect_uri,
        };

        let manager = Arc::new(RealOAuthManager::new(
            server_name.to_string(),
            remote.url.clone(),
            config,
        ));

        // Discovery plus dynamic client registration: fills the auth/token
        // endpoints when unset and registers a client when no client_id is
        // configured.
        manager
            .prepare()
            .await
            .map_err(|e| McpOAuthError::OAuthFailed(e.to_string()))?;

        self.oauth_clients
            .write()
            .await
            .insert(server_name.to_string(), manager.clone());
        Ok(manager)
    }

    async fn log_event(
        &self,
        server_name: &str,
        level: impl Into<String>,
        message: impl Into<String>,
    ) {
        let entry = McpServerLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            level: level.into(),
            message: message.into(),
        };
        self.logs
            .write()
            .await
            .entry(server_name.to_string())
            .or_default()
            .push(entry);
    }

    pub async fn has_server(&self, server_name: &str) -> bool {
        self.servers.read().await.contains_key(server_name)
    }

    pub async fn add_server(
        &self,
        server_name: String,
        config: McpRuntimeConfig,
        enabled: bool,
    ) -> McpServerInfo {
        let managed = ManagedServer { config, enabled };
        self.servers
            .write()
            .await
            .insert(server_name.clone(), managed.clone());

        let status = self
            .default_status_for(
                &server_name,
                &managed,
                self.oauth_status_for(&server_name).await,
            )
            .await;
        self.statuses
            .write()
            .await
            .insert(server_name.clone(), status.clone());
        self.log_event(&server_name, "info", "Server registered")
            .await;
        status
    }

    pub async fn connect(&self, server_name: &str) -> Result<McpServerInfo, McpOAuthError> {
        self.log_event(server_name, "info", "Connect requested")
            .await;
        let managed = self.managed_server(server_name).await?;
        let oauth_status = self.oauth_status_for(server_name).await;
        let oauth_required = managed.config.oauth_required();

        if !managed.enabled {
            let info = McpServerInfo {
                name: server_name.to_string(),
                status: "disabled".to_string(),
                tools: 0,
                resources: 0,
                error: None,
                oauth_required,
                oauth_status,
            };
            self.statuses
                .write()
                .await
                .insert(server_name.to_string(), info.clone());
            self.log_event(server_name, "info", "Server is disabled")
                .await;
            return Ok(info);
        }

        let info = match managed.config {
            McpRuntimeConfig::Local(local) => {
                if let Err(error) = self.clients.remove(server_name).await {
                    tracing::warn!(server = server_name, %error, "failed to remove existing MCP client");
                }

                let env = local
                    .env
                    .map(|map| map.into_iter().collect::<Vec<(String, String)>>());

                match self
                    .clients
                    .add_client(McpClientConfig {
                        name: server_name.to_string(),
                        command: local.command,
                        args: local.args,
                        env,
                        cwd: local.cwd,
                        timeouts: local.timeouts,
                        protocol: local.protocol,
                    })
                    .await
                {
                    Ok(_) => {
                        let tool_count = self
                            .clients
                            .tool_registry()
                            .list_for_server(server_name)
                            .await
                            .len();
                        self.log_event(server_name, "info", "Connected local MCP server")
                            .await;
                        McpServerInfo {
                            name: server_name.to_string(),
                            status: "connected".to_string(),
                            tools: tool_count,
                            resources: 0,
                            error: None,
                            oauth_required,
                            oauth_status: None,
                        }
                    }
                    Err(error) => {
                        self.log_event(server_name, "error", format!("Connect failed: {}", error))
                            .await;
                        McpServerInfo {
                            name: server_name.to_string(),
                            status: "failed".to_string(),
                            tools: 0,
                            resources: 0,
                            error: Some(error.to_string()),
                            oauth_required,
                            oauth_status: None,
                        }
                    }
                }
            }
            McpRuntimeConfig::Remote(remote) => {
                let has_static_headers = remote
                    .headers
                    .as_ref()
                    .is_some_and(|headers| !headers.is_empty());
                if oauth_required
                    && !has_static_headers
                    && oauth_status != Some(McpOAuthStatus::Authorized)
                {
                    self.log_event(server_name, "info", "Remote MCP requires OAuth")
                        .await;
                    McpServerInfo {
                        name: server_name.to_string(),
                        status: "needs_auth".to_string(),
                        tools: 0,
                        resources: 0,
                        error: None,
                        oauth_required,
                        oauth_status,
                    }
                } else {
                    if let Err(error) = self.clients.remove(server_name).await {
                        tracing::warn!(server = server_name, %error, "failed to remove existing MCP client");
                    }
                    match self
                        .clients
                        .add_http(
                            server_name.to_string(),
                            remote.url.clone(),
                            remote.headers.clone().filter(|headers| !headers.is_empty()),
                            remote.timeouts,
                            remote.protocol.clone(),
                        )
                        .await
                    {
                        Ok(_) => {
                            let tool_count = self
                                .clients
                                .tool_registry()
                                .list_for_server(server_name)
                                .await
                                .len();
                            self.log_event(server_name, "info", "Connected remote MCP server")
                                .await;
                            McpServerInfo {
                                name: server_name.to_string(),
                                status: "connected".to_string(),
                                tools: tool_count,
                                resources: 0,
                                error: None,
                                oauth_required,
                                oauth_status,
                            }
                        }
                        Err(McpClientError::Unauthorized) => {
                            self.log_event(server_name, "info", "Remote MCP requires OAuth")
                                .await;
                            McpServerInfo {
                                name: server_name.to_string(),
                                status: "needs_auth".to_string(),
                                tools: 0,
                                resources: 0,
                                error: None,
                                oauth_required: true,
                                oauth_status,
                            }
                        }
                        Err(error) => {
                            self.log_event(
                                server_name,
                                "error",
                                format!("Connect failed: {}", error),
                            )
                            .await;
                            McpServerInfo {
                                name: server_name.to_string(),
                                status: "failed".to_string(),
                                tools: 0,
                                resources: 0,
                                error: Some(error.to_string()),
                                oauth_required,
                                oauth_status,
                            }
                        }
                    }
                }
            }
        };

        self.statuses
            .write()
            .await
            .insert(server_name.to_string(), info.clone());
        self.log_event(
            server_name,
            "info",
            format!("Status updated: {}", info.status),
        )
        .await;
        Ok(info)
    }

    pub async fn disconnect(&self, server_name: &str) -> Result<McpServerInfo, McpOAuthError> {
        self.log_event(server_name, "info", "Disconnect requested")
            .await;
        let managed = self.managed_server(server_name).await?;
        self.clients
            .remove(server_name)
            .await
            .map_err(|e| McpOAuthError::RuntimeError(e.to_string()))?;

        let info = McpServerInfo {
            name: server_name.to_string(),
            status: "disabled".to_string(),
            tools: 0,
            resources: 0,
            error: None,
            oauth_required: managed.config.oauth_required(),
            oauth_status: self.oauth_status_for(server_name).await,
        };
        self.statuses
            .write()
            .await
            .insert(server_name.to_string(), info.clone());
        self.log_event(server_name, "info", "Disconnected").await;
        Ok(info)
    }

    pub async fn start_oauth(&self, server_name: &str) -> Result<McpOAuthState, McpOAuthError> {
        let managed = self.managed_server(server_name).await?;
        let remote = match managed.config {
            McpRuntimeConfig::Remote(remote) => remote,
            McpRuntimeConfig::Local(_) => {
                return Err(McpOAuthError::OAuthNotSupported(server_name.to_string()));
            }
        };

        if !remote.oauth_enabled {
            return Err(McpOAuthError::OAuthNotSupported(server_name.to_string()));
        }

        if self
            .oauth_states
            .read()
            .await
            .get(server_name)
            .is_some_and(|state| state.status == McpOAuthStatus::Pending)
        {
            return Err(McpOAuthError::OAuthInProgress);
        }

        // Build the real authorization URL via the OAuth client so the PKCE
        // verifier stored with it can complete the exchange on callback.
        let oauth_client = self.oauth_client_for(server_name).await?;
        let authorization_url = oauth_client
            .start_auth()
            .await
            .map_err(|e| McpOAuthError::OAuthFailed(e.to_string()))?;

        let state = McpOAuthState {
            server_name: server_name.to_string(),
            authorization_url,
            client_id: remote.client_id.clone(),
            status: McpOAuthStatus::Pending,
        };

        self.oauth_states
            .write()
            .await
            .insert(server_name.to_string(), state.clone());
        self.log_event(server_name, "info", "OAuth flow started")
            .await;

        let mut statuses = self.statuses.write().await;
        let info = statuses
            .entry(server_name.to_string())
            .or_insert_with(|| McpServerInfo {
                name: server_name.to_string(),
                status: "needs_auth".to_string(),
                tools: 0,
                resources: 0,
                error: None,
                oauth_required: true,
                oauth_status: Some(McpOAuthStatus::Pending),
            });
        info.status = "needs_auth".to_string();
        info.error = None;
        info.oauth_required = true;
        info.oauth_status = Some(McpOAuthStatus::Pending);
        drop(statuses);

        Ok(state)
    }

    pub async fn handle_callback(
        &self,
        server_name: &str,
        code: &str,
        returned_state: Option<&str>,
    ) -> Result<McpServerInfo, McpOAuthError> {
        self.managed_server(server_name).await?;

        {
            let states = self.oauth_states.read().await;
            states
                .get(server_name)
                .ok_or_else(|| McpOAuthError::OAuthFailed("No pending OAuth flow".to_string()))?;
        }

        // The caller must echo back the CSRF state. Falling back to the stored
        // state would make finish_auth's comparison vacuous, so reject a
        // missing value before any discovery or token exchange happens.
        let state = returned_state.ok_or_else(|| {
            McpOAuthError::OAuthFailed("OAuth callback requires the state parameter".to_string())
        })?;
        let oauth_client = self.oauth_client_for(server_name).await?;

        oauth_client
            .finish_auth(code, state)
            .await
            .map_err(|e| McpOAuthError::OAuthFailed(e.to_string()))?;

        // Only mark authorized once real tokens are stored.
        if !oauth_client.has_stored_tokens().await {
            return Err(McpOAuthError::OAuthFailed(
                "token exchange did not store credentials".to_string(),
            ));
        }

        let mut states = self.oauth_states.write().await;
        if let Some(state) = states.get_mut(server_name) {
            state.status = McpOAuthStatus::Authorized;
        }
        drop(states);
        self.log_event(server_name, "info", "OAuth callback completed")
            .await;

        self.connect(server_name).await
    }

    pub async fn authenticate(&self, server_name: &str) -> Result<McpServerInfo, McpOAuthError> {
        let managed = self.managed_server(server_name).await?;
        if !managed.config.oauth_required() {
            return Err(McpOAuthError::OAuthNotSupported(server_name.to_string()));
        }

        let current = self.oauth_status_for(server_name).await;
        if current == Some(McpOAuthStatus::Authorized) {
            self.log_event(server_name, "info", "OAuth already authorized")
                .await;
            return self.connect(server_name).await;
        }

        self.start_oauth(server_name).await?;
        self.connect(server_name).await
    }

    pub async fn get_server(&self, server_name: &str) -> Option<McpServerInfo> {
        if let Some(status) = self.statuses.read().await.get(server_name) {
            return Some(status.clone());
        }

        let managed = self.servers.read().await.get(server_name).cloned()?;
        Some(
            self.default_status_for(
                server_name,
                &managed,
                self.oauth_status_for(server_name).await,
            )
            .await,
        )
    }

    pub async fn list_servers(&self) -> Vec<McpServerInfo> {
        let servers = self.servers.read().await.clone();
        let statuses = self.statuses.read().await.clone();

        let mut out = Vec::with_capacity(servers.len());
        for (name, managed) in servers {
            if let Some(status) = statuses.get(&name) {
                out.push(status.clone());
                continue;
            }
            out.push(
                self.default_status_for(&name, &managed, self.oauth_status_for(&name).await)
                    .await,
            );
        }
        out
    }

    pub async fn get_logs(
        &self,
        server_name: &str,
    ) -> Result<Vec<McpServerLogEntry>, McpOAuthError> {
        self.managed_server(server_name).await?;
        Ok(self
            .logs
            .read()
            .await
            .get(server_name)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn restart(&self, server_name: &str) -> Result<McpServerInfo, McpOAuthError> {
        self.managed_server(server_name).await?;
        self.log_event(server_name, "info", "Restart requested")
            .await;
        let _ = self.disconnect(server_name).await;
        self.connect(server_name).await
    }

    pub async fn remove_oauth(&self, server_name: &str) -> bool {
        let removed = self
            .oauth_states
            .write()
            .await
            .remove(server_name)
            .is_some();

        // Logging out must actually delete the stored credentials, otherwise a
        // later connect would silently reuse the old tokens. Delete from disk
        // regardless of whether this process holds a cached client.
        self.oauth_clients.write().await.remove(server_name);
        if let Err(error) = opencode_mcp::auth::remove(server_name).await {
            tracing::warn!(server = server_name, %error, "failed to delete stored MCP OAuth credentials");
        }

        self.log_event(server_name, "info", "OAuth state removed")
            .await;

        if let Some(managed) = self.servers.read().await.get(server_name).cloned() {
            let mut info = self
                .statuses
                .read()
                .await
                .get(server_name)
                .cloned()
                .unwrap_or_else(|| McpServerInfo {
                    name: server_name.to_string(),
                    status: "disabled".to_string(),
                    tools: 0,
                    resources: 0,
                    error: None,
                    oauth_required: managed.config.oauth_required(),
                    oauth_status: None,
                });

            info.oauth_status = None;
            if managed.config.oauth_required() {
                info.status = if managed.enabled {
                    "needs_auth".to_string()
                } else {
                    "disabled".to_string()
                };
                info.error = None;
            }
            self.statuses
                .write()
                .await
                .insert(server_name.to_string(), info);
        }

        removed
    }

    async fn managed_server(&self, server_name: &str) -> Result<ManagedServer, McpOAuthError> {
        self.servers
            .read()
            .await
            .get(server_name)
            .cloned()
            .ok_or_else(|| McpOAuthError::ServerNotFound(server_name.to_string()))
    }

    async fn oauth_status_for(&self, server_name: &str) -> Option<McpOAuthStatus> {
        self.oauth_states
            .read()
            .await
            .get(server_name)
            .map(|state| state.status.clone())
    }

    async fn default_status_for(
        &self,
        server_name: &str,
        managed: &ManagedServer,
        oauth_status: Option<McpOAuthStatus>,
    ) -> McpServerInfo {
        let oauth_required = managed.config.oauth_required();
        let status = if !managed.enabled {
            "disabled"
        } else if oauth_required && oauth_status != Some(McpOAuthStatus::Authorized) {
            "needs_auth"
        } else {
            "disabled"
        };

        McpServerInfo {
            name: server_name.to_string(),
            status: status.to_string(),
            tools: 0,
            resources: 0,
            error: None,
            oauth_required,
            oauth_status,
        }
    }
}

impl Default for McpOAuthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum McpOAuthError {
    #[error("MCP server not found: {0}")]
    ServerNotFound(String),

    #[error("MCP server does not support OAuth: {0}")]
    OAuthNotSupported(String),

    #[error("OAuth already in progress")]
    OAuthInProgress,

    #[error("OAuth failed: {0}")]
    OAuthFailed(String),

    #[error("MCP runtime error: {0}")]
    RuntimeError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_server_requires_auth_before_connecting() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: true,
                    client_id: None,
                    authorization_url: None,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let status = manager
            .connect("remote")
            .await
            .expect("connect should return runtime status");
        assert_eq!(status.status, "needs_auth");
        assert_eq!(status.oauth_status, None);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn remove_oauth_reverts_status_to_needs_auth() {
        // `remove_oauth` deletes persisted credentials, so it touches the same
        // process-global store as the OAuth flow tests.
        let _guard = auth_store_lock();
        let auth_file = temp_auth_file("logout");
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &auth_file);

        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: true,
                    client_id: None,
                    authorization_url: None,
                    ..Default::default()
                }),
                true,
            )
            .await;

        // Seed an authorized state directly: the point of this test is the
        // removal behaviour, not the exchange.
        {
            let mut states = manager.oauth_states.write().await;
            states.insert(
                "remote".to_string(),
                McpOAuthState {
                    server_name: "remote".to_string(),
                    authorization_url: "https://idp.example.com/authorize".to_string(),
                    client_id: None,
                    status: McpOAuthStatus::Authorized,
                },
            );
        }

        assert!(manager.remove_oauth("remote").await);

        let status = manager
            .get_server("remote")
            .await
            .expect("remote server should exist");
        assert_eq!(status.status, "needs_auth");
        assert_eq!(status.oauth_status, None);

        std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
        let _ = std::fs::remove_file(&auth_file);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn logout_deletes_persisted_credentials() {
        let _guard = auth_store_lock();
        let auth_file = temp_auth_file("logout-delete");
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &auth_file);

        let server_url = "https://example.com/mcp";
        opencode_mcp::auth::update_tokens(
            "remote",
            opencode_mcp::auth::OAuthTokens {
                access_token: "secret-token".to_string(),
                refresh_token: None,
                expires_at: None,
                scope: None,
            },
            Some(server_url),
        )
        .await
        .expect("store tokens");
        assert!(
            opencode_mcp::auth::get_for_url("remote", server_url)
                .await
                .is_some_and(|entry| entry.tokens.is_some()),
            "precondition: credentials are stored"
        );

        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: server_url.to_string(),
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        manager.remove_oauth("remote").await;

        // Logging out must remove the credentials from disk, not just from
        // memory, or a later connect would silently reuse them.
        let remaining = opencode_mcp::auth::get_for_url("remote", server_url).await;
        assert!(
            remaining.and_then(|entry| entry.tokens).is_none(),
            "credentials must be deleted on logout"
        );

        std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
        let _ = std::fs::remove_file(&auth_file);
    }

    #[derive(Clone)]
    struct MockMcpState {
        status: axum::http::StatusCode,
        seen_authorization: Arc<tokio::sync::Mutex<Vec<Option<String>>>>,
        /// When set, the mock also publishes RFC 9728 protected-resource
        /// metadata naming this authorization server issuer.
        authorization_server: Option<String>,
    }

    /// Serializes tests that redirect the process-global MCP credential store.
    ///
    /// `OPENCODE_MCP_AUTH_FILE` is shared by the whole test binary, so tests
    /// that set it must not run concurrently with each other.
    fn auth_store_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Returns a temp path for a test-scoped MCP credential store.
    fn temp_auth_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "opencode-mcp-auth-{tag}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    /// Serves the RFC 9728 protected-resource metadata document.
    async fn mock_protected_resource_metadata(
        axum::extract::State(state): axum::extract::State<MockMcpState>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;

        match state.authorization_server {
            Some(issuer) => axum::Json(serde_json::json!({
                "resource": issuer,
                "authorization_servers": [issuer],
            }))
            .into_response(),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn mock_mcp_handler(
        axum::extract::State(state): axum::extract::State<MockMcpState>,
        headers: axum::http::HeaderMap,
        body: String,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;

        state.seen_authorization.lock().await.push(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
        );

        if !state.status.is_success() {
            return state.status.into_response();
        }

        let request: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let result = if method == "tools/list" {
            serde_json::json!({"tools": []})
        } else {
            serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "serverInfo": {"name": "mock", "version": "0.1"}
            })
        };
        (
            [("content-type", "application/json")],
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
            .into_response()
    }

    async fn spawn_mock_mcp(
        status: axum::http::StatusCode,
    ) -> (String, Arc<tokio::sync::Mutex<Vec<Option<String>>>>) {
        spawn_mock_mcp_with_auth(status, None).await
    }

    /// Spawns a mock MCP server. When `authorization_server` is set, the server
    /// also publishes protected-resource metadata for discovery.
    async fn spawn_mock_mcp_with_auth(
        status: axum::http::StatusCode,
        authorization_server: Option<String>,
    ) -> (String, Arc<tokio::sync::Mutex<Vec<Option<String>>>>) {
        let seen_authorization = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let state = MockMcpState {
            status,
            seen_authorization: seen_authorization.clone(),
            authorization_server,
        };
        let app = axum::Router::new()
            .route("/mcp", axum::routing::post(mock_mcp_handler))
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                axum::routing::get(mock_protected_resource_metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(mock_protected_resource_metadata),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/mcp"), seen_authorization)
    }

    #[tokio::test]
    async fn remote_server_connects_with_static_headers() {
        let (url, seen_authorization) = spawn_mock_mcp(axum::http::StatusCode::OK).await;
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url,
                    oauth_enabled: false,
                    headers: Some(HashMap::from([(
                        "Authorization".to_string(),
                        "Bearer test-token".to_string(),
                    )])),
                    ..Default::default()
                }),
                true,
            )
            .await;

        let status = manager
            .connect("remote")
            .await
            .expect("connect should return runtime status");
        assert_eq!(status.status, "connected");
        assert_eq!(status.tools, 0);

        let seen = seen_authorization.lock().await;
        assert!(
            seen.iter()
                .flatten()
                .any(|header| header == "Bearer test-token"),
            "Authorization header must reach the remote server: {seen:?}"
        );
    }

    #[tokio::test]
    async fn remote_server_returns_needs_auth_on_401() {
        let (url, _) = spawn_mock_mcp(axum::http::StatusCode::UNAUTHORIZED).await;
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url,
                    oauth_enabled: false,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let status = manager.connect("remote").await.expect("connect result");
        assert_eq!(status.status, "needs_auth");
    }

    #[tokio::test]
    async fn remote_server_reports_connect_failure() {
        let (url, _) = spawn_mock_mcp(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url,
                    oauth_enabled: false,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let status = manager.connect("remote").await.expect("connect result");
        assert_eq!(status.status, "failed");
        assert!(status.error.is_some());
    }
    #[tokio::test]
    async fn remote_server_honors_configured_timeouts() {
        let (url, _) = spawn_mock_mcp(axum::http::StatusCode::OK).await;
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url,
                    oauth_enabled: false,
                    headers: None,
                    timeouts: McpTimeouts {
                        startup_ms: 7000,
                        catalog_ms: 8000,
                        execution_ms: 9000,
                    },
                    ..Default::default()
                }),
                true,
            )
            .await;

        let status = manager.connect("remote").await.expect("connect result");
        assert_eq!(status.status, "connected");
    }
    /// Mock authorization server: publishes RFC 9728 resource metadata, RFC
    /// 8414 authorization-server metadata, and exchanges an authorization code
    /// for a token.
    async fn spawn_mock_authorization_server() -> (String, Arc<tokio::sync::Mutex<Vec<String>>>) {
        use axum::response::IntoResponse;

        let seen_requests: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        #[derive(Clone)]
        struct AuthState {
            pub issuer: String,
            pub seen: Arc<tokio::sync::Mutex<Vec<String>>>,
        }

        async fn resource_metadata(
            axum::extract::State(state): axum::extract::State<AuthState>,
        ) -> axum::response::Response {
            state
                .seen
                .lock()
                .await
                .push("resource-metadata".to_string());
            axum::Json(serde_json::json!({
                "resource": state.issuer,
                "authorization_servers": [state.issuer],
            }))
            .into_response()
        }

        async fn authorization_server_metadata(
            axum::extract::State(state): axum::extract::State<AuthState>,
        ) -> axum::response::Response {
            state
                .seen
                .lock()
                .await
                .push("authorization-server-metadata".to_string());
            axum::Json(serde_json::json!({
                "issuer": state.issuer,
                "authorization_endpoint": format!("{}/authorize", state.issuer),
                "token_endpoint": format!("{}/token", state.issuer),
                "registration_endpoint": format!("{}/register", state.issuer),
                "code_challenge_methods_supported": ["S256"],
            }))
            .into_response()
        }

        async fn register(
            axum::extract::State(state): axum::extract::State<AuthState>,
            body: String,
        ) -> axum::response::Response {
            state.seen.lock().await.push("register".to_string());
            let _ = body;
            axum::Json(serde_json::json!({
                "client_id": "dynamic-client-id",
                "client_secret": "dynamic-client-secret",
            }))
            .into_response()
        }

        async fn token(
            axum::extract::State(state): axum::extract::State<AuthState>,
            body: String,
        ) -> axum::response::Response {
            state.seen.lock().await.push("token".to_string());
            // Reject unless the caller presents the authorization code and a
            // PKCE verifier, which is what the client must send.
            if !body.contains("code=oauth-code") || !body.contains("code_verifier=") {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({"error": "invalid_request"})),
                )
                    .into_response();
            }
            axum::Json(serde_json::json!({
                "access_token": "access-token-123",
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": "refresh-token-123",
            }))
            .into_response()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock auth server");
        let addr = listener.local_addr().expect("mock auth addr");
        let issuer = format!("http://{addr}");

        let state = AuthState {
            issuer: issuer.clone(),
            seen: seen_requests.clone(),
        };
        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                axum::routing::get(resource_metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(resource_metadata),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(authorization_server_metadata),
            )
            .route("/register", axum::routing::post(register))
            .route("/token", axum::routing::post(token))
            .with_state(state);

        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        (issuer, seen_requests)
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn oauth_flow_discovers_endpoints_and_stores_tokens() {
        let auth_file = temp_auth_file("flow");
        let _guard = auth_store_lock();
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &auth_file);

        let (issuer, seen) = spawn_mock_authorization_server().await;
        // The MCP server publishes the protected-resource metadata that names
        // the authorization server, which is where discovery starts.
        let (mcp_url, _) =
            spawn_mock_mcp_with_auth(axum::http::StatusCode::OK, Some(issuer.clone())).await;

        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: mcp_url,
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let oauth_client = manager
            .oauth_client_for("remote")
            .await
            .expect("oauth client with discovery");
        assert!(
            seen.lock()
                .await
                .iter()
                .any(|request| request == "authorization-server-metadata"),
            "discovery must fetch authorization-server metadata"
        );

        // No credentials yet, so the server must not look authorized.
        assert!(!oauth_client.has_stored_tokens().await);

        let state = manager.start_oauth("remote").await.expect("start oauth");
        assert!(
            state.authorization_url.contains("/authorize"),
            "authorization URL must come from discovery: {}",
            state.authorization_url
        );

        let stored_state = opencode_mcp::auth::get_oauth_state("remote")
            .await
            .expect("persisted csrf state");

        let status = manager
            .handle_callback("remote", "oauth-code", Some(&stored_state))
            .await
            .expect("callback exchanges the code");
        assert_eq!(status.oauth_status, Some(McpOAuthStatus::Authorized));
        assert!(
            oauth_client.has_stored_tokens().await,
            "tokens must be stored after the exchange"
        );
        assert!(seen.lock().await.iter().any(|request| request == "token"));

        std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
        let _ = std::fs::remove_file(&auth_file);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn callback_without_stored_tokens_fails() {
        let auth_file = temp_auth_file("negative");
        let _guard = auth_store_lock();
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &auth_file);

        let (issuer, _) = spawn_mock_authorization_server().await;
        let (mcp_url, _) =
            spawn_mock_mcp_with_auth(axum::http::StatusCode::OK, Some(issuer.clone())).await;

        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: mcp_url,
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        manager.start_oauth("remote").await.expect("start oauth");
        let stored_state = opencode_mcp::auth::get_oauth_state("remote")
            .await
            .expect("csrf state");

        // A wrong code is rejected by the authorization server, so the server
        // must not be reported as authorized.
        let result = manager
            .handle_callback("remote", "wrong-code", Some(&stored_state))
            .await;
        assert!(result.is_err(), "invalid code must not authorize");
        let status = manager.get_server("remote").await.expect("remote server");
        assert_ne!(status.oauth_status, Some(McpOAuthStatus::Authorized));

        std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
        let _ = std::fs::remove_file(&auth_file);
    }

    /// Local and non-OAuth remote servers must never be treated as OAuth-capable.
    #[test]
    fn oauth_required_only_for_enabled_remote_servers() {
        let local = McpRuntimeConfig::Local(LocalMcpConfig {
            command: "noop".to_string(),
            args: Vec::new(),
            env: None,
            cwd: None,
            timeouts: McpTimeouts {
                startup_ms: 1,
                catalog_ms: 1,
                execution_ms: 1,
            },
            protocol: None,
        });
        assert!(!local.oauth_required());

        let remote_without_oauth = McpRuntimeConfig::Remote(RemoteMcpConfig {
            oauth_enabled: false,
            ..Default::default()
        });
        assert!(!remote_without_oauth.oauth_required());

        let remote_with_oauth = McpRuntimeConfig::Remote(RemoteMcpConfig {
            oauth_enabled: true,
            ..Default::default()
        });
        assert!(remote_with_oauth.oauth_required());
    }

    #[test]
    fn oauth_status_uses_lowercase_wire_values() {
        assert_eq!(
            serde_json::to_string(&McpOAuthStatus::Pending).expect("serialize pending"),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&McpOAuthStatus::Authorized).expect("serialize authorized"),
            "\"authorized\""
        );
        assert_eq!(
            serde_json::to_string(&McpOAuthStatus::Failed).expect("serialize failed"),
            "\"failed\""
        );
        let decoded: McpOAuthStatus = serde_json::from_str("\"pending\"").expect("deserialize");
        assert_eq!(decoded, McpOAuthStatus::Pending);
    }

    #[test]
    fn oauth_state_serde_round_trip() {
        let state = McpOAuthState {
            server_name: "remote".to_string(),
            authorization_url: "https://idp.example.com/authorize".to_string(),
            client_id: None,
            status: McpOAuthStatus::Pending,
        };
        let encoded = serde_json::to_string(&state).expect("serialize state");
        let decoded: McpOAuthState = serde_json::from_str(&encoded).expect("deserialize state");
        assert_eq!(decoded.server_name, state.server_name);
        assert_eq!(decoded.authorization_url, state.authorization_url);
        assert_eq!(decoded.status, McpOAuthStatus::Pending);
    }

    #[tokio::test]
    async fn start_oauth_rejects_unknown_server() {
        let manager = McpOAuthManager::new();
        let error = manager
            .start_oauth("missing")
            .await
            .expect_err("unknown server must fail");
        assert!(matches!(error, McpOAuthError::ServerNotFound(_)));
    }

    #[tokio::test]
    async fn start_oauth_rejects_local_server() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "local".to_string(),
                McpRuntimeConfig::Local(LocalMcpConfig {
                    command: "noop".to_string(),
                    args: Vec::new(),
                    env: None,
                    cwd: None,
                    timeouts: McpTimeouts {
                        startup_ms: 1,
                        catalog_ms: 1,
                        execution_ms: 1,
                    },
                    protocol: None,
                }),
                true,
            )
            .await;

        let error = manager
            .start_oauth("local")
            .await
            .expect_err("local servers have no oauth flow");
        assert!(matches!(error, McpOAuthError::OAuthNotSupported(_)));
    }

    #[tokio::test]
    async fn start_oauth_rejects_remote_without_oauth_enabled() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: false,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let error = manager
            .start_oauth("remote")
            .await
            .expect_err("oauth must be explicitly enabled");
        assert!(matches!(error, McpOAuthError::OAuthNotSupported(_)));
    }

    #[tokio::test]
    async fn handle_callback_rejects_unknown_server() {
        let manager = McpOAuthManager::new();
        let error = manager
            .handle_callback("missing", "dummy-code", Some("dummy-state"))
            .await
            .expect_err("unknown server must fail");
        assert!(matches!(error, McpOAuthError::ServerNotFound(_)));
    }

    #[tokio::test]
    async fn handle_callback_requires_pending_flow() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let error = manager
            .handle_callback("remote", "dummy-code", Some("dummy-state"))
            .await
            .expect_err("callback without a started flow must fail");
        assert!(
            matches!(&error, McpOAuthError::OAuthFailed(message) if message == "No pending OAuth flow"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn handle_callback_requires_returned_state() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;
        // Seed a pending flow so the missing state is the only failure.
        {
            let mut states = manager.oauth_states.write().await;
            states.insert(
                "remote".to_string(),
                McpOAuthState {
                    server_name: "remote".to_string(),
                    authorization_url: "https://idp.example.com/authorize".to_string(),
                    client_id: None,
                    status: McpOAuthStatus::Pending,
                },
            );
        }

        let error = manager
            .handle_callback("remote", "dummy-code", None)
            .await
            .expect_err("a callback without state must be rejected");
        assert!(
            matches!(&error, McpOAuthError::OAuthFailed(message) if message.contains("state")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn authenticate_rejects_non_oauth_server() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: false,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let error = manager
            .authenticate("remote")
            .await
            .expect_err("non-oauth server cannot authenticate");
        assert!(matches!(error, McpOAuthError::OAuthNotSupported(_)));
    }

    #[tokio::test]
    async fn disabled_server_reports_disabled_status() {
        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: true,
                    ..Default::default()
                }),
                false,
            )
            .await;

        let status = manager
            .connect("remote")
            .await
            .expect("connect returns runtime status");
        assert_eq!(status.status, "disabled");
        assert!(status.oauth_required);
        assert_eq!(status.oauth_status, None);
    }

    #[tokio::test]
    async fn server_registration_records_logs_and_listing() {
        let manager = McpOAuthManager::new();
        assert!(!manager.has_server("remote").await);

        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: "https://example.com/mcp".to_string(),
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        assert!(manager.has_server("remote").await);
        let listed = manager.list_servers().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "remote");
        assert_eq!(listed[0].status, "needs_auth");
        assert!(listed[0].oauth_required);

        let logs = manager
            .get_logs("remote")
            .await
            .expect("registered server has logs");
        assert!(logs
            .iter()
            .any(|entry| entry.message == "Server registered"));

        let error = manager
            .get_logs("missing")
            .await
            .expect_err("unknown server has no logs");
        assert!(matches!(error, McpOAuthError::ServerNotFound(_)));
    }

    #[tokio::test]
    async fn disconnect_rejects_unknown_server() {
        let manager = McpOAuthManager::new();
        let error = manager
            .disconnect("missing")
            .await
            .expect_err("unknown server cannot disconnect");
        assert!(matches!(error, McpOAuthError::ServerNotFound(_)));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn start_oauth_rejects_second_concurrent_flow() {
        let auth_file = temp_auth_file("in-progress");
        let _guard = auth_store_lock();
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &auth_file);

        let (issuer, _) = spawn_mock_authorization_server().await;
        let (mcp_url, _) =
            spawn_mock_mcp_with_auth(axum::http::StatusCode::OK, Some(issuer.clone())).await;

        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: mcp_url,
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        manager
            .start_oauth("remote")
            .await
            .expect("first flow starts");
        let error = manager
            .start_oauth("remote")
            .await
            .expect_err("second concurrent flow must be rejected");
        assert!(matches!(error, McpOAuthError::OAuthInProgress));

        std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
        let _ = std::fs::remove_file(&auth_file);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn each_start_oauth_generates_a_fresh_state() {
        let auth_file = temp_auth_file("fresh-state");
        let _guard = auth_store_lock();
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &auth_file);

        let (issuer, _) = spawn_mock_authorization_server().await;
        let (mcp_url, _) =
            spawn_mock_mcp_with_auth(axum::http::StatusCode::OK, Some(issuer.clone())).await;

        let manager = McpOAuthManager::new();
        manager
            .add_server(
                "remote".to_string(),
                McpRuntimeConfig::Remote(RemoteMcpConfig {
                    url: mcp_url,
                    oauth_enabled: true,
                    ..Default::default()
                }),
                true,
            )
            .await;

        let first = manager.start_oauth("remote").await.expect("first flow");
        let first_state = opencode_mcp::auth::get_oauth_state("remote")
            .await
            .expect("first csrf state");
        assert!(
            first.authorization_url.contains(&first_state),
            "authorization URL must carry the generated state"
        );

        manager.remove_oauth("remote").await;
        let second = manager.start_oauth("remote").await.expect("second flow");
        let second_state = opencode_mcp::auth::get_oauth_state("remote")
            .await
            .expect("second csrf state");
        assert_ne!(
            first_state, second_state,
            "a new flow must use a fresh csrf state"
        );
        assert!(second.authorization_url.contains(&second_state));

        std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
        let _ = std::fs::remove_file(&auth_file);
    }
}
