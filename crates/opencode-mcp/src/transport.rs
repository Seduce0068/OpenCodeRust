use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
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
    response_rx: Mutex<tokio::sync::mpsc::UnboundedReceiver<JsonRpcMessage>>,
    response_tx: tokio::sync::mpsc::UnboundedSender<JsonRpcMessage>,
}

impl HttpTransport {
    pub fn new(url: String, headers: Option<HashMap<String, String>>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
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
            let text = resp.text().await.map_err(|e| {
                McpClientError::TransportError(format!("Failed to read SSE body: {}", e))
            })?;
            for line in text.lines() {
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(message) = JsonRpcMessage::parse_json_str(data) {
                        let _ = self.response_tx.send(message);
                    }
                }
            }
        } else {
            // Plain JSON response.
            let text = resp.text().await.map_err(|e| {
                McpClientError::TransportError(format!("Failed to read response body: {}", e))
            })?;
            if !text.is_empty() {
                let message = JsonRpcMessage::parse_json_str(&text).map_err(|e| {
                    McpClientError::ProtocolError(format!("Failed to parse response: {}", e))
                })?;
                let _ = self.response_tx.send(message);
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
    response_rx: Mutex<tokio::sync::mpsc::UnboundedReceiver<JsonRpcMessage>>,
    response_tx: tokio::sync::mpsc::UnboundedSender<JsonRpcMessage>,
    /// Handle to the background SSE listener task so we can abort on close.
    sse_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SseTransport {
    pub fn new(url: String, headers: Option<HashMap<String, String>>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            url,
            headers: headers.unwrap_or_default(),
            client: reqwest::Client::new(),
            response_rx: Mutex::new(rx),
            response_tx: tx,
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

        let handle = tokio::spawn(async move {
            while let Some(event) = es.next().await {
                match event {
                    Ok(Event::Message(msg)) => {
                        let data = msg.data.trim().to_string();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        match JsonRpcMessage::parse_json_str(&data) {
                            Ok(msg) => {
                                if tx.send(msg).is_err() {
                                    break;
                                }
                            }
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
                        break;
                    }
                }
            }
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
        let mut rx = self.response_rx.lock().await;
        match rx.recv().await {
            Some(msg) => Ok(Some(msg)),
            None => Ok(None),
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
