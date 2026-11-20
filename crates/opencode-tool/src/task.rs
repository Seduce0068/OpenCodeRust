use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Metadata, PermissionRequest, Tool, ToolContext, ToolError, ToolResult};

/// `task` ツールの再帰上限。子サブセッションがさらに `task` を呼べるため、
/// 深さを追跡してこの値に達したら拒否する。
pub const MAX_SUBAGENT_DEPTH: u32 = 3;

/// `ToolContext::extra` に現在の深さを格納するキー。
pub const SUBAGENT_DEPTH_KEY: &str = "subagent_depth";

/// コンテキストに記録された現在のサブエージェント深さを返す。
///
/// キーが無い場合はルート（0）として扱う。値が不正な場合も推測せず 0 とみなす
/// （深さは信用できる呼び出し元が設定する値であり、ユーザー入力ではない）。
fn subagent_depth(ctx: &ToolContext) -> u32 {
    ctx.extra
        .get(SUBAGENT_DEPTH_KEY)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0)
}

/// 子サブセッションへ深さを伝播する。
///
/// 子の `ToolContext` は実行側（opencode-agent / opencode-session）で作り直され
/// `extra` が引き継がれないため、実効的な伝播は `disabled_tools` 経由で行う。
/// 上限に達した深さの子には `task` を渡さず、再帰をここで断つ。
fn propagate_depth_to_disabled_tools(disabled_tools: &mut Vec<String>, child_depth: u32) {
    if child_depth >= MAX_SUBAGENT_DEPTH && !disabled_tools.iter().any(|tool| tool == "task") {
        disabled_tools.push("task".to_string());
    }
}

pub struct TaskTool;

impl TaskTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskInput {
    description: String,
    prompt: String,
    subagent_type: String,
    task_id: Option<String>,
    command: Option<String>,
    load_skills: Option<Vec<String>>,
    #[serde(default)]
    run_in_background: bool,
}

#[async_trait]
impl Tool for TaskTool {
    fn id(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Launch a specialized subagent to handle a complex task. Use this to delegate tasks that require specialized expertise or multi-step reasoning."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "The type of specialized agent to use for this task (e.g., 'explore', 'librarian', 'oracle')"
                },
                "description": {
                    "type": "string",
                    "description": "A short (3-5 words) description of the task"
                },
                "prompt": {
                    "type": "string",
                    "description": "The task for the agent to perform"
                },
                "task_id": {
                    "type": "string",
                    "description": "Resume a previous task by passing its task_id"
                },
                "command": {
                    "type": "string",
                    "description": "The command that triggered this task (optional)"
                },
                "load_skills": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Skills to load for the sub-agent (optional)"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the task in background (default: false)"
                }
            },
            "required": ["subagent_type", "description", "prompt"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: TaskInput =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        // `bypassAgentCheck` の有無にかかわらず、深さ上限は常に適用する。
        // エラー文言には深さの値・トークン値・パスを一切含めない。
        let depth = subagent_depth(&ctx);
        if depth >= MAX_SUBAGENT_DEPTH {
            return Err(ToolError::PermissionDenied(
                "subagent depth limit exceeded".to_string(),
            ));
        }

        let bypass_check = ctx
            .extra
            .get("bypassAgentCheck")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !bypass_check {
            ctx.ask_permission(
                PermissionRequest::new("task")
                    .with_pattern(&input.subagent_type)
                    .with_metadata("description", serde_json::json!(&input.description))
                    .with_metadata("subagent_type", serde_json::json!(&input.subagent_type))
                    .always_allow(),
            )
            .await?;
        }

        let agent = get_agent(&input.subagent_type);
        let mut disabled_tools = get_disabled_tools(agent.as_ref(), input.load_skills.as_ref());
        propagate_depth_to_disabled_tools(&mut disabled_tools, depth.saturating_add(1));
        let preferred_model = if let Some(model) = agent.as_ref().and_then(|a| {
            a.model
                .as_ref()
                .map(|m| format!("{}:{}", m.provider_id, m.model_id))
        }) {
            Some(model)
        } else {
            ctx.do_get_last_model().await
        };

        let session_id = if let Some(task_id) = &input.task_id {
            task_id.clone()
        } else {
            ctx.do_create_subsession(
                input.subagent_type.clone(),
                Some(input.description.clone()),
                preferred_model.clone(),
                disabled_tools.clone(),
            )
            .await?
        };

        let title = input.description.clone();
        let result_text = ctx
            .do_prompt_subsession(session_id.clone(), input.prompt.clone())
            .await?;
        let model = parse_model_ref(preferred_model.as_deref());

        let output = format!(
            "task_id: {} (for resuming to continue this task if needed)\n\n<task_result>\n{}\n</task_result>",
            session_id,
            result_text
        );

        let mut metadata = Metadata::new();
        metadata.insert("sessionId".into(), serde_json::json!(session_id));
        metadata.insert(
            "model".into(),
            serde_json::json!({
                "modelID": model.model_id,
                "providerID": model.provider_id,
            }),
        );

        Ok(ToolResult {
            title,
            output,
            metadata,
            truncated: false,
        })
    }
}

struct AgentInfo {
    name: String,
    model: Option<AgentModel>,
    can_use_task: bool,
}

struct AgentModel {
    model_id: String,
    provider_id: String,
}

fn get_agent(name: &str) -> Option<AgentInfo> {
    let agents = get_available_agents();
    agents.into_iter().find(|a| a.name == name)
}

fn get_available_agents() -> Vec<AgentInfo> {
    vec![
        AgentInfo {
            name: "general".to_string(),
            model: None,
            can_use_task: false,
        },
        AgentInfo {
            name: "explore".to_string(),
            model: None,
            can_use_task: false,
        },
        AgentInfo {
            name: "plan".to_string(),
            model: None,
            can_use_task: false,
        },
        AgentInfo {
            name: "title".to_string(),
            model: None,
            can_use_task: false,
        },
        AgentInfo {
            name: "summary".to_string(),
            model: None,
            can_use_task: false,
        },
        AgentInfo {
            name: "compaction".to_string(),
            model: None,
            can_use_task: false,
        },
        AgentInfo {
            name: "build".to_string(),
            model: None,
            can_use_task: true,
        },
    ]
}

fn get_disabled_tools(
    agent: Option<&AgentInfo>,
    _load_skills: Option<&Vec<String>>,
) -> Vec<String> {
    let mut disabled = vec!["todowrite".to_string(), "todoread".to_string()];

    let has_task_permission = agent.map(|a| a.can_use_task).unwrap_or(false);
    if !has_task_permission {
        disabled.push("task".to_string());
    }

    disabled
}

fn parse_model_ref(raw: Option<&str>) -> AgentModel {
    let Some(raw) = raw else {
        return AgentModel {
            model_id: "default".to_string(),
            provider_id: "default".to_string(),
        };
    };

    let pair = raw.split_once(':').or_else(|| raw.split_once('/'));
    if let Some((provider, model)) = pair {
        return AgentModel {
            model_id: model.to_string(),
            provider_id: provider.to_string(),
        };
    }

    AgentModel {
        model_id: raw.to_string(),
        provider_id: "default".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn task_creates_subsession_and_prompts_it() {
        let create_calls = Arc::new(Mutex::new(Vec::<(
            String,
            Option<String>,
            Option<String>,
            Vec<String>,
        )>::new()));
        let prompt_calls = Arc::new(Mutex::new(Vec::<(String, String)>::new()));

        let ctx = ToolContext::new("session-1".into(), "message-1".into(), ".".into())
            .with_get_last_model(|_session_id| async move { Ok(Some("provider-x:model-y".into())) })
            .with_create_subsession({
                let create_calls = create_calls.clone();
                move |agent, title, model, disabled_tools| {
                    let create_calls = create_calls.clone();
                    async move {
                        create_calls
                            .lock()
                            .await
                            .push((agent, title, model, disabled_tools));
                        Ok("task_build_123".to_string())
                    }
                }
            })
            .with_prompt_subsession({
                let prompt_calls = prompt_calls.clone();
                move |session_id, prompt| {
                    let prompt_calls = prompt_calls.clone();
                    async move {
                        prompt_calls.lock().await.push((session_id, prompt));
                        Ok("subagent output".to_string())
                    }
                }
            });

        let args = serde_json::json!({
            "description": "Investigate issue",
            "prompt": "Please inspect runtime behavior",
            "subagent_type": "build"
        });

        let result = TaskTool::new().execute(args, ctx).await.unwrap();

        assert_eq!(result.title, "Investigate issue");
        assert!(result
            .output
            .contains("task_id: task_build_123 (for resuming to continue this task if needed)"));
        assert!(result
            .output
            .contains("<task_result>\nsubagent output\n</task_result>"));
        assert_eq!(
            result.metadata.get("sessionId"),
            Some(&serde_json::json!("task_build_123"))
        );
        assert_eq!(
            result.metadata.get("model"),
            Some(&serde_json::json!({
                "modelID": "model-y",
                "providerID": "provider-x"
            }))
        );

        let create_calls = create_calls.lock().await.clone();
        assert_eq!(create_calls.len(), 1);
        assert_eq!(create_calls[0].0, "build");
        assert_eq!(create_calls[0].1, Some("Investigate issue".to_string()));
        assert_eq!(create_calls[0].2, Some("provider-x:model-y".to_string()));
        assert_eq!(
            create_calls[0].3,
            vec!["todowrite".to_string(), "todoread".to_string()]
        );

        let prompt_calls = prompt_calls.lock().await.clone();
        assert_eq!(prompt_calls.len(), 1);
        assert_eq!(prompt_calls[0].0, "task_build_123");
        assert_eq!(prompt_calls[0].1, "Please inspect runtime behavior");
    }

    #[tokio::test]
    async fn task_reuses_existing_task_id_without_creating_subsession() {
        let created = Arc::new(Mutex::new(false));
        let prompted = Arc::new(Mutex::new(Vec::<(String, String)>::new()));

        let ctx = ToolContext::new("session-1".into(), "message-1".into(), ".".into())
            .with_get_last_model(|_session_id| async move { Ok(Some("provider-x:model-y".into())) })
            .with_create_subsession({
                let created = created.clone();
                move |_agent, _title, _model, _disabled_tools| {
                    let created = created.clone();
                    async move {
                        *created.lock().await = true;
                        Ok("should_not_be_used".to_string())
                    }
                }
            })
            .with_prompt_subsession({
                let prompted = prompted.clone();
                move |session_id, prompt| {
                    let prompted = prompted.clone();
                    async move {
                        prompted.lock().await.push((session_id, prompt));
                        Ok("continued output".to_string())
                    }
                }
            });

        let args = serde_json::json!({
            "description": "Continue task",
            "prompt": "Continue where you left off",
            "subagent_type": "build",
            "task_id": "task_existing_42"
        });

        let result = TaskTool::new().execute(args, ctx).await.unwrap();

        assert!(!(*created.lock().await));
        let prompted = prompted.lock().await.clone();
        assert_eq!(prompted.len(), 1);
        assert_eq!(prompted[0].0, "task_existing_42");
        assert_eq!(prompted[0].1, "Continue where you left off");
        assert!(result
            .output
            .contains("task_id: task_existing_42 (for resuming to continue this task if needed)"));
    }

    fn recording_ctx(
        created: Arc<Mutex<bool>>,
        disabled: Arc<Mutex<Option<Vec<String>>>>,
    ) -> ToolContext {
        ToolContext::new("session-1".into(), "message-1".into(), ".".into())
            .with_create_subsession({
                let created = created.clone();
                let disabled = disabled.clone();
                move |_agent, _title, _model, disabled_tools| {
                    let created = created.clone();
                    let disabled = disabled.clone();
                    async move {
                        *created.lock().await = true;
                        *disabled.lock().await = Some(disabled_tools);
                        Ok("task_child".to_string())
                    }
                }
            })
            .with_prompt_subsession(
                |_session_id, _prompt| async move { Ok("child output".to_string()) },
            )
    }

    #[tokio::test]
    async fn task_rejects_when_depth_limit_exceeded() {
        let created = Arc::new(Mutex::new(false));
        let disabled = Arc::new(Mutex::new(None));
        let mut ctx = recording_ctx(created.clone(), disabled);
        ctx.extra.insert(
            SUBAGENT_DEPTH_KEY.into(),
            serde_json::json!(MAX_SUBAGENT_DEPTH),
        );

        let args = serde_json::json!({
            "description": "Too deep",
            "prompt": "Should not run",
            "subagent_type": "build"
        });

        let err = TaskTool::new().execute(args, ctx).await.unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)));
        assert!(!(*created.lock().await));
    }

    #[tokio::test]
    async fn task_depth_limit_applies_with_bypass_agent_check() {
        let created = Arc::new(Mutex::new(false));
        let disabled = Arc::new(Mutex::new(None));
        let mut ctx = recording_ctx(created.clone(), disabled);
        ctx.extra.insert(
            SUBAGENT_DEPTH_KEY.into(),
            serde_json::json!(MAX_SUBAGENT_DEPTH),
        );
        ctx.extra
            .insert("bypassAgentCheck".into(), serde_json::json!(true));

        let args = serde_json::json!({
            "description": "Too deep with bypass",
            "prompt": "Should not run",
            "subagent_type": "build"
        });

        let err = TaskTool::new().execute(args, ctx).await.unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)));
        assert!(!(*created.lock().await));
    }

    #[tokio::test]
    async fn task_disables_task_tool_in_child_at_depth_limit() {
        let created = Arc::new(Mutex::new(false));
        let disabled = Arc::new(Mutex::new(None));
        let mut ctx = recording_ctx(created.clone(), disabled.clone());
        ctx.extra.insert(
            SUBAGENT_DEPTH_KEY.into(),
            serde_json::json!(MAX_SUBAGENT_DEPTH - 1),
        );

        let args = serde_json::json!({
            "description": "Last allowed launch",
            "prompt": "Run once",
            "subagent_type": "build"
        });

        TaskTool::new().execute(args, ctx).await.unwrap();

        assert!(*created.lock().await);
        let disabled = disabled.lock().await.clone().unwrap();
        assert!(disabled.iter().any(|tool| tool == "task"));
    }

    #[tokio::test]
    async fn task_keeps_task_tool_enabled_below_depth_limit() {
        let created = Arc::new(Mutex::new(false));
        let disabled = Arc::new(Mutex::new(None));
        let mut ctx = recording_ctx(created.clone(), disabled.clone());
        ctx.extra.insert(
            SUBAGENT_DEPTH_KEY.into(),
            serde_json::json!(MAX_SUBAGENT_DEPTH - 2),
        );

        let args = serde_json::json!({
            "description": "Allowed launch",
            "prompt": "Run",
            "subagent_type": "build"
        });

        TaskTool::new().execute(args, ctx).await.unwrap();

        assert!(*created.lock().await);
        let disabled = disabled.lock().await.clone().unwrap();
        assert!(!disabled.iter().any(|tool| tool == "task"));
    }

    #[test]
    fn parse_model_ref_with_none() {
        let parsed = parse_model_ref(None);
        assert_eq!(parsed.model_id, "default");
        assert_eq!(parsed.provider_id, "default");
    }

    #[test]
    fn parse_model_ref_with_colon_and_slash() {
        let colon = parse_model_ref(Some("provider:model"));
        assert_eq!(colon.provider_id, "provider");
        assert_eq!(colon.model_id, "model");

        let slash = parse_model_ref(Some("provider/model"));
        assert_eq!(slash.provider_id, "provider");
        assert_eq!(slash.model_id, "model");
    }

    #[test]
    fn parse_model_ref_bare_defaults_provider() {
        let parsed = parse_model_ref(Some("gpt-4"));
        assert_eq!(parsed.model_id, "gpt-4");
        assert_eq!(parsed.provider_id, "default");
    }
}
