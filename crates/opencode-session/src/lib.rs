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

// crate 直下の再エクスポートは明示リストで統一する。
// 同名アイテムは一意に選び、他方は module 経由で参照する:
//   - ModelRef / CompactionPart: message_v2 を直下に公開（prompt / compaction 版は module 経由）
//   - FileDiff / SessionSummary: session を直下に公開（message_v2 / revert / summary 版は module 経由）
//   - to_model_messages: llm を直下に公開（message_v2 版は module 経由）
pub use compaction::{
    can_compact_messages, estimate_message_tokens, generate_continue_message,
    messages_to_prune_format, run_compaction, CompactionConfig, CompactionEngine, CompactionInput,
    CompactionResult, CompactionSummary, CreateCompactionInput, MessageForPrune, ModelLimits,
    PruneToolPart, SessionOps, TokenUsage, ToolPartStatus,
};
pub use instruction::{
    resolve_agents_for_file, resolve_for_file, InstructionFile, InstructionLoader,
    InstructionSource, AGENTS_MD, CLAUDE_MD, CONTEXT_MD, COPILOT_MD, CURSOR_MD,
};
pub use llm::{
    collect_stream, inject_litellm_dummy_tool, notify_text_complete, repair_tool_call,
    resolve_tools_with_permissions, to_model_messages, CostRateTier, CostRates, LlmAgent,
    LlmModelRef, LlmProcessor, ProcessResult, ProcessorEvent, StreamInput, StreamOutput,
    StreamProcessor, StreamResult, ToolCallResult, ToolCallState, ToolChoice, OUTPUT_TOKEN_MAX,
};
pub use message::{MessagePart, MessageRole, MessageUsage, PartType, SessionMessage};
pub use message_v2::{
    error_from_anyhow, filter_compacted, AgentPart, AgentSource, ApiError, AssistantTime,
    AssistantTokens, CacheTokens, CompactionPart, CompletedTime, ErrorTime, FilePart,
    FilePartSource, FileSourceText, LspPosition, LspRange, MessageError, MessageEvent, MessageInfo,
    MessagePath, MessageWithParts, ModelContext, ModelRef, OutputFormat, Part, PartDelta,
    ReasoningTime, RetryPart, RetryTime, RunningTime, StepFinishPart, StepStartPart, StepTokens,
    SubtaskPart, TextTime, ToolPart, ToolState, UserSummary, UserTime,
};
pub use prompt::{
    create_structured_output_tool, extract_file_references, extract_structured_output,
    generate_session_title, generate_session_title_llm, insert_reminders, max_steps_for_agent,
    merge_tool_definitions, resolve_command_template, resolve_prompt_parts, resolve_tools,
    resolve_tools_with_mcp, resolve_tools_with_mcp_registry, shell_exec, should_compact,
    structured_output_system_prompt, tool_definitions_from_schemas, trigger_compaction,
    was_plan_agent, AgentParams, CommandInput, PartInput, PermissionCallback, PromptError,
    PromptInput, QuestionCallback, ResolvedTool, SessionPrompt, SessionUpdateHook, ShellInput,
    StructuredOutputConfig, SubtaskExecutor, ToolSchema,
};
pub use retry::{
    delay, retryable, sleep_with_cancel, ApiErrorInfo, RetryState, SleepCancelled,
    RETRY_BACKOFF_FACTOR, RETRY_INITIAL_DELAY, RETRY_MAX_DELAY, RETRY_MAX_DELAY_NO_HEADERS,
};
pub use revert::{
    cleanup, revert, unrevert, RevertInfo, RevertInput, RevertManager, RevertSummary,
};
pub use session::{
    BusyError, DynamicPermissionAllow, FileDiff, PermissionRuleset, RunStatus, Session,
    SessionError, SessionEvent, SessionFilter, SessionManager, SessionRevert, SessionRow,
    SessionShare, SessionStateEvent, SessionStateManager, SessionStatus, SessionSummary,
    SessionTime, SessionUsage,
};
pub use status::{SessionStatusInfo, SessionStatusManager};
pub use summary::{
    clean_diffs, compute_diff, generate_title_from_messages, load_session_diffs,
    persist_session_diffs, set_session_summary, summarize, summarize_into_session,
    summarize_message, summarize_message_for_session, unquote_git_path, SessionSummaryData,
    SummarizeInput, SummaryFileDiff, TitleGenerationRequest, TitleGenerationResponse,
};
pub use system::{EnvironmentContext, SystemPrompt};
pub use todo::{parse_priority, parse_status, TodoInfo, TodoManager, TodoPriority, TodoStatus};
