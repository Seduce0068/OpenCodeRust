//! OpenCode 全体で共有するドメイン型を定義するクレート。
//!
//! エージェント、セッション、メッセージ、PTY、Todo などのデータ構造を提供し、
//! クレートをまたいで受け渡す値の形を統一する。各型は `serde` による
//! シリアライズに対応している。
//!
//! # 使い方
//!
//! 必要な型をクレート直下から `use opencode_types::{Session, SessionMessage};` のように
//! import して利用する。

/// エージェントの定義・表示設定とプロバイダへのリクエスト情報。
///
/// `AgentInfo`, `AgentID`, `AgentMode`, `AgentColor`, `ProviderRequest` を提供する。
pub mod agent;
/// セッション・PTY などのリソースを識別する ID 型。
///
/// `SessionID` と `PtyID` を提供し、いずれも `Display` を実装する。
pub mod identifier;
/// セッション内のメッセージと、その構成要素であるパートの型。
///
/// `SessionMessage`（`user` / `assistant` コンストラクタと `get_text` を含む）、
/// `MessagePart`, `MessageRole`, `PartType` を提供する。
pub mod message;
/// プロジェクトを識別する `ProjectID` を提供する。
pub mod project;
/// 疑似ターミナル（PTY）の情報と、作成・更新時の入力型を提供する。
///
/// `PtyInfo`, `PtyStatus`, `CreateInput`, `UpdateInput`, `PtySize` を含む。
pub mod pty;
/// セッションの状態・サマリ・権限・トークン使用量などの型を提供する。
///
/// `Session`, `SessionStatus`, `SessionSummary`, `TimeInfo`, `TokenUsage` などを含む。
pub mod session;
/// セッション内 Todo の型と、文字列から列挙型へのパース関数を提供する。
///
/// `TodoItem`, `TodoStatus`, `TodoPriority`, `parse_status`, `parse_priority` を含む。
pub mod todo;

pub use agent::*;
pub use identifier::*;
pub use message::*;
pub use project::*;
pub use pty::*;
pub use session::*;
pub use todo::*;
