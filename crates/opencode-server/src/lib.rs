//! OpenCode の HTTP API サーバーを構成するクレート。
//!
//! `routes` が axum のルーターを定義し、`server` が `ServerState` と
//! `run_server` による起動、権限・質問のブローカーを提供する。
//! `pty` / `mcp_oauth` / `oauth` / `worktree` はターミナルセッション、
//! MCP・プロバイダの OAuth、git worktree 操作を担う。
//!
//! # 使い方
//!
//! `run_server(addr)` を await するとミドルウェア付きルーターが組み立てられ
//! 指定アドレスでサーバーが起動する。テストでは `ServerState` を直接生成して
//! `routes::router()` に渡せる。

#![allow(
    ambiguous_glob_reexports,
    clippy::manual_unwrap_or_default,
    clippy::redundant_guards,
    clippy::manual_strip
)]

pub mod error;
pub mod mcp_oauth;
pub mod oauth;
pub mod pty;
pub mod routes;
pub mod server;
pub mod worktree;

pub use error::*;
pub use mcp_oauth::*;
pub use oauth::*;
pub use pty::*;
pub use routes::*;
pub use server::*;
pub use worktree::*;
