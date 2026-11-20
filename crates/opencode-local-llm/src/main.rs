//! Local LLM MCP Server - Rust implementation
//!
//! Provides tools for querying local LLMs via ollama.
//!
//! # 配線状況
//!
//! 本クレートは現行の実行系で利用中である。リポジトリ直下 `opencode.jsonc` の
//! `mcp.local-llm` が `enabled: true` で
//! `command = ["cargo", "run", "--bin", "opencode-local-llm"]` として設定されており、
//! stdio MCP サーバとして起動される（stdin の JSON-RPC 行を読み、stdout へ応答を書く）。
//!
//! 「Rust クレートからライブラリ依存されていない」ことと「実行系で使われない」は別である。
//! 前者は Cargo の依存グラフ上、他クレートが本クレートをライブラリとして依存していない
//! という事実であり、後者は実行時にプロセスとして起動されないという事実である。
//! 本クレートは前者に該当するが、上記のとおり stdio MCP サーバとして起動されるため
//! 後者には該当せず、現行の実行系から利用される。
//!
//! 本次変更ではコードの挙動を一切変更しない。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;

/// ollama サブプロセスの既定タイムアウト（秒）。
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 300;

/// 同時に実行する `tools/call` の上限。超過時はサブプロセスを起動せずに
/// 即座 JSON-RPC エラーを返し、ollama プロセスの暴走によるリソース枯渇を防ぐ。
const MAX_CONCURRENT_TOOL_CALLS: usize = 8;

/// 同時実行上限超過時に返す JSON-RPC サーバ定義エラーコード（-32000 番台）。
const TOOL_CALL_LIMIT_EXCEEDED_CODE: i32 = -32000;

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
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();

    // stdin のブロッキング読み取りを専用スレッドへ退避する。通常スレッドなので
    // main が戻った時点でプロセスは終了し、shutdown 時に read_line の待機が
    // 終了を妨げない。行はチャネルへ渡し、ループ側は select! で推論完了と
    // 新着リクエストを等しく監視する。
    let _stdin_reader = std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if line_tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    serve_requests(
        line_rx,
        |response: JsonRpcResponse| {
            let json = serde_json::to_string(&response).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "failed to serialize JSON-RPC response",
                )
            })?;
            writeln!(stdout, "{}", json)?;
            stdout.flush()
        },
        |request: JsonRpcRequest, initialized: bool| async move {
            let mut initialized = initialized;
            respond_or_error(request, &mut initialized).await
        },
    )
    .await
}

/// 入力行チャネルを読み、軽量リクエストは同期的に、長時間かかる `tools/call` は
/// `tokio::spawn` で並列に処理するメインループ。
///
/// バックグラウンドタスクはレスポンスを `mpsc` チャネルへ送り、ループが
/// 単一の stdout 書き込み位置から出力する。各レスポンスは JSON-RPC の `id` を
/// 保持したまま多重化されるため、完了順が入れ替わっても対応が崩れない。
/// `shutdown` / `initialize` / `tools/list` は推論の完了を待たずに処理され、
/// `shutdown` を受け取った時点で未完了タスクを abort してループを抜ける。
async fn serve_requests<F, Fut>(
    mut requests: mpsc::UnboundedReceiver<String>,
    mut emit: impl FnMut(JsonRpcResponse) -> io::Result<()>,
    run_tool_call: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(JsonRpcRequest, bool) -> Fut,
    Fut: Future<Output = JsonRpcResponse> + Send + 'static,
{
    let mut initialized = false;
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<JsonRpcResponse>();
    let mut tasks = JoinSet::new();
    // 同時 tools/call 数の上限。permit は spawn したタスクへ移し、
    // 完了（panic 時の unwind を含む）まで保持させて並列度を制限する。
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_TOOL_CALLS));
    // 入力が閉じた後に待つべき推論タスク数。正常タスクはレスポンス送信後に
    // 完了するためレスポンス受信で減算するが、panic してレスポンスを送らない
    // タスクは join_next の JoinError で減算する（EOF 後のハング防止）。
    let mut in_flight: usize = 0;
    let mut input_closed = false;

    loop {
        // EOF 済みで未完了タスクが無ければ、以降に待つイベントは存在しない。
        if input_closed && in_flight == 0 {
            break;
        }
        // join_next のガードは bool で渡し、tasks の可変借用と競合させない。
        let has_tasks = !tasks.is_empty();

        tokio::select! {
            biased;

            Some(response) = response_rx.recv() => {
                in_flight = in_flight.saturating_sub(1);
                emit(response)?;
            }

            maybe_line = requests.recv(), if !input_closed => {
                let Some(line) = maybe_line else {
                    // 入力終了。以降は未完了タスクのレスポンスだけを待つ。
                    input_closed = true;
                    continue;
                };

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
                    Ok(request) => request,
                    Err(_) => {
                        // パースエラーの詳細には入力断片が含まれ得るため、
                        // 固定文言のみを返し、prompt/token をエラーへ載せない。
                        emit(parse_error_response())?;
                        continue;
                    }
                };

                if request.method == "tools/call" {
                    // 同時実行数に上限を設け、超過時はサブプロセスを起動せず
                    // JSON-RPC エラーを即座に返す（リソース枯渇の防止）。
                    match permits.clone().try_acquire_owned() {
                        Ok(permit) => {
                            // 300 秒の ollama 待機でループを止めない。spawn 時点の
                            // initialized 状態を渡し、初期化前の呼び出しも確実に拒否する。
                            let future = run_tool_call(request, initialized);
                            let tx = response_tx.clone();
                            tasks.spawn(async move {
                                // permit は完了（panic 時の unwind を含む）まで保持する。
                                let _permit = permit;
                                let response = future.await;
                                let _ = tx.send(response);
                            });
                            in_flight += 1;
                        }
                        Err(_) => {
                            let id = request.id.clone();
                            emit(error_response(
                                id,
                                TOOL_CALL_LIMIT_EXCEEDED_CODE,
                                "Too many concurrent tool calls",
                            ))?;
                        }
                    }
                    continue;
                }

                let is_shutdown = request.method == "shutdown";
                let response = respond_or_error(request, &mut initialized).await;
                emit(response)?;
                if is_shutdown {
                    tasks.abort_all();
                    return Ok(());
                }
            }

            // タスクの完了を監視する。panic してレスポンスを送らなかった
            // タスク（JoinError）も完了として数え、in_flight を減算する。
            // これによりレスポンスが届かないタスクが残っても EOF 後にハングしない。
            completed = tasks.join_next(), if has_tasks => {
                if let Some(Err(_join_error)) = completed {
                    in_flight = in_flight.saturating_sub(1);
                }
                // 正常完了 (Ok) はレスポンス受信側で減算済みのため、ここでは減算しない。
            }
        }
    }

    // shutdown / EOF で推論中の ollama 子プロセスも即座に終了させる。
    // kill_on_drop により abort 時に子プロセスが kill される。
    tasks.abort_all();
    Ok(())
}

/// `handle_request` の失敗を JSON-RPC エラーへ変換する。
///
/// エラーの本文は固定文言とし、prompt / token を含む可能性のある文字列を
/// クライアントやログへ出さない。
async fn respond_or_error(request: JsonRpcRequest, initialized: &mut bool) -> JsonRpcResponse {
    let id = request.id.clone();
    match handle_request(request, initialized).await {
        Ok(response) => response,
        Err(_) => error_response(id, -32603, "Internal error"),
    }
}

/// 入力が不正な JSON だった場合の固定エラーレスポンス。
fn parse_error_response() -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: None,
        result: None,
        error: Some(JsonRpcError {
            code: -32700,
            message: "Parse error".to_string(),
            data: Some(json!("invalid JSON-RPC request")),
        }),
    }
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
        "shutdown" => Ok(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
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

    // --- 直列処理バグの回帰テスト -------------------------------------
    // 実際の ollama / 外部プロセスを起動せず、ガート付きのフェイク・ハンドラで
    // 「推論中」を再現する。確定性的にタイムアウト付きで検証する。

    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{broadcast, oneshot};
    use tokio::time::timeout;

    fn rpc_line(id: i32, method: &str, params: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        })
        .to_string()
    }

    fn initialize_line(id: i32) -> String {
        rpc_line(
            id,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }),
        )
    }

    fn tool_call_line(id: i32) -> String {
        rpc_line(
            id,
            "tools/call",
            json!({
                "name": "local_llm_query",
                "arguments": { "model": "test-model", "prompt": "hello" }
            }),
        )
    }

    async fn next_response(rx: &mut mpsc::UnboundedReceiver<JsonRpcResponse>) -> JsonRpcResponse {
        timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("response must arrive without waiting for inference")
            .expect("response channel must stay open")
    }

    type BoxedResponseFuture = Pin<Box<dyn Future<Output = JsonRpcResponse> + Send>>;

    fn boxed_future<F>(future: F) -> BoxedResponseFuture
    where
        F: Future<Output = JsonRpcResponse> + Send + 'static,
    {
        Box::pin(future)
    }

    /// 推論完了までガートで待たせ、開始を oneshot で通知するフェイク。
    fn gated_tool_handler(
        started: oneshot::Sender<()>,
        gate: oneshot::Receiver<()>,
    ) -> impl Fn(JsonRpcRequest, bool) -> BoxedResponseFuture + Send + 'static {
        let started = Arc::new(Mutex::new(Some(started)));
        let gate = Arc::new(Mutex::new(Some(gate)));
        move |request: JsonRpcRequest, _initialized: bool| {
            let id = request.id.clone();
            let started = Arc::clone(&started);
            let gate = Arc::clone(&gate);
            boxed_future(async move {
                if let Some(tx) = started.lock().expect("lock").take() {
                    let _ = tx.send(());
                }
                let gate_rx = gate.lock().expect("lock").take();
                if let Some(rx) = gate_rx {
                    let _ = rx.await;
                }
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({ "content": [] })),
                    error: None,
                }
            })
        }
    }

    #[tokio::test]
    async fn inference_does_not_block_followup_requests() {
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let (emit_tx, mut emit_rx) = mpsc::unbounded_channel::<JsonRpcResponse>();
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (gate_tx, gate_rx) = oneshot::channel::<()>();

        line_tx.send(initialize_line(1)).expect("send");
        line_tx.send(tool_call_line(2)).expect("send");

        let server = tokio::spawn(async move {
            serve_requests(
                line_rx,
                |response: JsonRpcResponse| {
                    emit_tx
                        .send(response)
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver closed"))
                },
                gated_tool_handler(started_tx, gate_rx),
            )
            .await
        });

        // 全行を事前投入して sender を閉じると、ループがバッファを一気に
        // 処理して EOF に達し、spawn 直後のタスクが poll 前に abort される。
        // まず開始を確認してから後続リクエストを送る。
        timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("tool call must be started in the background")
            .expect("started signal");

        line_tx
            .send(rpc_line(3, "tools/list", json!({})))
            .expect("send");

        // 推論がガートで止まっている間に initialize と tools/list が応答する。
        let first = next_response(&mut emit_rx).await;
        assert_eq!(first.id, Some(json!(1)));
        let list = next_response(&mut emit_rx).await;
        assert_eq!(
            list.id,
            Some(json!(3)),
            "tools/list must not wait for inference"
        );
        assert!(
            list.result.is_some(),
            "tools/list must succeed during inference"
        );

        gate_tx.send(()).expect("release inference gate");
        let tool = next_response(&mut emit_rx).await;
        assert_eq!(tool.id, Some(json!(2)));

        drop(line_tx);
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server must exit after EOF")
            .expect("serve_requests must succeed")
            .expect("inner result must succeed");
    }

    #[tokio::test]
    async fn shutdown_is_handled_immediately_during_inference() {
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let (emit_tx, mut emit_rx) = mpsc::unbounded_channel::<JsonRpcResponse>();
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (_gate_tx, gate_rx) = oneshot::channel::<()>();

        line_tx.send(initialize_line(1)).expect("send");
        line_tx.send(tool_call_line(2)).expect("send");

        let server = tokio::spawn(async move {
            serve_requests(
                line_rx,
                |response: JsonRpcResponse| {
                    emit_tx
                        .send(response)
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver closed"))
                },
                gated_tool_handler(started_tx, gate_rx),
            )
            .await
        });

        // 推論タスクが開始する前に shutdown を処理すると abort で started が
        // 届かないため、開始を確認してから shutdown を投入する。
        timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("tool call must be started in the background")
            .expect("started signal");

        line_tx
            .send(rpc_line(3, "shutdown", json!({})))
            .expect("send");

        let first = next_response(&mut emit_rx).await;
        assert_eq!(first.id, Some(json!(1)));
        let shutdown = next_response(&mut emit_rx).await;
        assert_eq!(shutdown.id, Some(json!(3)));
        assert!(shutdown.result.is_some());
        assert!(shutdown.error.is_none());

        // ガートを解放しないまま、つまり推論完了を待たずにループが終了する。
        timeout(Duration::from_secs(2), server)
            .await
            .expect("shutdown must stop the loop without waiting for the 300s inference")
            .expect("serve_requests must succeed")
            .expect("inner result must succeed");
    }

    #[tokio::test]
    async fn responses_are_multiplexed_by_id_when_tools_finish_out_of_order() {
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let (emit_tx, mut emit_rx) = mpsc::unbounded_channel::<JsonRpcResponse>();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<i32>();
        let (gate2_tx, gate2_rx) = oneshot::channel::<()>();
        let (gate3_tx, gate3_rx) = oneshot::channel::<()>();

        line_tx.send(initialize_line(1)).expect("send");
        line_tx.send(tool_call_line(2)).expect("send");
        line_tx.send(tool_call_line(3)).expect("send");
        // sender は両方の推論が開始するまで保持する（早期 EOF で abort されないように）。

        let gates = Arc::new(Mutex::new(std::collections::HashMap::from([
            (2, gate2_rx),
            (3, gate3_rx),
        ])));

        let server = tokio::spawn(async move {
            serve_requests(
                line_rx,
                |response: JsonRpcResponse| {
                    emit_tx
                        .send(response)
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver closed"))
                },
                move |request: JsonRpcRequest, _initialized: bool| {
                    let id = request.id.clone();
                    let number = id.as_ref().and_then(Value::as_i64).unwrap_or(0) as i32;
                    let started_tx = started_tx.clone();
                    let gates = Arc::clone(&gates);
                    async move {
                        let _ = started_tx.send(number);
                        let gate = gates.lock().expect("lock").remove(&number);
                        if let Some(rx) = gate {
                            let _ = rx.await;
                        }
                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({ "content": [] })),
                            error: None,
                        }
                    }
                },
            )
            .await
        });

        // 2 つの推論が両方開始するのを待つ（順序は問わない）。
        for _ in 0..2 {
            timeout(Duration::from_secs(2), started_rx.recv())
                .await
                .expect("tool calls must start")
                .expect("started signal");
        }
        let first = next_response(&mut emit_rx).await;
        assert_eq!(first.id, Some(json!(1)));

        // id=3 を先に完了させても、レスポンスの id は対応したまま届く。
        gate3_tx.send(()).expect("release id=3");
        let third = next_response(&mut emit_rx).await;
        assert_eq!(third.id, Some(json!(3)));
        gate2_tx.send(()).expect("release id=2");
        let second = next_response(&mut emit_rx).await;
        assert_eq!(second.id, Some(json!(2)));

        drop(line_tx);
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server must exit after EOF")
            .expect("serve_requests must succeed")
            .expect("inner result must succeed");
    }

    /// 回帰テスト: 推論タスクが panic してレスポンスを送らない場合でも、
    /// join_next の JoinError で in_flight が減算され EOF 後にハングしない。
    #[tokio::test]
    async fn server_exits_after_eof_when_tool_task_panics() {
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let (emit_tx, mut emit_rx) = mpsc::unbounded_channel::<JsonRpcResponse>();
        let (started_tx, started_rx) = oneshot::channel::<()>();

        line_tx.send(initialize_line(1)).expect("send");
        line_tx.send(tool_call_line(2)).expect("send");

        let started = Arc::new(Mutex::new(Some(started_tx)));
        let server = tokio::spawn(async move {
            serve_requests(
                line_rx,
                |response: JsonRpcResponse| {
                    emit_tx
                        .send(response)
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver closed"))
                },
                move |_request: JsonRpcRequest, _initialized: bool| {
                    let started = Arc::clone(&started);
                    boxed_future(async move {
                        if let Some(tx) = started.lock().expect("lock").take() {
                            let _ = tx.send(());
                        }
                        panic!("simulated tool panic");
                    })
                },
            )
            .await
        });

        // panic する前にタスクが開始したことを確認する。
        timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("tool call must be started in the background")
            .expect("started signal");

        let first = next_response(&mut emit_rx).await;
        assert_eq!(first.id, Some(json!(1)));

        // 推論レスポンスは届かないが、EOF でサーバが終了しなければならない。
        drop(line_tx);
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server must exit after EOF even when a tool task panicked")
            .expect("serve_requests must succeed")
            .expect("inner result must succeed");
    }

    /// 回帰テスト: 同時 tools/call が上限に達した時、以降の呼び出しは
    /// サブプロセスを起動せず JSON-RPC エラーとして即座に返る。
    #[tokio::test]
    async fn tool_call_limit_exceeded_returns_json_rpc_error() {
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let (emit_tx, mut emit_rx) = mpsc::unbounded_channel::<JsonRpcResponse>();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<i32>();
        let (gate_tx, _) = broadcast::channel::<()>(1);
        let gate_tx = Arc::new(gate_tx);
        let gate_tx_for_test = Arc::clone(&gate_tx);

        line_tx.send(initialize_line(1)).expect("send");
        for id in 10..10 + MAX_CONCURRENT_TOOL_CALLS as i32 {
            line_tx.send(tool_call_line(id)).expect("send");
        }

        let server = tokio::spawn(async move {
            serve_requests(
                line_rx,
                |response: JsonRpcResponse| {
                    emit_tx
                        .send(response)
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver closed"))
                },
                move |request: JsonRpcRequest, _initialized: bool| {
                    let id = request.id.clone();
                    let number = id.as_ref().and_then(Value::as_i64).unwrap_or(0) as i32;
                    let started_tx = started_tx.clone();
                    let gate_tx = Arc::clone(&gate_tx);
                    async move {
                        let _ = started_tx.send(number);
                        // subscribe は同期的に行い、ゲート解放より前に登録する。
                        let mut gate = gate_tx.subscribe();
                        let _ = gate.recv().await;
                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({ "content": [] })),
                            error: None,
                        }
                    }
                },
            )
            .await
        });

        let init = next_response(&mut emit_rx).await;
        assert_eq!(init.id, Some(json!(1)));

        // 上限いっぱいの推論がすべて開始するまで待つ。
        for _ in 0..MAX_CONCURRENT_TOOL_CALLS {
            timeout(Duration::from_secs(2), started_rx.recv())
                .await
                .expect("tool calls must start")
                .expect("started signal");
        }

        // 上限超過の呼び出しは即座に JSON-RPC エラーになる。
        let overflow_id = 10 + MAX_CONCURRENT_TOOL_CALLS as i32;
        line_tx.send(tool_call_line(overflow_id)).expect("send");
        let overflow = next_response(&mut emit_rx).await;
        assert_eq!(overflow.id, Some(json!(overflow_id)));
        let error = overflow.error.expect("overflow must be a JSON-RPC error");
        assert_eq!(error.code, TOOL_CALL_LIMIT_EXCEEDED_CODE);

        // ガートを解放して保留中の推論を完了させ、サーバを正常終了させる。
        let _ = gate_tx_for_test.send(());
        for _ in 0..MAX_CONCURRENT_TOOL_CALLS {
            let response = next_response(&mut emit_rx).await;
            assert!(
                response.result.is_some(),
                "held tool calls must still complete normally"
            );
        }
        drop(line_tx);
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server must exit after EOF")
            .expect("serve_requests must succeed")
            .expect("inner result must succeed");
    }
}
