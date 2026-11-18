//! ツール実行の権限判定を担うクレート。
//!
//! `ruleset` が設定から権限ルール（allow / deny / ask）を構築・評価し、
//! `engine` が確認待ちの管理とユーザーへの問い合わせ（`ask` / `respond`）を行う。
//! `arity` は bash コマンドのトークンから権限一致に使う主要なコマンド部分を
//! 抽出する補助を提供する。
//!
//! # 使い方
//!
//! `from_config` でルールセットを作り、`PermissionEngine` に登録して
//! `is_approved` / `ask` で実行の可否を確認する。

pub mod arity;
pub mod engine;
pub mod ruleset;

pub use arity::*;
pub use engine::*;
pub use ruleset::*;
