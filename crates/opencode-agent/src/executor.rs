use futures::stream::BoxStream;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{AgentInfo, Conversation, ToolCall};
use opencode_plugin::{HookContext, HookEvent};
use opencode_provider::{ChatRequest, Provider, ProviderError, ProviderRegistry, StreamEvent};
use opencode_tool::{ToolContext, ToolError, ToolRegistry};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// 型付き `ProviderError` を保持する。
    ///
    /// `String` へ平坦化すると status / headers / source チェーンが失われ、
    /// session 側の retry 判定が機能しなくなるため `agent.rs` の定義と統一する。
    #[error("Provider error: {0}")]
    ProviderError(#[from] ProviderError),

    #[error("Tool error: {0}")]
    ToolError(String),

    #[error("Max steps exceeded")]
    MaxStepsExceeded,

    #[error("No provider available")]
    NoProvider,

    #[error("Invalid response")]
    InvalidResponse,
}

pub struct AgentExecutor {
    agent: AgentInfo,
    conversation: Conversation,
    providers: Arc<ProviderRegistry>,
    tools: Arc<ToolRegistry>,
    disabled_tools: HashSet<String>,
    subsessions: Arc<Mutex<HashMap<String, SubsessionState>>>,
    max_steps: u32,
}

#[derive(Debug, Clone)]
struct SubsessionState {
    agent: AgentInfo,
    conversation: Conversation,
    disabled_tools: HashSet<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedSubsessionState {
    pub agent: AgentInfo,
    pub conversation: Conversation,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

impl AgentExecutor {
    pub fn new(
        agent: AgentInfo,
        providers: Arc<ProviderRegistry>,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        let max_steps = agent.max_steps.unwrap_or(100);
        let conversation = Conversation::new();

        Self {
            agent,
            conversation,
            providers,
            tools,
            disabled_tools: HashSet::new(),
            subsessions: Arc::new(Mutex::new(HashMap::new())),
            max_steps,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.conversation = Conversation::with_system_prompt(prompt);
        self
    }

    pub fn with_disabled_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        self.disabled_tools = tools.into_iter().collect();
        self
    }

    pub fn with_persisted_subsessions(
        mut self,
        states: HashMap<String, PersistedSubsessionState>,
    ) -> Self {
        let subsessions = states
            .into_iter()
            .map(|(id, state)| {
                (
                    id,
                    SubsessionState {
                        agent: state.agent,
                        conversation: state.conversation,
                        disabled_tools: state.disabled_tools.into_iter().collect(),
                    },
                )
            })
            .collect();
        self.subsessions = Arc::new(Mutex::new(subsessions));
        self
    }

    pub fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    pub fn conversation_mut(&mut self) -> &mut Conversation {
        &mut self.conversation
    }

    pub async fn export_subsessions(&self) -> HashMap<String, PersistedSubsessionState> {
        self.subsessions
            .lock()
            .await
            .iter()
            .map(|(id, state)| {
                (
                    id.clone(),
                    PersistedSubsessionState {
                        agent: state.agent.clone(),
                        conversation: state.conversation.clone(),
                        disabled_tools: state.disabled_tools.iter().cloned().collect(),
                    },
                )
            })
            .collect()
    }

    pub async fn execute(&mut self, user_message: impl Into<String>) -> Result<String, AgentError> {
        self.conversation.add_user_message(user_message);

        // Plugin hook: session.start
        opencode_plugin::trigger(
            HookContext::new(HookEvent::SessionStart)
                .with_data("agent", serde_json::json!(&self.agent.name))
                .with_data("max_steps", serde_json::json!(self.max_steps)),
        )
        .await;

        let mut steps = 0;
        let mut final_response = String::new();

        while steps < self.max_steps {
            steps += 1;

            let provider = self.get_provider()?;
            let model_id = self.get_model_id(&provider);

            // Plugin hook: chat.system.transform — let plugins modify system prompt per step
            opencode_plugin::trigger(
                HookContext::new(HookEvent::ChatSystemTransform)
                    .with_data("agent", serde_json::json!(&self.agent.name))
                    .with_data("model_id", serde_json::json!(&model_id))
                    .with_data("step", serde_json::json!(steps)),
            )
            .await;

            let request = ChatRequest::new(model_id, self.conversation.to_provider_messages());

            let stream = provider
                .chat_stream(request)
                .await
                .map_err(AgentError::ProviderError)?;

            let (response, reasoning, tool_calls, invalid_arguments) =
                self.process_stream(stream).await?;

            if tool_calls.is_empty() {
                final_response = response.clone();
                self.conversation
                    .add_assistant_message(response, Some(reasoning.clone()));
                break;
            }

            self.conversation.add_assistant_message_with_tools(
                response,
                Some(reasoning),
                tool_calls.clone(),
            );

            for tool_call in tool_calls {
                let (content, is_error) = self
                    .run_tool_call(&tool_call, invalid_arguments.get(&tool_call.id), true)
                    .await;

                self.conversation.add_tool_result(
                    &tool_call.id,
                    &tool_call.name,
                    content,
                    is_error,
                );
            }
        }

        // Plugin hook: session.end
        opencode_plugin::trigger(
            HookContext::new(HookEvent::SessionEnd)
                .with_data("agent", serde_json::json!(&self.agent.name))
                .with_data("steps", serde_json::json!(steps)),
        )
        .await;

        if steps >= self.max_steps {
            return Err(AgentError::MaxStepsExceeded);
        }

        Ok(final_response)
    }

    async fn execute_subsession(
        &mut self,
        user_message: impl Into<String>,
    ) -> Result<String, AgentError> {
        self.conversation.add_user_message(user_message);

        let mut steps = 0;
        let mut final_response = String::new();

        while steps < self.max_steps {
            steps += 1;

            let provider = self.get_provider()?;
            let model_id = self.get_model_id(&provider);
            let request = ChatRequest::new(model_id, self.conversation.to_provider_messages());

            let stream = provider
                .chat_stream(request)
                .await
                .map_err(AgentError::ProviderError)?;

            let (response, reasoning, tool_calls, invalid_arguments) =
                self.process_stream(stream).await?;

            if tool_calls.is_empty() {
                final_response = response.clone();
                self.conversation
                    .add_assistant_message(response, Some(reasoning.clone()));
                break;
            }

            self.conversation.add_assistant_message_with_tools(
                response,
                Some(reasoning),
                tool_calls.clone(),
            );

            for tool_call in tool_calls {
                let (content, is_error) = self
                    .run_tool_call(&tool_call, invalid_arguments.get(&tool_call.id), false)
                    .await;

                self.conversation.add_tool_result(
                    &tool_call.id,
                    &tool_call.name,
                    content,
                    is_error,
                );
            }
        }

        if steps >= self.max_steps {
            return Err(AgentError::MaxStepsExceeded);
        }

        Ok(final_response)
    }

    pub async fn execute_streaming(
        &mut self,
        user_message: String,
    ) -> Result<BoxStream<'static, Result<StreamEvent, AgentError>>, AgentError> {
        self.conversation.add_user_message(user_message);

        let provider = self.get_provider()?;
        let model_id = self.get_model_id(&provider);
        let request = ChatRequest::new(model_id, self.conversation.to_provider_messages());

        let stream = provider
            .chat_stream(request)
            .await
            .map_err(AgentError::ProviderError)?;

        Ok(stream.map(|r| r.map_err(AgentError::ProviderError)).boxed())
    }

    fn get_provider(&self) -> Result<Arc<dyn Provider>, AgentError> {
        if let Some(ref model_ref) = self.agent.model {
            self.providers
                .get(&model_ref.provider_id)
                .ok_or(AgentError::NoProvider)
        } else {
            let providers = self.providers.list();
            if providers.is_empty() {
                return Err(AgentError::NoProvider);
            }
            Ok(providers.into_iter().next().unwrap())
        }
    }

    fn get_model_id(&self, provider: &Arc<dyn Provider>) -> String {
        if let Some(ref model_ref) = self.agent.model {
            model_ref.model_id.clone()
        } else {
            let models = provider.models();
            models.first().map(|m| m.id.clone()).unwrap_or_default()
        }
    }

    /// 引数 JSON のパース失敗を表す `ToolError` を生成する。
    ///
    /// 生の引数文字列はパスや秘密情報を含む可能性があるため、エラーには含めない。
    fn invalid_arguments_error(tool_name: &str) -> ToolError {
        ToolError::InvalidArguments(format!(
            "Tool '{tool_name}' received arguments that are not valid JSON; retry with a well-formed JSON object"
        ))
    }

    /// 単一のツール呼び出しを実行し、LLM へ返す (content, is_error) を求める。
    ///
    /// 引数パース失敗済みの呼び出しは実行せず、`InvalidArguments` エラーを
    /// エラー ToolResult として記録し、LLM に修正を促す。
    async fn run_tool_call(
        &self,
        tool_call: &ToolCall,
        invalid: Option<&ToolError>,
        allow_subsessions: bool,
    ) -> (String, bool) {
        if let Some(error) = invalid {
            return (error.to_string(), true);
        }
        let result = if allow_subsessions {
            self.execute_tool(tool_call).await
        } else {
            self.execute_tool_without_subsessions(tool_call).await
        };
        match result {
            Ok(output) => (output, false),
            Err(e) => (e.to_string(), true),
        }
    }

    async fn process_stream(
        &mut self,
        mut stream: opencode_provider::StreamResult,
    ) -> Result<(String, String, Vec<ToolCall>, HashMap<String, ToolError>), AgentError> {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut tool_args_buffers: HashMap<String, String> = HashMap::new();
        let mut invalid_arguments: HashMap<String, ToolError> = HashMap::new();

        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::TextDelta(text)) => {
                    content.push_str(&text);
                }
                Ok(StreamEvent::ReasoningDelta { text, .. }) => {
                    reasoning.push_str(&text);
                }
                Ok(StreamEvent::ToolCallStart { id, name }) => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        name,
                        arguments: serde_json::Value::Null,
                    });
                    tool_args_buffers.insert(id, String::new());
                }
                Ok(StreamEvent::ToolCallDelta { id, input }) => {
                    if let Some(buf) = tool_args_buffers.get_mut(&id) {
                        buf.push_str(&input);
                    }
                }
                Ok(StreamEvent::ToolCallEnd { id, name, input: _ }) => {
                    if let Some(args_str) = tool_args_buffers.remove(&id) {
                        if let Some(tc) = tool_calls.iter_mut().find(|t| t.id == id) {
                            // 空文字は「引数なし」として扱い、それ以外の不正 JSON は
                            // Null をツールへ渡さず InvalidArguments として記録する。
                            if args_str.trim().is_empty() {
                                tc.arguments = serde_json::json!({});
                            } else {
                                match serde_json::from_str::<serde_json::Value>(&args_str) {
                                    Ok(args) => tc.arguments = args,
                                    Err(_) => {
                                        invalid_arguments.insert(
                                            id.clone(),
                                            Self::invalid_arguments_error(&name),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(StreamEvent::Done) => break,
                Ok(StreamEvent::Error(e)) => {
                    // StreamEvent::Error は文字列ペイロードのため型情報は無いが、
                    // AgentError 側を構造化したままにするため ProviderError で包む。
                    return Err(AgentError::ProviderError(ProviderError::ApiError(e)));
                }
                Err(e) => {
                    return Err(AgentError::ProviderError(e));
                }
                _ => {}
            }
        }

        Ok((content, reasoning, tool_calls, invalid_arguments))
    }

    async fn execute_tool(&self, tool_call: &ToolCall) -> Result<String, ToolError> {
        if self.disabled_tools.contains(&tool_call.name) {
            return Err(ToolError::PermissionDenied(format!(
                "Tool '{}' is disabled for this subagent session",
                tool_call.name
            )));
        }
        let target = permission_target_from_args(&tool_call.name, &tool_call.arguments);
        self.ensure_tool_allowed(&tool_call.name, &target)?;

        let directory = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let current_model = self.current_model_string();
        let base_ctx = ToolContext::new("default".to_string(), "default".to_string(), directory)
            .with_agent(self.agent.name.clone())
            .with_get_last_model({
                let current_model = current_model.clone();
                move |_session_id| {
                    let current_model = current_model.clone();
                    async move { Ok(current_model) }
                }
            });
        let ctx = self.with_subsession_callbacks(base_ctx);

        self.tools
            .execute(&tool_call.name, tool_call.arguments.clone(), ctx)
            .await
            .map(|r| r.output)
    }

    async fn execute_tool_without_subsessions(
        &self,
        tool_call: &ToolCall,
    ) -> Result<String, ToolError> {
        if self.disabled_tools.contains(&tool_call.name) {
            return Err(ToolError::PermissionDenied(format!(
                "Tool '{}' is disabled for this subagent session",
                tool_call.name
            )));
        }
        let target = permission_target_from_args(&tool_call.name, &tool_call.arguments);
        self.ensure_tool_allowed(&tool_call.name, &target)?;

        let directory = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let current_model = self.current_model_string();
        let base_ctx = ToolContext::new("default".to_string(), "default".to_string(), directory)
            .with_agent(self.agent.name.clone())
            .with_get_last_model({
                let current_model = current_model.clone();
                move |_session_id| {
                    let current_model = current_model.clone();
                    async move { Ok(current_model) }
                }
            });
        let ctx = self.with_subsession_callbacks(base_ctx);

        self.tools
            .execute(&tool_call.name, tool_call.arguments.clone(), ctx)
            .await
            .map(|r| r.output)
    }

    fn with_subsession_callbacks(&self, ctx: ToolContext) -> ToolContext {
        let subsessions = self.subsessions.clone();
        let providers = self.providers.clone();
        let tools = self.tools.clone();
        // 親セッションの無効ツールを子セッションへ継承するためのスナップショット。
        let parent_disabled_tools = self.disabled_tools.clone();

        ctx.with_create_subsession({
            let subsessions = subsessions.clone();
            let parent_disabled_tools = parent_disabled_tools.clone();
            move |agent_name, _title, model, disabled_tools| {
                let subsessions = subsessions.clone();
                // ツール側指定と親の制限の union を取り、親の制限が子で緩和されないようにする。
                // コールバックは Fn なので、捕捉した親 set は呼び出しごとに clone して borrow する。
                let merged_disabled_tools: HashSet<String> = disabled_tools
                    .into_iter()
                    .chain(parent_disabled_tools.iter().cloned())
                    .collect();
                async move {
                    let cwd = std::env::current_dir().unwrap_or_default();
                    let registry = crate::AgentRegistry::from_project_dir(&cwd);
                    let mut agent = registry.get(&agent_name).cloned().ok_or_else(|| {
                        ToolError::InvalidArguments(format!(
                            "Unknown agent type: {} is not a valid agent type",
                            agent_name
                        ))
                    })?;

                    if let Some((provider_id, model_id)) = parse_model_string(model.as_deref()) {
                        agent = agent.with_model(model_id, provider_id);
                    }

                    let conversation = if let Some(system_prompt) = &agent.system_prompt {
                        Conversation::with_system_prompt(system_prompt.clone())
                    } else {
                        Conversation::new()
                    };

                    let session_id =
                        format!("task_{}_{}", agent_name, uuid::Uuid::new_v4().simple());
                    let mut store = subsessions.lock().await;
                    store.insert(
                        session_id.clone(),
                        SubsessionState {
                            agent,
                            conversation,
                            disabled_tools: merged_disabled_tools,
                        },
                    );
                    Ok(session_id)
                }
            }
        })
        .with_prompt_subsession({
            let subsessions = subsessions.clone();
            let providers = providers.clone();
            let tools = tools.clone();
            move |session_id, prompt| {
                let subsessions = subsessions.clone();
                let providers = providers.clone();
                let tools = tools.clone();
                async move {
                    let state = {
                        let store = subsessions.lock().await;
                        store.get(&session_id).cloned()
                    }
                    .ok_or_else(|| {
                        ToolError::ExecutionError(format!(
                            "Unknown subagent session: {}. Start without task_id first.",
                            session_id
                        ))
                    })?;

                    let mut executor =
                        AgentExecutor::new(state.agent, providers.clone(), tools.clone())
                            .with_disabled_tools(state.disabled_tools.iter().cloned());
                    executor.conversation = state.conversation;

                    let output = executor.execute_subsession(prompt).await.map_err(|e| {
                        ToolError::ExecutionError(format!("Subagent execution failed: {}", e))
                    })?;

                    let mut store = subsessions.lock().await;
                    if let Some(state) = store.get_mut(&session_id) {
                        state.conversation = executor.conversation.clone();
                    }

                    Ok(output)
                }
            }
        })
    }

    fn current_model_string(&self) -> Option<String> {
        if let Some(model) = self.agent.model.as_ref() {
            return Some(format!("{}:{}", model.provider_id, model.model_id));
        }

        let provider = self.get_provider().ok()?;
        let model_id = self.get_model_id(&provider);
        if model_id.is_empty() {
            return None;
        }
        Some(format!("{}:{}", provider.id(), model_id))
    }

    fn ensure_tool_allowed(&self, tool_name: &str, pattern: &str) -> Result<(), ToolError> {
        match self.agent.tool_permission_decision(tool_name, pattern) {
            crate::PermissionDecision::Allow => Ok(()),
            crate::PermissionDecision::Ask => Err(ToolError::PermissionDenied(format!(
                "Tool '{}' requires explicit approval for agent '{}'",
                tool_name, self.agent.name
            ))),
            crate::PermissionDecision::Deny => Err(ToolError::PermissionDenied(format!(
                "Tool '{}' is denied by agent '{}' permission rules",
                tool_name, self.agent.name
            ))),
        }
    }
}

fn parse_model_string(raw: Option<&str>) -> Option<(String, String)> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }

    let (provider, model) = raw.split_once(':').or_else(|| raw.split_once('/'))?;

    if provider.is_empty() || model.is_empty() {
        return None;
    }

    Some((provider.to_string(), model.to_string()))
}

/// Shell metacharacters that make a command string unsafe for literal
/// permission-pattern matching (chaining, redirection, expansion, substitution).
const UNSAFE_COMMAND_CHARS: &[char] = &[
    '&', ';', '|', '>', '<', '$', '`', '\n', '\r', '\0', '(', ')', '{', '}', '\\',
];

/// Resolve `.` / `..` lexically and express the result as a project-relative
/// path with `/` separators.
///
/// Returns `None` when the path escapes the project root (or cannot be made
/// relative to it), so the caller can fall back to `"*"` instead of matching an
/// allowlist prefix such as `src/*` against a path that actually writes outside.
fn normalized_file_pattern(raw: &str) -> Option<String> {
    let path = std::path::Path::new(raw);
    let relative = if path.is_absolute() {
        let root = std::env::current_dir().ok()?;
        path.strip_prefix(&root).ok()?.to_path_buf()
    } else {
        path.to_path_buf()
    };

    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().into_owned());
            }
            // `None` means the path traverses above the project root.
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            // CurDir / RootDir / Prefix carry no target information here.
            _ => {}
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Extract the value a tool acts on, used as the target of permission matching.
///
/// Tool argument names differ by tool: edit-like tools and `read` use
/// `file_path`, `bash` uses `command`, and the search/list tools use `path`.
/// File paths are normalized before matching, and bash commands containing
/// shell metacharacters are treated as unmatchable. In every unmatchable case
/// `"*"` is returned so wildcard rules keep their previous behaviour while
/// specific allowlist rules cannot be bypassed (fail-safe fallback).
fn permission_target_from_args(tool_name: &str, args: &serde_json::Value) -> String {
    let key = match tool_name {
        "write" | "edit" | "multiedit" | "patch" | "apply_patch" | "read" => "file_path",
        "bash" => "command",
        "grep" | "glob" | "ls" => "path",
        _ => return "*".to_string(),
    };

    let Some(raw) = args.get(key).and_then(serde_json::Value::as_str) else {
        return "*".to_string();
    };

    match key {
        "command" => {
            if raw.chars().any(|c| UNSAFE_COMMAND_CHARS.contains(&c)) {
                "*".to_string()
            } else {
                raw.to_string()
            }
        }
        "file_path" | "path" => normalized_file_pattern(raw).unwrap_or_else(|| "*".to_string()),
        _ => "*".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencode_permission::{PermissionAction, PermissionRule};

    fn build_executor(agent: AgentInfo) -> AgentExecutor {
        AgentExecutor::new(
            agent,
            Arc::new(ProviderRegistry::new()),
            Arc::new(ToolRegistry::new()),
        )
    }

    #[tokio::test]
    async fn persisted_subsessions_roundtrip() {
        let mut conversation = Conversation::with_system_prompt("subagent prompt");
        conversation.add_user_message("inspect project");
        conversation.add_assistant_message("working on it", None);

        let mut persisted = HashMap::new();
        persisted.insert(
            "task_explore_1".to_string(),
            PersistedSubsessionState {
                agent: AgentInfo::explore().with_model("gpt-4.1-mini", "openai"),
                conversation: conversation.clone(),
                disabled_tools: vec!["write".to_string(), "edit".to_string()],
            },
        );

        let executor = build_executor(AgentInfo::general()).with_persisted_subsessions(persisted);
        let exported = executor.export_subsessions().await;
        let state = exported
            .get("task_explore_1")
            .expect("expected persisted subsession");

        assert_eq!(state.agent.name, "explore");
        assert_eq!(
            state.conversation.messages.len(),
            conversation.messages.len()
        );

        let mut disabled = state.disabled_tools.clone();
        disabled.sort();
        assert_eq!(disabled, vec!["edit".to_string(), "write".to_string()]);
    }

    #[tokio::test]
    async fn create_subsession_merges_parent_disabled_tools() {
        let executor = build_executor(AgentInfo::general())
            .with_disabled_tools(["write".to_string(), "bash".to_string()]);
        let ctx = executor.with_subsession_callbacks(ToolContext::new(
            "session".to_string(),
            "message".to_string(),
            ".".to_string(),
        ));

        let session_id = ctx
            .do_create_subsession(
                "general".to_string(),
                None,
                None,
                vec!["edit".to_string(), "bash".to_string()],
            )
            .await
            .expect("create subsession should succeed");

        let exported = executor.export_subsessions().await;
        let state = exported
            .get(&session_id)
            .expect("expected persisted subsession");
        let mut disabled: Vec<String> = state.disabled_tools.to_vec();
        disabled.sort();
        assert_eq!(
            disabled,
            vec!["bash".to_string(), "edit".to_string(), "write".to_string()]
        );
    }

    #[test]
    fn executor_enforces_explore_allowlist() {
        let executor = build_executor(AgentInfo::explore());

        assert!(executor.ensure_tool_allowed("grep", "*").is_ok());

        let denied = executor
            .ensure_tool_allowed("write", "*")
            .expect_err("write should be denied for explore");
        assert!(
            matches!(denied, ToolError::PermissionDenied(_)),
            "expected permission denied, got: {denied}"
        );
    }

    #[test]
    fn executor_blocks_ask_permissions_without_user_approval() {
        let agent = AgentInfo::custom("review").with_permission(vec![PermissionRule {
            permission: "bash".to_string(),
            pattern: "*".to_string(),
            action: PermissionAction::Ask,
        }]);
        let executor = build_executor(agent);

        let denied = executor
            .ensure_tool_allowed("bash", "*")
            .expect_err("ask should block direct execution");
        assert!(
            matches!(denied, ToolError::PermissionDenied(_)),
            "expected permission denied, got: {denied}"
        );
    }

    #[test]
    fn permission_target_extraction_uses_tool_specific_keys() {
        assert_eq!(
            permission_target_from_args(
                "edit",
                &serde_json::json!({"file_path": "src/main.rs", "old_string": "x"})
            ),
            "src/main.rs"
        );
        assert_eq!(
            permission_target_from_args("write", &serde_json::json!({"file_path": "docs/a.md"})),
            "docs/a.md"
        );
        assert_eq!(
            permission_target_from_args("read", &serde_json::json!({"file_path": "a.env"})),
            "a.env"
        );
        assert_eq!(
            permission_target_from_args("bash", &serde_json::json!({"command": "git status"})),
            "git status"
        );
        assert_eq!(
            permission_target_from_args("grep", &serde_json::json!({"path": "src"})),
            "src"
        );

        // Unavailable or non-string targets fall back to the wildcard.
        assert_eq!(
            permission_target_from_args("bash", &serde_json::json!({})),
            "*"
        );
        assert_eq!(
            permission_target_from_args("edit", &serde_json::json!({"file_path": 42})),
            "*"
        );
        assert_eq!(
            permission_target_from_args("webfetch", &serde_json::json!({"url": "https://x"})),
            "*"
        );
    }

    #[test]
    fn executor_enforces_pattern_specific_rules_from_args() {
        let agent = AgentInfo::custom("review").with_permission(vec![
            PermissionRule {
                permission: "edit".to_string(),
                pattern: "*".to_string(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "edit".to_string(),
                pattern: "src/*".to_string(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "bash".to_string(),
                pattern: "*".to_string(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "bash".to_string(),
                pattern: "git*".to_string(),
                action: PermissionAction::Allow,
            },
        ]);
        let executor = build_executor(agent);

        let args = serde_json::json!({"file_path": "src/lib.rs"});
        let target = permission_target_from_args("edit", &args);
        assert!(executor.ensure_tool_allowed("edit", &target).is_ok());

        let args = serde_json::json!({"file_path": "docs/lib.rs"});
        let target = permission_target_from_args("edit", &args);
        assert!(executor.ensure_tool_allowed("edit", &target).is_err());

        let args = serde_json::json!({"command": "git status"});
        let target = permission_target_from_args("bash", &args);
        assert!(executor.ensure_tool_allowed("bash", &target).is_ok());

        let args = serde_json::json!({"command": "curl https://example.com"});
        let target = permission_target_from_args("bash", &args);
        assert!(executor.ensure_tool_allowed("bash", &target).is_err());
    }

    #[test]
    fn bash_commands_with_shell_metacharacters_fall_back_to_wildcard() {
        for command in [
            "git status && curl http://evil",
            "git status; curl http://evil",
            "git status | tee /tmp/out",
            "git status > /tmp/out",
            "git status < /tmp/in",
            "echo $HOME",
            "echo `id`",
            "git status\ncurl http://evil",
            "git status || curl http://evil",
            "git status $(id)",
        ] {
            assert_eq!(
                permission_target_from_args("bash", &serde_json::json!({"command": command})),
                "*",
                "command with shell metacharacters must not be pattern-matched: {command}"
            );
        }

        // A plain command is still passed through for literal pattern matching.
        assert_eq!(
            permission_target_from_args("bash", &serde_json::json!({"command": "git status"})),
            "git status"
        );
    }

    #[test]
    fn executor_rejects_bash_metacharacter_bypass_of_git_allowlist() {
        let agent = AgentInfo::custom("review").with_permission(vec![
            PermissionRule {
                permission: "bash".to_string(),
                pattern: "*".to_string(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "bash".to_string(),
                pattern: "git*".to_string(),
                action: PermissionAction::Allow,
            },
        ]);
        let executor = build_executor(agent);

        let args = serde_json::json!({"command": "git status && curl http://evil"});
        let target = permission_target_from_args("bash", &args);
        assert_eq!(target, "*");
        assert!(
            executor.ensure_tool_allowed("bash", &target).is_err(),
            "chained command must not inherit the git* allow rule"
        );
    }

    #[test]
    fn file_paths_are_normalized_before_permission_matching() {
        assert_eq!(
            permission_target_from_args(
                "edit",
                &serde_json::json!({"file_path": "src/../../etc/passwd"})
            ),
            "*",
            "a path that escapes the project root must fall back to the wildcard"
        );
        assert_eq!(
            permission_target_from_args(
                "write",
                &serde_json::json!({"file_path": "src/../lib.rs"})
            ),
            "lib.rs",
            "in-project .. segments must be resolved before matching"
        );
        assert_eq!(
            permission_target_from_args("read", &serde_json::json!({"file_path": "./src/main.rs"})),
            "src/main.rs"
        );

        let cwd = std::env::current_dir().expect("current dir should be available");
        let inside = cwd.join("src/lib.rs").to_string_lossy().into_owned();
        assert_eq!(
            permission_target_from_args("edit", &serde_json::json!({"file_path": inside})),
            "src/lib.rs",
            "absolute paths under the project root become project-relative"
        );

        let outside = cwd
            .parent()
            .expect("current dir should have a parent")
            .join("outside-project.txt")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            permission_target_from_args("edit", &serde_json::json!({"file_path": outside})),
            "*",
            "absolute paths outside the project root must fall back to the wildcard"
        );
    }

    #[test]
    fn executor_rejects_traversal_paths_outside_allowlist() {
        let agent = AgentInfo::custom("review").with_permission(vec![
            PermissionRule {
                permission: "edit".to_string(),
                pattern: "*".to_string(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "edit".to_string(),
                pattern: "src/*".to_string(),
                action: PermissionAction::Allow,
            },
        ]);
        let executor = build_executor(agent);

        let args = serde_json::json!({"file_path": "src/../../etc/passwd"});
        let target = permission_target_from_args("edit", &args);
        assert_eq!(target, "*");
        assert!(
            executor.ensure_tool_allowed("edit", &target).is_err(),
            "a path escaping the project root must not inherit the src/* allow rule"
        );
    }

    /// 不正な引数 JSON は Null に丸められず InvalidArguments として扱われること。
    #[tokio::test]
    async fn process_stream_marks_invalid_tool_arguments() {
        let events = vec![
            Ok::<StreamEvent, opencode_provider::ProviderError>(StreamEvent::ToolCallStart {
                id: "bad".to_string(),
                name: "read".to_string(),
            }),
            Ok(StreamEvent::ToolCallDelta {
                id: "bad".to_string(),
                input: "{invalid /secret/path}".to_string(),
            }),
            Ok(StreamEvent::ToolCallEnd {
                id: "bad".to_string(),
                name: "read".to_string(),
                input: serde_json::json!("{invalid /secret/path}"),
            }),
            Ok(StreamEvent::ToolCallStart {
                id: "good".to_string(),
                name: "write".to_string(),
            }),
            Ok(StreamEvent::ToolCallDelta {
                id: "good".to_string(),
                input: "{\"file_path\":\"a.txt\"}".to_string(),
            }),
            Ok(StreamEvent::ToolCallEnd {
                id: "good".to_string(),
                name: "write".to_string(),
                input: serde_json::json!({"file_path": "a.txt"}),
            }),
            Ok(StreamEvent::Done),
        ];
        let stream: opencode_provider::StreamResult = Box::pin(futures::stream::iter(events));
        let mut executor = build_executor(AgentInfo::general());

        let (_, _, tool_calls, invalid) = executor
            .process_stream(stream)
            .await
            .expect("stream processing should succeed");

        assert_eq!(tool_calls.len(), 2);
        assert!(
            matches!(
                invalid.get("bad"),
                Some(error) if matches!(error, ToolError::InvalidArguments(_))
            ),
            "malformed JSON must become InvalidArguments"
        );
        assert!(!invalid.contains_key("good"));
        assert_eq!(
            tool_calls[1].arguments,
            serde_json::json!({"file_path": "a.txt"})
        );
        let rendered = invalid.get("bad").expect("error").to_string();
        assert!(
            rendered.contains("Invalid arguments"),
            "LLM needs a correction hint: {rendered}"
        );
        assert!(
            !rendered.contains("/secret/path"),
            "raw arguments must not leak into the error"
        );
    }

    /// InvalidArguments はツールを実行せず、エラー ToolResult として記録されること。
    #[tokio::test]
    async fn invalid_arguments_are_recorded_as_error_tool_result() {
        let mut executor = build_executor(AgentInfo::general());
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            name: "read".to_string(),
            arguments: serde_json::Value::Null,
        };
        let error = AgentExecutor::invalid_arguments_error("read");

        let (content, is_error) = executor.run_tool_call(&tool_call, Some(&error), true).await;
        executor
            .conversation
            .add_tool_result(&tool_call.id, &tool_call.name, &content, is_error);

        let recorded = executor
            .conversation
            .messages
            .last()
            .expect("tool result must be recorded");
        assert!(
            is_error,
            "invalid arguments must be reported as a tool error"
        );
        assert!(matches!(recorded.role, crate::MessageRole::Tool));
        assert!(recorded.is_error);
        assert!(recorded.content.contains("Invalid arguments"));
        assert!(!recorded.content.contains("/secret/path"));
    }

    /// 回帰ガード: AgentError が型付き ProviderError を保持し、
    /// retry 判定・status・source チェーンが失われないこと。
    #[test]
    fn agent_error_preserves_structured_provider_error() {
        use opencode_provider::retry::IsRetryable;

        let error: AgentError =
            ProviderError::api_error_from_status(503, "Service Unavailable").into();
        match &error {
            AgentError::ProviderError(ProviderError::ApiErrorWithStatus {
                status_code, ..
            }) => {
                assert_eq!(*status_code, 503);
            }
            other => panic!("expected structured provider error, got {other:?}"),
        }
        if let AgentError::ProviderError(inner) = error {
            assert_eq!(inner.status_code(), Some(503));
            assert!(
                inner.is_retryable().is_some(),
                "retry decision material must survive the AgentError boundary"
            );
        }

        let with_source: AgentError = ProviderError::network_error_with_source(
            "connection reset",
            std::io::Error::other("underlying cause"),
        )
        .into();
        match &with_source {
            AgentError::ProviderError(ProviderError::NetworkErrorWithSource { source, .. }) => {
                assert!(source.downcast_ref::<std::io::Error>().is_some());
            }
            other => panic!("expected source-preserving provider error, got {other:?}"),
        }
    }
}
