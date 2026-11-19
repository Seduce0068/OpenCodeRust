//! Local LLM MCP Server - Rust implementation
//!
//! Provides tools for querying local LLMs via ollama.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

/// ollama サブプロセスの既定タイムアウト（秒）。
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 300;

/// サブプロセス出力・MCP レスポンスの 1 テキストあたりの最大バイト数（16 MiB）。
///
/// 暴走モデルや入れ替えられた ollama バイナリが際限なく出力しても
/// メモリを食い潰さないための上限。
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// model 名の最大長（文字数）。
const MAX_MODEL_NAME_CHARS: usize = 256;

/// 上限超過時に出力へ付与する、利用者に可視な切り詰めマーカー。
const TRUNCATION_MARKER: &str = "\n... [truncated: output exceeded 16 MiB limit]\n";

/// サブプロセスのタイムアウトを環境変数で上書きできるようにする。
///
/// `OPENCODE_LOCAL_LLM_TIMEOUT_SECS` に正の整数を設定した場合のみ使用し、
/// 不正値・0 の場合は既定値にフォールバックする（設定外部化と堅牢性の両立）。
fn command_timeout() -> Duration {
    std::env::var("OPENCODE_LOCAL_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS))
}

/// 上限を超えない範囲だけ `buf` に蓄積してパイプを読み切る。
///
/// 上限到達後も子プロセスがパイプへの書き込みでブロックしてデッドロックするのを
/// 防ぐため、残りのバイトは小さな一時バッファで読み捨てる（メモリ使用量は
/// `limit` に抑制される）。蓄積データには [`TRUNCATION_MARKER`] を付与し、
/// 切り詰めが発生したかどうかを返す。
async fn read_pipe_limited<R: AsyncRead + Unpin>(
    pipe: &mut Option<R>,
    buf: &mut Vec<u8>,
    limit: usize,
) -> io::Result<bool> {
    let Some(pipe) = pipe.as_mut() else {
        return Ok(false);
    };

    let mut chunk = [0u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        if buf.len() < limit {
            let room = limit - buf.len();
            let keep = read.min(room);
            buf.extend_from_slice(&chunk[..keep]);
            if keep < read {
                truncated = true;
            }
        } else {
            truncated = true;
        }
        // 上限超過後のバイトは蓄積せず読み捨てる（子プロセスのブロック防止）。
    }
    if truncated {
        buf.extend_from_slice(TRUNCATION_MARKER.as_bytes());
    }
    Ok(truncated)
}

/// パイプを既定上限（[`MAX_OUTPUT_BYTES`]）付きで読み切る。
async fn read_pipe<R: AsyncRead + Unpin>(
    pipe: &mut Option<R>,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    read_pipe_limited(pipe, buf, MAX_OUTPUT_BYTES).await?;
    Ok(())
}

/// 文字列を最大 `limit` バイトに切り詰め、超過分をマーカーで置き換える。
///
/// MCP レスポンスへ巨大テキストが流入しないための最終ゲート。
/// UTF-8 の文字境界を尊重し、境界上で `limit` 以下まで切り下げる。
fn truncate_utf8(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + TRUNCATION_MARKER.len());
    out.push_str(&text[..end]);
    out.push_str(TRUNCATION_MARKER);
    out
}

/// ollama の model 名として安全な文字列かどうかを検証する。
///
/// `-` で始まる値が ollama CLI のオプションとして解釈されるのを防ぐため、
/// 先頭は英数字、それ以降は英数字と `:` `-` `_` `.` `/` のみを許可する。
/// 実在の `q4_K_M` のような大文字の量子化タグや `library/llama3` のような
/// 名前空間付き名も受け付ける。
fn is_valid_model_name(model: &str) -> bool {
    if model.is_empty() || model.chars().count() > MAX_MODEL_NAME_CHARS {
        return false;
    }
    let mut chars = model.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | '/'))
}

/// `tools/call` の失敗を JSON-RPC エラーへ変換するための型。
#[derive(Debug)]
struct ToolCallError {
    code: i32,
    message: String,
}

impl ToolCallError {
    /// 不正な引数（JSON-RPC: Invalid params / -32602）。
    fn invalid_arguments(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code: -32602,
            message: format!("InvalidArguments: {}", message),
        }
    }

    /// その他の内部エラー（JSON-RPC: Internal error / -32603）。
    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            code: -32603,
            message: error.to_string(),
        }
    }
}

/// 子プロセスをタイムアウト付きで実行し、結果を返す。
///
/// タイムアウト時は `kill_on_drop` に加えて明示的に `kill()`（SIGKILL + reap）を
/// 呼び、orphan プロセスを残さない。`wait()` は `&mut Child` を取るため、
/// タイムアウトで Future をドロップしても `Child` の所有権が残り kill できる。
/// （`wait_with_output()` は `Child` を move するため使用しない。）
async fn output_with_timeout(
    mut cmd: Command,
    timeout_duration: Duration,
) -> Result<Output, Box<dyn std::error::Error>> {
    // `std::process::Command::output()` と同じく stdout/stderr を pipe に接続する。
    // tokio の `spawn()` は既定で pipe を設定しないため、明示指定が必要。
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    // 待機とパイプ読み取りを並列化する。直列にすると出力がパイプ容量を
    // 超えた瞬間に子プロセスが書き込みでブロックし、正常系がタイムアウトする。
    let collect = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let (status, _, _) = tokio::try_join!(
            child.wait(),
            read_pipe(&mut stdout_pipe, &mut stdout),
            read_pipe(&mut stderr_pipe, &mut stderr),
        )?;
        Ok::<Output, io::Error>(Output {
            status,
            stdout,
            stderr,
        })
    };

    match timeout(timeout_duration, collect).await {
        Ok(result) => Ok(result?),
        Err(_) => {
            let _ = child.kill().await;
            Err(format!("ollama command timed out after {:?}", timeout_duration).into())
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: Value,
    #[serde(rename = "clientInfo")]
    client_info: Value,
}

#[derive(Debug, Serialize)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
}

#[derive(Debug, Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    let mut line = String::new();

    let mut initialized = false;

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break; // EOF
        }

        let line_trimmed = line.trim();
        if line_trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(line_trimmed) {
            Ok(req) => req,
            Err(e) => {
                send_error(
                    &mut stdout,
                    None,
                    -32700,
                    "Parse error",
                    Some(json!(e.to_string())),
                )?;
                continue;
            }
        };

        let response = handle_request(request, &mut initialized).await?;
        let response_json = serde_json::to_string(&response)?;
        writeln!(stdout, "{}", response_json)?;
        stdout.flush()?;
    }

    Ok(())
}

fn send_error(
    stdout: &mut io::Stdout,
    id: Option<Value>,
    code: i32,
    message: &str,
    data: Option<Value>,
) -> io::Result<()> {
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
            data,
        }),
    };
    let response_json = serde_json::to_string(&response)?;
    writeln!(stdout, "{}", response_json)?;
    stdout.flush()
}

async fn handle_request(
    request: JsonRpcRequest,
    initialized: &mut bool,
) -> Result<JsonRpcResponse, Box<dyn std::error::Error>> {
    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => {
            let _params: InitializeParams =
                serde_json::from_value(request.params.unwrap_or(json!({})))?;
            *initialized = true;
            Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!(InitializeResult {
                    protocol_version: "2024-11-05".to_string(),
                    capabilities: ServerCapabilities {
                        tools: ToolsCapability {
                            list_changed: false
                        },
                    },
                    server_info: ServerInfo {
                        name: "opencode-local-llm".to_string(),
                        version: "0.1.0".to_string(),
                    },
                })),
                error: None,
            })
        }
        "initialized" => Ok(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: Some(json!({})),
            error: None,
        }),
        "tools/list" => {
            if !*initialized {
                return Ok(error_response(id, -32002, "Server not initialized"));
            }
            Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "tools": [
                        Tool {
                            name: "local_llm_query".to_string(),
                            description: "Query a local LLM via ollama".to_string(),
                            input_schema: json!({
                                "type": "object",
                                "properties": {
                                    "model": { "type": "string", "description": "Model name (e.g., gemma4:4b)" },
                                    "prompt": { "type": "string", "description": "Input prompt" },
                                    "system": { "type": "string", "description": "System prompt (optional)" }
                                },
                                "required": ["model", "prompt"]
                            }),
                        },
                        Tool {
                            name: "local_llm_list_models".to_string(),
                            description: "List available local models".to_string(),
                            input_schema: json!({ "type": "object", "properties": {} }),
                        }
                    ]
                })),
                error: None,
            })
        }
        "tools/call" => {
            if !*initialized {
                return Ok(error_response(id, -32002, "Server not initialized"));
            }
            let params = request.params.unwrap_or(json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let result = match tool_name {
                "local_llm_query" => call_local_llm(arguments).await,
                "local_llm_list_models" => list_local_models().await,
                _ => {
                    return Ok(error_response(
                        id,
                        -32601,
                        &format!("Unknown tool: {}", tool_name),
                    ))
                }
            };
            // ツール失敗でサーバ全体が終了しないよう JSON-RPC エラーへ変換する。
            // 併せて MCP レスポンス本文にも上限を適用し、巨大テキストが
            // クライアントのコンテキストへ流入しないようにする。
            let result = match result {
                Ok(text) => truncate_utf8(&text, MAX_OUTPUT_BYTES),
                Err(err) => return Ok(error_response(id, err.code, &err.message)),
            };

            Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "content": [{ "type": "text", "text": result }],
                    "isError": false
                })),
                error: None,
            })
        }
        _ => Ok(error_response(
            id,
            -32601,
            &format!("Method not found: {}", request.method),
        )),
    }
}

fn error_response(id: Option<Value>, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
            data: None,
        }),
    }
}

async fn call_local_llm(args: Value) -> Result<String, ToolCallError> {
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolCallError::invalid_arguments("model required"))?;
    if !is_valid_model_name(model) {
        return Err(ToolCallError::invalid_arguments(format!(
            "invalid model name: {:?}",
            model
        )));
    }
    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolCallError::invalid_arguments("prompt required"))?;
    let system = args.get("system").and_then(|v| v.as_str()).unwrap_or("");

    let mut cmd = Command::new("ollama");
    // ollama CLI（cobra）のオプション終端記号。モデル名を検証済みでも、
    // 位置引数がオプションとして解釈されないよう二重に防御する。
    cmd.args(["run", "--", model]);
    if !system.is_empty() {
        cmd.env("OLLAMA_SYSTEM", system);
    }
    cmd.arg(prompt);

    let output = output_with_timeout(cmd, command_timeout())
        .await
        .map_err(ToolCallError::internal)?;
    if output.status.success() {
        Ok(truncate_utf8(
            String::from_utf8_lossy(&output.stdout).trim(),
            MAX_OUTPUT_BYTES,
        ))
    } else {
        Err(ToolCallError::internal(format!(
            "ollama error: {}",
            truncate_utf8(&String::from_utf8_lossy(&output.stderr), MAX_OUTPUT_BYTES)
        )))
    }
}

async fn list_local_models() -> Result<String, ToolCallError> {
    let mut cmd = Command::new("ollama");
    cmd.args(["list"]);

    let output = output_with_timeout(cmd, command_timeout())
        .await
        .map_err(ToolCallError::internal)?;
    if output.status.success() {
        Ok(truncate_utf8(
            &String::from_utf8_lossy(&output.stdout),
            MAX_OUTPUT_BYTES,
        ))
    } else {
        Err(ToolCallError::internal(format!(
            "ollama list error: {}",
            truncate_utf8(&String::from_utf8_lossy(&output.stderr), MAX_OUTPUT_BYTES)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_builds_json_rpc_error() {
        let response = error_response(Some(json!(7)), -32601, "Method not found");
        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, Some(json!(7)));
        assert!(response.result.is_none());
        let error = response.error.expect("error should be present");
        assert_eq!(error.code, -32601);
        assert_eq!(error.message, "Method not found");
        assert!(error.data.is_none());
    }

    #[test]
    fn error_response_allows_null_id() {
        let response = error_response(None, -32600, "Invalid request");
        assert!(response.id.is_none());
        assert_eq!(response.error.expect("error").code, -32600);
    }

    #[test]
    fn error_response_serializes_without_result() {
        let response = error_response(Some(json!("id")), -1, "fail");
        let value = serde_json::to_value(response).expect("serialize");
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], "id");
        assert!(value.get("result").is_none());
        assert_eq!(value["error"]["code"], -1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_with_timeout_returns_error_promptly_on_timeout() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let started = std::time::Instant::now();
        let err = output_with_timeout(cmd, Duration::from_millis(50))
            .await
            .expect_err("sleep 30 should exceed the 50ms timeout");
        assert!(err.to_string().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must return promptly, elapsed: {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_with_timeout_returns_stdout_on_success() {
        let mut cmd = Command::new("echo");
        cmd.arg("ok");
        let output = output_with_timeout(cmd, Duration::from_secs(5))
            .await
            .expect("echo should complete successfully");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_with_timeout_handles_output_larger_than_pipe_buffer() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 200000 /dev/zero | tr '\\0' x"]);
        let output = output_with_timeout(cmd, Duration::from_secs(10))
            .await
            .expect("large output should complete without blocking on the pipe");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200000);
    }

    #[test]
    fn model_name_grammar_accepts_real_world_names() {
        for name in [
            "gemma4:4b",
            "llama3.2",
            "qwen2.5:14b-instruct-q4_K_M",
            "library/llama3",
            "hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF",
        ] {
            assert!(is_valid_model_name(name), "should accept: {}", name);
        }
    }

    #[test]
    fn model_name_grammar_rejects_unsafe_names() {
        for name in [
            "",
            "-v",
            "--verbose",
            "model name",
            "model;rm",
            "model\nfoo",
            ".hidden",
            "/etc/passwd",
        ] {
            assert!(!is_valid_model_name(name), "should reject: {:?}", name);
        }
        let too_long = "a".repeat(MAX_MODEL_NAME_CHARS + 1);
        assert!(!is_valid_model_name(too_long.as_str()));
    }

    #[tokio::test]
    async fn call_local_llm_rejects_invalid_model_without_spawning_ollama() {
        let args = json!({ "model": "--verbose", "prompt": "hi" });
        let err = call_local_llm(args)
            .await
            .expect_err("leading '-' model must be rejected before spawning");
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("InvalidArguments"));
        assert!(err.message.contains("--verbose"));
    }

    #[tokio::test]
    async fn handle_request_returns_invalid_arguments_for_bad_model() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(42)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "local_llm_query",
                "arguments": { "model": "-h", "prompt": "hi" }
            })),
        };
        let mut initialized = true;
        let response = handle_request(request, &mut initialized)
            .await
            .expect("handler should return a JSON-RPC response, not terminate the server");
        let value = serde_json::to_value(response).expect("serialize");
        assert_eq!(value["error"]["code"], -32602);
        assert!(value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("InvalidArguments"));
        assert!(value.get("result").is_none());
    }

    #[tokio::test]
    async fn read_pipe_limited_caps_huge_output_and_appends_marker() {
        // 256 KiB を 1 KiB の上限で読ませ、上限を超えて蓄積されないことを検証。
        let data = vec![b'x'; 256 * 1024];
        let mut pipe: Option<&[u8]> = Some(data.as_slice());
        let mut buf = Vec::new();
        let truncated = read_pipe_limited(&mut pipe, &mut buf, 1024)
            .await
            .expect("read should succeed");
        assert!(truncated);
        assert_eq!(buf.len(), 1024 + TRUNCATION_MARKER.len());
        assert!(buf.ends_with(TRUNCATION_MARKER.as_bytes()));
    }

    #[tokio::test]
    async fn read_pipe_limited_keeps_all_bytes_when_under_limit() {
        let data = vec![b'a'; 512];
        let expected = data.clone();
        let mut pipe: Option<&[u8]> = Some(data.as_slice());
        let mut buf = Vec::new();
        let truncated = read_pipe_limited(&mut pipe, &mut buf, 1024)
            .await
            .expect("read should succeed");
        assert!(!truncated);
        assert_eq!(buf, expected);
    }

    #[test]
    fn truncate_utf8_limits_oversized_mcp_text() {
        let text = "a".repeat(2048);
        let out = truncate_utf8(&text, 1024);
        assert!(out.ends_with(TRUNCATION_MARKER));
        assert_eq!(out.len(), 1024 + TRUNCATION_MARKER.len());
        assert_eq!(truncate_utf8("short", 1024), "short");
    }

    #[test]
    fn truncate_utf8_keeps_utf8_char_boundaries() {
        let text = "あ".repeat(100); // 1 文字 3 バイト
        let out = truncate_utf8(&text, 10);
        let marker_start = out.len() - TRUNCATION_MARKER.len();
        assert!(std::str::from_utf8(&out.as_bytes()[..marker_start]).is_ok());
        assert!(out.ends_with(TRUNCATION_MARKER));
    }
}
