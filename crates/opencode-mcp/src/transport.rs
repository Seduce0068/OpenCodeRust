use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::protocol::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest};
use crate::McpClientError;

/// 受信する JSON-RPC フレーム本体の上限（10 MiB）。
const MAX_FRAME_SIZE: usize = 10 * 1024 * 1024;
/// 1 フレームのヘッダ行数の上限（空行を含む）。
const MAX_HEADER_LINES: usize = 64;
/// 1 ヘッダ行の最大長（改行を除く）。ヘッダ値にトークンが含まれても
/// エラー本文には出さないため、値そのものはログに残さない。
const MAX_HEADER_LINE_BYTES: usize = 8192;
/// HTTP レスポンス本文の累積バイト上限（stdio と同じ 10 MiB）。
const MAX_HTTP_BODY_SIZE: usize = MAX_FRAME_SIZE;
/// SSE の 1 イベント（`data:` ペイロード）のバイト上限（10 MiB 相当）。
const MAX_SSE_EVENT_SIZE: usize = MAX_FRAME_SIZE;
/// レスポンス受信用 bounded mpsc キューの容量。
/// これを超える詰まりは「切断」として扱い、キューを無限に伸ばさない。
const RESPONSE_QUEUE_CAPACITY: usize = 64;

// ---------------------------------------------------------------------------
// Shared helpers (HTTP / SSE)
// ---------------------------------------------------------------------------

/// `reqwest::Response::bytes_stream()` で本文を読み、累積バイト数が `limit` を
/// 超えた時点で打ち切って `ProtocolError` を返す。
///
/// `resp.text()` と違い途中でサイズを検証できるため、巨大な本文をメモリに
/// 展開しない。読み取った本文値はエラー本文に一切含めない。
async fn read_body_capped(
    resp: reqwest::Response,
    limit: usize,
    label: &'static str,
) -> Result<String, McpClientError> {
    use futures::StreamExt;

    let mut stream = resp.bytes_stream();
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| McpClientError::TransportError(format!("Failed to read {label}: {e}")))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(McpClientError::ProtocolError(format!(
                "{label} exceeds size limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body)
        .map_err(|_| McpClientError::ProtocolError(format!("{label} is not valid UTF-8")))
}

/// SSE の `data:` ペイロードが上限内か検証する。値そのものはエラーに出さない。
fn validate_sse_event_size(data: &str) -> Result<(), McpClientError> {
    if data.len() > MAX_SSE_EVENT_SIZE {
        return Err(McpClientError::ProtocolError(
            "SSE event exceeds size limit".to_string(),
        ));
    }
    Ok(())
}

/// bounded キューへ 1 メッセージを入れる。キューが一杯（=受信側が追いついて
/// いない）または閉じている場合は切断扱いの `TransportError` を返し、
/// バッファを無限に伸ばさない。
fn enqueue_response(
    tx: &tokio::sync::mpsc::Sender<JsonRpcMessage>,
    message: JsonRpcMessage,
) -> Result<(), McpClientError> {
    tx.try_send(message)
        .map_err(|_| McpClientError::TransportError("Response queue is full or closed".to_string()))
}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn send(&self, request: &JsonRpcRequest) -> Result<(), McpClientError>;

    /// Send a JSON-RPC notification. Notifications carry no `id` and compliant
    /// servers never answer them, so this must not wait for a response.
    ///
    /// The default returns an error so that out-of-tree implementations keep
    /// compiling without silently claiming the notification was delivered.
    /// Returning `Ok(())` would be a false success, and routing through `send`
    /// would attach an `id` and violate the JSON-RPC notification format.
    /// In-tree transports all override this with a real implementation.
    async fn send_notification(
        &self,
        _notification: &JsonRpcNotification,
    ) -> Result<(), McpClientError> {
        Err(McpClientError::ProtocolError(
            "transport does not support JSON-RPC notifications".to_string(),
        ))
    }

    async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError>;
    async fn close(&self) -> Result<(), McpClientError>;
}

// ---------------------------------------------------------------------------
// StdioTransport
// ---------------------------------------------------------------------------

pub struct StdioTransport {
    process: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    /// Persistent reader so buffered bytes are not lost between messages.
    stdout: Mutex<Option<BufReader<ChildStdout>>>,
}

impl StdioTransport {
    pub async fn new(
        command: &str,
        args: &[String],
        env: Option<Vec<(String, String)>>,
        cwd: Option<String>,
    ) -> Result<Self, McpClientError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }
        let mut child = cmd.spawn().map_err(|e| {
            McpClientError::TransportError(format!("Failed to spawn process: {}", e))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpClientError::TransportError("Failed to get stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpClientError::TransportError("Failed to get stdout".to_string()))?;

        Ok(Self {
            process: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(Some(BufReader::new(stdout))),
        })
    }

    /// Protocol violation: kill the child so no further frames are consumed.
    /// Callers return `ProtocolError` with a generic reason; header/body bytes
    /// (which may carry credentials) are never echoed into the error.
    async fn disconnect(&self) {
        if let Some(child) = self.process.lock().await.as_mut() {
            let _ = child.kill().await;
        }
    }

    /// Write one Content-Length framed JSON body to the child process.
    /// Shared by request and notification sends; framing is identical.
    async fn write_frame(&self, content: &str) -> Result<(), McpClientError> {
        let mut stdin_guard = self.stdin.lock().await;
        let stdin = stdin_guard
            .as_mut()
            .ok_or_else(|| McpClientError::TransportError("Process not running".to_string()))?;

        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        stdin
            .write_all(message.as_bytes())
            .await
            .map_err(|e| McpClientError::TransportError(format!("Failed to write: {}", e)))?;

        stdin
            .flush()
            .await
            .map_err(|e| McpClientError::TransportError(format!("Failed to flush: {}", e)))?;

        Ok(())
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send(&self, request: &JsonRpcRequest) -> Result<(), McpClientError> {
        let content = serde_json::to_string(request).map_err(|e| {
            McpClientError::ProtocolError(format!("Failed to serialize request: {}", e))
        })?;
        self.write_frame(&content).await
    }

    async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), McpClientError> {
        let content = serde_json::to_string(notification).map_err(|e| {
            McpClientError::ProtocolError(format!("Failed to serialize notification: {}", e))
        })?;
        self.write_frame(&content).await
    }

    async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
        let mut stdout_guard = self.stdout.lock().await;
        let reader = stdout_guard
            .as_mut()
            .ok_or_else(|| McpClientError::TransportError("No stdout".to_string()))?;

        let mut content_length: Option<usize> = None;
        let mut header_lines = 0usize;
        loop {
            // `read_line` には行長制限がないため、上限付きで 1 バイトずつ読む。
            // BufReader がバッファリングするので効率上の問題はない。
            let mut line: Vec<u8> = Vec::with_capacity(128);
            let mut byte = [0u8; 1];
            loop {
                let bytes_read = reader.read(&mut byte).await.map_err(|e| {
                    McpClientError::TransportError(format!("Failed to read header: {}", e))
                })?;
                if bytes_read == 0 {
                    if line.is_empty() {
                        return Ok(None);
                    }
                    self.disconnect().await;
                    return Err(McpClientError::ProtocolError(
                        "Truncated header line".to_string(),
                    ));
                }
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                if line.len() > MAX_HEADER_LINE_BYTES {
                    self.disconnect().await;
                    return Err(McpClientError::ProtocolError(
                        "Header line too long".to_string(),
                    ));
                }
            }

            header_lines += 1;
            if header_lines > MAX_HEADER_LINES {
                self.disconnect().await;
                return Err(McpClientError::ProtocolError(
                    "Too many header lines".to_string(),
                ));
            }

            let header_line = String::from_utf8_lossy(&line);
            let trimmed = header_line.trim();

            if trimmed.is_empty() {
                // End of headers: the body follows.
                if let Some(content_length) = content_length {
                    if content_length > MAX_FRAME_SIZE {
                        self.disconnect().await;
                        return Err(McpClientError::ProtocolError(
                            "Content-Length exceeds frame size limit".to_string(),
                        ));
                    }
                    let mut content_buf = vec![0u8; content_length];
                    reader.read_exact(&mut content_buf).await.map_err(|e| {
                        McpClientError::TransportError(format!("Failed to read content: {}", e))
                    })?;

                    let content = String::from_utf8_lossy(&content_buf);
                    let message = JsonRpcMessage::parse_json_str(&content).map_err(|e| {
                        McpClientError::ProtocolError(format!("Failed to parse message: {}", e))
                    })?;

                    return Ok(Some(message));
                }
                continue;
            }

            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                let parsed: usize = value.trim().parse().map_err(|e| {
                    McpClientError::ProtocolError(format!("Invalid content length: {}", e))
                })?;
                if parsed > MAX_FRAME_SIZE {
                    self.disconnect().await;
                    return Err(McpClientError::ProtocolError(
                        "Content-Length exceeds frame size limit".to_string(),
                    ));
                }
                content_length = Some(parsed);
            }
        }
    }

    async fn close(&self) -> Result<(), McpClientError> {
        // ロック順序は receive() と同じ stdout → stdin → process にする。
        // receive() が stdout を保持したまま disconnect() で process を待つ
        // 間に close() が process を掴むとデッドロックする恐れがある。
        let mut stdout_guard = self.stdout.lock().await;
        *stdout_guard = None;
        let mut stdin_guard = self.stdin.lock().await;
        *stdin_guard = None;
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            child.kill().await.map_err(|e| {
                McpClientError::TransportError(format!("Failed to kill process: {}", e))
            })?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HttpTransport (StreamableHTTP)
// ---------------------------------------------------------------------------

/// Transport that sends JSON-RPC requests over HTTP POST and reads streaming
/// (potentially chunked) JSON responses. Mirrors the TS `StreamableHTTPClientTransport`.
pub struct HttpTransport {
    url: String,
    headers: HashMap<String, String>,
    client: reqwest::Client,
    /// Buffer for responses received via streaming that haven't been consumed yet.
    /// Bounded so a slow/stuck consumer cannot grow the queue without limit.
    response_rx: Mutex<tokio::sync::mpsc::Receiver<JsonRpcMessage>>,
    response_tx: tokio::sync::mpsc::Sender<JsonRpcMessage>,
}

impl HttpTransport {
    pub fn new(url: String, headers: Option<HashMap<String, String>>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        Self {
            url,
            headers: headers.unwrap_or_default(),
            client: reqwest::Client::new(),
            response_rx: Mutex::new(rx),
            response_tx: tx,
        }
    }

    /// POST a serialized JSON-RPC body and return the HTTP response so the
    /// caller can decide whether to consume it. Shared by both send paths.
    async fn post_body(&self, body: String) -> Result<reqwest::Response, McpClientError> {
        let mut builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");

        for (key, value) in &self.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        let resp =
            builder.body(body).send().await.map_err(|e| {
                McpClientError::TransportError(format!("HTTP request failed: {}", e))
            })?;

        if !resp.status().is_success() {
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(McpClientError::Unauthorized);
            }
            return Err(McpClientError::TransportError(format!(
                "HTTP {} from server",
                resp.status()
            )));
        }

        Ok(resp)
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn send(&self, request: &JsonRpcRequest) -> Result<(), McpClientError> {
        let body = serde_json::to_string(request).map_err(|e| {
            McpClientError::ProtocolError(format!("Failed to serialize request: {}", e))
        })?;
        let resp = self.post_body(body).await?;

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("text/event-stream") {
            // Server chose to stream the response via SSE inside the POST response.
            let text = read_body_capped(resp, MAX_HTTP_BODY_SIZE, "SSE body").await?;
            for line in text.lines() {
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    validate_sse_event_size(data)?;
                    if let Ok(message) = JsonRpcMessage::parse_json_str(data) {
                        enqueue_response(&self.response_tx, message)?;
                    }
                }
            }
        } else {
            // Plain JSON response.
            let text = read_body_capped(resp, MAX_HTTP_BODY_SIZE, "response body").await?;
            if !text.is_empty() {
                let message = JsonRpcMessage::parse_json_str(&text).map_err(|e| {
                    McpClientError::ProtocolError(format!("Failed to parse response: {}", e))
                })?;
                enqueue_response(&self.response_tx, message)?;
            }
        }

        Ok(())
    }

    async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), McpClientError> {
        let body = serde_json::to_string(notification).map_err(|e| {
            McpClientError::ProtocolError(format!("Failed to serialize notification: {}", e))
        })?;
        // A notification has no id and produces no JSON-RPC response. Do not
        // parse/buffer the HTTP body into `response_rx`, or the next request
        // would consume a stale message.
        let _resp = self.post_body(body).await?;
        Ok(())
    }

    async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
        let mut rx = self.response_rx.lock().await;
        match rx.recv().await {
            Some(msg) => Ok(Some(msg)),
            None => Ok(None),
        }
    }

    async fn close(&self) -> Result<(), McpClientError> {
        // Nothing to tear down – the reqwest client will be dropped with the struct.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SseTransport
// ---------------------------------------------------------------------------

/// Transport that connects to an SSE endpoint for receiving messages and
/// POSTs JSON-RPC requests to the same base URL. Mirrors the TS
/// `SSEClientTransport`.
pub struct SseTransport {
    url: String,
    headers: HashMap<String, String>,
    client: reqwest::Client,
    response_rx: Mutex<tokio::sync::mpsc::Receiver<JsonRpcMessage>>,
    response_tx: tokio::sync::mpsc::Sender<JsonRpcMessage>,
    /// バックグラウンド SSE リスナが検出した上限超過 등의理由（本文は含まない）。
    violation: Arc<Mutex<Option<String>>>,
    /// 違反検出時に待機中の `receive()` を起こすための通知。
    violation_notify: Arc<tokio::sync::Notify>,
    /// Handle to the background SSE listener task so we can abort on close.
    sse_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SseTransport {
    pub fn new(url: String, headers: Option<HashMap<String, String>>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        Self {
            url,
            headers: headers.unwrap_or_default(),
            client: reqwest::Client::new(),
            response_rx: Mutex::new(rx),
            response_tx: tx,
            violation: Arc::new(Mutex::new(None)),
            violation_notify: Arc::new(tokio::sync::Notify::new()),
            sse_task: Mutex::new(None),
        }
    }

    /// Start the background SSE listener. Must be called before `send`/`receive`.
    pub async fn connect(&self) -> Result<(), McpClientError> {
        use futures::StreamExt;
        use reqwest_eventsource::{Event, EventSource};

        let mut builder = self.client.get(&self.url);
        builder = builder.header("Accept", "text/event-stream");
        for (key, value) in &self.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        let mut es = EventSource::new(builder).map_err(|e| {
            McpClientError::TransportError(format!("Failed to create SSE connection: {}", e))
        })?;

        let tx = self.response_tx.clone();
        let violation = Arc::clone(&self.violation);
        let notify = Arc::clone(&self.violation_notify);

        let handle = tokio::spawn(async move {
            while let Some(event) = es.next().await {
                match event {
                    Ok(Event::Message(msg)) => {
                        let data = msg.data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        if validate_sse_event_size(data).is_err() {
                            // 上限超過: 本文は残さず理由だけ記録して切断する。
                            *violation.lock().await =
                                Some("SSE event exceeds size limit".to_string());
                            notify.notify_one();
                            break;
                        }
                        match JsonRpcMessage::parse_json_str(data) {
                            Ok(msg) => match tx.try_send(msg) {
                                Ok(()) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    // bounded キューが一杯なら無限に溜めず切断する。
                                    *violation.lock().await =
                                        Some("Response queue is full".to_string());
                                    notify.notify_one();
                                    break;
                                }
                                // 受信側が閉じられた: これ以上配信先がない。
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                            },
                            Err(e) => {
                                tracing::warn!("SSE: failed to parse message: {}", e);
                            }
                        }
                    }
                    Ok(Event::Open) => {
                        tracing::debug!("SSE connection opened");
                    }
                    Err(e) => {
                        tracing::error!("SSE error: {}", e);
                        // エラー本文（トークン等を含む可能性がある）は残さず、
                        // 理由だけ記録して待機中の receive() を必ず起こす。
                        *violation.lock().await = Some("SSE stream error".to_string());
                        notify.notify_one();
                        break;
                    }
                }
            }
            // ここに到達した時点で SSE ストリームはもうイベントを配信しない
            // （SSE エラー / TrySendError::Closed / 正常なストリーム終了）。
            // どの break 経路でも violation を設定し notify_one() を呼んで
            // receive() が rx.recv() で永久にハングしないようにする。
            // 上限超過・キュー詰まり・エラーで既に理由がある場合は上書きしない。
            {
                let mut guard = violation.lock().await;
                if guard.is_none() {
                    *guard = Some("SSE stream closed".to_string());
                }
            }
            notify.notify_one();
        });

        let mut task = self.sse_task.lock().await;
        *task = Some(handle);

        Ok(())
    }

    /// POST a serialized JSON-RPC body to the message endpoint. Shared by both
    /// send paths; incoming messages still arrive via the SSE listener.
    async fn post_body(&self, body: String) -> Result<(), McpClientError> {
        let mut builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json");

        for (key, value) in &self.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        let resp = builder
            .body(body)
            .send()
            .await
            .map_err(|e| McpClientError::TransportError(format!("HTTP POST failed: {}", e)))?;

        if !resp.status().is_success() {
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(McpClientError::Unauthorized);
            }
            return Err(McpClientError::TransportError(format!(
                "HTTP {} from server",
                resp.status()
            )));
        }

        Ok(())
    }
}

impl Drop for SseTransport {
    fn drop(&mut self) {
        // `Drop` は非同期コンテキストとは限らないため `lock().await` は使えない。
        // `try_lock()` で待機せずに取得する。同時に `connect()` / `close()` が
        // ガードを保持している場合は、その側が `abort()` を担当するため、
        // ここで待つ必要も待てる必要もない。
        //
        // 401 リトライ等で旧 transport が単にドロップされる経路でも、
        // JoinHandle がデタッチされてタスクがリークしないよう保険として働く。
        if let Ok(mut task) = self.sse_task.try_lock() {
            if let Some(handle) = task.take() {
                handle.abort();
            }
        }
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn send(&self, request: &JsonRpcRequest) -> Result<(), McpClientError> {
        let body = serde_json::to_string(request).map_err(|e| {
            McpClientError::ProtocolError(format!("Failed to serialize request: {}", e))
        })?;
        self.post_body(body).await
    }

    async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), McpClientError> {
        let body = serde_json::to_string(notification).map_err(|e| {
            McpClientError::ProtocolError(format!("Failed to serialize notification: {}", e))
        })?;
        self.post_body(body).await
    }

    async fn receive(&self) -> Result<Option<JsonRpcMessage>, McpClientError> {
        // バックグラウンドリスナが上限超過/キュー詰まりを検出済みなら、
        // バッファに残っていても切断扱いのエラーを返す。
        if let Some(reason) = self.violation.lock().await.as_ref() {
            return Err(McpClientError::ProtocolError(reason.clone()));
        }

        let mut rx = self.response_rx.lock().await;
        tokio::select! {
            result = rx.recv() => {
                match self.violation.lock().await.as_ref() {
                    Some(reason) => Err(McpClientError::ProtocolError(reason.clone())),
                    None => Ok(result),
                }
            }
            _ = self.violation_notify.notified() => {
                match self.violation.lock().await.as_ref() {
                    Some(reason) => Err(McpClientError::ProtocolError(reason.clone())),
                    None => Ok(None),
                }
            }
        }
    }

    async fn close(&self) -> Result<(), McpClientError> {
        let mut task = self.sse_task.lock().await;
        if let Some(handle) = task.take() {
            handle.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod stdio_framing_tests {
    use super::*;

    #[tokio::test]
    async fn reads_consecutive_content_length_framed_messages() {
        let first = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let second = r#"{"jsonrpc":"2.0","id":2,"result":{"ok":true}}"#;
        let script = format!(
            "printf 'Content-Length: {n}\\r\\n\\r\\n{s1}Content-Length: {n}\\r\\n\\r\\n{s2}'",
            n = first.len(),
            s1 = first,
            s2 = second
        );

        let transport = StdioTransport::new("sh", &["-c".to_string(), script], None, None)
            .await
            .expect("spawn framing fixture");

        let first_message = transport
            .receive()
            .await
            .expect("first receive")
            .expect("first message");
        match first_message {
            JsonRpcMessage::Response(response) => assert_eq!(response.id, 1),
            other => panic!("expected response, got {other:?}"),
        }

        let second_message = transport
            .receive()
            .await
            .expect("second receive")
            .expect("second message");
        match second_message {
            JsonRpcMessage::Response(response) => assert_eq!(response.id, 2),
            other => panic!("expected response, got {other:?}"),
        }

        transport.close().await.expect("close");
    }

    #[tokio::test]
    async fn oversized_content_length_is_rejected_and_disconnects() {
        // `printf` はシェルを使わずに不正なフレームを 1 回だけ書き出す。
        let transport = StdioTransport::new(
            "printf",
            &["Content-Length: 10485761\r\n\r\n".to_string()],
            None,
            None,
        )
        .await
        .expect("spawn framing fixture");

        let error = transport.receive().await.expect_err("must reject");
        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        // 切断済みなので後続の読み取りはストリーム終端になる。
        assert!(transport
            .receive()
            .await
            .expect("read after disconnect")
            .is_none());
        let _ = transport.close().await;
    }

    #[tokio::test]
    async fn excessive_header_lines_are_rejected() {
        let headers = "X-Pad: 1\r\n".repeat(MAX_HEADER_LINES + 8);
        let transport = StdioTransport::new("printf", &[format!("{headers}\r\n")], None, None)
            .await
            .expect("spawn framing fixture");
        let error = transport.receive().await.expect_err("must reject");
        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        let _ = transport.close().await;
    }

    #[tokio::test]
    async fn oversized_header_line_is_rejected() {
        let header = format!("X-Pad: {}\r\n", "a".repeat(MAX_HEADER_LINE_BYTES + 1));
        let transport = StdioTransport::new("printf", &[header], None, None)
            .await
            .expect("spawn framing fixture");
        let error = transport.receive().await.expect_err("must reject");
        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        let _ = transport.close().await;
    }
}

#[cfg(test)]
mod http_sse_limit_tests {
    //! HTTP / SSE トランスポートのサイズ上限と bounded キューの回帰テスト。
    //! 新規依存なしのループバック HTTP モック（127.0.0.1 のみ、外部送信なし）を使う。
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Once;
    use std::thread;
    use std::time::{Duration, Instant};

    fn ensure_no_proxy() {
        static SET_NO_PROXY: Once = Once::new();
        SET_NO_PROXY.call_once(|| {
            std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
            std::env::set_var("no_proxy", "127.0.0.1,localhost");
        });
    }

    /// 1 接続だけ捌く最小のループバック HTTP モック。
    /// サイズ超過でクライアントが早期切断してもスレッドを失敗させないため、
    /// レスポンスの書き込みエラーは無視する。
    fn serve_once(content_type: &'static str, body: String) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
        let addr = listener.local_addr().expect("loopback addr must succeed");
        let url = format!("http://{addr}/mcp");
        let handle = thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("mock listener must be nonblocking");
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            panic!("mock timed out waiting for a connection");
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => panic!("mock accept failed: {err}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("mock read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("mock write timeout");

            // リクエスト（ヘッダ + Content-Length 分のボディ）を読み捨てる。
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).expect("mock must read request");
                assert!(read > 0, "unexpected EOF while reading request");
                buf.extend_from_slice(&chunk[..read]);
                if let Some(position) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]);
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                let read = stream
                    .read(&mut chunk)
                    .expect("mock must read request body");
                assert!(read > 0, "unexpected EOF while reading request body");
                buf.extend_from_slice(&chunk[..read]);
            }

            let response_headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response_headers.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        });
        (url, handle)
    }

    #[tokio::test]
    async fn ordinary_http_response_is_still_buffered_and_received() {
        ensure_no_proxy();
        let body = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        let (url, handle) = serve_once("application/json", body.to_string());

        let transport = HttpTransport::new(url, None);
        transport
            .send(&JsonRpcRequest::new(7, "ping"))
            .await
            .expect("send must succeed");
        let message = transport
            .receive()
            .await
            .expect("receive must succeed")
            .expect("message must be buffered");
        match message {
            JsonRpcMessage::Response(response) => assert_eq!(response.id, 7),
            other => panic!("expected response, got {other:?}"),
        }
        handle.join().expect("mock thread must not panic");
    }

    #[tokio::test]
    async fn oversized_http_response_body_is_rejected_without_echoing_body() {
        ensure_no_proxy();
        let marker = "SECRET_BODY_MARKER_DO_NOT_LEAK";
        // 有効な JSON に上限を超える padding を入れる。
        let padding = format!("{marker}{}", "x".repeat(MAX_HTTP_BODY_SIZE));
        let body = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"pad\":\"{padding}\"}}}}");
        assert!(body.len() > MAX_HTTP_BODY_SIZE);
        let (url, handle) = serve_once("application/json", body);

        let transport = HttpTransport::new(url, None);
        let error = transport
            .send(&JsonRpcRequest::new(1, "ping"))
            .await
            .expect_err("oversized body must be rejected");

        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        let rendered = error.to_string();
        assert!(
            !rendered.contains(marker),
            "error must not echo the response body: {rendered}"
        );
        handle.join().expect("mock thread must not panic");
    }

    #[test]
    fn oversized_sse_event_is_rejected_without_echoing_data() {
        let data = "s".repeat(MAX_SSE_EVENT_SIZE + 1);
        let error = validate_sse_event_size(&data).expect_err("must reject");
        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        assert!(!error.to_string().contains(data.as_str()));
        validate_sse_event_size("{}").expect("normal event must pass");
    }

    #[test]
    fn enqueue_response_rejects_when_bounded_queue_is_full() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<JsonRpcMessage>(RESPONSE_QUEUE_CAPACITY);
        for id in 0..RESPONSE_QUEUE_CAPACITY as u64 {
            let message = JsonRpcMessage::parse_json_str(&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}"
            ))
            .expect("parse fixture");
            enqueue_response(&tx, message).expect("queue must accept up to capacity");
        }
        let extra = JsonRpcMessage::parse_json_str(r#"{"jsonrpc":"2.0","id":999,"result":{}}"#)
            .expect("parse fixture");
        let error = enqueue_response(&tx, extra).expect_err("full queue must reject");
        assert!(
            matches!(&error, McpClientError::TransportError(_)),
            "unexpected error: {error}"
        );
        assert!(
            rx.try_recv().is_ok(),
            "buffered messages must remain intact"
        );
    }

    #[tokio::test]
    async fn consecutive_sse_events_disconnect_when_bounded_queue_fills() {
        ensure_no_proxy();
        // キュー容量を確実に超える数のイベントを連続で送出する。
        let mut sse_body = String::new();
        for id in 0..(RESPONSE_QUEUE_CAPACITY + 8) {
            sse_body.push_str(&format!(
                "data: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}\n\n"
            ));
        }
        let (url, handle) = serve_once("text/event-stream", sse_body);

        let transport = SseTransport::new(url, None);
        transport.connect().await.expect("connect must succeed");

        // リスナがキュー詰まりを検出するまで待つ（タイムアウト付き）。
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if transport.violation.lock().await.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                panic!("SSE listener did not detect bounded queue overflow");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let error = transport
            .receive()
            .await
            .expect_err("overflow must surface as a transport error");
        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        let rendered = error.to_string();
        assert!(
            !rendered.contains("jsonrpc") && !rendered.contains('{'),
            "error must not echo event data: {rendered}"
        );
        transport.close().await.expect("close");
        handle.join().expect("mock thread must not panic");
    }

    /// R4 指摘 2: SSE がエラー（ここではストリーム終端由来の StreamEnded）を
    /// 出したとき、バックグラウンドリスナが violation を設定して receive() を
    /// 起こすため、receive() がエラーを返さず永久にハングしないことを検証する。
    #[tokio::test]
    async fn sse_error_is_surfaced_by_receive_without_hanging() {
        ensure_no_proxy();
        // 空の SSE ボディ: EventSource は Open の後に StreamEnded を Err として流す。
        let (url, handle) = serve_once("text/event-stream", String::new());

        let transport = SseTransport::new(url, None);
        transport.connect().await.expect("connect must succeed");

        let outcome = tokio::time::timeout(Duration::from_secs(5), transport.receive()).await;
        let error = outcome
            .expect("receive must not hang after an SSE error")
            .expect_err("SSE error must surface as an error");
        assert!(
            matches!(&error, McpClientError::ProtocolError(_)),
            "unexpected error: {error}"
        );
        // エラー本文を借用していないこと（JSON-RPC の断片が混ざらないこと）も確認。
        let rendered = error.to_string();
        assert!(!rendered.contains("jsonrpc") && !rendered.contains('{'));

        transport.close().await.expect("close");
        handle.join().expect("mock thread must not panic");
    }

    /// 接続を開いたままイベントを送らない SSE モック。
    /// リスナタスクが自然終了しないため、Drop の abort を観測できる。
    fn serve_sse_keep_open() -> (
        String,
        Arc<AtomicBool>,
        Arc<AtomicBool>,
        thread::JoinHandle<()>,
    ) {
        ensure_no_proxy();
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
        let addr = listener.local_addr().expect("loopback addr must succeed");
        let url = format!("http://{addr}/mcp");
        let connected = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let connected_thread = Arc::clone(&connected);
        let stop_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("mock listener must be nonblocking");
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            panic!("mock timed out waiting for a connection");
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => panic!("mock accept failed: {err}"),
                }
            };
            connected_thread.store(true, Ordering::Release);
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("mock read timeout");

            // リクエスト（ヘッダ + Content-Length 分のボディ）を読み捨てる。
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).expect("mock must read request");
                assert!(read > 0, "unexpected EOF while reading request");
                buf.extend_from_slice(&chunk[..read]);
                if let Some(position) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]);
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                let read = stream
                    .read(&mut chunk)
                    .expect("mock must read request body");
                assert!(read > 0, "unexpected EOF while reading request body");
                buf.extend_from_slice(&chunk[..read]);
            }

            // SSE ヘッダだけ送り、接続を開いたままイベントは送らない。
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n",
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while !stop_thread.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        (url, connected, stop, handle)
    }

    /// 401 リトライ等で旧 transport が単にドロップされる経路のリーク回帰。
    #[tokio::test]
    async fn dropping_sse_transport_aborts_background_listener() {
        let (url, connected, stop, server) = serve_sse_keep_open();

        let transport = SseTransport::new(url, None);
        transport.connect().await.expect("connect must succeed");

        // リスナが実際に接続するまで待ち、自然終了済みのタスクを
        // 計測しないようにする。
        let deadline = Instant::now() + Duration::from_secs(5);
        while !connected.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "SSE listener never connected");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let abort_handle = {
            let task = transport.sse_task.lock().await;
            task.as_ref()
                .expect("listener task must be registered")
                .abort_handle()
        };
        // 現行 tokio の AbortHandle には is_aborted() が無いため、
        // Drop が完了し、二重 abort も安全であることだけを検証する。
        drop(transport);
        abort_handle.abort();

        stop.store(true, Ordering::Release);
        server.join().expect("mock thread must not panic");
    }
}
