#![allow(clippy::manual_strip)]

pub mod auth;
pub mod client;
pub mod discovery;
pub mod oauth;
pub mod protocol;
pub mod tool;
pub mod transport;

pub use client::{
    McpClient, McpClientError, McpClientRegistry, McpServerConfig, McpStatus,
    MCP_TOOLS_CHANGED_EVENT,
};
pub use discovery::{
    discover_endpoints, AuthorizationServerMetadata, DiscoveryError, OAuthEndpoints,
    ProtectedResourceMetadata, RegisteredClient,
};
pub use oauth::{AuthStatus, McpOAuthConfig, McpOAuthManager, OAuthError, OAuthRegistry};
pub use protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
pub use tool::{McpTool, McpToolRegistry};
pub use transport::{HttpTransport, McpTransport, SseTransport, StdioTransport};
