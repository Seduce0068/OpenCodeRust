//! Model Context Protocol (MCP) クライアントと OAuth 認証を提供するクレート。
//!
//! `client` が stdio / HTTP / SSE トランスポート経由の MCP サーバー接続と
//! ツール呼び出しを担い、`tool` がツールのレジストリを提供する。
//! `discovery` / `oauth` / `auth` はリモートサーバーの OAuth 2.0 + PKCE フローと
//! 認証情報の永続化を実装し、`protocol` が JSON-RPC の型を定義する。
//!
//! # 使い方
//!
//! `McpServerConfig` を作って `McpClient::connect_stdio` 等で接続し、
//! `call_tool` でツールを実行する。`McpClientRegistry` で複数サーバーを管理できる。

#![allow(clippy::manual_strip)]

pub mod auth;
pub mod client;
pub mod discovery;
pub mod oauth;
pub mod protocol;
pub mod tool;
pub mod transport;

pub use client::{McpClient, McpClientError, McpClientRegistry, McpServerConfig, McpStatus};
pub use discovery::{
    discover_endpoints, AuthorizationServerMetadata, DiscoveryError, OAuthEndpoints,
    ProtectedResourceMetadata, RegisteredClient,
};
pub use oauth::{AuthStatus, McpOAuthConfig, McpOAuthManager, OAuthError, OAuthRegistry};
pub use protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
pub use tool::{McpTool, McpToolRegistry};
pub use transport::{HttpTransport, McpTransport, SseTransport, StdioTransport};
