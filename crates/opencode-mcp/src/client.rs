use chrono::Utc;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use crate::oauth::McpOAuthManager;
use crate::protocol::*;
use crate::tool::McpToolRegistry;
use crate::transport::{HttpTransport, McpTransport, SseTransport, StdioTransport};

/// Per-server log-line cap (RES-03), matching `opencode-server`'s
/// `MAX_LOG_ENTRIES` (200). Older lines are dropped so reconnect storms
/// cannot grow the in-memory log without limit.
const MAX_LOG_ENTRIES: usize = 200;

// ---------------------------------------------------------------------------
// McpStatus – mirrors the TS discriminated union `MCP.Status`
// ---------------------------------------------------------------------------

/// Connection status of an MCP server.
///
/// Uses Rust's enum-with-data to model the same state machine as the TS
/// `Status` discriminated union, but with compile-time exhaustiveness checks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    Disabled,
    Failed { error: String },
    NeedsAuth,
    NeedsClientRegistration { error: String },
}

impl McpStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, McpStatus::Connected)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, McpStatus::Failed { .. })
    }

    pub fn is_needs_auth(&self) -> bool {
        matches!(self, McpStatus::NeedsAuth)
    }
}

impl std::fmt::Display for McpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpStatus::Connected => write!(f, "connected"),
            McpStatus::Disabled => write!(f, "disabled"),
            McpStatus::Failed { error } => write!(f, "failed: {error}"),
            McpStatus::NeedsAuth => write!(f, "needs_auth"),
            McpStatus::NeedsClientRegistration { error } => {
                write!(f, "needs_client_registration: {error}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("Transport error: {0}")]
    TransportError(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("Not initialized")]
    NotInitialized,

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Timeout")]
    Timeout,

    #[error("Unauthorized")]
    Unauthorized,

    #[error("OAuth error: {0}")]
    OAuthError(String),
}

// ---------------------------------------------------------------------------
// McpServerConfig
// ---------------------------------------------------------------------------

/// Per-operation request timeouts for an MCP server, in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct McpTimeouts {
    pub startup_ms: u64,
    pub catalog_ms: u64,
    pub execution_ms: u64,
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self {
            startup_ms: DEFAULT_STARTUP_MS,
            catalog_ms: DEFAULT_CATALOG_MS,
            execution_ms: DEFAULT_EXECUTION_MS,
        }
    }
}

/// Documented MCP defaults: startup 30s, catalog 30s, execution 12h.
pub const DEFAULT_STARTUP_MS: u64 = 30_000;
pub const DEFAULT_CATALOG_MS: u64 = 30_000;
pub const DEFAULT_EXECUTION_MS: u64 = 43_200_000;

/// Protocol revision sent by default and used when a caller selects the
/// unimplemented `auto` negotiation mode.
const LEGACY_PROTOCOL_REVISION: &str = "2025-11-25";

/// Absolute upper bound for one progress-aware request, as a multiple of the
/// configured timeout. Progress notifications may push the effective deadline
/// out, but never beyond `call_start + timeout * this multiplier`.
const PROGRESS_HARD_DEADLINE_MULTIPLIER: u32 = 4;

/// Maximum number of progress notifications that may extend the effective
/// deadline of a single request. Bounds extension work independently of the
/// wall-clock hard deadline.
const MAX_PROGRESS_DEADLINE_EXTENSIONS: u32 = 64;

impl McpTimeouts {
    pub fn uniform(millis: u64) -> Self {
        Self {
            startup_ms: millis,
            catalog_ms: millis,
            execution_ms: millis,
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<Vec<(String, String)>>,
    pub cwd: Option<String>,
    pub timeouts: McpTimeouts,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone)]
enum RegistryConnectionConfig {
    Stdio(McpServerConfig),
    Http {
        url: String,
        headers: Option<HashMap<String, String>>,
        timeouts: McpTimeouts,
    },
    Sse {
        url: String,
        headers: Option<HashMap<String, String>>,
        timeouts: McpTimeouts,
    },
}

// ---------------------------------------------------------------------------
// McpClient
// ---------------------------------------------------------------------------

pub struct McpClient {
    server_name: String,
    // Stored as Arc so a request can clone the transport and drop the Mutex
    // guard before awaiting receive(); holding the guard across receive()
    // would block every other request, close() and reconnect on this client.
    transport: Mutex<Option<Arc<dyn McpTransport>>>,
    /// Serializes a whole request exchange (send + response receive). The
    /// transport has no id-based routing, so two callers waiting on the same
    /// `transport.receive()` would consume each other's responses (`Some(_) =>
    /// continue`) and time out. Holding this lock across send and the receive
    /// loop guarantees one in-flight request at a time, while the separate
    /// `transport` Mutex stays free for `close()` / reconnect during a pending
    /// receive (R2 regression).
    request_lock: Mutex<()>,
    request_id: AtomicU64,
    initialized: RwLock<bool>,
    capabilities: RwLock<Option<ServerCapabilities>>,
    tool_registry: Arc<McpToolRegistry>,
    protocol_version: String,
    startup_timeout_ms: u64,
    catalog_timeout_ms: u64,
    execution_timeout_ms: u64,
    status: RwLock<McpStatus>,
    oauth_manager: RwLock<Option<Arc<McpOAuthManager>>>,
    /// Set to true when a `notifications/tools/list_changed` is received.
    tools_changed: std::sync::atomic::AtomicBool,
    /// Signaled by `close()` before it tears down the transport. Every in-flight
    /// `receive()` wait selects on this notification so shutdown can wake it
    /// instead of deadlocking on transport-internal locks (SIG-01).
    shutdown_notify: Arc<tokio::sync::Notify>,
    /// Sticky shutdown flag. `close()` stores this flag first and wakes the
    /// in-flight waiter with `notify_one()`, which stores a permit for a
    /// `notified()` created later. Every request checkpoint also reads the
    /// flag, so a waiter created after the wake-up still aborts.
    /// the in-flight call (R4). Idempotent across repeated `close()` calls.
    shutdown: Arc<AtomicBool>,
}

impl McpClient {
    pub fn new(server_name: String, tool_registry: Arc<McpToolRegistry>) -> Self {
        Self {
            server_name,
            transport: Mutex::new(None),
            request_lock: Mutex::new(()),
            request_id: AtomicU64::new(0),
            initialized: RwLock::new(false),
            capabilities: RwLock::new(None),
            tool_registry,
            protocol_version: LEGACY_PROTOCOL_REVISION.to_string(),
            startup_timeout_ms: DEFAULT_STARTUP_MS,
            catalog_timeout_ms: DEFAULT_CATALOG_MS,
            execution_timeout_ms: DEFAULT_EXECUTION_MS,
            status: RwLock::new(McpStatus::Disabled),
            oauth_manager: RwLock::new(None),
            tools_changed: std::sync::atomic::AtomicBool::new(false),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Applies one timeout to every operation (legacy convenience).
    pub fn with_timeout(self, timeout_ms: u64) -> Self {
        self.with_timeouts(McpTimeouts::uniform(timeout_ms))
    }

    pub fn with_timeouts(mut self, timeouts: McpTimeouts) -> Self {
        self.startup_timeout_ms = timeouts.startup_ms;
        self.catalog_timeout_ms = timeouts.catalog_ms;
        self.execution_timeout_ms = timeouts.execution_ms;
        self
    }

    /// Sets the MCP protocol revision to negotiate during `initialize`.
    ///
    /// `auto` and `legacy` are advertised aliases, but the negotiation/selection
    /// logic was never implemented. Raw-string callers (e.g. the HTTP config
    /// API) can still send either one, so normalize both to the legacy revision
    /// with a value-free warning before any `initialize` request is built.
    pub fn with_protocol_version(mut self, version: impl Into<String>) -> Self {
        let version = version.into();
        self.protocol_version = if version == "auto" || version == "legacy" {
            tracing::warn!(
                "mcp.protocol auto/legacy mode is unimplemented; normalizing to the legacy protocol (value not logged)"
            );
            LEGACY_PROTOCOL_REVISION.to_string()
        } else {
            version
        };
        self
    }

    /// Builder form of [`McpClient::set_oauth_manager`].
    pub async fn with_oauth_manager(mut self, manager: Arc<McpOAuthManager>) -> Self {
        self.oauth_manager = RwLock::new(Some(manager));
        self
    }

    /// Attach an OAuth manager for token-based auth on HTTP/SSE transports.
    pub async fn set_oauth_manager(&self, manager: Arc<McpOAuthManager>) {
        let mut guard = self.oauth_manager.write().await;
        *guard = Some(manager);
    }

    // -- Status accessors ----------------------------------------------------

    pub async fn status(&self) -> McpStatus {
        self.status.read().await.clone()
    }

    pub async fn set_status(&self, status: McpStatus) {
        let mut guard = self.status.write().await;
        *guard = status;
    }

    // -- Factory helpers -----------------------------------------------------

    /// Create a client that communicates over stdio with a child process.
    pub async fn stdio(
        server_name: String,
        tool_registry: Arc<McpToolRegistry>,
        config: McpServerConfig,
    ) -> Result<Self, McpClientError> {
        let client = Self::new(server_name, tool_registry);
        client.connect_stdio(config).await?;
        Ok(client)
    }
    /// Create a client that communicates over StreamableHTTP.
    pub async fn http(
        server_name: String,
        tool_registry: Arc<McpToolRegistry>,
        url: String,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, McpClientError> {
        let client = Self::new(server_name, tool_registry);
        client.connect_http(url, headers).await?;
        Ok(client)
    }

    /// Create a client that communicates over SSE.
    pub async fn sse(
        server_name: String,
        tool_registry: Arc<McpToolRegistry>,
        url: String,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, McpClientError> {
        let client = Self::new(server_name, tool_registry);
        client.connect_sse(url, headers).await?;
        Ok(client)
    }

    // -- Connection methods ---------------------------------------------------

    pub async fn connect_stdio(&self, config: McpServerConfig) -> Result<(), McpClientError> {
        let result = self.connect_stdio_inner(config).await;
        match &result {
            Ok(()) => self.set_status(McpStatus::Connected).await,
            Err(e) => {
                self.set_status(McpStatus::Failed {
                    error: e.to_string(),
                })
                .await;
            }
        }
        result
    }

    async fn connect_stdio_inner(&self, config: McpServerConfig) -> Result<(), McpClientError> {
        let transport =
            StdioTransport::new(&config.command, &config.args, config.env, config.cwd).await?;
        {
            let mut t = self.transport.lock().await;
            *t = Some(Arc::new(transport));
        }
        self.initialize().await?;
        self.load_tools().await?;
        Ok(())
    }
    pub async fn connect_http(
        &self,
        url: String,
        headers: Option<HashMap<String, String>>,
    ) -> Result<(), McpClientError> {
        let result = self.connect_http_inner(url, headers).await;
        match &result {
            Ok(()) => self.set_status(McpStatus::Connected).await,
            Err(McpClientError::Unauthorized) => {
                self.set_status(McpStatus::NeedsAuth).await;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("registration") || msg.contains("client_id") {
                    self.set_status(McpStatus::NeedsClientRegistration { error: msg })
                        .await;
                } else {
                    self.set_status(McpStatus::Failed { error: msg }).await;
                }
            }
        }
        result
    }

    async fn connect_http_inner(
        &self,
        url: String,
        headers: Option<HashMap<String, String>>,
    ) -> Result<(), McpClientError> {
        let base_headers = headers.unwrap_or_default();

        // First attempt: inject the current token (refreshing it if expired).
        // Clone first so the read guard is dropped before `get_token().await`
        // (which may perform a refresh HTTP round-trip) and cannot block a
        // concurrent `set_oauth_manager` write.
        let mgr = self.oauth_manager.read().await.clone();
        let mut merged_headers = base_headers.clone();
        if let Some(mgr) = mgr {
            match mgr.get_token().await {
                Ok(Some(token)) => {
                    merged_headers.insert("Authorization".to_string(), format!("Bearer {token}"));
                }
                Ok(None) => {
                    // No token available – caller should initiate auth.
                    return Err(McpClientError::Unauthorized);
                }
                Err(e) => {
                    return Err(McpClientError::OAuthError(e.to_string()));
                }
            }
        }

        let transport = HttpTransport::new(url.clone(), Some(merged_headers));
        {
            let mut t = self.transport.lock().await;
            *t = Some(Arc::new(transport));
        }
        match self.initialize().await {
            Ok(()) => {
                self.load_tools().await?;
                Ok(())
            }
            Err(McpClientError::Unauthorized) => {
                // The server rejected the token. Try refreshing it once if an
                // OAuth manager is available, then retry the whole handshake.
                let mgr = self.oauth_manager.read().await.clone();
                if let Some(mgr) = mgr {
                    if mgr.refresh_token().await.is_ok() {
                        if let Ok(Some(token)) = mgr.get_token().await {
                            let mut retry_headers = base_headers.clone();
                            retry_headers
                                .insert("Authorization".to_string(), format!("Bearer {token}"));
                            let transport = HttpTransport::new(url.clone(), Some(retry_headers));
                            {
                                let mut t = self.transport.lock().await;
                                *t = Some(Arc::new(transport));
                            }
                            self.initialize().await?;
                            self.load_tools().await?;
                            return Ok(());
                        }
                    }
                }
                Err(McpClientError::Unauthorized)
            }
            Err(error) => Err(error),
        }
    }
    pub async fn connect_sse(
        &self,
        url: String,
        headers: Option<HashMap<String, String>>,
    ) -> Result<(), McpClientError> {
        let result = self.connect_sse_inner(url, headers).await;
        match &result {
            Ok(()) => self.set_status(McpStatus::Connected).await,
            Err(McpClientError::Unauthorized) => {
                self.set_status(McpStatus::NeedsAuth).await;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("registration") || msg.contains("client_id") {
                    self.set_status(McpStatus::NeedsClientRegistration { error: msg })
                        .await;
                } else {
                    self.set_status(McpStatus::Failed { error: msg }).await;
                }
            }
        }
        result
    }

    async fn connect_sse_inner(
        &self,
        url: String,
        headers: Option<HashMap<String, String>>,
    ) -> Result<(), McpClientError> {
        let base_headers = headers.unwrap_or_default();

        let mgr = self.oauth_manager.read().await.clone();
        let mut merged_headers = base_headers.clone();
        if let Some(mgr) = mgr {
            match mgr.get_token().await {
                Ok(Some(token)) => {
                    merged_headers.insert("Authorization".to_string(), format!("Bearer {token}"));
                }
                Ok(None) => return Err(McpClientError::Unauthorized),
                Err(e) => return Err(McpClientError::OAuthError(e.to_string())),
            }
        }

        let transport = SseTransport::new(url.clone(), Some(merged_headers));
        transport.connect().await?;
        {
            let mut t = self.transport.lock().await;
            *t = Some(Arc::new(transport));
        }
        match self.initialize().await {
            Ok(()) => {
                self.load_tools().await?;
                Ok(())
            }
            Err(McpClientError::Unauthorized) => {
                let mgr = self.oauth_manager.read().await.clone();
                if let Some(mgr) = mgr {
                    if mgr.refresh_token().await.is_ok() {
                        if let Ok(Some(token)) = mgr.get_token().await {
                            let mut retry_headers = base_headers.clone();
                            retry_headers
                                .insert("Authorization".to_string(), format!("Bearer {token}"));
                            let transport = SseTransport::new(url.clone(), Some(retry_headers));
                            transport.connect().await?;
                            {
                                let mut t = self.transport.lock().await;
                                *t = Some(Arc::new(transport));
                            }
                            self.initialize().await?;
                            self.load_tools().await?;
                            return Ok(());
                        }
                    }
                }
                Err(McpClientError::Unauthorized)
            }
            Err(error) => Err(error),
        }
    }

    // -- Internal helpers ----------------------------------------------------

    async fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether `close()` has been called. The flag is sticky and SeqCst, so it
    /// also covers wake-ups that `Notify::notify_waiters()` could not deliver
    /// because no `notified()` future was registered yet.
    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Canonical error for a request aborted by shutdown. `close()` stores the
    /// flag before notifying, so a wake-up from `shutdown_notify` must observe
    /// `is_shutdown() == true`; the assertion documents that contract. No
    /// token value, request id, or path is included in the message.
    fn shutdown_error(&self) -> McpClientError {
        debug_assert!(
            self.is_shutdown(),
            "shutdown notification received without the shutdown flag"
        );
        McpClientError::TransportError("Connection closed".to_string())
    }

    /// Send a JSON-RPC notification (no `id`, no response expected).
    async fn send_notification(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), McpClientError> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        };
        let transport = {
            let guard = self.transport.lock().await;
            guard
                .as_ref()
                .cloned()
                .ok_or(McpClientError::NotInitialized)?
        };
        transport.send_notification(&notification).await
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, McpClientError> {
        // Keep send + receive atomic per request so concurrent callers cannot
        // steal responses carrying another request's id.
        let _request_guard = self.request_lock.lock().await;

        // close() may have run before this caller registered any notified()
        // future. The sticky flag closes that gap without trusting the wake-up.
        if self.is_shutdown() {
            return Err(McpClientError::TransportError(
                "Connection closed".to_string(),
            ));
        }

        let id = self.next_id().await;
        let request = JsonRpcRequest::new(id, method);
        let request = if let Some(p) = params {
            request.with_params(p)
        } else {
            request
        };

        // Clone the transport out so the Mutex guard is released before any
        // await. Keeping it across `receive()` would block other requests and
        // reconnects for the entire lifetime of this response wait.
        let transport = {
            let guard = self.transport.lock().await;
            guard
                .as_ref()
                .cloned()
                .ok_or(McpClientError::NotInitialized)?
        };
        // Own the notifier locally so every select! below never borrows `self`
        // and never holds a Mutex guard across an await. `close()` signals it
        // before tearing down the transport, waking a waiter that would
        // otherwise hang behind transport-internal locks.
        let shutdown_notify = self.shutdown_notify.clone();

        // Race send() itself against shutdown: a transport that hangs inside
        // send() (e.g. an unanswered HTTP POST) must be released by close()
        // instead of waiting until some outer timeout. The flag checks before
        // and after this select! cover the window where close() fires before
        // notified() is registered.
        if self.is_shutdown() {
            return Err(McpClientError::TransportError(
                "Connection closed".to_string(),
            ));
        }
        tokio::select! {
            result = transport.send(&request) => result?,
            _ = shutdown_notify.notified() => {
                return Err(self.shutdown_error());
            }
        }
        if self.is_shutdown() {
            return Err(McpClientError::TransportError(
                "Connection closed".to_string(),
            ));
        }

        let response = loop {
            // Re-check on every iteration: close() may have fired during
            // handle_notification() or immediately after a `continue`, i.e.
            // while no notified() future was registered to receive it.
            if self.is_shutdown() {
                return Err(McpClientError::TransportError(
                    "Connection closed".to_string(),
                ));
            }
            let message = tokio::select! {
                result = transport.receive() => result?,
                _ = shutdown_notify.notified() => {
                    return Err(self.shutdown_error());
                }
            };
            match message {
                Some(JsonRpcMessage::Response(resp)) if resp.id == id => break resp,
                Some(JsonRpcMessage::Notification(notif)) => {
                    self.handle_notification(notif).await;
                    // handle_notification() awaits internal locks; close() can
                    // fire there with no notified() future registered.
                    if self.is_shutdown() {
                        return Err(McpClientError::TransportError(
                            "Connection closed".to_string(),
                        ));
                    }
                    continue;
                }
                Some(_) => continue,
                None => {
                    return Err(McpClientError::TransportError(
                        "Connection closed".to_string(),
                    ));
                }
            }
        };

        if let Some(error) = response.error {
            return Err(McpClientError::ServerError(error.message));
        }

        Ok(response)
    }

    async fn send_request_with_progress_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout_ms: u64,
    ) -> Result<JsonRpcResponse, McpClientError> {
        // Serialize send + receive so two concurrent callers never wait on the
        // same `transport.receive()` and consume each other's id-tagged reply.
        let _request_guard = self.request_lock.lock().await;

        // Sticky flag closes the gap where close() ran before any notified()
        // future was registered for this request.
        if self.is_shutdown() {
            return Err(McpClientError::TransportError(
                "Connection closed".to_string(),
            ));
        }

        let id = self.next_id().await;
        let request = JsonRpcRequest::new(id, method);
        let request = if let Some(p) = params {
            request.with_params(p)
        } else {
            request
        };

        // Clone the transport out so the Mutex guard is released before any
        // await. Holding it across `receive()` would block other requests,
        // close() and reconnects for the whole response wait.
        let transport = {
            let guard = self.transport.lock().await;
            guard
                .as_ref()
                .cloned()
                .ok_or(McpClientError::NotInitialized)?
        };

        // Own the notifier locally so the select! in the receive loop never
        // borrows `self` and never holds a Mutex guard across the wait.
        let shutdown_notify = self.shutdown_notify.clone();

        // The deadline is fixed BEFORE send: reqwest has no default request
        // timeout, so an HTTP/SSE POST that accepts the connection but never
        // answers must not hang forever. `call_start` also bounds the receive
        // wait below, keeping send + receive under one absolute budget.
        let timeout_duration = Duration::from_millis(timeout_ms);
        let call_start = tokio::time::Instant::now();
        // Absolute bound fixed at call start. Progress notifications may extend
        // the effective deadline, but never beyond this instant.
        let hard_deadline =
            call_start + timeout_duration.saturating_mul(PROGRESS_HARD_DEADLINE_MULTIPLIER);
        let mut deadline = call_start + timeout_duration;
        let mut extensions_used = 0u32;

        // Cover send() itself with the same absolute bound. Progress cannot
        // extend the send phase, but using hard_deadline keeps a single,
        // explicit upper bound for the whole request exchange. The shutdown
        // branch is essential: without it a hung POST could wait for the full
        // hard deadline (~48h at the documented defaults) after close().
        if self.is_shutdown() {
            return Err(McpClientError::TransportError(
                "Connection closed".to_string(),
            ));
        }
        tokio::select! {
            timed = tokio::time::timeout_at(hard_deadline, transport.send(&request)) => {
                timed.map_err(|_| McpClientError::Timeout)??;
            }
            _ = shutdown_notify.notified() => {
                return Err(self.shutdown_error());
            }
        }
        if self.is_shutdown() {
            return Err(McpClientError::TransportError(
                "Connection closed".to_string(),
            ));
        }

        let response = loop {
            // `timeout_at` alone can be starved: if the transport keeps yielding
            // messages without ever going pending, its inner future wins every
            // poll and the timeout is never observed. Check the absolute bound
            // explicitly on every iteration so a progress flood always ends.
            // shutdown を deadline より先に確認する（biased で timeout 分岐が
            // 先に評価されても、close() 由来なら Connection closed を返す、R4）。
            if self.is_shutdown() {
                return Err(self.shutdown_error());
            }
            if tokio::time::Instant::now() >= hard_deadline {
                return Err(McpClientError::Timeout);
            }
            let effective_deadline = deadline.min(hard_deadline);
            // `biased` restores the old `timeout_at` priority: the timeout
            // branch is evaluated first, so an elapsed effective deadline
            // always yields Timeout even if receive() is also ready. Default
            // random selection could otherwise let a late response win.
            let message = tokio::select! {
                biased;
                timed = tokio::time::timeout_at(effective_deadline, transport.receive()) => {
                    match timed {
                        Ok(result) => result?,
                        Err(_) => {
                            if self.is_shutdown() {
                                return Err(self.shutdown_error());
                            }
                            return Err(McpClientError::Timeout);
                        }
                    }
                }
                _ = shutdown_notify.notified() => {
                    return Err(self.shutdown_error());
                }
            };

            match message {
                Some(JsonRpcMessage::Response(resp)) if resp.id == id => break resp,
                Some(JsonRpcMessage::Notification(notif)) => {
                    if Self::is_progress_notification(&notif) {
                        if let Some(next) = Self::next_progress_deadline(
                            tokio::time::Instant::now(),
                            timeout_duration,
                            hard_deadline,
                            extensions_used,
                        ) {
                            extensions_used += 1;
                            deadline = next;
                        }
                    }
                    self.handle_notification(notif).await;
                    // handle_notification() awaits locks; re-check immediately
                    // so a close() during it is not lost to the next select!.
                    if self.is_shutdown() {
                        return Err(McpClientError::TransportError(
                            "Connection closed".to_string(),
                        ));
                    }
                    continue;
                }
                Some(_) => continue,
                None => {
                    return Err(McpClientError::TransportError(
                        "Connection closed".to_string(),
                    ));
                }
            }
        };

        if let Some(error) = response.error {
            return Err(McpClientError::ServerError(error.message));
        }

        Ok(response)
    }

    async fn initialize(&self) -> Result<(), McpClientError> {
        let params = InitializeParams {
            protocol_version: self.protocol_version.clone(),
            ..InitializeParams::default()
        };
        let params = serde_json::to_value(params)
            .map_err(|e| McpClientError::ProtocolError(e.to_string()))?;

        let response = tokio::time::timeout(
            Duration::from_millis(self.startup_timeout_ms),
            self.send_request("initialize", Some(params)),
        )
        .await
        .map_err(|_| McpClientError::Timeout)??;

        let result: InitializeResult = response
            .result
            .ok_or_else(|| McpClientError::ProtocolError("No result in initialize response".into()))
            .and_then(|r| {
                serde_json::from_value(r).map_err(|e| {
                    McpClientError::ProtocolError(format!("Failed to parse initialize result: {e}"))
                })
            })?;

        {
            let mut caps = self.capabilities.write().await;
            *caps = Some(result.capabilities);
        }

        // `notifications/initialized` is a JSON-RPC notification: it has no id
        // and compliant servers never reply, so we do NOT wait for a response.
        // The send itself is still bounded, because reqwest has no default
        // request timeout and an unresponsive server could otherwise block the
        // HTTP/SSE POST forever. Failure and timeout remain best-effort.
        let _ = tokio::time::timeout(
            Duration::from_millis(self.startup_timeout_ms),
            self.send_notification("notifications/initialized", None),
        )
        .await;

        {
            let mut init = self.initialized.write().await;
            *init = true;
        }

        Ok(())
    }

    /// Handle a server notification received during request/response.
    async fn handle_notification(&self, notif: JsonRpcNotification) {
        match notif.method.as_str() {
            "notifications/tools/list_changed" => {
                tracing::info!(
                    server = %self.server_name,
                    "MCP server tools changed, flagging for reload"
                );
                self.tools_changed.store(true, Ordering::SeqCst);
            }
            "notifications/resources/list_changed" => {
                tracing::debug!(
                    server = %self.server_name,
                    "MCP server resources changed (not yet handled)"
                );
            }
            "notifications/prompts/list_changed" => {
                tracing::debug!(
                    server = %self.server_name,
                    "MCP server prompts changed (not yet handled)"
                );
            }
            other => {
                tracing::debug!(
                    server = %self.server_name,
                    method = other,
                    "Unhandled MCP notification"
                );
            }
        }
    }

    fn is_progress_notification(notif: &JsonRpcNotification) -> bool {
        notif.method == "notifications/progress" || notif.method == "$/progress"
    }

    /// Compute the effective deadline after a progress notification.
    ///
    /// Returns `None` once the extension budget is exhausted. Any accepted
    /// extension is clamped to `hard_deadline`, so progress can never push the
    /// effective deadline past the absolute bound fixed at call start.
    fn next_progress_deadline(
        now: tokio::time::Instant,
        timeout: Duration,
        hard_deadline: tokio::time::Instant,
        extensions_used: u32,
    ) -> Option<tokio::time::Instant> {
        if extensions_used >= MAX_PROGRESS_DEADLINE_EXTENSIONS {
            return None;
        }
        Some((now + timeout).min(hard_deadline))
    }

    /// If the server sent a `tools/list_changed` notification, reload tools.
    /// Call this after operations that might trigger notifications.
    pub async fn refresh_tools_if_needed(&self) -> Result<(), McpClientError> {
        if self.tools_changed.swap(false, Ordering::SeqCst) {
            if let Err(err) = self.load_tools().await {
                // Restore the pending flag: `call_tool` discards this Result via
                // `.ok()`, so without the restore the stale catalog would stay
                // pinned and never be reloaded.
                self.tools_changed.store(true, Ordering::SeqCst);
                return Err(err);
            }
        }
        Ok(())
    }

    async fn load_tools(&self) -> Result<(), McpClientError> {
        let response = tokio::time::timeout(
            Duration::from_millis(self.catalog_timeout_ms),
            self.send_request("tools/list", None),
        )
        .await
        .map_err(|_| McpClientError::Timeout)??;

        let result: ListToolsResult = response
            .result
            .ok_or_else(|| McpClientError::ProtocolError("No result in tools/list response".into()))
            .and_then(|r| {
                serde_json::from_value(r).map_err(|e| {
                    McpClientError::ProtocolError(format!("Failed to parse tools/list result: {e}"))
                })
            })?;

        self.tool_registry.clear_server(&self.server_name).await;
        self.tool_registry
            .register_batch(&self.server_name, result.tools)
            .await;

        Ok(())
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<CallToolResult, McpClientError> {
        let params = CallToolParams {
            name: name.to_string(),
            arguments,
        };

        let params_value = serde_json::to_value(params)
            .map_err(|e| McpClientError::ProtocolError(e.to_string()))?;

        let response = self
            .send_request_with_progress_timeout(
                "tools/call",
                Some(params_value),
                self.execution_timeout_ms,
            )
            .await?;

        // After tool call, check if tools changed notification was received
        self.refresh_tools_if_needed().await.ok();

        let result: CallToolResult = response
            .result
            .ok_or_else(|| McpClientError::ProtocolError("No result in tools/call response".into()))
            .and_then(|r| {
                serde_json::from_value(r).map_err(|e| {
                    McpClientError::ProtocolError(format!("Failed to parse tools/call result: {e}"))
                })
            })?;

        Ok(result)
    }

    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpClientError> {
        let params = ReadResourceParams {
            uri: uri.to_string(),
        };
        let params_value = serde_json::to_value(params)
            .map_err(|e| McpClientError::ProtocolError(e.to_string()))?;

        let response = self
            .send_request_with_progress_timeout(
                "resources/read",
                Some(params_value),
                self.catalog_timeout_ms,
            )
            .await?;
        let result: ReadResourceResult = response
            .result
            .ok_or_else(|| {
                McpClientError::ProtocolError("No result in resources/read response".into())
            })
            .and_then(|r| {
                serde_json::from_value(r).map_err(|e| {
                    McpClientError::ProtocolError(format!(
                        "Failed to parse resources/read result: {e}"
                    ))
                })
            })?;

        Ok(result)
    }

    pub async fn close(&self) -> Result<(), McpClientError> {
        // Set the sticky flag FIRST, then notify_one() (which stores a permit
        // even if no waiter is registered yet). SeqCst + idempotent store makes
        // repeated close() calls safe.
        self.shutdown.store(true, Ordering::SeqCst);

        // Wake every in-flight `receive()` waiter BEFORE taking the transport.
        // StdioTransport::receive() holds the stdout guard while it waits for a
        // reply; StdioTransport::close() needs stdout -> stdin -> process, so a
        // pending receive() would otherwise block close() forever (SIG-01).
        // Woken waiters return TransportError("Connection closed") from the
        // select! in send_request / send_request_with_progress_timeout.
        self.shutdown_notify.notify_one();

        // Take the transport out and drop the Mutex guard before awaiting
        // close(), so in-flight requests waiting on receive() are not blocked
        // by (or blocking) the shutdown path.
        let transport = {
            let mut guard = self.transport.lock().await;
            guard.take()
        };
        if let Some(t) = transport {
            t.close().await?;
        }

        self.tool_registry.clear_server(&self.server_name).await;
        self.set_status(McpStatus::Disabled).await;

        Ok(())
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub async fn is_initialized(&self) -> bool {
        *self.initialized.read().await
    }
}

// ---------------------------------------------------------------------------
// McpClientRegistry
// ---------------------------------------------------------------------------

pub struct McpClientRegistry {
    clients: RwLock<HashMap<String, Arc<McpClient>>>,
    tool_registry: Arc<McpToolRegistry>,
    /// Per-server status, including servers that failed to connect or are
    /// disabled.  Entries here may not have a corresponding client.
    statuses: RwLock<HashMap<String, McpStatus>>,
    connection_configs: RwLock<HashMap<String, RegistryConnectionConfig>>,
    logs: RwLock<HashMap<String, VecDeque<String>>>,
    /// OAuth managers keyed by server name. A client must only ever receive
    /// its own server's manager, otherwise a bearer token could be sent to a
    /// different OAuth-protected server.
    oauth_managers: RwLock<HashMap<String, Arc<McpOAuthManager>>>,
}
impl McpClientRegistry {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            tool_registry: Arc::new(McpToolRegistry::new()),
            statuses: RwLock::new(HashMap::new()),
            connection_configs: RwLock::new(HashMap::new()),
            logs: RwLock::new(HashMap::new()),
            oauth_managers: RwLock::new(HashMap::new()),
        }
    }

    // -- Status helpers ------------------------------------------------------

    /// Record the status for a server (called internally after connect
    /// attempts and also usable externally).
    pub async fn set_status(&self, name: &str, status: McpStatus) {
        self.statuses.write().await.insert(name.to_string(), status);
    }

    /// Get the status for a single server.
    pub async fn get_status(&self, name: &str) -> Option<McpStatus> {
        self.statuses.read().await.get(name).cloned()
    }

    /// Return all servers with their current status (including those that
    /// are not connected).
    pub async fn list_with_status(&self) -> Vec<(String, McpStatus)> {
        self.statuses
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    async fn log_event(&self, name: &str, message: impl Into<String>) {
        let line = format!("[{}] {}", Utc::now().to_rfc3339(), message.into());
        let mut logs = self.logs.write().await;
        let entries = logs.entry(name.to_string()).or_default();
        entries.push_back(line);
        // Bounded ring buffer: keep only the newest lines, matching the
        // per-server cap used by the OAuth manager. Timestamp/debounce
        // behavior of the existing event stream is unchanged.
        while entries.len() > MAX_LOG_ENTRIES {
            entries.pop_front();
        }
    }

    // -- Client management ---------------------------------------------------

    pub async fn add_stdio(
        &self,
        config: McpServerConfig,
    ) -> Result<Arc<McpClient>, McpClientError> {
        let name = config.name.clone();
        self.connection_configs.write().await.insert(
            name.clone(),
            RegistryConnectionConfig::Stdio(config.clone()),
        );
        self.log_event(&name, "Connecting via stdio").await;

        let mut client_impl =
            McpClient::new(name.clone(), self.tool_registry.clone()).with_timeouts(config.timeouts);
        if let Some(protocol) = config.protocol.as_ref() {
            client_impl = client_impl.with_protocol_version(protocol.clone());
        }
        let client = Arc::new(client_impl);

        match client.connect_stdio(config).await {
            Ok(()) => {
                self.set_status(&name, McpStatus::Connected).await;
                self.clients.write().await.insert(name, client.clone());
                self.log_event(client.server_name(), "Connected").await;
                Ok(client)
            }
            Err(e) => {
                let status = client.status().await;
                self.set_status(&name, status).await;
                self.log_event(&name, format!("Connect failed: {}", e))
                    .await;
                // R1-RES-01: 試行前に挿入した接続設定を失敗時に残さない。
                self.connection_configs.write().await.remove(&name);
                Err(e)
            }
        }
    }
    /// Attaches an OAuth manager for a single server so its HTTP/SSE
    /// connections can send a bearer token obtained through the
    /// authorization-code flow.
    ///
    /// Managers are keyed by server name: a token obtained for server A must
    /// never be injected into a client for server B.
    pub async fn set_oauth_manager(
        &self,
        server_name: impl Into<String>,
        manager: Arc<McpOAuthManager>,
    ) {
        self.oauth_managers
            .write()
            .await
            .insert(server_name.into(), manager);
    }

    /// Removes the OAuth manager registered for a server (e.g. on logout).
    /// Returns `true` when a manager was present.
    pub async fn remove_oauth_manager(&self, server_name: &str) -> bool {
        self.oauth_managers
            .write()
            .await
            .remove(server_name)
            .is_some()
    }

    /// Whether an OAuth manager is currently registered for a server.
    pub async fn has_oauth_manager(&self, server_name: &str) -> bool {
        self.oauth_managers.read().await.contains_key(server_name)
    }

    pub async fn add_http(
        &self,
        name: String,
        url: String,
        headers: Option<HashMap<String, String>>,
        timeouts: McpTimeouts,
        protocol: Option<String>,
    ) -> Result<Arc<McpClient>, McpClientError> {
        self.connection_configs.write().await.insert(
            name.clone(),
            RegistryConnectionConfig::Http {
                url: url.clone(),
                headers: headers.clone(),
                timeouts,
            },
        );
        self.log_event(&name, "Connecting via http").await;

        let mut client_impl =
            McpClient::new(name.clone(), self.tool_registry.clone()).with_timeouts(timeouts);
        if let Some(protocol) = protocol {
            client_impl = client_impl.with_protocol_version(protocol);
        }
        let oauth_manager = self.oauth_managers.read().await.get(&name).cloned();
        if let Some(manager) = oauth_manager {
            client_impl = client_impl.with_oauth_manager(manager).await;
        }
        let client = Arc::new(client_impl);

        match client.connect_http(url, headers).await {
            Ok(()) => {
                self.set_status(&name, McpStatus::Connected).await;
                self.clients.write().await.insert(name, client.clone());
                self.log_event(client.server_name(), "Connected").await;
                Ok(client)
            }
            Err(e) => {
                let status = client.status().await;
                self.set_status(&name, status).await;
                self.log_event(&name, format!("Connect failed: {}", e))
                    .await;
                // R1-RES-01: 試行前に挿入した接続設定を失敗時に残さない。
                self.connection_configs.write().await.remove(&name);
                Err(e)
            }
        }
    }

    pub async fn add_sse(
        &self,
        name: String,
        url: String,
        headers: Option<HashMap<String, String>>,
        timeouts: McpTimeouts,
        protocol: Option<String>,
    ) -> Result<Arc<McpClient>, McpClientError> {
        self.connection_configs.write().await.insert(
            name.clone(),
            RegistryConnectionConfig::Sse {
                url: url.clone(),
                headers: headers.clone(),
                timeouts,
            },
        );
        self.log_event(&name, "Connecting via sse").await;

        let mut client_impl =
            McpClient::new(name.clone(), self.tool_registry.clone()).with_timeouts(timeouts);
        if let Some(protocol) = protocol {
            client_impl = client_impl.with_protocol_version(protocol);
        }
        let oauth_manager = self.oauth_managers.read().await.get(&name).cloned();
        if let Some(manager) = oauth_manager {
            client_impl = client_impl.with_oauth_manager(manager).await;
        }
        let client = Arc::new(client_impl);

        match client.connect_sse(url, headers).await {
            Ok(()) => {
                self.set_status(&name, McpStatus::Connected).await;
                self.clients.write().await.insert(name, client.clone());
                self.log_event(client.server_name(), "Connected").await;
                Ok(client)
            }
            Err(e) => {
                let status = client.status().await;
                self.set_status(&name, status).await;
                self.log_event(&name, format!("Connect failed: {}", e))
                    .await;
                // R1-RES-01: 試行前に挿入した接続設定を失敗時に残さない。
                self.connection_configs.write().await.remove(&name);
                Err(e)
            }
        }
    }

    /// Backwards-compatible alias for `add_stdio`.
    pub async fn add_client(
        &self,
        config: McpServerConfig,
    ) -> Result<Arc<McpClient>, McpClientError> {
        self.add_stdio(config).await
    }

    pub async fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.read().await.get(name).cloned()
    }

    /// RES-03: per-server ログを破棄する（remove / restart の共通処理）。
    async fn drop_server_log(&self, name: &str) {
        self.logs.write().await.remove(name);
    }

    /// remove / purge の共通後始末。
    /// `drop_connection_config` が true のときだけ接続設定も破棄する
    /// （remove は restart 契約のため残す）。
    async fn drop_client(
        &self,
        name: &str,
        drop_connection_config: bool,
    ) -> Result<(), McpClientError> {
        let client = self.clients.write().await.remove(name);
        // close が失敗してもクライアントは既に map から削除済みのため、
        // ステータスとログの整合性を先に回復してからエラーを返す。
        let close_result = match client {
            Some(client) => client.close().await,
            None => Ok(()),
        };
        self.set_status(name, McpStatus::Disabled).await;
        // 切断ログを残す意味はないため、消去してから状態を完了させる。
        self.drop_server_log(name).await;
        if drop_connection_config {
            self.connection_configs.write().await.remove(name);
        }
        close_result
    }

    /// クライアントを切断し、status / log を後始末する。
    /// restart に備えて connection_configs は保持する（既存契約）。
    pub async fn remove(&self, name: &str) -> Result<(), McpClientError> {
        self.drop_client(name, false).await
    }

    /// remove と同じ後始末を行った上で、restart 用の接続設定も破棄する。
    /// サーバーを完全に削除し、再追加まで restart できないようにする場合に使う。
    pub async fn purge(&self, name: &str) -> Result<(), McpClientError> {
        self.drop_client(name, true).await
    }

    pub async fn list(&self) -> Vec<(String, Arc<McpClient>)> {
        self.clients
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn tool_registry(&self) -> Arc<McpToolRegistry> {
        self.tool_registry.clone()
    }

    pub async fn get_logs(&self, name: &str) -> Vec<String> {
        self.logs
            .read()
            .await
            .get(name)
            .map(|entries| entries.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn restart(&self, name: &str) -> Result<Arc<McpClient>, McpClientError> {
        self.log_event(name, "Restart requested").await;

        let config = self
            .connection_configs
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| {
                McpClientError::ProtocolError(format!("No restart config found for {}", name))
            })?;

        if let Some(client) = self.clients.write().await.remove(name) {
            client.close().await?;
        }
        // 再接続前に古い接続のログを破棄する（RES-03）。
        self.drop_server_log(name).await;

        match config {
            RegistryConnectionConfig::Stdio(config) => self.add_stdio(config).await,
            RegistryConnectionConfig::Http {
                url,
                headers,
                timeouts,
            } => {
                self.add_http(name.to_string(), url, headers, timeouts, None)
                    .await
            }
            RegistryConnectionConfig::Sse {
                url,
                headers,
                timeouts,
            } => {
                self.add_sse(name.to_string(), url, headers, timeouts, None)
                    .await
            }
        }
    }
}

impl Default for McpClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use tokio::time::{sleep, Duration};

    /// The unimplemented `auto` mode must never reach `initialize` as a raw
    /// protocolVersion, whether it came from config or the HTTP config API.
    #[test]
    fn with_protocol_version_normalizes_auto_to_legacy() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_protocol_version("auto");
        assert_eq!(client.protocol_version, LEGACY_PROTOCOL_REVISION);
    }

    /// The HTTP config API accepts the documented `legacy` alias as a raw
    /// string. It must be normalized before becoming a wire protocolVersion;
    /// otherwise `initialize` sends a value no server understands.
    #[test]
    fn with_protocol_version_normalizes_legacy_alias_from_http_config_api() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_protocol_version("legacy");
        assert_eq!(client.protocol_version, LEGACY_PROTOCOL_REVISION);
    }

    /// An explicit newer revision must be passed through unchanged.
    #[test]
    fn with_protocol_version_preserves_explicit_new_revision() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_protocol_version("2026-07-28");
        assert_eq!(client.protocol_version, "2026-07-28");
    }

    struct MockTransport {
        messages: Mutex<VecDeque<(Duration, Option<JsonRpcMessage>)>>,
    }

    impl MockTransport {
        fn new(messages: Vec<(Duration, Option<JsonRpcMessage>)>) -> Self {
            Self {
                messages: Mutex::new(VecDeque::from(messages)),
            }
        }
    }

    #[async_trait]
    impl McpTransport for MockTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            let next = self.messages.lock().await.pop_front();
            match next {
                Some((delay, message)) => {
                    sleep(delay).await;
                    Ok(message)
                }
                None => Ok(None),
            }
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn call_tool_resets_timeout_on_progress_notification() {
        let tool_registry = Arc::new(McpToolRegistry::new());
        let client = McpClient::new("test-server".to_string(), tool_registry).with_timeout(30);

        let progress = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: "notifications/progress".to_string(),
            params: Some(serde_json::json!({ "progress": 0.5 })),
        };
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: 1,
            result: Some(serde_json::json!({
                "content": [{ "type": "text", "text": "ok" }]
            })),
            error: None,
        };
        let transport = MockTransport::new(vec![
            (
                Duration::from_millis(15),
                Some(JsonRpcMessage::Notification(progress)),
            ),
            (
                Duration::from_millis(20),
                Some(JsonRpcMessage::Response(response)),
            ),
        ]);

        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(transport));
        }

        let result = client
            .call_tool("slow-tool", Some(serde_json::json!({ "q": "value" })))
            .await
            .expect("tool call should complete before timeout when progress resets deadline");

        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("ok"));
    }

    /// Both wire spellings of a progress notification must be recognized by
    /// `is_progress_notification`: the MCP one and the LSP-style `$/progress`.
    #[test]
    fn is_progress_notification_accepts_both_method_spellings() {
        for method in ["notifications/progress", "$/progress"] {
            let notif = JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: method.to_string(),
                params: Some(serde_json::json!({ "progress": 1 })),
            };
            assert!(
                McpClient::is_progress_notification(&notif),
                "must recognize {method} as a progress notification"
            );
        }

        let unrelated = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: "notifications/message".to_string(),
            params: None,
        };
        assert!(!McpClient::is_progress_notification(&unrelated));
    }

    /// Loopback mock: deliver a progress notification, then a response that
    /// arrives AFTER the original deadline. Without a deadline extension the
    /// `timeout_at(initial_deadline)` would fire, so a successful call proves
    /// the extension actually happened (not just that the response was fast).
    async fn assert_progress_notification_extends_deadline(method: &str) {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_timeout(100); // initial deadline 100ms / hard deadline 400ms

        let progress = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(serde_json::json!({ "progress": 0.5 })),
        };
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: 1,
            result: Some(serde_json::json!({
                "content": [{ "type": "text", "text": "ok" }]
            })),
            error: None,
        };
        // 40ms で progress、そこから 80ms 後（= 通算 120ms）で response。
        // progress が無視されれば 100ms の初期 deadline で Timeout になる。
        let transport = MockTransport::new(vec![
            (
                Duration::from_millis(40),
                Some(JsonRpcMessage::Notification(progress)),
            ),
            (
                Duration::from_millis(80),
                Some(JsonRpcMessage::Response(response)),
            ),
        ]);
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(transport));
        }

        let started = tokio::time::Instant::now();
        let result = client
            .call_tool("slow-tool", None)
            .await
            .expect("response past the initial deadline must succeed when progress extends it");
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "response must arrive after the initial 100ms deadline to prove extension"
        );
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn standard_progress_notification_extends_deadline_via_loopback() {
        assert_progress_notification_extends_deadline("notifications/progress").await;
    }

    #[tokio::test]
    async fn dollar_progress_notification_extends_deadline_via_loopback() {
        assert_progress_notification_extends_deadline("$/progress").await;
    }

    /// A transport that never answers, only emits progress notifications at a
    /// fixed interval. Models a server keeping a call alive forever by
    /// streaming progress.
    struct InfiniteProgressTransport {
        interval: Duration,
        method: &'static str,
    }

    #[async_trait]
    impl McpTransport for InfiniteProgressTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            sleep(self.interval).await;
            Ok(Some(JsonRpcMessage::Notification(JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: self.method.to_string(),
                params: Some(serde_json::json!({ "progress": 1 })),
            })))
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    /// The extension helper must refuse to extend past the extension budget,
    /// and clamp any accepted extension to the absolute hard deadline.
    #[test]
    fn progress_deadline_extension_is_bounded() {
        let now = tokio::time::Instant::now();
        let timeout = Duration::from_millis(10);
        let hard = now + Duration::from_millis(40);

        assert_eq!(
            McpClient::next_progress_deadline(now, timeout, hard, 0),
            Some(now + Duration::from_millis(10))
        );
        // A late progress message would push past the hard deadline -> clamp.
        assert_eq!(
            McpClient::next_progress_deadline(now + Duration::from_millis(35), timeout, hard, 1),
            Some(hard)
        );
        // Budget exhausted -> no further extension at all.
        assert_eq!(
            McpClient::next_progress_deadline(now, timeout, hard, MAX_PROGRESS_DEADLINE_EXTENSIONS),
            None
        );
    }

    /// Regression: an endless stream of progress notifications must not keep
    /// tools/call alive forever. The hard deadline fixed at call start wins,
    /// even though every progress message would otherwise refresh the deadline.
    #[tokio::test]
    async fn infinite_progress_times_out_at_hard_deadline() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_timeout(20);
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(InfiniteProgressTransport {
                interval: Duration::from_millis(1),
                method: "notifications/progress",
            }));
        }

        let started = tokio::time::Instant::now();
        let error = client
            .call_tool("streaming-tool", None)
            .await
            .expect_err("endless progress must still time out");
        assert!(
            matches!(&error, McpClientError::Timeout),
            "unexpected: {error}"
        );

        let hard = Duration::from_millis(20).saturating_mul(PROGRESS_HARD_DEADLINE_MULTIPLIER);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(30),
            "progress should have kept the call alive past the initial deadline"
        );
        assert!(
            elapsed < hard + Duration::from_millis(200),
            "call must terminate near the hard deadline, not continue indefinitely"
        );
    }

    /// `$/progress` も `is_progress_notification` に認識されるため、無限に流れる
    /// 代替 progress でも初期 deadline を超えてハード deadline まで延長される。
    /// 認識されなければ初期 20ms で Timeout となり経過時間アサーションが落ちる。
    #[tokio::test]
    async fn infinite_dollar_progress_times_out_at_hard_deadline() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_timeout(20);
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(InfiniteProgressTransport {
                interval: Duration::from_millis(1),
                method: "$/progress",
            }));
        }

        let started = tokio::time::Instant::now();
        let error = client
            .call_tool("streaming-tool", None)
            .await
            .expect_err("endless $/progress must still time out");
        assert!(
            matches!(&error, McpClientError::Timeout),
            "unexpected: {error}"
        );

        let hard = Duration::from_millis(20).saturating_mul(PROGRESS_HARD_DEADLINE_MULTIPLIER);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(30),
            "$/progress must be recognized so the call survives past the initial 20ms deadline"
        );
        assert!(
            elapsed < hard + Duration::from_millis(200),
            "call must terminate near the hard deadline, not continue indefinitely"
        );
    }

    /// A transport whose receive() signals entry, then stays pending for a
    /// fixed window before closing the connection.
    struct SlowReceiveTransport {
        entered: Arc<std::sync::atomic::AtomicBool>,
        delay: Duration,
    }

    #[async_trait]
    impl McpTransport for SlowReceiveTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            self.entered.store(true, Ordering::SeqCst);
            sleep(self.delay).await;
            Ok(None)
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    /// Regression: while a request awaits receive(), the outer transport Mutex
    /// must be free for other callers (other requests, close, reconnect). With
    /// the old guard-held-across-receive code this lock attempt would block for
    /// the whole receive window and the short timeout below would fire.
    #[tokio::test]
    async fn transport_mutex_is_released_while_awaiting_receive() {
        let client = Arc::new(
            McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
                .with_timeout(5_000),
        );
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(SlowReceiveTransport {
                entered: entered.clone(),
                delay: Duration::from_millis(300),
            }));
        }

        let call_client = client.clone();
        let call = tokio::spawn(async move { call_client.call_tool("slow", None).await });

        // Wait until receive() is definitely pending before probing the lock.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !entered.load(Ordering::SeqCst) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "receive() was never entered"
            );
            sleep(Duration::from_millis(5)).await;
        }

        let guard = tokio::time::timeout(Duration::from_millis(80), client.transport.lock())
            .await
            .expect("transport Mutex must be available while receive() is pending");
        drop(guard);

        let joined = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("call must finish");
        // receive() が None を返すと TransportError になる。
        // このテストの目的は receive() 待ち中に Mutex が取得できたことなので、
        // call の最終エラー種別は問わない（panic していなければよい）。
        assert!(joined.is_ok(), "call task must not panic");
    }

    /// Models StdioTransport's lock ordering (SIG-01): `receive()` holds the
    /// stdout guard while waiting for a reply that never arrives, while
    /// `close()` must acquire that same guard before it can finish. Without
    /// the shutdown notification, close() blocks forever on stdout.
    struct DeadlockProneTransport {
        stdout: Arc<Mutex<()>>,
        entered: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl McpTransport for DeadlockProneTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            let _stdout_guard = self.stdout.lock().await;
            self.entered.store(true, Ordering::SeqCst);
            // Never answer: stay pending while holding the stdout guard.
            std::future::pending().await
        }

        async fn close(&self) -> Result<(), McpClientError> {
            // StdioTransport::close() acquires stdout before stdin/process.
            let _stdout_guard = self.stdout.lock().await;
            Ok(())
        }
    }

    /// Regression (SIG-01), progress-timeout path: an in-flight call() parked
    /// in receive() must be woken by close(), and close() must complete instead
    /// of deadlocking on the stdout guard held by that receive().
    #[tokio::test]
    async fn close_unblocks_in_flight_call_without_deadlock() {
        let client = Arc::new(
            McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
                .with_timeout(5_000),
        );
        let stdout = Arc::new(Mutex::new(()));
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(DeadlockProneTransport {
                stdout: stdout.clone(),
                entered: entered.clone(),
            }));
        }

        let call_client = client.clone();
        let call = tokio::spawn(async move { call_client.call_tool("hung", None).await });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !entered.load(Ordering::SeqCst) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "receive() was never entered"
            );
            sleep(Duration::from_millis(5)).await;
        }

        // Without the notification, close() would block on stdout forever and
        // this timeout would fire.
        let close_client = client.clone();
        let close_result =
            tokio::time::timeout(
                Duration::from_secs(1),
                async move { close_client.close().await },
            )
            .await
            .expect("close() must not deadlock against an in-flight receive()");
        assert!(close_result.is_ok(), "close() must succeed");

        let error = tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .expect("in-flight call must finish after close()")
            .expect("call task must not panic")
            .expect_err("in-flight call must fail after shutdown");
        assert!(
            matches!(&error, McpClientError::TransportError(msg) if msg == "Connection closed"),
            "unexpected error: {error}"
        );
    }

    /// Regression (SIG-01), plain send_request path: `initialize()` parked in
    /// receive() must be woken by close() with the same TransportError.
    #[tokio::test]
    async fn close_unblocks_in_flight_initialize_without_deadlock() {
        let client = Arc::new(
            McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
                .with_timeout(5_000),
        );
        let stdout = Arc::new(Mutex::new(()));
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(DeadlockProneTransport {
                stdout: stdout.clone(),
                entered: entered.clone(),
            }));
        }

        let init_client = client.clone();
        let init = tokio::spawn(async move { init_client.initialize().await });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !entered.load(Ordering::SeqCst) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "receive() was never entered"
            );
            sleep(Duration::from_millis(5)).await;
        }

        let close_client = client.clone();
        tokio::time::timeout(
            Duration::from_secs(1),
            async move { close_client.close().await },
        )
        .await
        .expect("close() must not deadlock against an in-flight initialize()")
        .expect("close() must succeed");

        let error = tokio::time::timeout(Duration::from_secs(1), init)
            .await
            .expect("in-flight initialize() must finish after close()")
            .expect("init task must not panic")
            .expect_err("in-flight initialize() must fail after shutdown");
        assert!(
            matches!(&error, McpClientError::TransportError(msg) if msg == "Connection closed"),
            "unexpected error: {error}"
        );
    }

    /// Transport whose `send()` signals entry and then stays pending: models an
    /// HTTP POST that is accepted but never answered.
    struct SignalingHangingSendTransport {
        entered: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl McpTransport for SignalingHangingSendTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            self.entered.store(true, Ordering::SeqCst);
            std::future::pending().await
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            Ok(None)
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    /// Regression (R4): close() during send() must abort the in-flight call.
    /// Without the send-side shutdown select!, `notify_waiters()` cannot wake
    /// a future that never registered `notified()`, so a hung POST would wait
    /// for the hard deadline (up to ~48h at the documented defaults).
    #[tokio::test]
    async fn close_during_hanging_send_aborts_in_flight_call() {
        let client = Arc::new(
            McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
                .with_timeout(60_000),
        );
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(SignalingHangingSendTransport {
                entered: entered.clone(),
            }));
        }

        let call_client = client.clone();
        let call = tokio::spawn(async move { call_client.call_tool("hung", None).await });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !entered.load(Ordering::SeqCst) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "send() was never entered"
            );
            sleep(Duration::from_millis(5)).await;
        }

        let close_client = client.clone();
        tokio::time::timeout(
            Duration::from_secs(1),
            async move { close_client.close().await },
        )
        .await
        .expect("close() must not hang while send() is pending")
        .expect("close() must succeed");

        let error = tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .expect("in-flight call must finish right after close()")
            .expect("call task must not panic")
            .expect_err("call must fail after shutdown");
        assert!(
            matches!(&error, McpClientError::TransportError(msg) if msg == "Connection closed"),
            "unexpected error: {error}"
        );
    }

    /// Regression (R4): close() before a request registers any `notified()`
    /// future must still be observed. `notify_waiters()` stores no permit, so a
    /// later `notified()` would miss the wake-up entirely; only the sticky flag
    /// can abort such a call.
    #[tokio::test]
    async fn close_before_request_registers_notified_aborts_call() {
        let client = Arc::new(
            McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
                .with_timeout(60_000),
        );
        // receive() would otherwise hang forever; the flag must short-circuit
        // before the transport is even touched.
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(HangingTransport));
        }

        client.close().await.expect("close() must succeed");
        assert!(client.is_shutdown(), "shutdown flag must be sticky");

        let error = tokio::time::timeout(Duration::from_secs(1), client.call_tool("hung", None))
            .await
            .expect("call must not hang after close()")
            .expect_err("call must fail when already shut down");
        assert!(
            matches!(&error, McpClientError::TransportError(msg) if msg == "Connection closed"),
            "unexpected error: {error}"
        );
    }

    /// A response that only becomes available after the effective deadline.
    struct LateResponseTransport {
        delay: Duration,
    }

    #[async_trait]
    impl McpTransport for LateResponseTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            sleep(self.delay).await;
            Ok(Some(JsonRpcMessage::Response(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: 1,
                result: Some(serde_json::json!({
                    "content": [{ "type": "text", "text": "late" }]
                })),
                error: None,
            })))
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    /// Regression (R4): default random branch selection could let a response
    /// arriving after `effective_deadline` win over the timeout. `biased;`
    /// restores the old `timeout_at` priority, so Timeout is deterministic.
    #[tokio::test]
    async fn progress_timeout_wins_over_late_response() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_timeout(40); // effective deadline 40ms, hard deadline 160ms
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(LateResponseTransport {
                delay: Duration::from_millis(120),
            }));
        }

        let error = client
            .call_tool("slow", None)
            .await
            .expect_err("response after the effective deadline must not succeed");
        assert!(
            matches!(error, McpClientError::Timeout),
            "unexpected error: {error}"
        );
    }

    /// The shutdown flag is idempotent: repeated close() calls must remain safe.
    #[tokio::test]
    async fn repeated_close_is_safe() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()));
        client.close().await.expect("first close must succeed");
        client.close().await.expect("second close must succeed");
        assert!(client.is_shutdown());
    }

    #[tokio::test]
    async fn refresh_tools_if_needed_reloads_tools() {
        let tool_registry = Arc::new(McpToolRegistry::new());
        let client = McpClient::new("server-a".to_string(), tool_registry.clone());

        tool_registry
            .register(crate::tool::McpTool::new(
                "server-a",
                "stale",
                Some("stale tool".to_string()),
                serde_json::json!({ "type": "object" }),
            ))
            .await;

        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: 1,
            result: Some(serde_json::json!({
                "tools": [{ "name": "fresh", "description": "fresh tool", "inputSchema": { "type": "object" } }]
            })),
            error: None,
        };
        let transport = MockTransport::new(vec![(
            Duration::from_millis(0),
            Some(JsonRpcMessage::Response(response)),
        )]);

        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(transport));
        }

        client
            .handle_notification(JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: "notifications/tools/list_changed".to_string(),
                params: None,
            })
            .await;

        client
            .refresh_tools_if_needed()
            .await
            .expect("tools should refresh successfully");

        let tools = tool_registry.list_for_server("server-a").await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "fresh");
    }

    #[tokio::test]
    async fn refresh_tools_if_needed_restores_flag_on_load_failure() {
        let tool_registry = Arc::new(McpToolRegistry::new());
        let client = McpClient::new("server-a".to_string(), tool_registry.clone());

        // 空のトランスポートは receive() が None を返すため load_tools が失敗する。
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(MockTransport::new(vec![])));
        }

        client
            .handle_notification(JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: "notifications/tools/list_changed".to_string(),
                params: None,
            })
            .await;

        assert!(
            client.refresh_tools_if_needed().await.is_err(),
            "load_tools の失敗は呼び出し側へ伝わること"
        );
        assert!(
            client.tools_changed.load(Ordering::SeqCst),
            "ロード失敗時に tools_changed が復元されること"
        );

        // 復元されたフラグにより、トランスポート回復後の再ロードが保証されること。
        // 先頭の失敗ロードが id=1 を使ったため、ここでのリクエストは id=2 になる。
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: 2,
            result: Some(serde_json::json!({
                "tools": [{ "name": "fresh", "description": "fresh tool", "inputSchema": { "type": "object" } }]
            })),
            error: None,
        };
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(MockTransport::new(vec![(
                Duration::from_millis(0),
                Some(JsonRpcMessage::Response(response)),
            )])));
        }

        client
            .refresh_tools_if_needed()
            .await
            .expect("reload should succeed once the transport recovers");

        assert!(!client.tools_changed.load(Ordering::SeqCst));
        let tools = tool_registry.list_for_server("server-a").await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "fresh");
    }

    struct HangingTransport;

    #[async_trait]
    impl McpTransport for HangingTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            std::future::pending().await
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn initialize_uses_startup_timeout() {
        let tool_registry = Arc::new(McpToolRegistry::new());
        let client =
            McpClient::new("test-server".to_string(), tool_registry).with_timeouts(McpTimeouts {
                startup_ms: 20,
                catalog_ms: 200,
                execution_ms: 200,
            });
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(HangingTransport));
        }

        let error = client
            .initialize()
            .await
            .expect_err("initialize must time out");
        assert!(matches!(error, McpClientError::Timeout));
    }

    #[tokio::test]
    async fn read_resource_uses_catalog_timeout() {
        let tool_registry = Arc::new(McpToolRegistry::new());
        let client =
            McpClient::new("test-server".to_string(), tool_registry).with_timeouts(McpTimeouts {
                startup_ms: 200,
                catalog_ms: 20,
                execution_ms: 200,
            });
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(HangingTransport));
        }

        let error = client
            .read_resource("file:///test")
            .await
            .expect_err("read_resource must time out");
        assert!(matches!(error, McpClientError::Timeout));
    }

    /// Transport with no id routing: `receive()` returns a response for the
    /// most recently sent request id after a short delay. Without request-level
    /// serialization two concurrent callers would both read the same reply and
    /// the one whose id does not match would loop forever.
    struct EchoTransport {
        last_id: Mutex<Option<u64>>,
        delay: Duration,
    }

    impl EchoTransport {
        fn new(delay: Duration) -> Self {
            Self {
                last_id: Mutex::new(None),
                delay,
            }
        }
    }

    #[async_trait]
    impl McpTransport for EchoTransport {
        async fn send(&self, request: &JsonRpcRequest) -> Result<(), McpClientError> {
            *self.last_id.lock().await = Some(request.id);
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            sleep(self.delay).await;
            let id = self.last_id.lock().await.unwrap_or(0);
            Ok(Some(JsonRpcMessage::Response(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({
                    "content": [{ "type": "text", "text": format!("resp-{id}") }]
                })),
                error: None,
            })))
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    /// Regression (A): two concurrent `call_tool` invocations must each receive
    /// the response matching their own request id, not steal each other's reply.
    #[tokio::test]
    async fn concurrent_call_tools_route_responses_by_id() {
        let client = Arc::new(
            McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
                .with_timeout(2_000),
        );
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(EchoTransport::new(Duration::from_millis(10))));
        }

        let call_a = {
            let client = client.clone();
            tokio::spawn(async move { client.call_tool("tool-a", None).await })
        };
        let call_b = {
            let client = client.clone();
            tokio::spawn(async move { client.call_tool("tool-b", None).await })
        };

        let result_a = tokio::time::timeout(Duration::from_secs(2), call_a)
            .await
            .expect("call a must not hang")
            .expect("call a task must succeed")
            .expect("call a must succeed");
        let result_b = tokio::time::timeout(Duration::from_secs(2), call_b)
            .await
            .expect("call b must not hang")
            .expect("call b task must succeed")
            .expect("call b must succeed");

        let mut texts = vec![
            result_a.content[0].text.clone().unwrap_or_default(),
            result_b.content[0].text.clone().unwrap_or_default(),
        ];
        texts.sort();
        assert_eq!(texts, vec!["resp-1".to_string(), "resp-2".to_string()]);
    }

    /// Transport whose `send()` never returns. Models an HTTP POST that is
    /// accepted but never answered.
    struct HangingSendTransport;

    #[async_trait]
    impl McpTransport for HangingSendTransport {
        async fn send(&self, _request: &JsonRpcRequest) -> Result<(), McpClientError> {
            std::future::pending().await
        }

        async fn send_notification(
            &self,
            _notification: &JsonRpcNotification,
        ) -> Result<(), McpClientError> {
            Ok(())
        }

        async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
            Ok(None)
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }
    }

    /// Regression (B): a send() that never returns must be bounded by the
    /// request deadline, not hang forever.
    #[tokio::test]
    async fn hanging_send_times_out() {
        let client = McpClient::new("test-server".to_string(), Arc::new(McpToolRegistry::new()))
            .with_timeout(30);
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(HangingSendTransport));
        }

        let started = tokio::time::Instant::now();
        let error = tokio::time::timeout(Duration::from_secs(2), client.call_tool("hung", None))
            .await
            .expect("send must not hang forever")
            .expect_err("unanswered send must time out");
        assert!(matches!(error, McpClientError::Timeout));

        // hard deadline = 30ms * PROGRESS_HARD_DEADLINE_MULTIPLIER (4) = 120ms.
        let bound = Duration::from_millis(30).saturating_mul(PROGRESS_HARD_DEADLINE_MULTIPLIER);
        assert!(
            started.elapsed() < bound + Duration::from_millis(500),
            "send must be covered by the hard deadline, not wait indefinitely"
        );
    }

    /// The registry keeps one OAuth manager per server name; registering a
    /// second server must not overwrite the first one's manager (otherwise
    /// its bearer token would be sent to the wrong server).
    #[tokio::test]
    async fn registry_keeps_oauth_managers_keyed_by_server_name() {
        use crate::oauth::McpOAuthConfig;

        let registry = McpClientRegistry::new();
        let alpha = Arc::new(McpOAuthManager::new(
            "alpha".to_string(),
            "https://alpha.invalid".to_string(),
            McpOAuthConfig::default(),
        ));
        let beta = Arc::new(McpOAuthManager::new(
            "beta".to_string(),
            "https://beta.invalid".to_string(),
            McpOAuthConfig::default(),
        ));

        registry.set_oauth_manager("alpha", alpha.clone()).await;
        registry.set_oauth_manager("beta", beta.clone()).await;

        let managers = registry.oauth_managers.read().await;
        assert_eq!(managers.len(), 2);
        assert!(Arc::ptr_eq(
            managers.get("alpha").expect("alpha manager"),
            &alpha
        ));
        assert!(Arc::ptr_eq(
            managers.get("beta").expect("beta manager"),
            &beta
        ));
        assert!(managers.get("gamma").is_none());
    }

    /// Logout must drop only the named server's manager; otherwise
    /// `add_http`/`add_sse` and `registry.restart` keep injecting stale
    /// credentials even after the stored tokens were deleted.
    #[tokio::test]
    async fn remove_oauth_manager_drops_only_the_named_server() {
        use crate::oauth::McpOAuthConfig;

        let registry = McpClientRegistry::new();
        let alpha = Arc::new(McpOAuthManager::new(
            "alpha".to_string(),
            "https://alpha.invalid".to_string(),
            McpOAuthConfig::default(),
        ));
        let beta = Arc::new(McpOAuthManager::new(
            "beta".to_string(),
            "https://beta.invalid".to_string(),
            McpOAuthConfig::default(),
        ));

        registry.set_oauth_manager("alpha", alpha).await;
        registry.set_oauth_manager("beta", beta.clone()).await;
        assert!(registry.has_oauth_manager("alpha").await);
        assert!(registry.has_oauth_manager("beta").await);

        assert!(registry.remove_oauth_manager("alpha").await);
        assert!(!registry.has_oauth_manager("alpha").await);
        assert!(
            registry.oauth_managers.read().await.get("alpha").is_none(),
            "removed manager must no longer be retrievable"
        );
        assert!(Arc::ptr_eq(
            registry
                .oauth_managers
                .read()
                .await
                .get("beta")
                .expect("beta must survive removal of alpha"),
            &beta
        ));
        assert!(!registry.remove_oauth_manager("alpha").await);
    }

    /// RES-03 regression: the per-server log is a bounded ring buffer that
    /// evicts the oldest lines and keeps insertion order.
    #[tokio::test]
    async fn log_event_bounds_per_server_log() {
        let registry = McpClientRegistry::new();
        let extra = 5usize;
        let total = MAX_LOG_ENTRIES + extra;
        for index in 0..total {
            registry.log_event("alpha", format!("entry-{index}")).await;
        }

        let logs = registry.logs.read().await;
        let entries = logs.get("alpha").expect("alpha log must exist");
        assert_eq!(entries.len(), MAX_LOG_ENTRIES);
        assert!(
            entries
                .front()
                .expect("oldest line")
                .ends_with(format!("entry-{extra}").as_str()),
            "oldest lines must be dropped first"
        );
        assert!(
            entries
                .back()
                .expect("newest line")
                .ends_with(format!("entry-{}", total - 1).as_str()),
            "newest line must be retained"
        );
    }

    /// RES-03 regression: removing a server must drop its log so a reconnect
    /// starts with a clean history instead of accumulating old entries.
    #[tokio::test]
    async fn remove_drops_per_server_log() {
        let registry = McpClientRegistry::new();
        registry.log_event("alpha", "before removal").await;
        assert_eq!(registry.get_logs("alpha").await.len(), 1);

        registry
            .remove("alpha")
            .await
            .expect("removing a server without a client must succeed");

        assert!(registry.get_logs("alpha").await.is_empty());
        assert!(!registry.logs.read().await.contains_key("alpha"));
    }

    /// add 成功後の状態を実サーバなしで再現するスタブ。
    async fn seed_connected_client(registry: &McpClientRegistry, name: &str) {
        let client = Arc::new(McpClient::new(name.to_string(), registry.tool_registry()));
        {
            let mut guard = client.transport.lock().await;
            *guard = Some(Arc::new(MockTransport::new(vec![])));
        }
        registry
            .clients
            .write()
            .await
            .insert(name.to_string(), client);
    }

    /// R1-RES-01: remove は restart 契約を維持するため connection_configs を残す。
    #[tokio::test]
    async fn remove_keeps_connection_config_for_restart() {
        let registry = McpClientRegistry::new();
        seed_connected_client(&registry, "alpha").await;
        registry.connection_configs.write().await.insert(
            "alpha".to_string(),
            RegistryConnectionConfig::Http {
                url: "https://alpha.invalid/mcp".to_string(),
                headers: None,
                timeouts: McpTimeouts::default(),
            },
        );

        registry.remove("alpha").await.expect("remove must succeed");

        assert!(registry.get("alpha").await.is_none());
        assert_eq!(
            registry.get_status("alpha").await,
            Some(McpStatus::Disabled)
        );
        assert!(
            registry
                .connection_configs
                .read()
                .await
                .contains_key("alpha"),
            "remove must keep the connection config so restart can reconnect"
        );
    }

    /// R1-RES-01: purge は remove と同じ後始末 (close/status/log) を行い、
    /// さらに connection_configs からもエントリを削除する。
    #[tokio::test]
    async fn purge_drops_connection_config_and_client_state() {
        let registry = McpClientRegistry::new();
        seed_connected_client(&registry, "alpha").await;
        registry.connection_configs.write().await.insert(
            "alpha".to_string(),
            RegistryConnectionConfig::Stdio(McpServerConfig {
                name: "alpha".to_string(),
                command: "unused".to_string(),
                args: Vec::new(),
                env: None,
                cwd: None,
                timeouts: McpTimeouts::default(),
                protocol: None,
            }),
        );
        registry.set_status("alpha", McpStatus::Connected).await;
        registry.log_event("alpha", "before purge").await;
        assert_eq!(registry.get_logs("alpha").await.len(), 1);

        registry.purge("alpha").await.expect("purge must succeed");

        assert!(registry.get("alpha").await.is_none());
        assert_eq!(
            registry.get_status("alpha").await,
            Some(McpStatus::Disabled)
        );
        assert!(registry.get_logs("alpha").await.is_empty());
        assert!(
            !registry
                .connection_configs
                .read()
                .await
                .contains_key("alpha"),
            "purge must drop the connection config"
        );
    }

    /// R1-RES-01: 接続失敗時に add_* が挿入した connection_configs が残存しない。
    /// 削除・再追加の繰り返しでエントリが単調増加しないことも確認する。
    #[tokio::test]
    async fn failed_add_leaves_no_connection_config() {
        let registry = McpClientRegistry::new();
        let failing_config = || McpServerConfig {
            name: "alpha".to_string(),
            command: "__opencode_mcp_missing_command__".to_string(),
            args: Vec::new(),
            env: None,
            cwd: None,
            timeouts: McpTimeouts::uniform(50),
            protocol: None,
        };

        for _ in 0..3 {
            let result = registry.add_stdio(failing_config()).await;
            assert!(result.is_err(), "nonexistent command must fail to connect");
            assert!(
                !registry
                    .connection_configs
                    .read()
                    .await
                    .contains_key("alpha"),
                "failed add must not leave a stale connection config"
            );
        }
        assert!(
            registry.connection_configs.read().await.is_empty(),
            "repeated failed adds must not grow connection_configs"
        );
    }
}
