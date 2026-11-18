//! TypeScript plugin subprocess support.
//!
//! This module provides the ability to load TS/JS plugins by spawning a
//! `plugin-host.ts` child process and communicating over Content-Length
//! framed JSON-RPC 2.0 (stdin/stdout).

pub mod auth;
pub mod client;
pub mod loader;
pub mod protocol;
pub mod runtime;

// Re-exports for convenience
pub use auth::{
    PluginAuthBridge, PluginAuthError, PluginFetchRequest, PluginFetchResponse,
    PluginFetchStreamResponse,
};
pub use client::{PluginContext, PluginSubprocess, PluginSubprocessError};
pub use loader::{PluginLoader, PluginLoaderError};
pub use runtime::{detect_runtime, JsRuntime};

use crate::HookEvent;

/// Map a TS hook name string to the corresponding `HookEvent` variant.
pub fn hook_name_to_event(name: &str) -> Option<HookEvent> {
    match name {
        "chat.headers" => Some(HookEvent::ChatHeaders),
        "chat.params" => Some(HookEvent::ChatParams),
        "chat.message" => Some(HookEvent::ChatMessage),
        "tool.execute.before" => Some(HookEvent::ToolExecuteBefore),
        "tool.execute.after" => Some(HookEvent::ToolExecuteAfter),
        "tool.definition" => Some(HookEvent::ToolDefinition),
        "permission.ask" => Some(HookEvent::PermissionAsk),
        "command.execute.before" => Some(HookEvent::CommandExecuteBefore),
        "shell.env" => Some(HookEvent::ShellEnv),
        "experimental.chat.system.transform" => Some(HookEvent::ChatSystemTransform),
        "experimental.chat.messages.transform" => Some(HookEvent::ChatMessagesTransform),
        "experimental.session.compacting" => Some(HookEvent::SessionCompacting),
        "experimental.text.complete" => Some(HookEvent::TextComplete),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_supported_hook_names() {
        let cases = [
            ("chat.headers", HookEvent::ChatHeaders),
            ("chat.params", HookEvent::ChatParams),
            ("chat.message", HookEvent::ChatMessage),
            ("tool.execute.before", HookEvent::ToolExecuteBefore),
            ("tool.execute.after", HookEvent::ToolExecuteAfter),
            ("tool.definition", HookEvent::ToolDefinition),
            ("permission.ask", HookEvent::PermissionAsk),
            ("command.execute.before", HookEvent::CommandExecuteBefore),
            ("shell.env", HookEvent::ShellEnv),
            (
                "experimental.chat.system.transform",
                HookEvent::ChatSystemTransform,
            ),
            (
                "experimental.chat.messages.transform",
                HookEvent::ChatMessagesTransform,
            ),
            (
                "experimental.session.compacting",
                HookEvent::SessionCompacting,
            ),
            ("experimental.text.complete", HookEvent::TextComplete),
        ];

        // サポート対象の全マッピングを漏れなく検証していることを担保する。
        assert_eq!(cases.len(), 13);
        for (name, expected) in cases {
            assert_eq!(
                hook_name_to_event(name),
                Some(expected),
                "unexpected mapping for hook name: {name}"
            );
        }
    }

    #[test]
    fn returns_none_for_unsupported_or_malformed_names() {
        for name in [
            "",
            "unknown.hook",
            "Chat.Headers",  // 大文字小文字は区別される
            "chat.headers ", // 末尾の空白は許容されない
            " chat.headers", // 先頭の空白は許容されない
            "chat.header",   // 類似だが不正な名前
            "ToolCall",      // Rust のバリアント名は TS フック名ではない
            "session.start", // 対応する HookEvent はあるが TS フック名のマッピングはない
        ] {
            assert_eq!(
                hook_name_to_event(name),
                None,
                "unexpected mapping for hook name: {name}"
            );
        }
    }
}
