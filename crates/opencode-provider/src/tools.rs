//! Tool definitions and preparation for the OpenAI Responses API and
//! OpenAI-compatible chat completions.
//!
//! Mirrors the TS files:
//! - `openai-responses-prepare-tools.ts`
//! - `openai-compatible-prepare-tools.ts`
//! - `tool/web-search.ts`, `tool/code-interpreter.ts`, `tool/file-search.ts`,
//!   `tool/image-generation.ts`, `tool/local-shell.ts`, `tool/web-search-preview.ts`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::responses::CallWarning;

// ---------------------------------------------------------------------------
// Provider-Defined Tool Types
// ---------------------------------------------------------------------------

/// Web search action types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebSearchAction {
    #[serde(rename = "search")]
    Search {
        #[serde(skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    #[serde(rename = "open_page")]
    OpenPage { url: String },
    #[serde(rename = "find")]
    Find { url: String, pattern: String },
}

/// User location for web search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLocation {
    #[serde(rename = "type")]
    pub location_type: String, // "approximate"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Web search args (for `openai.web_search`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebSearchArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<WebSearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<String>, // "low" | "medium" | "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<UserLocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
}

/// Web search preview args (for `openai.web_search_preview`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebSearchPreviewArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<UserLocation>,
}

/// Code interpreter args (for `openai.code_interpreter`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeInterpreterArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<CodeInterpreterContainer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodeInterpreterContainer {
    Id(String),
    Config(CodeInterpreterContainerConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeInterpreterContainerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<String>>,
}

/// Code interpreter input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeInterpreterInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub container_id: String,
}

/// Code interpreter output schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeInterpreterOutputResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<CodeInterpreterOutputItem>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodeInterpreterOutputItem {
    #[serde(rename = "logs")]
    Logs { logs: String },
    #[serde(rename = "image")]
    Image { url: String },
}

/// File search args (for `openai.file_search`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchArgs {
    pub vector_store_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_num_results: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking: Option<FileSearchRanking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchRanking {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_threshold: Option<f64>,
}

/// File search output schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchOutput {
    pub queries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<FileSearchResultItem>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchResultItem {
    #[serde(default)]
    pub attributes: HashMap<String, serde_json::Value>,
    pub file_id: String,
    pub filename: String,
    pub score: f64,
    pub text: String,
}

/// Image generation args (for `openai.image_generation`).
/// 10+ config options matching the TS `imageGenerationArgsSchema`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageGenerationArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>, // "auto" | "opaque" | "transparent"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_fidelity: Option<String>, // "low" | "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_image_mask: Option<ImageMask>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moderation: Option<String>, // "auto"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_compression: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>, // "png" | "jpeg" | "webp"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_images: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>, // "auto" | "low" | "medium" | "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>, // "auto" | "1024x1024" | "1024x1536" | "1536x1024"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageMask {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// Image generation output schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationOutput {
    pub result: String,
}

/// Local shell input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalShellInput {
    pub action: LocalShellInputAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalShellInputAction {
    #[serde(rename = "type")]
    pub action_type: String, // "exec"
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "workingDirectory")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Local shell output schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalShellOutput {
    pub output: String,
}

// ---------------------------------------------------------------------------
// File Search Filter Types
// ---------------------------------------------------------------------------

/// Comparison filter for file search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonFilter {
    pub key: String,
    #[serde(rename = "type")]
    pub filter_type: String, // "eq" | "ne" | "gt" | "gte" | "lt" | "lte"
    pub value: serde_json::Value, // string | number | boolean
}

/// Compound filter for file search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompoundFilter {
    #[serde(rename = "type")]
    pub filter_type: String, // "and" | "or"
    pub filters: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// OpenAI Responses Tool (wire format)
// ---------------------------------------------------------------------------

/// Tool definitions sent to the Responses API.
/// Mirrors TS `OpenAIResponsesTool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponsesTool {
    #[serde(rename = "function")]
    Function {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        parameters: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "web_search")]
    WebSearch {
        #[serde(skip_serializing_if = "Option::is_none")]
        filters: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        search_context_size: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        user_location: Option<UserLocation>,
    },
    #[serde(rename = "web_search_preview")]
    WebSearchPreview {
        #[serde(skip_serializing_if = "Option::is_none")]
        search_context_size: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        user_location: Option<UserLocation>,
    },
    #[serde(rename = "code_interpreter")]
    CodeInterpreter { container: serde_json::Value },
    #[serde(rename = "file_search")]
    FileSearch {
        vector_store_ids: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_num_results: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ranking_options: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filters: Option<serde_json::Value>,
    },
    #[serde(rename = "image_generation")]
    ImageGeneration {
        #[serde(skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_fidelity: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_image_mask: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        moderation: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_compression: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_format: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_images: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<String>,
    },
    #[serde(rename = "local_shell")]
    LocalShell {},
}

// ---------------------------------------------------------------------------
// Tool Choice Types
// ---------------------------------------------------------------------------

/// Tool choice for the Responses API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsesToolChoice {
    /// "auto", "none", "required"
    Mode(String),
    /// Specific tool type: { type: "function", name: "..." } etc.
    Specific(ResponsesToolChoiceSpecific),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesToolChoiceSpecific {
    #[serde(rename = "type")]
    pub choice_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Tool choice for OpenAI-compatible chat completions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatToolChoice {
    Mode(String),
    Specific(ChatToolChoiceSpecific),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolChoiceSpecific {
    #[serde(rename = "type")]
    pub choice_type: String,
    pub function: ChatToolChoiceFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolChoiceFunction {
    pub name: String,
}

/// Input tool definition from the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputTool {
    #[serde(rename = "function")]
    Function {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "inputSchema")]
        input_schema: serde_json::Value,
    },
    #[serde(rename = "provider-defined")]
    ProviderDefined {
        id: String,
        name: String,
        #[serde(default)]
        args: serde_json::Value,
    },
}

/// Input tool choice from the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "none")]
    None,
    #[serde(rename = "required")]
    Required,
    #[serde(rename = "tool")]
    Tool {
        #[serde(rename = "toolName")]
        tool_name: String,
    },
}

// ---------------------------------------------------------------------------
// Prepare Responses Tools
// ---------------------------------------------------------------------------

/// Result of preparing tools for the Responses API.
pub struct PreparedResponsesTools {
    pub tools: Option<Vec<ResponsesTool>>,
    pub tool_choice: Option<ResponsesToolChoice>,
    pub tool_warnings: Vec<CallWarning>,
}

/// Prepare tools for the OpenAI Responses API.
/// Mirrors TS `prepareResponsesTools()`.
pub fn prepare_responses_tools(
    tools: Option<&[InputTool]>,
    tool_choice: Option<&InputToolChoice>,
    strict_json_schema: bool,
) -> PreparedResponsesTools {
    let tools = match tools {
        Some(t) if !t.is_empty() => t,
        _ => {
            return PreparedResponsesTools {
                tools: None,
                tool_choice: None,
                tool_warnings: vec![],
            }
        }
    };

    let mut openai_tools = Vec::new();
    let mut tool_warnings = Vec::new();

    for tool in tools {
        match tool {
            InputTool::Function {
                name,
                description,
                input_schema,
            } => {
                openai_tools.push(ResponsesTool::Function {
                    name: name.clone(),
                    description: description.clone(),
                    parameters: input_schema.clone(),
                    strict: if strict_json_schema { Some(true) } else { None },
                });
            }
            InputTool::ProviderDefined { id, name, args } => match id.as_str() {
                "openai.file_search" => {
                    if let Ok(fs_args) = serde_json::from_value::<FileSearchArgs>(args.clone()) {
                        let ranking_options = fs_args.ranking.map(|r| {
                            serde_json::json!({
                                "ranker": r.ranker,
                                "score_threshold": r.score_threshold,
                            })
                        });
                        openai_tools.push(ResponsesTool::FileSearch {
                            vector_store_ids: fs_args.vector_store_ids,
                            max_num_results: fs_args.max_num_results,
                            ranking_options,
                            filters: fs_args.filters,
                        });
                    }
                }
                "openai.local_shell" => {
                    openai_tools.push(ResponsesTool::LocalShell {});
                }
                "openai.web_search_preview" => {
                    if let Ok(ws_args) =
                        serde_json::from_value::<WebSearchPreviewArgs>(args.clone())
                    {
                        openai_tools.push(ResponsesTool::WebSearchPreview {
                            search_context_size: ws_args.search_context_size,
                            user_location: ws_args.user_location,
                        });
                    }
                }
                "openai.web_search" => {
                    if let Ok(ws_args) = serde_json::from_value::<WebSearchArgs>(args.clone()) {
                        let filters = ws_args.filters.map(|f| {
                            serde_json::json!({
                                "allowed_domains": f.allowed_domains,
                            })
                        });
                        openai_tools.push(ResponsesTool::WebSearch {
                            filters,
                            search_context_size: ws_args.search_context_size,
                            user_location: ws_args.user_location,
                        });
                    }
                }
                "openai.code_interpreter" => {
                    if let Ok(ci_args) = serde_json::from_value::<CodeInterpreterArgs>(args.clone())
                    {
                        let container = match ci_args.container {
                            None => serde_json::json!({"type": "auto"}),
                            Some(CodeInterpreterContainer::Id(id)) => serde_json::Value::String(id),
                            Some(CodeInterpreterContainer::Config(cfg)) => {
                                serde_json::json!({
                                    "type": "auto",
                                    "file_ids": cfg.file_ids,
                                })
                            }
                        };
                        openai_tools.push(ResponsesTool::CodeInterpreter { container });
                    }
                }
                "openai.image_generation" => {
                    if let Ok(ig_args) = serde_json::from_value::<ImageGenerationArgs>(args.clone())
                    {
                        let input_image_mask = ig_args.input_image_mask.map(|m| {
                            serde_json::json!({
                                "file_id": m.file_id,
                                "image_url": m.image_url,
                            })
                        });
                        openai_tools.push(ResponsesTool::ImageGeneration {
                            background: ig_args.background,
                            input_fidelity: ig_args.input_fidelity,
                            input_image_mask,
                            model: ig_args.model,
                            moderation: ig_args.moderation,
                            output_compression: ig_args.output_compression,
                            output_format: ig_args.output_format,
                            partial_images: ig_args.partial_images,
                            quality: ig_args.quality,
                            size: ig_args.size,
                        });
                    }
                }
                _ => {
                    tool_warnings.push(CallWarning::UnsupportedTool {
                        tool_name: Some(name.clone()),
                    });
                }
            },
        }
    }

    let mapped_choice = tool_choice.map(|tc| match tc {
        InputToolChoice::Auto => ResponsesToolChoice::Mode("auto".to_string()),
        InputToolChoice::None => ResponsesToolChoice::Mode("none".to_string()),
        InputToolChoice::Required => ResponsesToolChoice::Mode("required".to_string()),
        InputToolChoice::Tool { tool_name } => {
            let builtin = [
                "code_interpreter",
                "file_search",
                "image_generation",
                "web_search_preview",
                "web_search",
            ];
            if builtin.contains(&tool_name.as_str()) {
                ResponsesToolChoice::Specific(ResponsesToolChoiceSpecific {
                    choice_type: tool_name.clone(),
                    name: None,
                })
            } else {
                ResponsesToolChoice::Specific(ResponsesToolChoiceSpecific {
                    choice_type: "function".to_string(),
                    name: Some(tool_name.clone()),
                })
            }
        }
    });

    PreparedResponsesTools {
        tools: Some(openai_tools),
        tool_choice: mapped_choice,
        tool_warnings,
    }
}

// ---------------------------------------------------------------------------
// OpenAI-Compatible Chat Tool (wire format)
// ---------------------------------------------------------------------------

/// Tool definition for OpenAI-compatible chat completions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTool {
    #[serde(rename = "type")]
    pub tool_type: String, // "function"
    pub function: ChatToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolFunction {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

/// Result of preparing tools for OpenAI-compatible chat.
pub struct PreparedChatTools {
    pub tools: Option<Vec<ChatTool>>,
    pub tool_choice: Option<ChatToolChoice>,
    pub tool_warnings: Vec<CallWarning>,
}

/// Prepare tools for OpenAI-compatible chat completions.
/// Mirrors TS `prepareTools()`.
pub fn prepare_tools(
    tools: Option<&[InputTool]>,
    tool_choice: Option<&InputToolChoice>,
) -> PreparedChatTools {
    let tools = match tools {
        Some(t) if !t.is_empty() => t,
        _ => {
            return PreparedChatTools {
                tools: None,
                tool_choice: None,
                tool_warnings: vec![],
            }
        }
    };

    let mut openai_tools = Vec::new();
    let mut tool_warnings = Vec::new();

    for tool in tools {
        match tool {
            InputTool::Function {
                name,
                description,
                input_schema,
            } => {
                openai_tools.push(ChatTool {
                    tool_type: "function".to_string(),
                    function: ChatToolFunction {
                        name: name.clone(),
                        description: description.clone(),
                        parameters: input_schema.clone(),
                    },
                });
            }
            InputTool::ProviderDefined { name, .. } => {
                tool_warnings.push(CallWarning::UnsupportedTool {
                    tool_name: Some(name.clone()),
                });
            }
        }
    }

    let mapped_choice = tool_choice.map(|tc| match tc {
        InputToolChoice::Auto => ChatToolChoice::Mode("auto".to_string()),
        InputToolChoice::None => ChatToolChoice::Mode("none".to_string()),
        InputToolChoice::Required => ChatToolChoice::Mode("required".to_string()),
        InputToolChoice::Tool { tool_name } => ChatToolChoice::Specific(ChatToolChoiceSpecific {
            choice_type: "function".to_string(),
            function: ChatToolChoiceFunction {
                name: tool_name.clone(),
            },
        }),
    });

    PreparedChatTools {
        tools: Some(openai_tools),
        tool_choice: mapped_choice,
        tool_warnings,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// ダミーの function ツール定義。HTTP 通信や秘密情報を一切含まない。
    fn function_tool(name: &str) -> InputTool {
        InputTool::Function {
            name: name.to_string(),
            description: Some("dummy description".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "string" } }
            }),
        }
    }

    fn provider_tool(id: &str, name: &str, args: serde_json::Value) -> InputTool {
        InputTool::ProviderDefined {
            id: id.to_string(),
            name: name.to_string(),
            args,
        }
    }

    #[test]
    fn responses_tools_without_tools_returns_none_and_ignores_choice() {
        let prepared = prepare_responses_tools(None, Some(&InputToolChoice::Required), true);
        assert!(prepared.tools.is_none());
        assert!(prepared.tool_choice.is_none());
        assert!(prepared.tool_warnings.is_empty());

        let empty: [InputTool; 0] = [];
        let prepared = prepare_responses_tools(
            Some(&empty),
            Some(&InputToolChoice::Tool {
                tool_name: "dummy_tool".to_string(),
            }),
            false,
        );
        assert!(prepared.tools.is_none());
        assert!(prepared.tool_choice.is_none());
        assert!(prepared.tool_warnings.is_empty());
    }

    #[test]
    fn chat_tools_without_tools_returns_none_and_ignores_choice() {
        let prepared = prepare_tools(None, Some(&InputToolChoice::Auto));
        assert!(prepared.tools.is_none());
        assert!(prepared.tool_choice.is_none());
        assert!(prepared.tool_warnings.is_empty());

        let empty: [InputTool; 0] = [];
        let prepared = prepare_tools(Some(&empty), Some(&InputToolChoice::Required));
        assert!(prepared.tools.is_none());
        assert!(prepared.tool_choice.is_none());
        assert!(prepared.tool_warnings.is_empty());
    }

    #[test]
    fn responses_tools_maps_function_and_strict_flag() {
        let tools = vec![function_tool("dummy_tool")];

        let strict = prepare_responses_tools(Some(&tools), None, true);
        let mapped = strict.tools.expect("tools should be present");
        assert_eq!(mapped.len(), 1);
        match &mapped[0] {
            ResponsesTool::Function {
                name,
                description,
                parameters,
                strict,
            } => {
                assert_eq!(name.as_str(), "dummy_tool");
                assert_eq!(description.as_deref(), Some("dummy description"));
                assert_eq!(parameters["type"], "object");
                assert_eq!(*strict, Some(true));
            }
            other => panic!("expected function tool, got {:?}", other),
        }
        assert!(strict.tool_warnings.is_empty());

        let non_strict = prepare_responses_tools(Some(&tools), None, false);
        match &non_strict.tools.expect("tools should be present")[0] {
            ResponsesTool::Function { strict, .. } => assert_eq!(*strict, None),
            other => panic!("expected function tool, got {:?}", other),
        }
    }

    #[test]
    fn chat_tools_maps_function_definition() {
        let tools = vec![function_tool("dummy_tool")];
        let prepared = prepare_tools(Some(&tools), None);
        let mapped = prepared.tools.expect("tools should be present");
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].tool_type, "function");
        assert_eq!(mapped[0].function.name, "dummy_tool");
        assert_eq!(
            mapped[0].function.description.as_deref(),
            Some("dummy description")
        );
        assert_eq!(mapped[0].function.parameters["type"], "object");
        assert!(prepared.tool_warnings.is_empty());
    }

    #[test]
    fn unsupported_provider_tools_produce_warnings() {
        let tools = vec![
            function_tool("dummy_tool"),
            provider_tool("unknown.tool", "unknown_tool", serde_json::json!({})),
        ];

        let responses = prepare_responses_tools(Some(&tools), None, false);
        assert_eq!(responses.tools.expect("tools").len(), 1);
        assert!(responses.tool_warnings.iter().any(|warning| {
            matches!(warning, CallWarning::UnsupportedTool { tool_name: Some(name) }
                if name.as_str() == "unknown_tool")
        }));

        let chat = prepare_tools(Some(&tools), None);
        assert_eq!(chat.tools.expect("tools").len(), 1);
        assert!(chat.tool_warnings.iter().any(|warning| {
            matches!(warning, CallWarning::UnsupportedTool { tool_name: Some(name) }
                if name.as_str() == "unknown_tool")
        }));
    }

    #[test]
    fn responses_tool_choice_maps_modes_and_named_tools() {
        let tools = vec![function_tool("dummy_tool")];
        let cases = [
            (InputToolChoice::Auto, "auto"),
            (InputToolChoice::None, "none"),
            (InputToolChoice::Required, "required"),
        ];
        for (choice, expected) in cases {
            let prepared = prepare_responses_tools(Some(&tools), Some(&choice), false);
            match prepared.tool_choice.expect("choice") {
                ResponsesToolChoice::Mode(mode) => assert_eq!(mode, expected),
                other => panic!("expected mode choice, got {:?}", other),
            }
        }

        let builtin = prepare_responses_tools(
            Some(&tools),
            Some(&InputToolChoice::Tool {
                tool_name: "web_search".to_string(),
            }),
            false,
        );
        match builtin.tool_choice.expect("choice") {
            ResponsesToolChoice::Specific(specific) => {
                assert_eq!(specific.choice_type, "web_search");
                assert!(specific.name.is_none());
            }
            other => panic!("expected specific choice, got {:?}", other),
        }

        let named = prepare_responses_tools(
            Some(&tools),
            Some(&InputToolChoice::Tool {
                tool_name: "dummy_tool".to_string(),
            }),
            false,
        );
        match named.tool_choice.expect("choice") {
            ResponsesToolChoice::Specific(specific) => {
                assert_eq!(specific.choice_type, "function");
                assert_eq!(specific.name.as_deref(), Some("dummy_tool"));
            }
            other => panic!("expected specific choice, got {:?}", other),
        }
    }

    #[test]
    fn chat_tool_choice_maps_modes_and_named_tools() {
        let tools = vec![function_tool("dummy_tool")];
        let cases = [
            (InputToolChoice::Auto, "auto"),
            (InputToolChoice::None, "none"),
            (InputToolChoice::Required, "required"),
        ];
        for (choice, expected) in cases {
            let prepared = prepare_tools(Some(&tools), Some(&choice));
            match prepared.tool_choice.expect("choice") {
                ChatToolChoice::Mode(mode) => assert_eq!(mode, expected),
                other => panic!("expected mode choice, got {:?}", other),
            }
        }

        let named = prepare_tools(
            Some(&tools),
            Some(&InputToolChoice::Tool {
                tool_name: "dummy_tool".to_string(),
            }),
        );
        match named.tool_choice.expect("choice") {
            ChatToolChoice::Specific(specific) => {
                assert_eq!(specific.choice_type, "function");
                assert_eq!(specific.function.name, "dummy_tool");
            }
            other => panic!("expected specific choice, got {:?}", other),
        }
    }

    #[test]
    fn responses_tools_convert_provider_defined_builtins() {
        let tools = vec![
            provider_tool("openai.local_shell", "local_shell", serde_json::json!({})),
            provider_tool("openai.web_search", "web_search", serde_json::json!({})),
            provider_tool(
                "openai.web_search_preview",
                "web_search_preview",
                serde_json::json!({}),
            ),
            provider_tool(
                "openai.file_search",
                "file_search",
                serde_json::json!({ "vector_store_ids": ["vs_dummy"] }),
            ),
            provider_tool(
                "openai.code_interpreter",
                "code_interpreter",
                serde_json::json!({}),
            ),
            provider_tool(
                "openai.image_generation",
                "image_generation",
                serde_json::json!({}),
            ),
        ];

        let prepared = prepare_responses_tools(Some(&tools), None, false);
        assert!(prepared.tool_warnings.is_empty());
        let mapped = prepared.tools.expect("tools should be present");
        assert_eq!(mapped.len(), 6);
        assert!(matches!(&mapped[0], ResponsesTool::LocalShell {}));
        assert!(matches!(&mapped[1], ResponsesTool::WebSearch { .. }));
        assert!(matches!(&mapped[2], ResponsesTool::WebSearchPreview { .. }));
        assert!(matches!(&mapped[3], ResponsesTool::FileSearch { .. }));
        assert!(matches!(&mapped[4], ResponsesTool::CodeInterpreter { .. }));
        assert!(matches!(&mapped[5], ResponsesTool::ImageGeneration { .. }));
    }

    #[test]
    fn responses_tools_file_search_maps_ranking_and_filters() {
        let tools = vec![provider_tool(
            "openai.file_search",
            "file_search",
            serde_json::json!({
                "vector_store_ids": ["vs_dummy"],
                "max_num_results": 3,
                "ranking": { "ranker": "auto" },
                "filters": { "key": "dummy" }
            }),
        )];

        let prepared = prepare_responses_tools(Some(&tools), None, false);
        match &prepared.tools.expect("tools")[0] {
            ResponsesTool::FileSearch {
                vector_store_ids,
                max_num_results,
                ranking_options,
                filters,
            } => {
                assert_eq!(vector_store_ids, &vec!["vs_dummy".to_string()]);
                assert_eq!(*max_num_results, Some(3));
                assert_eq!(
                    ranking_options.as_ref().expect("ranking_options")["ranker"],
                    "auto"
                );
                assert_eq!(filters.as_ref().expect("filters")["key"], "dummy");
            }
            other => panic!("expected file search tool, got {:?}", other),
        }
    }

    #[test]
    fn responses_tools_code_interpreter_container_variants() {
        let defaults = vec![provider_tool(
            "openai.code_interpreter",
            "code_interpreter",
            serde_json::json!({}),
        )];
        let prepared = prepare_responses_tools(Some(&defaults), None, false);
        match &prepared.tools.expect("tools")[0] {
            ResponsesTool::CodeInterpreter { container } => {
                assert_eq!(container["type"], "auto");
            }
            other => panic!("expected code interpreter tool, got {:?}", other),
        }

        let by_id = vec![provider_tool(
            "openai.code_interpreter",
            "code_interpreter",
            serde_json::json!({ "container": "cntr_dummy" }),
        )];
        let prepared = prepare_responses_tools(Some(&by_id), None, false);
        match &prepared.tools.expect("tools")[0] {
            ResponsesTool::CodeInterpreter { container } => {
                assert_eq!(container.as_str(), Some("cntr_dummy"));
            }
            other => panic!("expected code interpreter tool, got {:?}", other),
        }

        let by_config = vec![provider_tool(
            "openai.code_interpreter",
            "code_interpreter",
            serde_json::json!({ "container": { "file_ids": ["file_dummy"] } }),
        )];
        let prepared = prepare_responses_tools(Some(&by_config), None, false);
        match &prepared.tools.expect("tools")[0] {
            ResponsesTool::CodeInterpreter { container } => {
                assert_eq!(container["type"], "auto");
                assert_eq!(container["file_ids"][0], "file_dummy");
            }
            other => panic!("expected code interpreter tool, got {:?}", other),
        }
    }

    #[test]
    fn responses_tools_skip_provider_tool_with_invalid_args() {
        let tools = vec![provider_tool(
            "openai.file_search",
            "file_search",
            serde_json::json!({ "unexpected": "shape" }),
        )];
        let prepared = prepare_responses_tools(Some(&tools), None, false);
        assert!(prepared.tools.expect("tools").is_empty());
        assert!(prepared.tool_warnings.is_empty());
    }

    #[test]
    fn input_tool_serde_uses_wire_field_names_and_roundtrips() {
        let tool = function_tool("dummy_tool");
        let value = serde_json::to_value(&tool).expect("serialize");
        assert_eq!(value["type"], "function");
        assert_eq!(value["name"], "dummy_tool");
        assert_eq!(value["inputSchema"]["type"], "object");
        assert!(value.get("input_schema").is_none());

        let decoded: InputTool = serde_json::from_value(value).expect("deserialize");
        match decoded {
            InputTool::Function {
                name, input_schema, ..
            } => {
                assert_eq!(name, "dummy_tool");
                assert_eq!(input_schema["type"], "object");
            }
            other => panic!("expected function tool, got {:?}", other),
        }
    }

    #[test]
    fn input_tool_omits_absent_description() {
        let tool = InputTool::Function {
            name: "dummy_tool".to_string(),
            description: None,
            input_schema: serde_json::json!({}),
        };
        let value = serde_json::to_value(&tool).expect("serialize");
        assert!(value.get("description").is_none());
    }

    #[test]
    fn tagged_output_types_serialize_with_wire_discriminators() {
        let search = WebSearchAction::Search {
            query: Some("dummy query".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&search).expect("serialize"),
            serde_json::json!({ "type": "search", "query": "dummy query" })
        );

        let find = WebSearchAction::Find {
            url: "https://example.invalid/page".to_string(),
            pattern: "dummy".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&find).expect("serialize")["type"],
            "find"
        );

        let logs = CodeInterpreterOutputItem::Logs {
            logs: "dummy log".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&logs).expect("serialize"),
            serde_json::json!({ "type": "logs", "logs": "dummy log" })
        );

        let local_shell = ResponsesTool::LocalShell {};
        assert_eq!(
            serde_json::to_value(&local_shell).expect("serialize"),
            serde_json::json!({ "type": "local_shell" })
        );
    }

    #[test]
    fn local_shell_action_serde_renames_camel_case_fields() {
        let action = LocalShellInputAction {
            action_type: "exec".to_string(),
            command: vec!["echo".to_string(), "dummy".to_string()],
            timeout_ms: Some(1234),
            user: None,
            working_directory: Some("/tmp/dummy".to_string()),
            env: None,
        };
        let value = serde_json::to_value(LocalShellInput { action }).expect("serialize");
        assert_eq!(value["action"]["timeoutMs"], 1234);
        assert_eq!(value["action"]["workingDirectory"], "/tmp/dummy");
        assert!(value["action"].get("timeout_ms").is_none());
        assert!(value["action"].get("working_directory").is_none());
        assert!(value["action"].get("user").is_none());
    }

    #[test]
    fn user_location_serde_renames_location_type() {
        let location = UserLocation {
            location_type: "approximate".to_string(),
            city: Some("DummyCity".to_string()),
            country: None,
            region: None,
            timezone: None,
        };
        let value = serde_json::to_value(&location).expect("serialize");
        assert_eq!(value["type"], "approximate");
        assert_eq!(value["city"], "DummyCity");
        assert!(value.get("location_type").is_none());
        assert!(value.get("country").is_none());
    }

    #[test]
    fn chat_tool_serde_uses_wire_type_discriminator() {
        let tool = ChatTool {
            tool_type: "function".to_string(),
            function: ChatToolFunction {
                name: "dummy_tool".to_string(),
                description: None,
                parameters: serde_json::json!({}),
            },
        };
        let value = serde_json::to_value(&tool).expect("serialize");
        assert_eq!(value["type"], "function");
        assert!(value.get("tool_type").is_none());
        assert!(value["function"].get("description").is_none());
    }
}
