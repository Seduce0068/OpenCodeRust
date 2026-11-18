//! OpenCode の基盤機能を提供するクレート。
//!
//! イベントバス、ファイルシステム抽象、git 操作・変更監視、ID 生成、
//! リポジトリ参照のパースをまとめる。上位クレートからは
//! `opencode_core::bus::Bus` や `opencode_core::id::create` のように利用する。
//!
//! # 使い方
//!
//! `bus::define_event` でイベント定義を作り `Bus::publish` / `Bus::subscribe` で
//! やり取りする。`id::create` でプレフィックス付き ID を生成できる。

#![allow(clippy::double_must_use)]

/// 型付きイベントの定義と、publish / subscribe によるイベント配送を提供する。
///
/// `Bus`, `BusEvent`, `BusEventDef`, `define_event` を含む。
pub mod bus;
/// ルートディレクトリを基点にしたファイル読み取りとディレクトリ一覧を提供する。
///
/// `FileSystem` トレイトと、その tokio ベースの実装 `FileSystemImpl` を含む。
pub mod filesystem;
/// git リポジトリの情報と、git コマンドによる参照取得を提供する。
///
/// `Repository` と `GitClient`（`head` / `branch`）を含む。
pub mod git;
/// `.git` ディレクトリの変更を監視し、イベントとして配信する。
///
/// `GitWatcher` と `GitEvent` を提供する。
pub mod git_watcher;
/// プレフィックス付きの一意な ID の生成・解析を提供する。
///
/// `Prefix`, `create`, `timestamp`, `validate_prefix` を含む。
pub mod id;
/// GitHub ショートハンドや SCP 形式などのリポジトリ参照をパースする。
///
/// `Reference`, `RemoteReference`, `FileReference`, `BaseReference` と `parse` を含む。
pub mod repository;

pub use bus::*;
pub use filesystem::*;
pub use git::*;
pub use git_watcher::*;
pub use id::*;
pub use repository::*;
