//! エージェントの定義・生成・実行を担うクレート。
//!
//! `agent` がビルトインエージェント、モデル参照、権限判断の設定を定義し、
//! `executor` の `AgentExecutor` が LLM へのリクエストとツール呼び出しを実行する。
//! `message` は会話履歴（`Conversation`）とメッセージ・ツール呼び出しの型を提供し、
//! サブセッションの永続化状態も扱う。
//!
//! # 使い方
//!
//! `use opencode_agent::{AgentExecutor, AgentInfo};` のようにクレート直下から import し、
//! `AgentExecutor` に `GenerateInput` を渡してエージェントを実行する。

#![allow(
    ambiguous_glob_reexports,
    clippy::cloned_ref_to_slice_refs,
    clippy::field_reassign_with_default
)]

pub mod agent;
pub mod executor;
pub mod message;

pub use agent::*;
pub use executor::*;
pub use message::*;
