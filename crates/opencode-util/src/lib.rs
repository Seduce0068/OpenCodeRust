//! 横断的な共通ユーティリティを提供するクレート。
//!
//! - [`filesystem`]: `tokio::fs` / `walkdir` を用いた非同期ファイルシステム操作
//! - [`logging`]: `tracing` の初期化とログレコードの表現
//! - [`util`]: ワイルドカード・色・タイムアウト・ロック・トークン・書式・git・abort の小さなヘルパー
//!
//! 主な利用者は `opencode-cli`。特定のドメインに依存しない軽量な機能に限る。

/// 非同期ファイルシステム操作のヘルパー。
pub mod filesystem;
/// `tracing` 初期化とログレコードの表現。
pub mod logging;
/// 文字列・時間・ロックなどの汎用ヘルパー群。
pub mod util;

/// 非同期ファイルシステム操作のエントリポイント。
pub use filesystem::Filesystem;
/// トレーシング初期化とログレコード型の再エクスポート。
pub use logging::{init_tracing, Log, LogLevel};
/// 汎用ヘルパー群の再エクスポート。
pub use util::{abort, color, defer, format, git, lock, timeout, token, wildcard};
