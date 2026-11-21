//! Language Server Protocol (LSP) クライアントを提供するクレート。
//!
//! `LspClient` が外部 LSP サーバーを子プロセスとして起動し、JSON-RPC で
//! 初期化・テキスト同期・診断・定義／参照／ホバー・補完・リネーム・
//! コールヒエラルキーなどのリクエストを送る。`LspClientRegistry` が
//! 複数のサーバーをパス単位で管理し、`LspEvent` を broadcast で配信する。
//!
//! # 使い方
//!
//! `LspServerConfig` とルートパスを指定して `LspClient::start` で起動し、
//! `get_diagnostics` や `goto_definition` を呼び出す。`subscribe` で
//! 非同期イベントを受け取れる。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use lsp_types::{
    ClientCapabilities, Diagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, Range, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, VersionedTextDocumentIdentifier, WorkspaceFolder,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: lsp_types::SymbolKind,
    #[serde(default)]
    pub tags: Option<Vec<lsp_types::SymbolTag>>,
    #[serde(default)]
    pub detail: Option<String>,
    pub uri: lsp_types::Uri,
    pub range: Range,
    pub selection_range: Range,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyPrepareParams {
    #[serde(flatten)]
    pub text_document_position_params: lsp_types::TextDocumentPositionParams,
    #[serde(flatten)]
    pub work_done_progress_params: lsp_types::WorkDoneProgressParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyIncomingCallsParams {
    pub item: CallHierarchyItem,
    #[serde(flatten)]
    pub work_done_progress_params: lsp_types::WorkDoneProgressParams,
    #[serde(flatten)]
    pub partial_result_params: lsp_types::PartialResultParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyOutgoingCallsParams {
    pub item: CallHierarchyItem,
    #[serde(flatten)]
    pub work_done_progress_params: lsp_types::WorkDoneProgressParams,
    #[serde(flatten)]
    pub partial_result_params: lsp_types::PartialResultParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyIncomingCall {
    pub from: CallHierarchyItem,
    pub from_ranges: Vec<Range>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyOutgoingCall {
    pub to: CallHierarchyItem,
    pub from_ranges: Vec<Range>,
}

fn path_to_uri(path: &Path) -> Result<lsp_types::Uri, LspError> {
    let url = Url::from_file_path(path)
        .map_err(|_| LspError::InitializeError("Invalid file path".to_string()))?;
    lsp_types::Uri::from_str(url.as_str())
        .map_err(|e| LspError::InitializeError(format!("Invalid URI: {}", e)))
}

fn uri_to_path(uri: &lsp_types::Uri) -> PathBuf {
    Url::parse(&uri.to_string())
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .unwrap_or_default()
}

#[derive(Debug, Error)]
pub enum LspError {
    #[error("Failed to start LSP server: {0}")]
    ServerStartError(String),

    #[error("Failed to initialize LSP: {0}")]
    InitializeError(String),

    #[error("JSON-RPC error: {0}")]
    JsonRpcError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Server not initialized")]
    NotInitialized,

    #[error("Timeout waiting for response")]
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

pub struct LspServerConfig {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub initialization_options: Option<Value>,
}

/// Upper bound for a single JSON-RPC request/response round-trip.
///
/// Without this bound a request whose response never arrives (e.g. the server
/// crashed or ignores the method) would wait forever on the oneshot channel.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// LSP ベースプロトコル 1 メッセージの本文サイズ上限。
///
/// サーバー stdout は外部入力のため、異常に大きな `Content-Length` を
/// 受け取った際の大量メモリ確保を防ぐ。
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// `reader` から LSP ベースプロトコルの 1 メッセージを読み取る。
///
/// ヘッダーは空行まで読み、`Content-Length` 以外のヘッダー
/// （`Content-Type` 等）は無視する。ヘッダー終了後、宣言されたバイト数を
/// `read_exact` で読み、UTF-8 文字列として返す。本文は改行を含む
/// 整形済み JSON でありうるため、行単位には分割しない。
///
/// - どのヘッダーも読まずに EOF に達した場合は `Ok(None)`（正常終了）。
/// - `Content-Length` の欠落・不正値・上限超過、本文の途中 EOF、
///   UTF-8 として不正な本文はエラー。
async fn read_lsp_message<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<String>, LspError> {
    let mut content_length: Option<usize> = None;
    let mut saw_header = false;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            if !saw_header {
                return Ok(None);
            }
            return Err(LspError::JsonRpcError(
                "Unexpected EOF while reading LSP headers".to_string(),
            ));
        }
        saw_header = true;

        // CRLF と LF のどちらの改行にも対応する。
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let len = value.trim().parse::<usize>().map_err(|e| {
                    LspError::JsonRpcError(format!("Invalid Content-Length: {}", e))
                })?;
                content_length = Some(len);
            }
            // Content-Length 以外のヘッダーは無視する。
        }
    }

    let len = content_length
        .ok_or_else(|| LspError::JsonRpcError("Missing Content-Length header".to_string()))?;
    if len > MAX_MESSAGE_BYTES {
        return Err(LspError::JsonRpcError(format!(
            "Content-Length {} exceeds limit {}",
            len, MAX_MESSAGE_BYTES
        )));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let body = String::from_utf8(buf)
        .map_err(|e| LspError::JsonRpcError(format!("Invalid UTF-8 in LSP body: {}", e)))?;

    Ok(Some(body))
}

pub struct LspClient {
    root: PathBuf,
    stdin: Arc<Mutex<ChildStdin>>,
    request_id: Arc<Mutex<u64>>,
    pending_responses: Arc<RwLock<PendingResponses>>,
    diagnostics: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    file_versions: Arc<RwLock<HashMap<PathBuf, u32>>>,
    event_tx: broadcast::Sender<LspEvent>,
    /// Kept alive for the lifetime of the client so it can be reaped on drop.
    child: Option<Child>,
    /// Reader task handle, kept so it can be aborted on drop.
    reader_handle: Option<JoinHandle<()>>,
}

type PendingResponses = HashMap<u64, tokio::sync::oneshot::Sender<Result<Value, LspError>>>;

#[derive(Debug, Clone)]
pub enum LspEvent {
    Diagnostics { path: PathBuf, server_id: String },
}

impl LspClient {
    pub async fn start(config: LspServerConfig, root: PathBuf) -> Result<Self, LspError> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| LspError::ServerStartError(e.to_string()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::ServerStartError("Failed to get stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::ServerStartError("Failed to get stdout".to_string()))?;

        let (event_tx, _) = broadcast::channel(256);

        let mut client = Self {
            root,
            stdin: Arc::new(Mutex::new(stdin)),
            request_id: Arc::new(Mutex::new(0)),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            file_versions: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            child: Some(child),
            reader_handle: None,
        };

        let pending = client.pending_responses.clone();
        let diagnostics = client.diagnostics.clone();
        let server_id = config.id.clone();
        let event_tx_clone = client.event_tx.clone();

        client.reader_handle = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);

            loop {
                let body = match read_lsp_message(&mut reader).await {
                    Ok(Some(body)) => body,
                    // Clean EOF: the server closed stdout.
                    Ok(None) => break,
                    Err(e) => {
                        error!("Failed to read LSP message: {}", e);
                        break;
                    }
                };

                if let Ok(notification) = serde_json::from_str::<JsonRpcNotification>(&body) {
                    if notification.method == "textDocument/publishDiagnostics" {
                        if let Some(params) = notification.params {
                            if let Ok(diag_params) = serde_json::from_value::<
                                lsp_types::PublishDiagnosticsParams,
                            >(params)
                            {
                                let path = uri_to_path(&diag_params.uri);

                                diagnostics
                                    .write()
                                    .await
                                    .insert(path.clone(), diag_params.diagnostics);

                                let _ = event_tx_clone.send(LspEvent::Diagnostics {
                                    path,
                                    server_id: server_id.clone(),
                                });
                            }
                        }
                    }
                } else if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(&body) {
                    if let Some(sender) = pending.write().await.remove(&response.id) {
                        let result = if let Some(error) = response.error {
                            Err(LspError::JsonRpcError(error.message))
                        } else {
                            Ok(response.result.unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                }
            }
        }));

        client.initialize(config.initialization_options).await?;

        Ok(client)
    }

    async fn initialize(&mut self, initialization_options: Option<Value>) -> Result<(), LspError> {
        let workspace_uri = path_to_uri(&self.root)?;

        let params = InitializeParams {
            initialization_options,
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: workspace_uri,
                name: "workspace".to_string(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };

        let result = self
            .request("initialize", serde_json::to_value(params)?)
            .await?;
        debug!(?result, "LSP initialized");

        self.notify("initialized", Value::Null).await?;

        Ok(())
    }

    async fn next_id(&self) -> u64 {
        let mut id = self.request_id.lock().await;
        *id += 1;
        *id
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT)
            .await
    }

    /// Sends a JSON-RPC request and waits for its response at most `timeout`.
    ///
    /// The stdin mutex is released before waiting so that concurrent requests
    /// and notifications are not serialized behind a slow response.
    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, LspError> {
        let id = self.next_id().await;
        let (tx, rx) = tokio::sync::oneshot::channel();

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        // Serialize before registering the pending entry: a serialization
        // failure must not leave an orphaned entry in the map.
        let content = serde_json::to_string(&request)?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        self.pending_responses.write().await.insert(id, tx);

        // Scope the stdin guard so it is dropped before we await the response.
        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = stdin.write_all(message.as_bytes()).await {
                self.pending_responses.write().await.remove(&id);
                return Err(LspError::IoError(e));
            }
            if let Err(e) = stdin.flush().await {
                self.pending_responses.write().await.remove(&id);
                return Err(LspError::IoError(e));
            }
        }

        let outcome = tokio::time::timeout(timeout, rx).await;
        // Always drop our pending entry, on success (the reader already
        // removed it, so this is a no-op) and especially on timeout/drop.
        self.pending_responses.write().await.remove(&id);

        match outcome {
            Ok(Ok(value)) => value,
            // Sender dropped without a response, or the deadline elapsed.
            Ok(Err(_)) | Err(_) => Err(LspError::Timeout),
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: if params.is_null() { None } else { Some(params) },
        };

        let content = serde_json::to_string(&notification)?;
        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(message.as_bytes()).await?;
        stdin.flush().await?;

        Ok(())
    }

    pub async fn open_document(
        &self,
        path: &Path,
        content: &str,
        language_id: &str,
    ) -> Result<(), LspError> {
        let uri = path_to_uri(path)?;

        let version = {
            let mut versions = self.file_versions.write().await;
            let v = versions.entry(path.to_path_buf()).or_insert(0);
            *v
        };

        if version > 0 {
            let next_version = version + 1;
            self.file_versions
                .write()
                .await
                .insert(path.to_path_buf(), next_version);

            let params = DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri,
                    version: next_version as i32,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: content.to_string(),
                }],
            };

            self.notify("textDocument/didChange", serde_json::to_value(params)?)
                .await?;
        } else {
            self.file_versions
                .write()
                .await
                .insert(path.to_path_buf(), 0);
            self.diagnostics.write().await.remove(path);

            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: language_id.to_string(),
                    version: 0,
                    text: content.to_string(),
                },
            };

            self.notify("textDocument/didOpen", serde_json::to_value(params)?)
                .await?;
        }

        Ok(())
    }

    /// Closes a previously opened document.
    ///
    /// Drops both the tracked version and any cached diagnostics for `path`,
    /// then notifies the server with `textDocument/didClose`. Calling this for
    /// a file that was never opened is harmless: the removals are no-ops and
    /// the notification lets the server discard any state it still holds.
    pub async fn close_document(&self, path: &Path) -> Result<(), LspError> {
        let uri = path_to_uri(path)?;

        let params = DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        };

        // 通知を先に送り、成功後にローカル状態を削除する（R4）。
        // 失敗時にサーバーは open のままなのにローカルだけ消える乖離を防ぐ。
        self.notify("textDocument/didClose", serde_json::to_value(params)?)
            .await?;

        self.file_versions.write().await.remove(path);
        self.diagnostics.write().await.remove(path);

        Ok(())
    }

    pub async fn get_diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.diagnostics
            .read()
            .await
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns all diagnostics from all files this LSP server has reported on.
    pub async fn get_all_diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>> {
        self.diagnostics.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LspEvent> {
        self.event_tx.subscribe()
    }

    pub async fn goto_definition(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<lsp_types::Location>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::GotoDefinitionParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .request("textDocument/definition", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        let response: lsp_types::GotoDefinitionResponse = serde_json::from_value(result)?;
        match response {
            lsp_types::GotoDefinitionResponse::Scalar(loc) => Ok(Some(loc)),
            lsp_types::GotoDefinitionResponse::Array(locs) => Ok(locs.into_iter().next()),
            lsp_types::GotoDefinitionResponse::Link(links) => {
                Ok(links.into_iter().next().map(|l| lsp_types::Location {
                    uri: l.target_uri,
                    range: l.target_selection_range,
                }))
            }
        }
    }

    pub async fn completion(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<lsp_types::CompletionItem>>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let result = self
            .request("textDocument/completion", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        let response: lsp_types::CompletionResponse = serde_json::from_value(result)?;
        Ok(Some(match response {
            lsp_types::CompletionResponse::Array(items) => items,
            lsp_types::CompletionResponse::List(list) => list.items,
        }))
    }

    pub async fn references(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<lsp_types::Location>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::ReferenceParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp_types::ReferenceContext {
                include_declaration: true,
            },
        };

        let result = self
            .request("textDocument/references", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let locations: Vec<lsp_types::Location> = serde_json::from_value(result)?;
        Ok(locations)
    }

    pub async fn hover(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<lsp_types::Hover>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
        };

        let result = self
            .request("textDocument/hover", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        let hover: lsp_types::Hover = serde_json::from_value(result)?;
        Ok(Some(hover))
    }

    pub async fn document_symbol(
        &self,
        path: &Path,
    ) -> Result<Vec<lsp_types::SymbolInformation>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .request("textDocument/documentSymbol", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let symbols: lsp_types::DocumentSymbolResponse = serde_json::from_value(result)?;
        Ok(match symbols {
            lsp_types::DocumentSymbolResponse::Flat(symbols) => symbols,
            lsp_types::DocumentSymbolResponse::Nested(nested) => nested
                .into_iter()
                .flat_map(|s| flatten_document_symbol(&s))
                .collect(),
        })
    }

    pub async fn workspace_symbol(
        &self,
        query: &str,
    ) -> Result<Vec<lsp_types::SymbolInformation>, LspError> {
        let params = lsp_types::WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .request("workspace/symbol", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let symbols: Vec<lsp_types::SymbolInformation> = serde_json::from_value(result)?;
        Ok(symbols)
    }

    pub async fn goto_implementation(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<lsp_types::Location>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::request::GotoImplementationParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .request("textDocument/implementation", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let response: lsp_types::GotoDefinitionResponse = serde_json::from_value(result)?;
        Ok(match response {
            lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
            lsp_types::GotoDefinitionResponse::Array(locs) => locs,
            lsp_types::GotoDefinitionResponse::Link(links) => links
                .into_iter()
                .map(|l| lsp_types::Location {
                    uri: l.target_uri,
                    range: l.target_selection_range,
                })
                .collect(),
        })
    }

    pub async fn type_definition(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<lsp_types::Location>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: lsp_types::Position { line, character },
        };

        let result = self
            .request("textDocument/typeDefinition", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let response: lsp_types::GotoDefinitionResponse = serde_json::from_value(result)?;
        Ok(match response {
            lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
            lsp_types::GotoDefinitionResponse::Array(locs) => locs,
            lsp_types::GotoDefinitionResponse::Link(links) => links
                .into_iter()
                .map(|l| lsp_types::Location {
                    uri: l.target_uri,
                    range: l.target_selection_range,
                })
                .collect(),
        })
    }

    pub async fn rename(
        &self,
        path: &Path,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<Option<lsp_types::WorkspaceEdit>, LspError> {
        let uri = path_to_uri(path)?;

        let params = lsp_types::RenameParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };

        let result = self
            .request("textDocument/rename", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        let edit: lsp_types::WorkspaceEdit = serde_json::from_value(result)?;
        Ok(Some(edit))
    }

    pub async fn prepare_call_hierarchy(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        let uri = path_to_uri(path)?;

        let params = CallHierarchyPrepareParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
        };

        let result = self
            .request(
                "textDocument/prepareCallHierarchy",
                serde_json::to_value(params)?,
            )
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let items: Vec<CallHierarchyItem> = serde_json::from_value(result)?;
        Ok(items)
    }

    pub async fn incoming_calls(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<CallHierarchyIncomingCall>, LspError> {
        let items = self.prepare_call_hierarchy(path, line, character).await?;

        if items.is_empty() {
            return Ok(vec![]);
        }

        let item = &items[0];
        let params = CallHierarchyIncomingCallsParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .request("callHierarchy/incomingCalls", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let calls: Vec<CallHierarchyIncomingCall> = serde_json::from_value(result)?;
        Ok(calls)
    }

    pub async fn outgoing_calls(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<CallHierarchyOutgoingCall>, LspError> {
        let items = self.prepare_call_hierarchy(path, line, character).await?;

        if items.is_empty() {
            return Ok(vec![]);
        }

        let item = &items[0];
        let params = CallHierarchyOutgoingCallsParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = self
            .request("callHierarchy/outgoingCalls", serde_json::to_value(params)?)
            .await?;

        if result.is_null() {
            return Ok(vec![]);
        }

        let calls: Vec<CallHierarchyOutgoingCall> = serde_json::from_value(result)?;
        Ok(calls)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Stop the stdout reader task first, then terminate the server
        // process so no child is left running when the client goes away.
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

#[allow(deprecated)]
fn flatten_document_symbol(
    symbol: &lsp_types::DocumentSymbol,
) -> Vec<lsp_types::SymbolInformation> {
    let mut result = vec![];

    result.push(lsp_types::SymbolInformation {
        name: symbol.name.clone(),
        kind: symbol.kind,
        tags: symbol.tags.clone(),
        deprecated: symbol.deprecated,
        location: lsp_types::Location {
            uri: path_to_uri(&std::path::PathBuf::new())
                .unwrap_or_else(|_| lsp_types::Uri::from_str("file:///").unwrap()),
            range: symbol.selection_range,
        },
        container_name: symbol.detail.clone(),
    });

    if let Some(children) = &symbol.children {
        for child in children {
            result.extend(flatten_document_symbol(child));
        }
    }

    result
}

pub struct LspClientRegistry {
    clients: RwLock<HashMap<String, Arc<LspClient>>>,
}

impl LspClientRegistry {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, id: String, client: Arc<LspClient>) {
        self.clients.write().await.insert(id, client);
    }

    /// Removes the client registered under `id`, returning it if present.
    ///
    /// The returned `Arc` is handed back to the caller so dropping the last
    /// reference runs `LspClient::drop` and reaps the server process.
    pub async fn unregister(&self, id: &str) -> Option<Arc<LspClient>> {
        self.clients.write().await.remove(id)
    }

    pub async fn get(&self, id: &str) -> Option<Arc<LspClient>> {
        self.clients.read().await.get(id).cloned()
    }

    pub async fn list(&self) -> Vec<(String, Arc<LspClient>)> {
        self.clients
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Returns true if any LSP clients are currently registered.
    /// Mirrors the TS `LSP.hasClients(file)` which checks whether any server
    /// could handle the given file. Here we check if any registered client's
    /// id contains the detected language for the file.
    pub async fn has_clients(&self, path: &Path) -> bool {
        let clients = self.clients.read().await;
        if clients.is_empty() {
            return false;
        }
        let language = detect_language(path);
        clients.keys().any(|id| id.contains(language))
    }

    /// Opens or refreshes a file in all matching LSP clients.
    /// Mirrors the TS `LSP.touchFile(input, waitForDiagnostics)`.
    ///
    /// - Reads the file content from disk
    /// - For each registered client whose id matches the file's language,
    ///   calls `open_document` (which internally handles didOpen vs didChange)
    /// - If `wait_for_diagnostics` is true, waits briefly for diagnostics to arrive
    pub async fn touch_file(
        &self,
        path: &Path,
        wait_for_diagnostics: bool,
    ) -> Result<(), LspError> {
        let language = detect_language(path);
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(LspError::IoError)?;

        let clients = self.clients.read().await;
        let matching: Vec<Arc<LspClient>> = clients
            .iter()
            .filter(|(id, _)| id.contains(language))
            .map(|(_, c)| c.clone())
            .collect();
        drop(clients);

        for client in &matching {
            if let Err(e) = client.open_document(path, &content, language).await {
                error!("Failed to touch file {:?} in LSP: {}", path, e);
            }
        }

        if wait_for_diagnostics && !matching.is_empty() {
            // Give LSP servers a moment to produce diagnostics.
            // The TS version uses client.waitForDiagnostics which listens
            // for a publishDiagnostics notification. We approximate this
            // with a short sleep, since the broadcast-based event system
            // already stores diagnostics as they arrive.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        Ok(())
    }
}

impl Default for LspClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn detect_language(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("ts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("js") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("py") => "python",
        Some("go") => "go",
        Some("java") => "java",
        Some("c") => "c",
        Some("cpp") | Some("cc") | Some("cxx") => "cpp",
        Some("h") | Some("hpp") => "cpp",
        Some("rb") => "ruby",
        Some("php") => "php",
        Some("swift") => "swift",
        Some("kt") => "kotlin",
        Some("scala") => "scala",
        Some("lua") => "lua",
        Some("json") => "json",
        Some("yaml") | Some("yml") => "yaml",
        Some("toml") => "toml",
        Some("md") => "markdown",
        Some("html") => "html",
        Some("css") => "css",
        Some("scss") => "scss",
        _ => "plaintext",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language(Path::new("main.rs")), "rust");
        assert_eq!(detect_language(Path::new("index.ts")), "typescript");
        assert_eq!(detect_language(Path::new("app.tsx")), "typescriptreact");
        assert_eq!(detect_language(Path::new("main.py")), "python");
        assert_eq!(detect_language(Path::new("main.go")), "go");
        assert_eq!(detect_language(Path::new("unknown.xyz")), "plaintext");
    }

    #[test]
    fn test_json_rpc_request_serialization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "initialize".to_string(),
            params: Value::Null,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn test_json_rpc_response_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
        let response: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.id, 1);
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn test_has_clients_empty_registry() {
        let registry = LspClientRegistry::new();
        assert!(!registry.has_clients(Path::new("main.rs")).await);
        assert!(!registry.has_clients(Path::new("index.ts")).await);
    }

    #[tokio::test]
    async fn test_registry_default() {
        let registry = LspClientRegistry::default();
        let clients = registry.list().await;
        assert!(clients.is_empty());
    }

    /// Builds a client backed by a stub child so `notify` can be exercised
    /// without launching a real language server.
    #[cfg(unix)]
    fn stub_client() -> LspClient {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec sleep 30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        drop(stdout);

        let (event_tx, _) = broadcast::channel(16);
        LspClient {
            root: PathBuf::from("/tmp"),
            stdin: Arc::new(Mutex::new(stdin)),
            request_id: Arc::new(Mutex::new(0)),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            file_versions: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            child: Some(child),
            reader_handle: None,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_document_clears_version_and_diagnostics() {
        let client = stub_client();
        let path = Path::new("/tmp/project/src/main.rs");

        // Seed the same entries that `open_document` and publishDiagnostics
        // would have created.
        client
            .file_versions
            .write()
            .await
            .insert(path.to_path_buf(), 3);
        client
            .diagnostics
            .write()
            .await
            .insert(path.to_path_buf(), vec![Diagnostic::default()]);
        assert!(client.file_versions.read().await.contains_key(path));
        assert!(client.diagnostics.read().await.contains_key(path));

        client.close_document(path).await.unwrap();

        assert!(
            !client.file_versions.read().await.contains_key(path),
            "file version survived close_document"
        );
        assert!(
            !client.diagnostics.read().await.contains_key(path),
            "diagnostics survived close_document"
        );
        // Closing an unknown file must stay harmless and must not resurrect
        // either map.
        client
            .close_document(Path::new("/tmp/project/src/other.rs"))
            .await
            .unwrap();
        assert!(client.file_versions.read().await.is_empty());
        assert!(client.diagnostics.read().await.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unregister_removes_only_the_requested_client() {
        let registry = LspClientRegistry::new();
        let client = Arc::new(stub_client());
        registry.register("rust-analyzer".to_string(), client).await;
        assert!(registry.get("rust-analyzer").await.is_some());

        let removed = registry.unregister("rust-analyzer").await;
        assert!(removed.is_some());
        assert!(registry.get("rust-analyzer").await.is_none());
        assert!(registry.unregister("rust-analyzer").await.is_none());
    }

    #[test]
    fn path_uri_roundtrip() {
        let path = Path::new("/tmp/project/src/main.rs");
        let uri = path_to_uri(path).unwrap();
        assert_eq!(uri_to_path(&uri), path);
    }

    #[test]
    fn detect_language_c_family() {
        assert_eq!(detect_language(Path::new("a.cpp")), "cpp");
        assert_eq!(detect_language(Path::new("a.cc")), "cpp");
        assert_eq!(detect_language(Path::new("a.hpp")), "cpp");
        assert_eq!(detect_language(Path::new("a.c")), "c");
    }

    #[test]
    fn detect_language_config_and_docs() {
        assert_eq!(detect_language(Path::new("Cargo.toml")), "toml");
        assert_eq!(detect_language(Path::new("a.yaml")), "yaml");
        assert_eq!(detect_language(Path::new("a.yml")), "yaml");
        assert_eq!(detect_language(Path::new("README.md")), "markdown");
        assert_eq!(detect_language(Path::new("a.json")), "json");
    }

    #[test]
    fn detect_language_unknown_is_plaintext() {
        assert_eq!(detect_language(Path::new("file.unknown")), "plaintext");
        assert_eq!(detect_language(Path::new("LICENSE")), "plaintext");
    }

    #[test]
    fn uri_to_path_invalid_uri_is_empty() {
        use std::str::FromStr;
        let uri = lsp_types::Uri::from_str("not-a-uri").unwrap();
        assert_eq!(uri_to_path(&uri), PathBuf::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn request_times_out_and_removes_pending_entry() {
        // A child that never responds to JSON-RPC requests.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec sleep 30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        drop(stdout);

        let (event_tx, _) = broadcast::channel(16);
        let client = LspClient {
            root: PathBuf::from("/tmp"),
            stdin: Arc::new(Mutex::new(stdin)),
            request_id: Arc::new(Mutex::new(0)),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            diagnostics: Arc::new(RwLock::new(HashMap::new())),
            file_versions: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            child: Some(child),
            reader_handle: None,
        };

        let result = client
            .request_with_timeout("test/noReply", Value::Null, Duration::from_millis(50))
            .await;

        assert!(matches!(result, Err(LspError::Timeout)));
        assert!(
            client.pending_responses.read().await.is_empty(),
            "pending entry leaked after timeout"
        );
        // Dropping the client kills the child via `Drop::drop`.
    }

    // --- Content-Length framing tests (pure Cursor/BufReader, no server) ---

    #[tokio::test]
    async fn read_message_ignores_non_content_length_headers() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
        let message = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\nX-Custom: ignored\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = BufReader::new(Cursor::new(message.into_bytes()));

        let payload = read_lsp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(payload, body);

        let response: JsonRpcResponse = serde_json::from_str(&payload).unwrap();
        assert_eq!(response.id, 1);
        assert!(response.result.is_some());
    }

    #[tokio::test]
    async fn read_message_consecutive_messages() {
        let body1 = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/a.rs","diagnostics":[]}}"#;
        let body2 = r#"{"jsonrpc":"2.0","id":7,"result":null}"#;
        let message = format!(
            "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
            body1.len(),
            body1,
            body2.len(),
            body2
        );
        let mut reader = BufReader::new(Cursor::new(message.into_bytes()));

        let first = read_lsp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(first, body1);
        let notification: JsonRpcNotification = serde_json::from_str(&first).unwrap();
        assert_eq!(notification.method, "textDocument/publishDiagnostics");

        let second = read_lsp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(second, body2);
        let response: JsonRpcResponse = serde_json::from_str(&second).unwrap();
        assert_eq!(response.id, 7);

        assert!(read_lsp_message(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_message_pretty_printed_json() {
        let body = "{\n  \"jsonrpc\": \"2.0\",\n  \"id\": 42,\n  \"result\": {\n    \"capabilities\": {}\n  }\n}";
        let message = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = BufReader::new(Cursor::new(message.into_bytes()));

        let payload = read_lsp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(payload, body);

        let response: JsonRpcResponse = serde_json::from_str(&payload).unwrap();
        assert_eq!(response.id, 42);
        assert!(response.result.is_some());
    }

    #[tokio::test]
    async fn read_message_lf_only_and_lowercase_header() {
        let body = r#"{"jsonrpc":"2.0","id":9,"result":null}"#;
        let message = format!("content-length: {}\n\n{}", body.len(), body);
        let mut reader = BufReader::new(Cursor::new(message.into_bytes()));

        let payload = read_lsp_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(payload, body);
    }

    #[tokio::test]
    async fn read_message_truncated_body_is_error() {
        let body = "{}";
        let declared = body.len() + 64;
        let message = format!("Content-Length: {}\r\n\r\n{}", declared, body);
        let mut reader = BufReader::new(Cursor::new(message.into_bytes()));

        assert!(read_lsp_message(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn read_message_invalid_content_length_is_error() {
        let message = "Content-Length: not-a-number\r\n\r\n{}";
        let mut reader = BufReader::new(Cursor::new(message.as_bytes().to_vec()));

        assert!(read_lsp_message(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn read_message_missing_content_length_is_error() {
        let message = "Content-Type: application/vscode-jsonrpc\r\n\r\n{}";
        let mut reader = BufReader::new(Cursor::new(message.as_bytes().to_vec()));

        assert!(read_lsp_message(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn read_message_clean_eof_is_none() {
        let mut reader = BufReader::new(Cursor::new(Vec::<u8>::new()));

        assert!(read_lsp_message(&mut reader).await.unwrap().is_none());
    }
}
