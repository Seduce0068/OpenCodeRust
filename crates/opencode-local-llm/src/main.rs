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

/// パイプを EOF まで読み切る（パイプが無い場合は何もしない）。
async fn read_pipe<R: AsyncRead + Unpin>(
    pipe: &mut Option<R>,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    if let Some(pipe) = pipe.as_mut() {
        pipe.read_to_end(buf).await?;
    }
    Ok(())
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
                "local_llm_query" => call_local_llm(arguments).await?,
                "local_llm_list_models" => list_local_models().await?,
                _ => {
                    return Ok(error_response(
                        id,
                        -32601,
                        &format!("Unknown tool: {}", tool_name),
                    ))
                }
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

async fn call_local_llm(args: Value) -> Result<String, Box<dyn std::error::Error>> {
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or("model required")?;
    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or("prompt required")?;
    let system = args.get("system").and_then(|v| v.as_str()).unwrap_or("");

    let mut cmd = Command::new("ollama");
    cmd.args(["run", model]);
    if !system.is_empty() {
        cmd.env("OLLAMA_SYSTEM", system);
    }
    cmd.arg(prompt);

    let output = output_with_timeout(cmd, command_timeout()).await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!("ollama error: {}", String::from_utf8_lossy(&output.stderr)).into())
    }
}

async fn list_local_models() -> Result<String, Box<dyn std::error::Error>> {
    let mut cmd = Command::new("ollama");
    cmd.args(["list"]);

    let output = output_with_timeout(cmd, command_timeout()).await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "ollama list error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
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
}
