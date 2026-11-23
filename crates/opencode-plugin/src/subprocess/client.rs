//! `PluginSubprocess` — manages a single plugin host child process.
//!
//! Communicates via Content-Length framed JSON-RPC 2.0 over stdin/stdout,
//! mirroring the MCP stdio transport pattern.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

use super::protocol::{
    default_protocol_version, validate_jsonrpc_envelope, RpcError, RpcRequest, RpcResponse,
    PROTOCOL_VERSION,
};
use super::runtime::JsRuntime;

/// 受信する JSON-RPC フレーム本体の上限（10 MiB）。
const MAX_FRAME_SIZE: usize = 10 * 1024 * 1024;
/// 1 フレームのヘッダ行数の上限（空行を含む）。
const MAX_HEADER_LINES: usize = 64;
/// 1 ヘッダ行の最大長（改行を除く）。値はエラー本文に含めない。
const MAX_HEADER_LINE_BYTES: usize = 8192;
/// auth.fetch.stream のチャンクキュー容量（本番と回帰テストで共有）。
const STREAM_CHUNK_BUFFER: usize = 128;
/// ストリーム全体の上限は RPC タイムアウトのこの倍数（最大 30 分）。
const STREAM_TOTAL_TIMEOUT_FACTOR: u32 = 30;
const STREAM_MAX_TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// チャンクキューが満杯のとき、終端エラーを届けるための猶予。
const STREAM_OVERFLOW_ERROR_GRACE: Duration = Duration::from_millis(100);

// 開始レスポンスの headers デコード失敗はヘッダ値を一切ログに出さず、同一ストリーム内では
// 最初の 1 回だけ固定文言の構造化 warn を出す。
fn warn_stream_headers_decode_failure_once(warned: &mut bool) {
    if *warned {
        return;
    }
    *warned = true;
    tracing::warn!(
        event = "plugin_stream_headers_decode_failed",
        "plugin stream headers decode failed"
    );
}

/// 開始レスポンスの headers をデコードする。
/// 型不一致時は従来どおり空マップを返し、値を含まない固定 warn を 1 回だけ出す。
fn parse_stream_headers(
    result: &Value,
    warned: &mut bool,
) -> std::collections::HashMap<String, String> {
    match result.get("headers") {
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(headers) => headers,
            Err(_) => {
                warn_stream_headers_decode_failure_once(warned);
                std::collections::HashMap::new()
            }
        },
        None => std::collections::HashMap::new(),
    }
}

type StreamStart = Result<(u16, std::collections::HashMap<String, String>), PluginSubprocessError>;
type StreamChunk = Result<String, PluginSubprocessError>;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PluginSubprocessError {
    #[error("subprocess I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("plugin RPC error ({code}): {message}")]
    Rpc { code: i64, message: String },

    #[error("plugin subprocess not running")]
    NotRunning,

    #[error("plugin response timeout")]
    Timeout,

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl From<RpcError> for PluginSubprocessError {
    fn from(e: RpcError) -> Self {
        Self::Rpc {
            code: e.code,
            message: e.message,
        }
    }
}

/// Reject an `initialize` handshake whose protocol version differs from ours.
///
/// The field is optional on the wire for backward compatibility (older hosts
/// omit it and are treated as `PROTOCOL_VERSION`), but a host that advertises
/// a *different* version must fail the handshake instead of being silently
/// accepted.
fn check_protocol_version(version: u32) -> Result<(), PluginSubprocessError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(PluginSubprocessError::Protocol(format!(
            "plugin protocol version mismatch: expected {PROTOCOL_VERSION}, got {version}"
        )))
    }
}

// ---------------------------------------------------------------------------
// Result types (deserialized from host responses)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    pub name: String,
    pub hooks: Vec<String>,
    pub auth: Option<AuthMeta>,
    /// Handshake contract version reported by the host.
    #[serde(rename = "protocolVersion", default = "default_protocol_version")]
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthMeta {
    pub provider: String,
    pub methods: Vec<AuthMethodMeta>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthMethodMeta {
    #[serde(rename = "type")]
    pub method_type: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeResult {
    pub url: Option<String>,
    pub instructions: Option<String>,
    pub method: Option<String>,
    /// Opaque id used to correlate a later `auth.callback` with this authorize
    /// flow. Absent on older hosts, which then use the single-pending-flow path.
    #[serde(rename = "flowId", default)]
    pub flow_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthLoadResult {
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(rename = "hasCustomFetch")]
    pub has_custom_fetch: bool,
}

#[derive(Debug, Deserialize)]
pub struct AuthFetchResult {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: String,
}

pub struct AuthFetchStreamResult {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub chunks: mpsc::Receiver<StreamChunk>,
}

// ---------------------------------------------------------------------------
// Context passed to plugin on initialize
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PluginContext {
    pub worktree: String,
    pub directory: String,
    #[serde(rename = "serverUrl")]
    pub server_url: String,
}

// ---------------------------------------------------------------------------
// PluginSubprocess
// ---------------------------------------------------------------------------

pub struct PluginSubprocess {
    /// Human-readable plugin name (from initialize response).
    name: String,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    rpc_lock: Arc<Mutex<()>>,
    process: Mutex<Child>,
    request_id: AtomicU64,
    /// Hook names this plugin registered.
    hooks: Vec<String>,
    /// Auth metadata, if the plugin provides auth.
    auth_meta: Option<AuthMeta>,
    /// RPC call timeout.
    timeout: Duration,
}

impl PluginSubprocess {
    /// Spawn a plugin host subprocess and run the `initialize` handshake.
    ///
    /// `cwd` sets the working directory for the subprocess so that bare-specifier
    /// `import("pkg")` calls resolve against the correct `node_modules/`.
    pub async fn spawn(
        runtime: JsRuntime,
        host_script: &str,
        plugin_path: &str,
        context: PluginContext,
        cwd: Option<&std::path::Path>,
    ) -> Result<Self, PluginSubprocessError> {
        let args = runtime.run_args(host_script);
        let mut cmd = Command::new(runtime.command());
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // plugin logs go to parent stderr
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PluginSubprocessError::Protocol("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PluginSubprocessError::Protocol("no stdout".into()))?;

        let mut this = Self {
            name: String::new(),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            rpc_lock: Arc::new(Mutex::new(())),
            process: Mutex::new(child),
            request_id: AtomicU64::new(1),
            hooks: Vec::new(),
            auth_meta: None,
            timeout: Duration::from_secs(30),
        };

        // Send initialize, advertising the handshake contract version so the
        // host can reject a mismatched client just as we reject a mismatched
        // host result below.
        let params = serde_json::json!({
            "pluginPath": plugin_path,
            "context": context,
            "protocolVersion": PROTOCOL_VERSION,
        });
        let result: InitializeResult = this.call("initialize", Some(params)).await?;
        check_protocol_version(result.protocol_version)?;

        this.name = result.name;
        this.hooks = result.hooks;
        this.auth_meta = result.auth;

        Ok(this)
    }

    // -- Accessors ----------------------------------------------------------

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn hooks(&self) -> &[String] {
        &self.hooks
    }

    pub fn auth_meta(&self) -> Option<&AuthMeta> {
        self.auth_meta.as_ref()
    }

    // -- RPC methods --------------------------------------------------------

    /// Invoke a hook on the plugin.
    pub async fn invoke_hook(
        &self,
        hook: &str,
        input: Value,
        output: Value,
    ) -> Result<Value, PluginSubprocessError> {
        let params = serde_json::json!({
            "hook": hook,
            "input": input,
            "output": output,
        });
        let result: Value = self.call("hook.invoke", Some(params)).await?;
        Ok(result.get("output").cloned().unwrap_or(Value::Null))
    }

    /// Trigger OAuth authorization flow.
    pub async fn auth_authorize(
        &self,
        method_index: usize,
        inputs: Option<Value>,
    ) -> Result<AuthorizeResult, PluginSubprocessError> {
        let params = serde_json::json!({
            "methodIndex": method_index,
            "inputs": inputs.unwrap_or(Value::Null),
        });
        self.call("auth.authorize", Some(params)).await
    }

    /// Build `auth.callback` params. `flowId` is emitted only when present: the
    /// host distinguishes an absent field (legacy single-pending-flow path) via
    /// `params.flowId === undefined`, so a present-but-null value would break it.
    fn auth_callback_params(code: Option<&str>, flow_id: Option<&str>) -> Value {
        let mut params = serde_json::json!({ "code": code });
        if let Some(flow_id) = flow_id {
            params["flowId"] = Value::String(flow_id.to_string());
        }
        params
    }

    /// Complete OAuth callback on the legacy single-pending-flow path.
    ///
    /// Existing callers keep this signature; it delegates with `flow_id = None`.
    pub async fn auth_callback(&self, code: Option<&str>) -> Result<Value, PluginSubprocessError> {
        self.auth_callback_with_flow(code, None).await
    }

    /// Complete OAuth callback for a specific authorize flow.
    ///
    /// `flow_id` must be the value returned by `auth.authorize` as `flowId`.
    /// When it is `None`, the host falls back to the legacy single-flow behavior.
    pub async fn auth_callback_with_flow(
        &self,
        code: Option<&str>,
        flow_id: Option<&str>,
    ) -> Result<Value, PluginSubprocessError> {
        let params = Self::auth_callback_params(code, flow_id);
        self.call("auth.callback", Some(params)).await
    }

    /// Load auth provider configuration.
    pub async fn auth_load(&self, provider: &str) -> Result<AuthLoadResult, PluginSubprocessError> {
        let params = serde_json::json!({ "provider": provider });
        self.call("auth.load", Some(params)).await
    }

    /// Proxy an HTTP request through the plugin's custom fetch.
    pub async fn auth_fetch(
        &self,
        url: &str,
        method: &str,
        headers: &std::collections::HashMap<String, String>,
        body: Option<&str>,
    ) -> Result<AuthFetchResult, PluginSubprocessError> {
        let params = serde_json::json!({
            "url": url,
            "method": method,
            "headers": headers,
            "body": body,
        });
        self.call("auth.fetch", Some(params)).await
    }

    /// Deliver a chunk without ever blocking on a slow consumer: the reader task
    /// holds the RPC and stdout locks, so `send().await` here would deadlock all
    /// other RPCs. On overflow, terminate the stream with an explicit error.
    /// The bounded grace period only applies to that terminal error, never to
    /// normal chunks.
    async fn push_chunk(tx: &mpsc::Sender<StreamChunk>, item: StreamChunk) -> bool {
        match tx.try_send(item) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
            Err(mpsc::error::TrySendError::Full(_)) => {
                let deadline = tokio::time::Instant::now() + STREAM_OVERFLOW_ERROR_GRACE;
                loop {
                    let error = PluginSubprocessError::Protocol(
                        "plugin fetch stream chunk buffer overflow".into(),
                    );
                    match tx.try_send(Err(error)) {
                        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => break,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            if tokio::time::Instant::now() >= deadline {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }
                }
                false
            }
        }
    }

    /// Report a protocol/timeout failure to the caller: before the start frame
    /// via the oneshot, afterwards through the chunk channel.
    async fn report_stream_error(
        chunk_tx: &mpsc::Sender<StreamChunk>,
        start_tx: &mut Option<oneshot::Sender<StreamStart>>,
        error: PluginSubprocessError,
    ) {
        if let Some(tx) = start_tx.take() {
            let _ = tx.send(Err(error));
        } else {
            Self::push_chunk(chunk_tx, Err(error)).await;
        }
    }

    /// Proxy an HTTP request through the plugin's custom fetch as a real-time stream.
    pub async fn auth_fetch_stream(
        &self,
        url: &str,
        method: &str,
        headers: &std::collections::HashMap<String, String>,
        body: Option<&str>,
    ) -> Result<AuthFetchStreamResult, PluginSubprocessError> {
        let id = self.next_id();
        let params = serde_json::json!({
            "url": url,
            "method": method,
            "headers": headers,
            "body": body,
        });

        let rpc_guard = self.rpc_lock.clone().lock_owned().await;
        self.write_request(id, "auth.fetch.stream", Some(params))
            .await?;

        let (start_tx, start_rx) = oneshot::channel::<StreamStart>();
        let (chunk_tx, chunk_rx) = mpsc::channel(STREAM_CHUNK_BUFFER);
        let stdout = Arc::clone(&self.stdout);
        let idle_timeout = self.timeout;
        let total_timeout = self
            .timeout
            .saturating_mul(STREAM_TOTAL_TIMEOUT_FACTOR)
            .min(STREAM_MAX_TOTAL_TIMEOUT);

        // The reader task owns `rpc_guard` and the stdout lock for the entire
        // stream. Idle plus overall timeouts guarantee both are released even
        // when the plugin never sends `auth.fetch.stream.end`.
        let task = tokio::spawn(async move {
            let mut start_tx = Some(start_tx);
            let mut headers_warned = false;
            let mut reader = stdout.lock().await;
            let overall_deadline = tokio::time::Instant::now() + total_timeout;

            loop {
                let raw = match tokio::time::timeout_at(
                    overall_deadline,
                    tokio::time::timeout(idle_timeout, Self::read_raw_message(&mut reader)),
                )
                .await
                {
                    // Overall or idle timeout: end the stream and let the
                    // guard-drop below unblock every waiting RPC.
                    Err(_) | Ok(Err(_)) => {
                        Self::report_stream_error(
                            &chunk_tx,
                            &mut start_tx,
                            PluginSubprocessError::Timeout,
                        )
                        .await;
                        break;
                    }
                    Ok(Ok(result)) => match result {
                        Ok(raw) => raw,
                        Err(err) => {
                            Self::report_stream_error(&chunk_tx, &mut start_tx, err).await;
                            break;
                        }
                    },
                };

                if let Err(message) = validate_jsonrpc_envelope(&raw) {
                    Self::report_stream_error(
                        &chunk_tx,
                        &mut start_tx,
                        PluginSubprocessError::Protocol(message),
                    )
                    .await;
                    break;
                }

                if raw.get("id").and_then(Value::as_u64) == Some(id) {
                    let response: RpcResponse = match serde_json::from_value(raw) {
                        Ok(response) => response,
                        Err(err) => {
                            Self::report_stream_error(
                                &chunk_tx,
                                &mut start_tx,
                                PluginSubprocessError::Json(err),
                            )
                            .await;
                            break;
                        }
                    };

                    if let Some(error) = response.error {
                        Self::report_stream_error(
                            &chunk_tx,
                            &mut start_tx,
                            PluginSubprocessError::from(error),
                        )
                        .await;
                        break;
                    }

                    let result = response.result.unwrap_or(Value::Null);
                    let status = result
                        .get("status")
                        .and_then(Value::as_u64)
                        .and_then(|v| u16::try_from(v).ok())
                        .unwrap_or(200);
                    let headers = parse_stream_headers(&result, &mut headers_warned);
                    if let Some(tx) = start_tx.take() {
                        let _ = tx.send(Ok((status, headers)));
                    }
                    continue;
                }

                let method = raw.get("method").and_then(Value::as_str);
                let params = raw.get("params").cloned().unwrap_or(Value::Null);
                let request_id = params.get("requestId").and_then(Value::as_u64);
                if request_id != Some(id) {
                    continue;
                }

                match method {
                    Some("auth.fetch.stream.chunk") => {
                        match params.get("chunk").and_then(Value::as_str) {
                            Some(chunk) => {
                                if !Self::push_chunk(&chunk_tx, Ok(chunk.to_string())).await {
                                    break;
                                }
                            }
                            None => {
                                // PARSE-04: a chunk frame whose `chunk` is absent or not a
                                // string must fail the stream instead of being silently
                                // dropped — a trailing `stream.end` would otherwise make
                                // the truncation look like a clean completion. Use a fixed
                                // message only: never include the value, path or token.
                                Self::report_stream_error(
                                    &chunk_tx,
                                    &mut start_tx,
                                    PluginSubprocessError::Protocol(
                                        "invalid auth.fetch stream chunk".into(),
                                    ),
                                )
                                .await;
                                break;
                            }
                        }
                    }
                    Some("auth.fetch.stream.error") => {
                        let message = params
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("plugin custom fetch stream failed")
                            .to_string();
                        Self::report_stream_error(
                            &chunk_tx,
                            &mut start_tx,
                            PluginSubprocessError::Protocol(message),
                        )
                        .await;
                        break;
                    }
                    Some("auth.fetch.stream.end") => {
                        break;
                    }
                    _ => {}
                }
            }
            // Explicitly drop both locks on every exit path, including timeouts.
            drop(reader);
            drop(rpc_guard);
        });

        // Abort the reader task on every failure path so that `rpc_lock` and the
        // stdout lock are released even when the plugin never sends a start frame.
        let start = tokio::time::timeout(self.timeout, start_rx).await;
        let (status, response_headers) = match start {
            Ok(Ok(Ok(value))) => value,
            Ok(Ok(Err(err))) => {
                task.abort();
                return Err(err);
            }
            Ok(Err(_)) => {
                task.abort();
                return Err(PluginSubprocessError::NotRunning);
            }
            Err(_) => {
                task.abort();
                return Err(PluginSubprocessError::Timeout);
            }
        };

        Ok(AuthFetchStreamResult {
            status,
            headers: response_headers,
            chunks: chunk_rx,
        })
    }

    /// Gracefully shut down the plugin subprocess.
    ///
    /// Policy (SHUTDOWN-06): this is a best-effort cleanup. A failed `shutdown`
    /// RPC is warned about but never propagated, and the two-second wait plus
    /// the `kill()` fallback always run — the process must not depend solely on
    /// `kill_on_drop(true)`. The `Result` signature is kept for caller
    /// compatibility, but the method resolves to `Ok(())` on both paths:
    /// plugin-supplied RPC error text may contain tokens, URLs or paths, and
    /// `shutdown_all` logs any returned error, so neither the warning here nor
    /// the return value may carry that text. Callers only need to know the
    /// child has been reaped/terminated.
    pub async fn shutdown(&self) -> Result<(), PluginSubprocessError> {
        if self.call::<Value>("shutdown", None).await.is_err() {
            // Value-independent on purpose: never format the RPC error into the
            // log, since its message comes from the plugin and may be sensitive.
            tracing::warn!("plugin shutdown RPC failed; falling back to wait/kill");
        }
        // Give the process a moment to exit, then kill if needed. This path must
        // be reached even when the RPC above failed.
        let mut proc = self.process.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), proc.wait()).await;
        let _ = proc.kill().await;
        Ok(())
    }

    // -- Transport (Content-Length framing) ----------------------------------

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, PluginSubprocessError> {
        // Apply the timeout to lock acquisition as well: a hung stream task must
        // not turn a lock wait into an unbounded block.
        let _rpc_guard = tokio::time::timeout(self.timeout, self.rpc_lock.lock())
            .await
            .map_err(|_| PluginSubprocessError::Timeout)?;
        let id = self.next_id();
        self.write_request(id, method, params).await?;

        // Read response with timeout
        let response = tokio::time::timeout(self.timeout, self.read_response_for_id(id))
            .await
            .map_err(|_| PluginSubprocessError::Timeout)??;

        if let Some(err) = response.error {
            return Err(err.into());
        }

        let result = response.result.unwrap_or(Value::Null);
        serde_json::from_value(result).map_err(Into::into)
    }

    async fn write_request(
        &self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), PluginSubprocessError> {
        let request = RpcRequest::new(id, method, params);
        let content = serde_json::to_string(&request)?;
        let frame = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(frame.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_response_for_id(
        &self,
        expected_id: u64,
    ) -> Result<RpcResponse, PluginSubprocessError> {
        let mut reader = self.stdout.lock().await;
        loop {
            let raw = match Self::read_raw_message(&mut reader).await {
                Ok(raw) => raw,
                Err(err) => {
                    // フレーム同期が壊れたら子プロセスを終了させ、以降の
                    // 読み取りで無限に待たないようにする（切断）。
                    if let Ok(mut proc) = self.process.try_lock() {
                        let _ = proc.kill().await;
                    }
                    return Err(err);
                }
            };
            // Reject a malformed/unsupported envelope as a protocol error even
            // when its id does not match, so a bad host cannot keep us waiting.
            validate_jsonrpc_envelope(&raw).map_err(PluginSubprocessError::Protocol)?;
            if raw.get("id").and_then(Value::as_u64) != Some(expected_id) {
                continue;
            }
            let response: RpcResponse = serde_json::from_value(raw)?;
            return Ok(response);
        }
    }

    /// Read one Content-Length framed JSON-RPC message from stdout.
    async fn read_raw_message(
        reader: &mut BufReader<ChildStdout>,
    ) -> Result<Value, PluginSubprocessError> {
        // Read headers until empty line. `read_line` has no length bound, so
        // read one byte at a time with an explicit cap (BufReader buffers IO).
        // Header values are never included in error messages: they may carry
        // authorization tokens.
        let mut content_length: Option<usize> = None;
        let mut header_lines = 0usize;
        loop {
            let mut line: Vec<u8> = Vec::with_capacity(128);
            let mut byte = [0u8; 1];
            loop {
                let n = reader.read(&mut byte).await?;
                if n == 0 {
                    return Err(PluginSubprocessError::NotRunning);
                }
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                if line.len() > MAX_HEADER_LINE_BYTES {
                    return Err(PluginSubprocessError::Protocol(
                        "header line too long".into(),
                    ));
                }
            }

            header_lines += 1;
            if header_lines > MAX_HEADER_LINES {
                return Err(PluginSubprocessError::Protocol(
                    "too many header lines".into(),
                ));
            }

            let header_line = String::from_utf8_lossy(&line);
            let trimmed = header_line.trim();
            if trimmed.is_empty() {
                break;
            }

            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                let len: usize = rest.trim().parse().map_err(|e| {
                    PluginSubprocessError::Protocol(format!("bad Content-Length: {e}"))
                })?;
                if len > MAX_FRAME_SIZE {
                    return Err(PluginSubprocessError::Protocol(format!(
                        "Content-Length exceeds {} MiB limit",
                        MAX_FRAME_SIZE / (1024 * 1024)
                    )));
                }
                content_length = Some(len);
            }
        }

        let len = content_length.ok_or_else(|| {
            PluginSubprocessError::Protocol("missing Content-Length header".into())
        })?;

        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        let value: Value = serde_json::from_slice(&buf)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // tracing-subscriber を依存に追加せず、WARN イベントだけを捕捉する最小限の Subscriber。
    #[derive(Clone, Default)]
    struct WarnCapture {
        events: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    struct WarnVisitor {
        event: Option<String>,
        message: Option<String>,
    }

    impl tracing::field::Visit for WarnVisitor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            match field.name() {
                "event" => self.event = Some(value.to_owned()),
                "message" => self.message = Some(value.to_owned()),
                _ => {}
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = Some(format!("{value:?}"));
            }
        }
    }

    impl tracing::Subscriber for WarnCapture {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() > tracing::Level::WARN {
                return;
            }
            let mut visitor = WarnVisitor {
                event: None,
                message: None,
            };
            event.record(&mut visitor);
            self.events.lock().expect("mutex poisoned").push((
                visitor.event.unwrap_or_default(),
                visitor.message.unwrap_or_default(),
            ));
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    #[test]
    fn stream_headers_type_mismatch_warns_once_and_defaults_to_empty_map() {
        let capture = WarnCapture::default();
        let mut warned = false;
        let results = tracing::subscriber::with_default(capture.clone(), || {
            vec![
                parse_stream_headers(
                    &serde_json::json!({"headers": ["leak-marker-hdr-1"]}),
                    &mut warned,
                ),
                parse_stream_headers(
                    &serde_json::json!({"headers": "leak-marker-hdr-2"}),
                    &mut warned,
                ),
            ]
        });
        assert!(results.iter().all(|headers| headers.is_empty()));

        let events = capture.events.lock().expect("mutex poisoned");
        assert_eq!(events.len(), 1, "repeated failures must warn only once");
        assert_eq!(events[0].0, "plugin_stream_headers_decode_failed");
        assert!(events[0].1.contains("plugin stream headers decode failed"));
        assert!(!events[0].0.contains("leak-marker"));
        assert!(!events[0].1.contains("leak-marker"));
    }

    #[test]
    fn stream_headers_valid_or_absent_value_does_not_warn() {
        let capture = WarnCapture::default();
        let mut warned = false;
        let (valid, absent) = tracing::subscriber::with_default(capture.clone(), || {
            (
                parse_stream_headers(
                    &serde_json::json!({"headers": {"x-custom": "leak-marker-hdr-3"}}),
                    &mut warned,
                ),
                parse_stream_headers(&serde_json::json!({}), &mut warned),
            )
        });
        assert_eq!(
            valid.get("x-custom").map(String::as_str),
            Some("leak-marker-hdr-3")
        );
        assert!(absent.is_empty());
        assert!(capture.events.lock().expect("mutex poisoned").is_empty());
    }

    /// A stream start timeout must release `rpc_lock`; otherwise every later RPC
    /// call blocks forever on `call()`'s lock acquisition (todo.md H-08).
    #[tokio::test]
    async fn stream_start_timeout_releases_rpc_lock() {
        // `sleep` never writes a framed start response, so `start_rx` times out.
        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep subprocess");
        let stdin = child.stdin.take().expect("take stdin");
        let stdout = child.stdout.take().expect("take stdout");
        let sub = PluginSubprocess {
            name: String::new(),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            rpc_lock: Arc::new(Mutex::new(())),
            process: Mutex::new(child),
            request_id: AtomicU64::new(1),
            hooks: Vec::new(),
            auth_meta: None,
            timeout: Duration::from_millis(100),
        };

        let headers = std::collections::HashMap::new();
        let result = sub
            .auth_fetch_stream("http://example.invalid", "GET", &headers, None)
            .await;
        assert!(matches!(result, Err(PluginSubprocessError::Timeout)));

        // Before the fix, the spawned reader task still held this guard.
        let acquired = tokio::time::timeout(Duration::from_millis(100), sub.rpc_lock.lock()).await;
        assert!(
            acquired.is_ok(),
            "rpc_lock remained held after stream start timeout"
        );
    }

    fn stream_frame(value: serde_json::Value) -> Vec<u8> {
        let content = serde_json::to_string(&value).expect("serialize fixture frame");
        format!("Content-Length: {}\r\n\r\n{}", content.len(), content).into_bytes()
    }

    fn stream_start_frame(id: u64) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"status": 200, "headers": {}},
            "error": null,
        })
    }

    /// `cat` echoes whatever the client writes, so the test can inject framed
    /// responses without a shell or a second helper binary.
    async fn cat_subprocess(frames: &[Vec<u8>], timeout: Duration) -> PluginSubprocess {
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn cat fixture");
        let mut stdin = child.stdin.take().expect("take cat stdin");
        for frame in frames {
            stdin.write_all(frame).await.expect("write fixture frame");
        }
        let stdout = child.stdout.take().expect("take cat stdout");
        PluginSubprocess {
            name: String::new(),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            rpc_lock: Arc::new(Mutex::new(())),
            process: Mutex::new(child),
            request_id: AtomicU64::new(1),
            hooks: Vec::new(),
            auth_meta: None,
            timeout,
        }
    }

    #[tokio::test]
    async fn stream_without_end_releases_rpc_lock_on_timeout() {
        let sub = cat_subprocess(
            &[stream_frame(stream_start_frame(1))],
            Duration::from_millis(150),
        )
        .await;
        let headers = std::collections::HashMap::new();
        let stream = sub
            .auth_fetch_stream("http://example.invalid", "GET", &headers, None)
            .await
            .expect("start frame should arrive");
        assert_eq!(stream.status, 200);
        let mut chunks = stream.chunks;

        // The plugin never sends `stream.end`: the idle timeout must abort the
        // reader and hand the locks back.
        let terminal = tokio::time::timeout(Duration::from_secs(2), chunks.recv()).await;
        assert!(
            matches!(terminal, Ok(Some(Err(PluginSubprocessError::Timeout)))),
            "stream should time out without stream.end: {terminal:?}"
        );

        let acquired = tokio::time::timeout(Duration::from_secs(1), sub.rpc_lock.lock()).await;
        assert!(
            acquired.is_ok(),
            "rpc_lock remained held after stream timeout"
        );
        drop(acquired);

        // A later RPC must succeed instead of blocking on a leaked lock. `cat`
        // echoes the request frame, which doubles as a null-result response.
        let rpc = tokio::time::timeout(
            Duration::from_secs(2),
            sub.call::<serde_json::Value>("ping", None),
        )
        .await;
        assert!(rpc.is_ok(), "subsequent RPC timed out");
        assert!(rpc.expect("rpc result").is_ok(), "subsequent RPC failed");
    }

    #[tokio::test]
    async fn stream_chunk_overflow_ends_without_deadlock() {
        let mut frames = vec![stream_frame(stream_start_frame(1))];
        for index in 0..300 {
            frames.push(stream_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "auth.fetch.stream.chunk",
                "params": {"requestId": 1, "chunk": format!("chunk-{index}")},
            })));
        }
        let sub = cat_subprocess(&frames, Duration::from_millis(150)).await;
        let headers = std::collections::HashMap::new();
        let stream = sub
            .auth_fetch_stream("http://example.invalid", "GET", &headers, None)
            .await
            .expect("start frame should arrive");
        let mut chunks = stream.chunks;

        // Synchronize on the channel itself instead of a fixed sleep: the
        // bounded buffer holds 128 items, so `len() >= 128` is an explicit
        // signal that the reader has already pushed 128 chunks and is at the
        // overflow point. Draining only after that keeps the terminal error
        // deliverable within its grace window regardless of scheduling delay.
        let drain = async {
            while chunks.len() < STREAM_CHUNK_BUFFER {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let mut saw_overflow = false;
            while let Some(item) = chunks.recv().await {
                match item {
                    Ok(_) => {}
                    Err(PluginSubprocessError::Protocol(message))
                        if message.contains("overflow") =>
                    {
                        saw_overflow = true;
                        break;
                    }
                    // Do not format the error text: protocol messages originate
                    // from the plugin and may carry sensitive values.
                    Err(_) => panic!("unexpected stream error before overflow"),
                }
            }
            saw_overflow
        };
        // One deadline covers the whole sync + drain phase, so a slow CI worker
        // can no longer trip a per-receive timeout midway through 128+ chunks.
        let saw_overflow = tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .expect("chunk overflow must be observed before the deadline");
        assert!(saw_overflow, "chunk overflow must terminate with an error");

        let acquired = tokio::time::timeout(Duration::from_secs(1), sub.rpc_lock.lock()).await;
        assert!(
            acquired.is_ok(),
            "rpc_lock remained held after chunk overflow"
        );
    }

    /// PARSE-04: a non-string chunk must fail the stream with a fixed Protocol
    /// error even when a `stream.end` follows it — no chunk event may be
    /// delivered and the trailing end must not be mistaken for clean completion.
    #[tokio::test]
    async fn invalid_numeric_chunk_fails_even_with_trailing_stream_end() {
        let frames = vec![
            stream_frame(stream_start_frame(1)),
            stream_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "auth.fetch.stream.chunk",
                "params": {"requestId": 1, "chunk": 12345},
            })),
            stream_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "auth.fetch.stream.end",
                "params": {"requestId": 1},
            })),
        ];
        let sub = cat_subprocess(&frames, Duration::from_millis(500)).await;
        let headers = std::collections::HashMap::new();
        let stream = sub
            .auth_fetch_stream("http://example.invalid", "GET", &headers, None)
            .await
            .expect("start frame should arrive");
        let mut chunks = stream.chunks;

        // Drain until the channel closes: the reader breaks on the bad chunk,
        // so the only item must be the terminal error and no `Ok` chunk.
        let items = tokio::time::timeout(Duration::from_secs(2), async {
            let mut items = Vec::new();
            while let Some(item) = chunks.recv().await {
                items.push(item);
            }
            items
        })
        .await
        .expect("channel must close after the terminal error");
        assert_eq!(items.len(), 1, "invalid chunk must yield exactly one error");
        match &items[0] {
            Err(PluginSubprocessError::Protocol(message)) => {
                assert_eq!(message, "invalid auth.fetch stream chunk");
                assert!(!message.contains("12345"), "chunk value leaked into error");
            }
            other => panic!(
                "invalid chunk must fail with a fixed Protocol error, got {:?}",
                other.as_ref().map(|_| ())
            ),
        }
    }

    /// Normal string chunks keep the original wire contract: delivered verbatim,
    /// and `stream.end` closes the channel cleanly without an error.
    #[tokio::test]
    async fn valid_string_chunk_is_delivered_and_stream_ends_cleanly() {
        let payload = "normal-chunk-payload";
        let frames = vec![
            stream_frame(stream_start_frame(1)),
            stream_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "auth.fetch.stream.chunk",
                "params": {"requestId": 1, "chunk": payload},
            })),
            stream_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "auth.fetch.stream.end",
                "params": {"requestId": 1},
            })),
        ];
        let sub = cat_subprocess(&frames, Duration::from_millis(500)).await;
        let headers = std::collections::HashMap::new();
        let stream = sub
            .auth_fetch_stream("http://example.invalid", "GET", &headers, None)
            .await
            .expect("start frame should arrive");
        let mut chunks = stream.chunks;

        let first = tokio::time::timeout(Duration::from_secs(2), chunks.recv())
            .await
            .expect("chunk should arrive before deadline")
            .expect("stream must emit the chunk");
        assert_eq!(first.expect("chunk should be Ok"), payload);

        let terminal = tokio::time::timeout(Duration::from_secs(2), chunks.recv())
            .await
            .expect("channel close should arrive before deadline");
        assert!(
            terminal.is_none(),
            "clean stream.end must not emit an error"
        );
    }

    /// PARSE-04 covers every non-string form: number, null, object and missing.
    #[tokio::test]
    async fn non_string_chunk_forms_all_fail_without_leaking_values() {
        let invalid_params: [serde_json::Value; 4] = [
            serde_json::json!({"requestId": 1, "chunk": 7}),
            serde_json::json!({"requestId": 1, "chunk": null}),
            serde_json::json!({"requestId": 1, "chunk": {"secret": "leak-marker-chunk"}}),
            serde_json::json!({"requestId": 1}), // `chunk` absent entirely
        ];
        for params in invalid_params {
            let frames = vec![
                stream_frame(stream_start_frame(1)),
                stream_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "auth.fetch.stream.chunk",
                    "params": params,
                })),
                stream_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "auth.fetch.stream.end",
                    "params": {"requestId": 1},
                })),
            ];
            let sub = cat_subprocess(&frames, Duration::from_millis(500)).await;
            let headers = std::collections::HashMap::new();
            let stream = sub
                .auth_fetch_stream("http://example.invalid", "GET", &headers, None)
                .await
                .expect("start frame should arrive");
            let mut chunks = stream.chunks;

            let first = tokio::time::timeout(Duration::from_secs(2), chunks.recv())
                .await
                .expect("terminal error should arrive before deadline")
                .expect("stream must emit a terminal item, not close cleanly");
            match first {
                Err(PluginSubprocessError::Protocol(message)) => {
                    assert_eq!(message, "invalid auth.fetch stream chunk");
                    assert!(!message.contains("leak-marker"), "chunk value leaked");
                    assert!(!message.contains("secret"), "chunk key leaked");
                }
                other => panic!(
                    "non-string chunk must fail with a fixed Protocol error, got {:?}",
                    other.map(|_| ())
                ),
            }
            let next = tokio::time::timeout(Duration::from_secs(2), chunks.recv())
                .await
                .expect("channel close should arrive before deadline");
            assert!(
                next.is_none(),
                "no chunk event expected after invalid chunk"
            );
        }
    }

    /// `printf` はシェルを使わずに 1 フレームだけ書き出すテストfixture。
    async fn framed_child(frame: &str) -> (tokio::process::Child, BufReader<ChildStdout>) {
        let mut child = Command::new("printf")
            .arg(frame)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn printf fixture");
        let stdout = child.stdout.take().expect("take stdout");
        (child, BufReader::new(stdout))
    }

    #[tokio::test]
    async fn oversized_content_length_is_protocol_error() {
        let (_child, mut reader) = framed_child("Content-Length: 10485761\r\n\r\n").await;
        let error = PluginSubprocess::read_raw_message(&mut reader)
            .await
            .expect_err("must reject oversized frame");
        assert!(
            matches!(&error, PluginSubprocessError::Protocol(_)),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn excessive_header_lines_are_protocol_error() {
        let headers = "X-Pad: 1\r\n".repeat(MAX_HEADER_LINES + 8);
        let (_child, mut reader) = framed_child(&format!("{headers}\r\n")).await;
        let error = PluginSubprocess::read_raw_message(&mut reader)
            .await
            .expect_err("must reject excessive headers");
        assert!(
            matches!(&error, PluginSubprocessError::Protocol(_)),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn oversized_header_line_is_protocol_error() {
        let header = format!("X-Pad: {}\r\n", "a".repeat(MAX_HEADER_LINE_BYTES + 1));
        let (_child, mut reader) = framed_child(&header).await;
        let error = PluginSubprocess::read_raw_message(&mut reader)
            .await
            .expect_err("must reject oversized header line");
        assert!(
            matches!(&error, PluginSubprocessError::Protocol(_)),
            "unexpected error: {error}"
        );
    }

    /// フレーム上限はこのファイルの定数が単一情報源。
    /// `opencode-mcp` 側にも同名定数があるが両クレート間に依存関係はなく、
    /// 共通モジュールを新設できないため、双方で値を固定して漂移を検出する方針。
    #[test]
    fn frame_limit_constants_are_pinned() {
        assert_eq!(MAX_FRAME_SIZE, 10 * 1024 * 1024);
        assert_eq!(MAX_HEADER_LINES, 64);
        assert_eq!(MAX_HEADER_LINE_BYTES, 8192);
    }

    // -- Version negotiation regression tests -------------------------------

    #[test]
    fn initialize_result_defaults_protocol_version_when_absent() {
        // Backward compatibility: an older host that omits `protocolVersion`
        // is treated as the current version so existing hosts still work.
        let result: InitializeResult = serde_json::from_value(serde_json::json!({
            "name": "legacy-host",
            "hooks": [],
            "auth": null,
        }))
        .expect("legacy initialize result should deserialize");
        assert_eq!(result.protocol_version, PROTOCOL_VERSION);
        assert!(check_protocol_version(result.protocol_version).is_ok());
    }

    #[test]
    fn initialize_result_detects_protocol_version_mismatch() {
        let matching: InitializeResult = serde_json::from_value(serde_json::json!({
            "name": "host",
            "hooks": [],
            "auth": null,
            "protocolVersion": PROTOCOL_VERSION,
        }))
        .expect("matching initialize result should deserialize");
        assert!(check_protocol_version(matching.protocol_version).is_ok());

        let mismatched: InitializeResult = serde_json::from_value(serde_json::json!({
            "name": "host",
            "hooks": [],
            "auth": null,
            "protocolVersion": PROTOCOL_VERSION + 1,
        }))
        .expect("mismatched initialize result should deserialize");
        let error = check_protocol_version(mismatched.protocol_version)
            .expect_err("protocol version mismatch must fail the handshake");
        assert!(matches!(&error, PluginSubprocessError::Protocol(_)));
        assert!(
            error.to_string().contains("mismatch"),
            "unexpected error: {error}"
        );
    }

    /// Build a subprocess whose stdout replays a single caller-supplied frame.
    async fn framed_plugin_subprocess(body: &str) -> PluginSubprocess {
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut child = Command::new("printf")
            .arg(frame)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn printf fixture");
        let stdin = child.stdin.take().expect("take stdin");
        let stdout = child.stdout.take().expect("take stdout");
        PluginSubprocess {
            name: String::new(),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            rpc_lock: Arc::new(Mutex::new(())),
            process: Mutex::new(child),
            request_id: AtomicU64::new(1),
            hooks: Vec::new(),
            auth_meta: None,
            timeout: Duration::from_millis(500),
        }
    }

    #[tokio::test]
    async fn response_with_wrong_jsonrpc_is_protocol_error() {
        let sub = framed_plugin_subprocess(r#"{"jsonrpc":"1.0","id":1}"#).await;
        let error = sub
            .read_response_for_id(1)
            .await
            .expect_err("non-2.0 response must be rejected");
        assert!(matches!(&error, PluginSubprocessError::Protocol(_)));
        assert!(
            error.to_string().contains("jsonrpc"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn response_with_valid_jsonrpc_is_accepted() {
        let sub = framed_plugin_subprocess(r#"{"jsonrpc":"2.0","id":1,"result":1}"#).await;
        let response = sub
            .read_response_for_id(1)
            .await
            .expect("JSON-RPC 2.0 response should be accepted");
        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.result, Some(serde_json::json!(1)));
    }

    // -- Auth flowId regression tests ---------------------------------------

    #[test]
    fn authorize_result_deserializes_flow_id() {
        let result: AuthorizeResult = serde_json::from_value(serde_json::json!({
            "url": "https://example.invalid/auth",
            "method": "oauth2",
            "flowId": "flow-abc123",
        }))
        .expect("authorize result with flowId should deserialize");
        assert_eq!(result.flow_id.as_deref(), Some("flow-abc123"));

        // Backward compatibility: an older host that omits flowId still parses.
        let legacy: AuthorizeResult = serde_json::from_value(serde_json::json!({
            "url": "https://example.invalid/auth",
        }))
        .expect("legacy authorize result without flowId should deserialize");
        assert_eq!(legacy.flow_id, None);
    }

    #[test]
    fn auth_callback_params_include_flow_id_only_when_present() {
        let params =
            PluginSubprocess::auth_callback_params(Some("dummy-code"), Some("flow-abc123"));
        assert_eq!(
            params.get("flowId").and_then(Value::as_str),
            Some("flow-abc123")
        );
        assert!(params.get("code").is_some());

        // The legacy wrapper must not emit a `flowId` key at all: the host treats
        // only `undefined` as the single-pending-flow path (`=== undefined`).
        let legacy = PluginSubprocess::auth_callback_params(Some("dummy-code"), None);
        assert!(legacy.get("flowId").is_none());
        assert!(legacy.get("code").is_some());
    }

    // -- Shutdown fallback regression tests ---------------------------------

    /// SHUTDOWN-06: a failed `shutdown` RPC must not short-circuit the
    /// wait/kill fallback. `sleep 60` never answers the RPC (so `call` times
    /// out) and cannot exit on its own within the two-second grace window;
    /// if it is dead when `shutdown()` returns, the kill fallback ran.
    #[tokio::test]
    async fn shutdown_rpc_failure_still_kills_subprocess() {
        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep subprocess");
        let stdin = child.stdin.take().expect("take stdin");
        let stdout = child.stdout.take().expect("take stdout");
        let sub = PluginSubprocess {
            name: String::new(),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            rpc_lock: Arc::new(Mutex::new(())),
            process: Mutex::new(child),
            request_id: AtomicU64::new(1),
            hooks: Vec::new(),
            auth_meta: None,
            timeout: Duration::from_millis(100),
        };

        // The RPC times out, but shutdown is best-effort: it must still resolve
        // to Ok so no plugin-supplied error text reaches the caller's logger.
        sub.shutdown()
            .await
            .expect("shutdown must remain best-effort successful");

        let status = sub
            .process
            .lock()
            .await
            .try_wait()
            .expect("try_wait after shutdown");
        assert!(
            status.is_some(),
            "subprocess survived a failed shutdown RPC (kill fallback skipped)"
        );
    }
}
