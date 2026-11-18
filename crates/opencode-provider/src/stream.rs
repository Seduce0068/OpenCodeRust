use crate::provider::ProviderError;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    /// Stream has started.
    Start,
    /// Incremental text content.
    TextDelta(String),
    /// Start of a text block.
    TextStart,
    /// End of a text block.
    TextEnd,
    /// Start of a reasoning/thinking block.
    ReasoningStart {
        id: String,
    },
    /// Incremental reasoning text.
    ReasoningDelta {
        id: String,
        text: String,
    },
    /// End of a reasoning/thinking block.
    ReasoningEnd {
        id: String,
    },
    /// Start of tool input streaming (tool-input-start in TS).
    ToolInputStart {
        id: String,
        tool_name: String,
    },
    /// Incremental tool input JSON (tool-input-delta in TS).
    ToolInputDelta {
        id: String,
        delta: String,
    },
    /// End of tool input streaming (tool-input-end in TS).
    ToolInputEnd {
        id: String,
    },
    /// Full tool call event (after input is fully assembled).
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        input: String,
    },
    ToolCallEnd {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Tool result received.
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        input: Option<serde_json::Value>,
        output: ToolResultOutput,
    },
    /// Tool error received.
    ToolError {
        tool_call_id: String,
        tool_name: String,
        input: Option<serde_json::Value>,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ToolErrorKind>,
    },
    /// Start of a processing step (maps to start-step in TS).
    StartStep,
    /// End of a processing step with usage info (maps to finish-step in TS).
    FinishStep {
        finish_reason: Option<String>,
        usage: StreamUsage,
        provider_metadata: Option<serde_json::Value>,
    },
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    /// Stream finished (maps to "finish" in TS).
    Finish,
    Done,
    Error(String),
}

/// Type-safe tool error category for streaming tool failures.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    PermissionDenied,
    QuestionRejected,
    ExecutionError,
}

/// Output from a tool result event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultOutput {
    pub output: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<serde_json::Value>>,
}

/// Usage information from a step completion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

pub type StreamResult = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAISSEvent {
    #[serde(default)]
    pub choices: Vec<OpenAIChoice>,
    pub usage: Option<OpenAIUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIChoice {
    #[serde(default)]
    pub delta: Option<OpenAIDelta>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIDelta {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
    pub reasoning_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIToolCall {
    #[serde(default)]
    pub index: u32,
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<OpenAIFunction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAIFunction {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

fn openai_tool_call_id(tc: &OpenAIToolCall) -> String {
    tc.id
        .clone()
        .unwrap_or_else(|| format!("tool-call-{}", tc.index))
}

pub fn parse_openai_sse(data: &str) -> Option<StreamEvent> {
    if data == "[DONE]" {
        return Some(StreamEvent::Done);
    }

    let event: OpenAISSEvent = serde_json::from_str(data).ok()?;

    for choice in event.choices {
        if let Some(delta) = &choice.delta {
            if let Some(content) = &delta.content {
                if !content.is_empty() {
                    return Some(StreamEvent::TextDelta(content.clone()));
                }
            }

            if let Some(tool_calls) = &delta.tool_calls {
                for tc in tool_calls {
                    if let Some(func) = &tc.function {
                        if let Some(name) = &func.name {
                            return Some(StreamEvent::ToolCallStart {
                                id: openai_tool_call_id(tc),
                                name: name.clone(),
                            });
                        }
                        if let Some(args) = &func.arguments {
                            if !args.is_empty() {
                                return Some(StreamEvent::ToolCallDelta {
                                    id: openai_tool_call_id(tc),
                                    input: args.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        if let Some(reason) = &choice.finish_reason {
            if reason == "tool_calls" {
                return Some(StreamEvent::Done);
            }
        }
    }

    if let Some(usage) = event.usage {
        return Some(StreamEvent::Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
        });
    }

    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub index: Option<u32>,
    pub delta: Option<AnthropicDelta>,
    pub content_block: Option<AnthropicContentBlock>,
    pub message: Option<AnthropicMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicDelta {
    #[serde(rename = "type")]
    pub delta_type: Option<String>,
    pub text: Option<String>,
    pub partial_json: Option<String>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// content block 1 つ分の最小限の状態。
#[derive(Debug)]
struct AnthropicBlockState {
    block_type: String,
    id: Option<String>,
}

/// Anthropic SSE ストリームの状態を保持するパーサ。
///
/// `content_block_start` で得た tool_use の id を content block の index ごとに
/// 記憶し、後続の `content_block_delta` の `partial_json` へ正しい id を付与する。
/// また、`content_block_stop` ではストリームを終了させず、`message_stop` または
/// `message_delta` の `stop_reason` でのみ `Done` を返す。
#[derive(Debug, Default)]
pub struct AnthropicSseParser {
    /// content block index ごとのブロック種別と tool_use id。
    blocks: std::collections::HashMap<u32, AnthropicBlockState>,
    /// すでに `Done` を返したかどうか。終端イベントの重複を抑制する。
    finished: bool,
}

impl AnthropicSseParser {
    /// 空の状態でパーサを生成する。
    pub fn new() -> Self {
        Self::default()
    }

    /// SSE の `data:` 行（JSON 文字列）を 1 つ取り込み、対応する `StreamEvent` を返す。
    ///
    /// 終端イベントの後に呼び出された場合は `None` を返す。
    pub fn push(&mut self, data: &str) -> Option<StreamEvent> {
        if self.finished {
            return None;
        }

        let event: AnthropicEvent = serde_json::from_str(data).ok()?;

        match event.event_type.as_str() {
            "message_start" => {
                if let Some(usage) = event.message.and_then(|message| message.usage) {
                    return Some(StreamEvent::Usage {
                        prompt_tokens: usage.input_tokens,
                        completion_tokens: usage.output_tokens,
                    });
                }
            }
            "content_block_start" => {
                if let Some(block) = event.content_block {
                    let index = event.index.unwrap_or(0);
                    self.blocks.insert(
                        index,
                        AnthropicBlockState {
                            block_type: block.block_type.clone(),
                            id: block.id.clone(),
                        },
                    );

                    if block.block_type == "tool_use" {
                        return Some(StreamEvent::ToolCallStart {
                            id: block.id.unwrap_or_default(),
                            name: block.name.unwrap_or_default(),
                        });
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.delta {
                    if let Some(text) = delta.text {
                        return Some(StreamEvent::TextDelta(text));
                    }
                    if let Some(json) = delta.partial_json {
                        let id = event
                            .index
                            .and_then(|index| self.blocks.get(&index))
                            .and_then(|block| block.id.clone())
                            .unwrap_or_default();
                        return Some(StreamEvent::ToolCallDelta { id, input: json });
                    }
                }
            }
            "content_block_stop" => {
                let index = event.index.unwrap_or(0);
                return self.blocks.remove(&index).and_then(|block| {
                    let AnthropicBlockState { block_type, id } = block;
                    match block_type.as_str() {
                        "tool_use" => Some(StreamEvent::ToolInputEnd {
                            id: id.unwrap_or_default(),
                        }),
                        "text" => Some(StreamEvent::TextEnd),
                        _ => None,
                    }
                });
            }
            "message_delta" => {
                if event
                    .delta
                    .as_ref()
                    .and_then(|delta| delta.stop_reason.as_deref())
                    .is_some()
                {
                    self.finished = true;
                    return Some(StreamEvent::Done);
                }
            }
            "message_stop" => {
                self.finished = true;
                return Some(StreamEvent::Done);
            }
            _ => {}
        }

        None
    }
}

/// 既存互換の純粋関数ラッパー。
///
/// 1 回の呼び出しで 1 つの SSE データ行のみを処理し、呼び出しをまたぐ状態を保持しない。
/// そのため `content_block_delta` の `partial_json` に対応する tool_use id は空文字列と
/// なる。複数ブロック／複数 tool_use を正しく対応付けるには `AnthropicSseParser::push`
/// を用いること（呼び出し側 `anthropic.rs` の変更が必要）。
pub fn parse_anthropic_sse(data: &str) -> Option<StreamEvent> {
    AnthropicSseParser::new().push(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openai_sse_uses_fallback_id_for_tool_start() {
        let data =
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"bash"}}]}}]}"#;
        let event = parse_openai_sse(data).expect("event should parse");
        match event {
            StreamEvent::ToolCallStart { id, name } => {
                assert_eq!(id, "tool-call-2");
                assert_eq!(name, "bash");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn parse_openai_sse_uses_fallback_id_for_tool_delta() {
        let data = r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"arguments":"{\"x\":1}"}}]}}]}"#;
        let event = parse_openai_sse(data).expect("event should parse");
        match event {
            StreamEvent::ToolCallDelta { id, input } => {
                assert_eq!(id, "tool-call-2");
                assert_eq!(input, "{\"x\":1}");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    /// SSE データ行の列を状態付きパーサへ通し、発行されたイベント列を返す。
    fn anthropic_events(lines: &[&str]) -> Vec<StreamEvent> {
        let mut parser = AnthropicSseParser::new();
        lines.iter().filter_map(|line| parser.push(line)).collect()
    }

    #[test]
    fn anthropic_parser_maps_partial_json_to_tool_use_id_across_blocks() {
        let lines = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":12,"output_tokens":0}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_2","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"/tmp\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":"Done"}}"#,
            r#"{"type":"content_block_stop","index":3}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":1},"usage":{"output_tokens":20}}"#,
        ];

        let events = anthropic_events(&lines);

        // Done は末尾の 1 回だけ。content_block_stop で早期終了しないこと。
        let done_indices: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(i, event)| matches!(event, StreamEvent::Done).then_some(i))
            .collect();
        assert_eq!(done_indices.len(), 1);
        assert_eq!(done_indices[0], events.len() - 1);

        // 2 つの tool_use の id が partial_json に正しく対応付くこと。
        let tool_deltas: Vec<(&str, &str)> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallDelta { id, input } => Some((id.as_str(), input.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_deltas,
            vec![
                ("toolu_1", "{\"cmd\":"),
                ("toolu_1", "\"ls\"}"),
                ("toolu_2", "{\"path\":"),
                ("toolu_2", "\"/tmp\"}"),
            ]
        );

        // ブロック終了イベントが content_block_stop ごとに発行されること。
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextEnd)));
        let tool_input_ends: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolInputEnd { id } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_input_ends, vec!["toolu_1", "toolu_2"]);

        // 後続ブロックのテキストも届くこと（早期 Done の回帰検証）。
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello", "Done"]);
    }

    #[test]
    fn anthropic_wrapper_only_finishes_on_terminal_events() {
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        assert!(
            !matches!(parse_anthropic_sse(block_stop), Some(StreamEvent::Done)),
            "content_block_stop は Done を返さないこと"
        );

        let message_delta_stop = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#;
        assert!(matches!(
            parse_anthropic_sse(message_delta_stop),
            Some(StreamEvent::Done)
        ));

        let message_stop = r#"{"type":"message_stop"}"#;
        assert!(matches!(
            parse_anthropic_sse(message_stop),
            Some(StreamEvent::Done)
        ));
    }

    #[test]
    fn anthropic_stateful_parser_suppresses_duplicate_done() {
        let mut parser = AnthropicSseParser::new();
        let stop = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#;
        assert!(matches!(parser.push(stop), Some(StreamEvent::Done)));
        assert!(parser.push(r#"{"type":"message_stop"}"#).is_none());
    }

    #[test]
    fn anthropic_wrapper_retains_existing_text_and_tool_start_events() {
        let text =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#;
        assert!(matches!(
            parse_anthropic_sse(text),
            Some(StreamEvent::TextDelta(ref s)) if s.as_str() == "hi"
        ));

        let start = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_x","name":"bash"}}"#;
        match parse_anthropic_sse(start) {
            Some(StreamEvent::ToolCallStart { id, name }) => {
                assert_eq!(id, "toolu_x");
                assert_eq!(name, "bash");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }
}
