use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{Metadata, QuestionDef, QuestionOption, Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEnterParams {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExitParams {}

pub struct PlanEnterTool;

pub struct PlanExitTool;

const PLAN_FILE: &str = "PLAN.md";

/// 選択肢の承認ラベル（`QuestionOption.label` と同一値。wire 形式は変更しない）。
const APPROVE_LABEL: &str = "Yes";
/// 選択肢の拒否ラベル（`QuestionOption.label` と同一値。wire 形式は変更しない）。
const REJECT_LABEL: &str = "No";

/// 質問の回答が承認ラベルと一致する場合のみ承認する。
///
/// 許可リスト方式（fail-closed）とし、拒否ラベル・空文字列・未知ラベル・
/// 任意文字列・回答欠落はすべて `QuestionRejected` として扱う。
/// 大文字小文字は無視するが、前後の空白は除去しない（より緩い一致は行わない）。
fn require_approval(answers: &[Vec<String>], rejection_message: &str) -> Result<(), ToolError> {
    let approved = answers
        .first()
        .and_then(|answer| answer.first())
        .is_some_and(|label| label.eq_ignore_ascii_case(APPROVE_LABEL));

    if approved {
        Ok(())
    } else {
        Err(ToolError::QuestionRejected(rejection_message.to_string()))
    }
}

/// Create a user message and a synthetic text part via the ToolContext callbacks,
/// matching the TS `Session.updateMessage()` + `Session.updatePart()` pattern.
async fn create_user_message_with_part(
    ctx: &ToolContext,
    agent: &str,
    model: &Option<String>,
    text: &str,
) -> Result<(), ToolError> {
    let now = chrono::Utc::now().timestamp_millis();
    let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let part_id = format!("prt_{}", uuid::Uuid::new_v4().simple());

    // Build the MessageV2.User info matching the TS MessageInfo::User shape
    let mut user_msg = serde_json::json!({
        "id": message_id,
        "sessionID": ctx.session_id,
        "role": "user",
        "time": { "created": now },
        "agent": agent,
    });
    if let Some(ref m) = model {
        user_msg["model"] = serde_json::json!(m);
    }

    // Persist the message (mirrors TS Session.updateMessage)
    ctx.do_update_message(user_msg).await?;

    // Build the synthetic text part matching the TS MessageV2.TextPart shape
    let text_part = serde_json::json!({
        "id": part_id,
        "messageID": message_id,
        "sessionID": ctx.session_id,
        "type": "text",
        "text": text,
        "synthetic": true,
    });

    // Persist the part (mirrors TS Session.updatePart)
    ctx.do_update_part(text_part).await?;

    Ok(())
}

#[async_trait]
impl Tool for PlanEnterTool {
    fn id(&self) -> &str {
        "plan_enter"
    }

    fn description(&self) -> &str {
        "Switch to plan mode for research and planning. In plan mode, you can read files and create plans but cannot make changes. Use this when you need to thoroughly analyze a problem before implementing."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let plan_path = get_plan_path(&ctx);
        let plan_display = plan_path.display();

        let questions = vec![QuestionDef {
            question: format!(
                "Would you like to switch to the plan agent and create a plan saved to {}?",
                plan_display
            ),
            header: Some("Plan Mode".to_string()),
            options: vec![
                QuestionOption {
                    label: APPROVE_LABEL.to_string(),
                    description: Some("Switch to plan agent for research and planning".to_string()),
                },
                QuestionOption {
                    label: REJECT_LABEL.to_string(),
                    description: Some(
                        "Stay with build agent to continue making changes".to_string(),
                    ),
                },
            ],
            multiple: false,
        }];

        let answers = ctx.question(questions).await?;

        require_approval(&answers, "User rejected plan mode switch")?;

        let model = ctx.do_get_last_model().await;

        // Create a user message + synthetic part (mirrors TS Session.updateMessage + updatePart)
        let synthetic_text =
            "User has requested to enter plan mode. Switch to plan mode and begin planning.";
        create_user_message_with_part(&ctx, "plan", &model, synthetic_text).await?;

        ctx.do_switch_agent("plan".to_string(), model.clone())
            .await?;

        let mut metadata = Metadata::new();
        metadata.insert("agent".to_string(), serde_json::json!("plan"));
        metadata.insert("session_id".to_string(), serde_json::json!(ctx.session_id));
        if let Some(ref m) = model {
            metadata.insert("model".to_string(), serde_json::json!(m));
        }

        Ok(ToolResult {
            output: format!(
                "User confirmed to switch to plan mode. A new message has been created to switch you to plan mode. The plan file will be at {}. Begin planning.",
                plan_display
            ),
            title: "Switching to plan agent".to_string(),
            metadata,
            truncated: false,
        })
    }
}

#[async_trait]
impl Tool for PlanExitTool {
    fn id(&self) -> &str {
        "plan_exit"
    }

    fn description(&self) -> &str {
        "Exit plan mode and switch to build mode for implementation. Use this when you have completed your plan and are ready to make file changes."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let plan_path = get_plan_path(&ctx);
        let plan_display = plan_path.display();

        let questions = vec![QuestionDef {
            question: format!("Plan at {} is complete. Would you like to switch to the build agent and start implementing?", plan_display),
            header: Some("Build Agent".to_string()),
            options: vec![
                QuestionOption {
                    label: APPROVE_LABEL.to_string(),
                    description: Some("Switch to build agent and start implementing the plan".to_string()),
                },
                QuestionOption {
                    label: REJECT_LABEL.to_string(),
                    description: Some("Stay with plan agent to continue refining the plan".to_string()),
                },
            ],
            multiple: false,
        }];

        let answers = ctx.question(questions).await?;

        require_approval(&answers, "User rejected build mode switch")?;

        let model = ctx.do_get_last_model().await;

        let plan_path = get_plan_path(&ctx);
        let plan_display = plan_path.display();

        // Create a user message + synthetic part (mirrors TS Session.updateMessage + updatePart)
        let synthetic_text = format!(
            "The plan at {} has been approved, you can now edit files. Execute the plan.",
            plan_display
        );
        create_user_message_with_part(&ctx, "build", &model, &synthetic_text).await?;

        ctx.do_switch_agent("build".to_string(), model.clone())
            .await?;

        let mut metadata = Metadata::new();
        metadata.insert("agent".to_string(), serde_json::json!("build"));
        metadata.insert("session_id".to_string(), serde_json::json!(ctx.session_id));
        if let Some(ref m) = model {
            metadata.insert("model".to_string(), serde_json::json!(m));
        }

        Ok(ToolResult {
            output: "User approved switching to build agent. Wait for further instructions."
                .to_string(),
            title: "Switching to build agent".to_string(),
            metadata,
            truncated: false,
        })
    }
}

fn get_plan_path(ctx: &ToolContext) -> PathBuf {
    PathBuf::from(&ctx.worktree)
        .join(".opencode")
        .join(PLAN_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ToolContext {
        ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            "/work/tree".to_owned(),
        )
    }

    #[test]
    fn plan_enter_identity_and_schema() {
        let tool = PlanEnterTool;
        assert_eq!(tool.id(), "plan_enter");
        assert_eq!(tool.parameters()["type"], "object");
    }

    #[test]
    fn plan_exit_identity_and_schema() {
        let tool = PlanExitTool;
        assert_eq!(tool.id(), "plan_exit");
        assert_eq!(tool.parameters()["type"], "object");
    }

    #[test]
    fn get_plan_path_uses_worktree() {
        assert_eq!(
            get_plan_path(&context()),
            PathBuf::from("/work/tree/.opencode/PLAN.md")
        );
    }

    #[test]
    fn empty_params_deserialize() {
        let _: PlanEnterParams = serde_json::from_value(serde_json::json!({})).unwrap();
        let _: PlanExitParams = serde_json::from_value(serde_json::json!({})).unwrap();
    }

    use std::sync::{Arc, Mutex};

    /// 指定された回答を返す質問コールバックと、エージェント切替の記録先を返す。
    fn context_with_answer(answer: Option<&str>) -> (ToolContext, Arc<Mutex<Vec<String>>>) {
        let answer = answer.map(|a| a.to_owned());
        let switched = Arc::new(Mutex::new(Vec::new()));
        let switched_for_agent = switched.clone();
        let ctx = context()
            .with_ask_question(move |_questions: Vec<QuestionDef>| {
                let answer = answer.clone();
                async move {
                    match answer {
                        Some(label) => Ok(vec![vec![label]]),
                        None => Ok(vec![]),
                    }
                }
            })
            .with_switch_agent(move |agent, _model| {
                let switched = switched_for_agent.clone();
                async move {
                    switched.lock().unwrap().push(agent);
                    Ok(())
                }
            });
        (ctx, switched)
    }

    #[test]
    fn require_approval_accepts_only_approve_label_case_insensitively() {
        for label in ["Yes", "yes", "YES", "yEs"] {
            let answers = vec![vec![label.to_owned()]];
            assert!(
                require_approval(&answers, "rejected").is_ok(),
                "label must be approved: {label:?}"
            );
        }
    }

    #[test]
    fn require_approval_rejects_everything_else() {
        for label in ["No", "no", "N", "", "unknown", " YES ", "Yes!"] {
            let answers = vec![vec![label.to_owned()]];
            assert!(
                matches!(
                    require_approval(&answers, "rejected"),
                    Err(ToolError::QuestionRejected(_))
                ),
                "label must be rejected: {label:?}"
            );
        }
        // 回答が存在しない場合も fail-closed（旧実装の `unwrap_or("No")` と同挙動）。
        assert!(require_approval(&[], "rejected").is_err());
        assert!(require_approval(&[vec![]], "rejected").is_err());
    }

    #[tokio::test]
    async fn plan_enter_rejects_lowercase_no_without_switching_agent() {
        let (ctx, switched) = context_with_answer(Some("no"));
        let error = PlanEnterTool
            .execute(serde_json::json!({}), ctx)
            .await
            .expect_err("lowercase no must be rejected");
        assert!(matches!(error, ToolError::QuestionRejected(_)));
        assert!(switched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn plan_exit_rejects_lowercase_no_without_switching_agent() {
        let (ctx, switched) = context_with_answer(Some("no"));
        let error = PlanExitTool
            .execute(serde_json::json!({}), ctx)
            .await
            .expect_err("lowercase no must be rejected");
        assert!(matches!(error, ToolError::QuestionRejected(_)));
        assert!(switched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn plan_enter_accepts_lowercase_yes() {
        let (ctx, switched) = context_with_answer(Some("yes"));
        PlanEnterTool
            .execute(serde_json::json!({}), ctx)
            .await
            .expect("lowercase yes must be approved");
        let switched = switched.lock().unwrap();
        assert_eq!(switched.len(), 1);
        assert_eq!(switched[0].as_str(), "plan");
    }

    #[tokio::test]
    async fn plan_exit_accepts_lowercase_yes() {
        let (ctx, switched) = context_with_answer(Some("yes"));
        PlanExitTool
            .execute(serde_json::json!({}), ctx)
            .await
            .expect("lowercase yes must be approved");
        let switched = switched.lock().unwrap();
        assert_eq!(switched.len(), 1);
        assert_eq!(switched[0].as_str(), "build");
    }
}
