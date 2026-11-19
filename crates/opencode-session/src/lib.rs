//! セッションのライフサイクルと会話処理を統合するクレート。
//!
//! `SessionManager` がセッションの作成・一覧・状態管理を行い、`prompt` が
//! ユーザー入力から LLM リクエストを組み立ててストリーム処理する。
//! `llm` / `compaction` / `summary` / `revert` / `retry` / `todo` はストリーム処理、
//! 会話圧縮、要約、巻き戻し、再試行、Todo 管理を担い、`instruction` は
//! AGENTS.md などの指示ファイルを解決する。
//!
//! # 使い方
//!
//! `SessionManager::new()` でマネージャーを作り、`SessionPrompt` に
//! `PromptInput` を渡して会話を進める。

#![allow(
    ambiguous_glob_reexports,
    clippy::too_many_arguments,
    clippy::filter_next,
    clippy::manual_strip,
    clippy::collapsible_match,
    clippy::new_ret_no_self
)]
pub mod compaction;
pub mod instruction;
pub mod llm;
pub mod message;
pub mod message_v2;
pub mod prompt;
pub mod retry;
pub mod revert;
pub mod session;
pub mod snapshot;
pub mod status;
pub mod summary;
pub mod system;
pub mod todo;

pub use compaction::*;
pub use instruction::*;
pub use llm::*;
pub use message::*;
pub use message_v2::*;
pub use prompt::*;
pub use retry::*;
pub use revert::*;
// Bus 削除で session モジュール由来の glob re-export が未使用と判定されるが、
// 公開 API の互換維持のため残す（ambiguous_glob_reexports と同じ方針）。
#[allow(unused_imports)]
pub use session::*;
pub use status::*;
pub use summary::*;
pub use system::*;
pub use todo::*;

pub use session::{
    BusyError, FileDiff, PermissionRuleset, RunStatus, Session, SessionError, SessionEvent,
    SessionFilter, SessionManager, SessionRevert, SessionRow, SessionShare, SessionStateEvent,
    SessionStateManager, SessionStatus, SessionSummary, SessionTime, SessionUsage,
};
