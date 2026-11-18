//! OpenCode の設定を読み込み・検証・永続化するクレート。
//!
//! `schema` が `Config` やキーバインド・TUI・サーバー・エージェントなどの
//! 設定データ構造を定義し、`loader` がプロジェクト／グローバル設定ファイルの
//! 読み込みと更新を行う。`wellknown` はリモートの well-known 設定を取得し
//! キャッシュする。
//!
//! # 使い方
//!
//! `load_config(project_dir)` で `Config` を読み込み、`update_config` で変更を書き戻す。
//! リモート設定も含める場合は `load_config_with_remote` を使う。

pub mod loader;
pub mod schema;
pub mod wellknown;

pub use loader::*;
pub use schema::*;
