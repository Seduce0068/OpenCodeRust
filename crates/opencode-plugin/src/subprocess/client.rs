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
/// ストリーム全体の上限は RPC タイムアウトのこの倍数（最大 30 分）。
const STREAM_TOTAL_TIMEOUT_FACTOR: u32 = 30;
const STREAM_MAX_TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// チャンクキューが満杯のとき、終端エラーを届けるための猶予。
const STREAM_OVERFLOW_ERROR_GRACE: Duration = Duration::from_millis(100);

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

    /// Complete OAuth callback.
    pub async fn auth_callback(&self, code: Option<&str>) -> Result<Value, PluginSubprocessError> {
        let params = serde_json::json!({ "code": code });
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
        let (chunk_tx, chunk_rx) = mpsc::channel(128);
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
                    let headers = result
                        .get("headers")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
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
                        if let Some(chunk) = params.get("chunk").and_then(Value::as_str) {
                            if !Self::push_chunk(&chunk_tx, Ok(chunk.to_string())).await {
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
    pub async fn shutdown(&self) -> Result<(), PluginSubprocessError> {
        let _: Value = self.call("shutdown", None).await?;
        // Give the process a moment to exit, then kill if needed
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
                    return Err(PluginSubprocessError::Protocol(
                        "Content-Length exceeds 10 MiB limit".into(),
                    ));
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

        // Let the reader hit the 128-item buffer limit before draining.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut saw_overflow = false;
        while let Ok(Some(item)) = tokio::time::timeout(Duration::from_secs(2), chunks.recv()).await
        {
            match item {
                Ok(_) => {}
                Err(PluginSubprocessError::Protocol(message)) if message.contains("overflow") => {
                    saw_overflow = true;
                    break;
                }
                Err(err) => panic!("unexpected stream error: {err}"),
            }
        }
        assert!(saw_overflow, "chunk overflow must terminate with an error");

        let acquired = tokio::time::timeout(Duration::from_secs(1), sub.rpc_lock.lock()).await;
        assert!(
            acquired.is_ok(),
            "rpc_lock remained held after chunk overflow"
        );
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
}
