//! Persistent storage for MCP OAuth credentials.
//!
//! Mirrors the TypeScript `McpAuth` namespace – stores tokens, client info,
//! code verifiers and OAuth state in a JSON file inside the user data directory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::fs;

/// プロセス共有の書き込み直列化ロック。
///
/// `read_all() → 変更 → write_all()` は不可分でなければならない。2 並行の
/// `try_set` が同じ元データを読むと、後から書いた側が先の変更を上書きして
/// 片方のキー（例: OAuth フローの verifier/state）が失われる。`try_set` /
/// `try_remove` / `try_update_*` はこのロックを全体で保持し、
/// read-modify-write を直列化する。`read_all()` 自体は並行読み取りを許可する。
static WRITE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// 書き込み直列化ロックを取得する（初回呼び出しで初期化）。
fn write_lock() -> &'static tokio::sync::Mutex<()> {
    WRITE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 権限是正失敗の安全な詳細。
///
/// 生の [`std::io::Error`] を保持すると `source()` や `From` 変換先の
/// `Display` に OS エラーの生メッセージ（シークレットやパスを含みうる）が
/// 流出するため、ファイル名（basename のみ）と [`std::io::ErrorKind`] だけを残す。
#[derive(Debug)]
pub struct PermissionsFailure {
    file_name: String,
    kind: std::io::ErrorKind,
}

impl PermissionsFailure {
    fn new(file_name: &str, kind: std::io::ErrorKind) -> Self {
        Self {
            file_name: file_name.to_owned(),
            kind,
        }
    }

    /// ファイル名（フルパスではない）を返す。
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// OS エラーの種別を返す（生メッセージは保持しない）。
    pub fn kind(&self) -> std::io::ErrorKind {
        self.kind
    }
}

impl std::fmt::Display for PermissionsFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not secure '{}' ({:?})", self.file_name, self.kind)
    }
}

impl std::error::Error for PermissionsFailure {}

/// auth store の読み込み失敗。
///
/// 以前は I/O エラーと JSON 破損を「空の store」へ丸めて fail-open していた。
/// 認証状態や PKCE state を黙って捨てると security 上問題があるため、
/// 失敗を型として呼び出し側へ伝播できるようにする。
#[derive(Debug, thiserror::Error)]
pub enum AuthStoreError {
    #[error("failed to read MCP auth store: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse MCP auth store: {0}")]
    Json(#[from] serde_json::Error),
    /// 既存の認証ファイルを 0600 へ是正できず、読み込みを拒否した場合。
    ///
    /// `source()` も含めファイル名と `ErrorKind` 以外の情報（OS の生メッセージ、
    /// フルパス、トークン値）は一切出さない。
    #[error("MCP auth store permissions could not be secured; refusing to load credentials")]
    PermissionsNotSecured(#[source] PermissionsFailure),
}

impl From<AuthStoreError> for std::io::Error {
    fn from(error: AuthStoreError) -> Self {
        match error {
            AuthStoreError::Io(io_error) => io_error,
            AuthStoreError::Json(json_error) => std::io::Error::other(json_error),
            // 生の OS エラーを引き継ぐと固定文言の意味が無くなるため、
            // 変換先も固定文言の PermissionDenied にマッピングする。
            AuthStoreError::PermissionsNotSecured(_) => std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "MCP auth store permissions could not be secured",
            ),
        }
    }
}

/// OAuth tokens obtained from the authorization server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) at which the access token expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Dynamically-registered (or pre-configured) client information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthClientInfo {
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id_issued_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret_expires_at: Option<f64>,
}

/// A single entry in the auth store, keyed by MCP server name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<OAuthTokens>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<OAuthClientInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_state: Option<String>,
    /// The server URL these credentials belong to – invalidated on URL change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    /// Discovered authorization endpoint; persisted so a restart keeps working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    /// Discovered token endpoint; persisted so a restart keeps working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
}
/// Resolve the path to the auth JSON file.
fn auth_file_path() -> PathBuf {
    // `OPENCODE_MCP_AUTH_FILE` lets tests and embedded callers redirect the
    // credential store away from the user's real data directory.
    if let Ok(path) = std::env::var("OPENCODE_MCP_AUTH_FILE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode");
    data_dir.join("mcp-auth.json")
}

/// Read the entire auth store from disk, propagating I/O and JSON failures.
///
/// I/O エラー・JSON 破損を空 store へ丸めず [`AuthStoreError`] として返す。
/// 新規コードはこの関数（または `try_*` 公開 API）を使うこと。
///
/// Unix では既存ファイルを 0600 へ是正し、是正できない場合は
/// [`AuthStoreError::PermissionsNotSecured`] として fail-closed で拒否する。
async fn read_all() -> Result<HashMap<String, AuthEntry>, AuthStoreError> {
    let path = auth_file_path();
    // 未作成ファイルは初回状態なので空 store として扱う。破損や権限エラーは
    // fail-loud を維持するため、NotFound のみ特別扱いする。
    let contents = match fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(AuthStoreError::Io(error)),
    };
    // On Unix, tighten a pre-existing over-permissive store on access.
    // 是正できない場合は fail-closed として読み込みを拒否する（warn も併発）。
    #[cfg(unix)]
    tighten_permissions(&path).await?;
    Ok(serde_json::from_str(&contents)?)
}

/// 既存シグネチャ互換の**非推奨**ラッパ（fail-open）。
///
/// 旧呼び出し側がコンパイルを続けられるように残すが、読み込み失敗時は
/// 空 store を返す **fail-open** 動作である。失敗は `tracing::error!` へ残すが、
/// 認証状態を失いうるため新規コードは絶対に使わないこと。
/// 代わりに [`read_all`] もしくは `try_*` API を使うこと。
#[deprecated(note = "fail-open: 失敗時に空 store を返す。read_all / try_* を使うこと")]
pub async fn read_all_or_default() -> HashMap<String, AuthEntry> {
    match read_all().await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(
                %error,
                "MCP auth store read failed; falling back to empty store for compatibility"
            );
            HashMap::new()
        }
    }
}

/// 権限是正の失敗を warn に残しつつ、fail-closed のエラーを組み立てる。
///
/// ログ・エラー・`source()` にはファイル名と `ErrorKind` のみを含め、
/// フルパス・OS の生メッセージ・トークン値は出さない。
#[cfg(unix)]
fn permissions_failure(path: &std::path::Path, error: std::io::Error) -> AuthStoreError {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mcp-auth.json");
    let kind = error.kind();
    tracing::warn!(
        file = %file_name,
        kind = ?kind,
        "failed to tighten MCP auth store permissions; refusing to load credentials"
    );
    AuthStoreError::PermissionsNotSecured(PermissionsFailure::new(file_name, kind))
}

/// Bring an existing credential file back to owner-only permissions (Unix).
///
/// 是正できない場合は [`AuthStoreError::PermissionsNotSecured`] を返し、
/// 呼び出し側の `read_all` が fail-closed で読み込みを拒否する。
#[cfg(unix)]
async fn tighten_permissions(path: &std::path::Path) -> Result<(), AuthStoreError> {
    use std::os::unix::fs::PermissionsExt;

    let meta = fs::metadata(path)
        .await
        .map_err(|error| permissions_failure(path, error))?;
    if meta.permissions().mode() & 0o777 != 0o600 {
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|error| permissions_failure(path, error))?;
    }
    Ok(())
}

/// Write the entire auth store to disk (creates parent dirs as needed).
async fn write_all(data: &HashMap<String, AuthEntry>) -> Result<(), std::io::Error> {
    let path = auth_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let json = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
    write_credential_file(&path, json.as_bytes()).await
}

/// Platform-specific credential write.
///
/// On Unix the file is created with 0600 and swapped into place via an atomic
/// rename: readers never observe a partially written store, and the final
/// file is never group/world readable even if a 0644 file existed before.
#[cfg(unix)]
async fn write_credential_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

    let tmp_path = temp_sibling_path(path);

    // create_new avoids clobbering an unrelated file; mode(0o600) is the first
    // line of defence (umask can only ever make it stricter).
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    let mut file = opts.open(&tmp_path).await?;

    // Pin the exact 0600 regardless of an unusual umask.
    fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600)).await?;

    let written = async {
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        Ok::<(), std::io::Error>(())
    }
    .await;

    drop(file);

    if let Err(err) = written {
        let _ = fs::remove_file(&tmp_path).await;
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, path).await {
        let _ = fs::remove_file(&tmp_path).await;
        return Err(err);
    }

    Ok(())
}

/// Windows keeps the previous behaviour: a direct overwrite write.
#[cfg(not(unix))]
async fn write_credential_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    fs::write(path, bytes).await
}

/// Unique sibling path for the atomic-write temporary file.
#[cfg(unix)]
fn temp_sibling_path(path: &std::path::Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mcp-auth");

    path.with_file_name(format!(
        ".{stem}.{pid:x}.{nanos:x}.{seq:x}.tmp",
        pid = std::process::id()
    ))
}

// ---------------------------------------------------------------------------
// Public API – mirrors TS McpAuth namespace
// ---------------------------------------------------------------------------

/// Get the auth entry for a given MCP server name, propagating read failures.
pub async fn try_get(mcp_name: &str) -> Result<Option<AuthEntry>, AuthStoreError> {
    Ok(read_all().await?.get(mcp_name).cloned())
}

/// Get the auth entry for a given MCP server name (非推奨の互換ラッパ・fail-open)。
///
/// 読み込み失敗はログへ残し `None` を返す **fail-open** 動作である。
/// 認証情報を失いうるため新規コードは使用禁止。代わりに [`try_get`] を使うこと。
#[deprecated(note = "fail-open: 失敗時に None を返す。try_get を使うこと")]
pub async fn get(mcp_name: &str) -> Option<AuthEntry> {
    try_get(mcp_name).await.unwrap_or_else(|error| {
        tracing::error!(%error, mcp_name, "MCP auth entry read failed");
        None
    })
}

/// Get the auth entry only if it was stored for the same `server_url`.
pub async fn try_get_for_url(
    mcp_name: &str,
    server_url: &str,
) -> Result<Option<AuthEntry>, AuthStoreError> {
    let Some(entry) = try_get(mcp_name).await? else {
        return Ok(None);
    };
    Ok(match &entry.server_url {
        Some(url) if url == server_url => Some(entry),
        _ => None,
    })
}

/// 非推奨の互換ラッパ。[`try_get_for_url`] へ委譲し、読み込み失敗を `Err` として伝播する。
///
/// 以前は失敗を飲んで `None` を返す fail-open だったが、credential の
/// 誤放棄を防ぐため fail-loud に変更した。新規コードは [`try_get_for_url`] を使うこと。
#[deprecated(note = "try_get_for_url を使うこと（読み込み失敗を Err として伝播する）")]
pub async fn get_for_url(
    mcp_name: &str,
    server_url: &str,
) -> Result<Option<AuthEntry>, AuthStoreError> {
    try_get_for_url(mcp_name, server_url).await
}

/// `try_set` の実体（呼び出し側が [`write_lock`] を保持済みであること）。
///
/// ロックを二重取得してデッドロックしないよう、`try_update_*` 系からも
/// このロック無しのヘルパを呼ぶ。
async fn set_unlocked(
    mcp_name: &str,
    entry: AuthEntry,
    server_url: Option<&str>,
) -> Result<(), AuthStoreError> {
    let mut data = read_all().await?;
    let mut entry = entry;
    if let Some(url) = server_url {
        entry.server_url = Some(url.to_string());
    }
    data.insert(mcp_name.to_string(), entry);
    write_all(&data).await?;
    Ok(())
}

/// Persist an auth entry, propagating store read/write failures.
///
/// read-modify-write 全体をプロセス共有ロックで直列化し、並行呼び出しで
/// 片方の変更が失われることを防ぐ。
pub async fn try_set(
    mcp_name: &str,
    entry: AuthEntry,
    server_url: Option<&str>,
) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    set_unlocked(mcp_name, entry, server_url).await
}

/// Compatibility wrapper for [`try_set`] (legacy `std::io::Error` signature).
pub async fn set(
    mcp_name: &str,
    entry: AuthEntry,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    try_set(mcp_name, entry, server_url)
        .await
        .map_err(std::io::Error::from)
}

/// Remove all stored auth data for a server, propagating failures.
///
/// `try_set` と同じ書き込みロックを保持し、並行する更新と競合しないようにする。
pub async fn try_remove(mcp_name: &str) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    let mut data = read_all().await?;
    data.remove(mcp_name);
    write_all(&data).await?;
    Ok(())
}

/// Compatibility wrapper for [`try_remove`].
pub async fn remove(mcp_name: &str) -> Result<(), std::io::Error> {
    try_remove(mcp_name).await.map_err(std::io::Error::from)
}

/// Update only the tokens portion of an entry, propagating failures.
pub async fn try_update_tokens(
    mcp_name: &str,
    tokens: OAuthTokens,
    server_url: Option<&str>,
) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    let mut entry = try_get(mcp_name).await?.unwrap_or_default();
    entry.tokens = Some(tokens);
    set_unlocked(mcp_name, entry, server_url).await
}

/// Compatibility wrapper for [`try_update_tokens`].
pub async fn update_tokens(
    mcp_name: &str,
    tokens: OAuthTokens,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    try_update_tokens(mcp_name, tokens, server_url)
        .await
        .map_err(std::io::Error::from)
}

/// Store the discovered authorization/token endpoints, propagating failures.
pub async fn try_update_endpoints(
    mcp_name: &str,
    auth_url: &str,
    token_url: &str,
    server_url: Option<&str>,
) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    let mut entry = try_get(mcp_name).await?.unwrap_or_default();
    entry.auth_url = Some(auth_url.to_string());
    entry.token_url = Some(token_url.to_string());
    set_unlocked(mcp_name, entry, server_url).await
}

/// Compatibility wrapper for [`try_update_endpoints`].
pub async fn update_endpoints(
    mcp_name: &str,
    auth_url: &str,
    token_url: &str,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    try_update_endpoints(mcp_name, auth_url, token_url, server_url)
        .await
        .map_err(std::io::Error::from)
}

/// Update only the client info portion of an entry, propagating failures.
pub async fn try_update_client_info(
    mcp_name: &str,
    info: OAuthClientInfo,
    server_url: Option<&str>,
) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    let mut entry = try_get(mcp_name).await?.unwrap_or_default();
    entry.client_info = Some(info);
    set_unlocked(mcp_name, entry, server_url).await
}

/// Compatibility wrapper for [`try_update_client_info`].
pub async fn update_client_info(
    mcp_name: &str,
    info: OAuthClientInfo,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    try_update_client_info(mcp_name, info, server_url)
        .await
        .map_err(std::io::Error::from)
}

/// Store the PKCE code verifier, propagating failures.
pub async fn try_update_code_verifier(
    mcp_name: &str,
    code_verifier: &str,
) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    let mut entry = try_get(mcp_name).await?.unwrap_or_default();
    entry.code_verifier = Some(code_verifier.to_string());
    set_unlocked(mcp_name, entry, None).await
}

/// Compatibility wrapper for [`try_update_code_verifier`].
pub async fn update_code_verifier(
    mcp_name: &str,
    code_verifier: &str,
) -> Result<(), std::io::Error> {
    try_update_code_verifier(mcp_name, code_verifier)
        .await
        .map_err(std::io::Error::from)
}

/// Clear the stored code verifier, propagating failures.
pub async fn try_clear_code_verifier(mcp_name: &str) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    if let Some(mut entry) = try_get(mcp_name).await? {
        entry.code_verifier = None;
        set_unlocked(mcp_name, entry, None).await?;
    }
    Ok(())
}

/// Compatibility wrapper for [`try_clear_code_verifier`].
pub async fn clear_code_verifier(mcp_name: &str) -> Result<(), std::io::Error> {
    try_clear_code_verifier(mcp_name)
        .await
        .map_err(std::io::Error::from)
}

/// Store the OAuth state parameter, propagating failures.
pub async fn try_update_oauth_state(mcp_name: &str, state: &str) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    let mut entry = try_get(mcp_name).await?.unwrap_or_default();
    entry.oauth_state = Some(state.to_string());
    set_unlocked(mcp_name, entry, None).await
}

/// Compatibility wrapper for [`try_update_oauth_state`].
pub async fn update_oauth_state(mcp_name: &str, state: &str) -> Result<(), std::io::Error> {
    try_update_oauth_state(mcp_name, state)
        .await
        .map_err(std::io::Error::from)
}

/// Read the stored OAuth state, propagating read failures.
pub async fn try_get_oauth_state(mcp_name: &str) -> Result<Option<String>, AuthStoreError> {
    Ok(try_get(mcp_name).await?.and_then(|entry| entry.oauth_state))
}

/// 非推奨の互換ラッパ。[`try_get_oauth_state`] へ委譲し、読み込み失敗を `Err` として伝播する。
///
/// 以前は失敗を飲んで `None` を返す fail-open だったが、PKCE state の
/// 誤放棄を防ぐため fail-loud に変更した。新規コードは [`try_get_oauth_state`] を使うこと。
#[deprecated(note = "try_get_oauth_state を使うこと（読み込み失敗を Err として伝播する）")]
pub async fn get_oauth_state(mcp_name: &str) -> Result<Option<String>, AuthStoreError> {
    try_get_oauth_state(mcp_name).await
}

/// Clear the stored OAuth state, propagating failures.
pub async fn try_clear_oauth_state(mcp_name: &str) -> Result<(), AuthStoreError> {
    let _guard = write_lock().lock().await;
    if let Some(mut entry) = try_get(mcp_name).await? {
        entry.oauth_state = None;
        set_unlocked(mcp_name, entry, None).await?;
    }
    Ok(())
}

/// Compatibility wrapper for [`try_clear_oauth_state`].
pub async fn clear_oauth_state(mcp_name: &str) -> Result<(), std::io::Error> {
    try_clear_oauth_state(mcp_name)
        .await
        .map_err(std::io::Error::from)
}

/// Check whether stored tokens are expired.
/// Returns `None` if no tokens exist, `Some(false)` if not expired (or no
/// expiry set), `Some(true)` if expired.
pub fn is_token_expired(entry: &AuthEntry) -> Option<bool> {
    let tokens = entry.tokens.as_ref()?;
    match tokens.expires_at {
        Some(exp) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            Some(exp < now)
        }
        None => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn entry(tokens: Option<OAuthTokens>) -> AuthEntry {
        AuthEntry {
            tokens,
            client_info: None,
            code_verifier: None,
            oauth_state: None,
            server_url: None,
            auth_url: None,
            token_url: None,
        }
    }

    #[test]
    fn no_tokens_returns_none() {
        assert_eq!(is_token_expired(&entry(None)), None);
    }

    #[test]
    fn tokens_without_expiry_are_not_expired() {
        let tokens = OAuthTokens {
            access_token: "token".to_owned(),
            refresh_token: None,
            expires_at: None,
            scope: None,
        };
        assert_eq!(is_token_expired(&entry(Some(tokens))), Some(false));
    }

    #[test]
    fn tokens_with_past_expiry_are_expired() {
        let tokens = OAuthTokens {
            access_token: "token".to_owned(),
            refresh_token: None,
            expires_at: Some(1.0),
            scope: None,
        };
        assert_eq!(is_token_expired(&entry(Some(tokens))), Some(true));
    }

    #[test]
    fn tokens_with_future_expiry_are_valid() {
        let tokens = OAuthTokens {
            access_token: "token".to_owned(),
            refresh_token: None,
            expires_at: Some(9_999_999_999.0),
            scope: None,
        };
        assert_eq!(is_token_expired(&entry(Some(tokens))), Some(false));
    }

    // --- Secure auth-store regression tests --------------------------------

    /// Test-only credential file. Redirects the store via the env var.
    struct TempAuthFile {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempAuthFile {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "opencode-mcp-auth-{pid}-{label}",
                pid = std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let path = dir.join("mcp-auth.json");
            Self { dir, path }
        }

        fn activate(&self) {
            std::env::set_var("OPENCODE_MCP_AUTH_FILE", &self.path);
        }
    }

    impl Drop for TempAuthFile {
        fn drop(&mut self) {
            std::env::remove_var("OPENCODE_MCP_AUTH_FILE");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[cfg(unix)]
    fn permission_mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path)
            .expect("stat auth file")
            .permissions()
            .mode()
            & 0o777
    }

    /// 回帰ガード: 権限是正失敗時に fail-closed で拒否し、
    /// Display / source() / From 変換後のいずれにも原因やパスを漏らさない。
    #[cfg(unix)]
    #[test]
    fn permission_tightening_failure_fails_closed_without_leaking() {
        const SENTINEL: &str = "SENTINEL_DO_NOT_LEAK";
        let path = std::path::Path::new("/tmp/mcp-auth.json");
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, SENTINEL);

        let failure = permissions_failure(path, error);
        assert!(matches!(&failure, AuthStoreError::PermissionsNotSecured(_)));
        let text = failure.to_string();
        assert!(
            !text.contains(SENTINEL),
            "error must not embed the OS cause"
        );
        assert!(!text.contains("/tmp"), "error must not embed the path");

        // source() にも生の OS エラー（SENTINEL）やパスを出さない。
        let source = failure
            .source()
            .expect("PermissionsNotSecured must keep a safe source");
        let source_text = source.to_string();
        assert!(
            !source_text.contains(SENTINEL),
            "source must not embed the OS cause"
        );
        assert!(
            !source_text.contains("/tmp"),
            "source must not embed the path"
        );

        // From<AuthStoreError> for std::io::Error も固定文言へマッピングする。
        let io_error: std::io::Error = failure.into();
        let io_text = io_error.to_string();
        assert!(
            !io_text.contains(SENTINEL),
            "converted io::Error must not embed the OS cause"
        );
        assert!(
            !io_text.contains("/tmp"),
            "converted io::Error must not embed the path"
        );
        assert_eq!(io_error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    async fn scenario_new_file_is_owner_only() {
        let tmp = TempAuthFile::new("mode-0600");
        tmp.activate();

        let mut data = HashMap::new();
        data.insert(
            "server".to_owned(),
            entry(Some(OAuthTokens {
                access_token: "dummy-access".to_owned(),
                refresh_token: Some("dummy-refresh".to_owned()),
                expires_at: None,
                scope: None,
            })),
        );
        write_all(&data).await.expect("write auth store");

        assert_eq!(
            permission_mode(&tmp.path),
            0o600,
            "auth file must be readable/writable by its owner only"
        );
    }

    #[cfg(unix)]
    async fn scenario_existing_0644_tightened_on_read() {
        let tmp = TempAuthFile::new("tighten-read");
        tmp.activate();

        std::fs::write(&tmp.path, "{}").expect("seed auth file");
        std::fs::set_permissions(&tmp.path, std::fs::Permissions::from_mode(0o644))
            .expect("set 0644");

        let _ = read_all().await.expect("valid JSON store must load");

        assert_eq!(
            permission_mode(&tmp.path),
            0o600,
            "pre-existing 0644 auth file must be tightened on read"
        );
    }

    #[cfg(unix)]
    async fn scenario_existing_0644_tightened_on_write() {
        let tmp = TempAuthFile::new("tighten-write");
        tmp.activate();

        std::fs::write(&tmp.path, "{}").expect("seed auth file");
        std::fs::set_permissions(&tmp.path, std::fs::Permissions::from_mode(0o644))
            .expect("set 0644");

        write_all(&HashMap::new())
            .await
            .expect("rewrite auth store");

        assert_eq!(
            permission_mode(&tmp.path),
            0o600,
            "rewriting a 0644 auth file must yield 0600"
        );
    }

    #[cfg(unix)]
    async fn scenario_concurrent_writes_never_expose_partial_json() {
        let tmp = TempAuthFile::new("concurrent");
        tmp.activate();

        write_all(&HashMap::new()).await.expect("init auth store");

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let reader_path = tmp.path.clone();
        let reader_stop = stop.clone();
        let reader = tokio::spawn(async move {
            let mut bad_reads = 0usize;
            while !reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(contents) = fs::read_to_string(&reader_path).await {
                    if serde_json::from_str::<HashMap<String, AuthEntry>>(&contents).is_err() {
                        bad_reads += 1;
                    }
                }
                tokio::task::yield_now().await;
            }
            bad_reads
        });

        let writers: Vec<_> = (0..4)
            .map(|worker| {
                let worker_stop = stop.clone();
                tokio::spawn(async move {
                    while !worker_stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let mut data = HashMap::new();
                        data.insert(
                            format!("server-{worker}"),
                            entry(Some(OAuthTokens {
                                access_token: "a".repeat(4096),
                                refresh_token: Some("b".repeat(4096)),
                                expires_at: None,
                                scope: Some("scope".to_owned()),
                            })),
                        );
                        write_all(&data).await.expect("write auth store");
                    }
                })
            })
            .collect();

        // Let the writers race for a bounded number of iterations, then stop.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);

        for writer in writers {
            writer.await.expect("writer task");
        }
        let bad_reads = reader.await.expect("reader task");
        assert_eq!(
            bad_reads, 0,
            "atomic rename must keep readers from ever seeing a partial store"
        );
    }

    async fn scenario_missing_file_reads_empty() {
        let tmp = TempAuthFile::new("missing");
        tmp.activate();
        // 未作成ファイルは初回状態なので空を返す。破損・権限エラーは fail-loud
        // （下の scenario_corrupt_store_fails_loud で検証）。
        assert!(
            read_all()
                .await
                .expect("missing store is an empty store")
                .is_empty(),
            "a missing store must be treated as empty so first-time writes work"
        );
    }

    /// 回帰ガード: JSON 破損を空 store へ丸めず fail-loud にすること。
    #[allow(deprecated)]
    async fn scenario_corrupt_store_fails_loud() {
        let tmp = TempAuthFile::new("corrupt");
        tmp.activate();
        std::fs::write(&tmp.path, "{not valid json").expect("seed corrupt store");

        assert!(matches!(read_all().await, Err(AuthStoreError::Json(_))));
        assert!(try_get("server").await.is_err());
        assert!(try_get_for_url("server", "https://example.com")
            .await
            .is_err());
        assert!(try_get_oauth_state("server").await.is_err());
        assert!(try_set("server", entry(None), None).await.is_err());
        // 残した互換ラッパは fail-open（ログへ残して空を返す）。
        assert!(get("server").await.is_none());
        // 読み取り系の互換ラッパは try_* へ委譲し、Err を伝播する（fail-open ではない）。
        assert!(get_for_url("server", "https://example.com").await.is_err());
        assert!(get_oauth_state("server").await.is_err());
    }

    /// 回帰ガード: I/O エラーも fail-loud に伝播すること。
    #[cfg(unix)]
    async fn scenario_io_failure_fails_loud() {
        let tmp = TempAuthFile::new("io-failure");
        let directory = tmp.dir.join("not-a-file");
        std::fs::create_dir_all(&directory).expect("create directory");
        std::env::set_var("OPENCODE_MCP_AUTH_FILE", &directory);

        assert!(matches!(read_all().await, Err(AuthStoreError::Io(_))));
        assert!(try_get("server").await.is_err());
    }

    /// 回帰ガード: try_* API が正常系で既存の永続化動作を維持すること。
    async fn scenario_try_api_preserves_valid_store() {
        let tmp = TempAuthFile::new("try-api");
        tmp.activate();
        try_update_tokens(
            "server",
            OAuthTokens {
                access_token: "dummy-access".to_owned(),
                refresh_token: None,
                expires_at: None,
                scope: None,
            },
            Some("https://example.com"),
        )
        .await
        .expect("store tokens");

        let loaded = try_get_for_url("server", "https://example.com")
            .await
            .expect("read must succeed")
            .expect("entry must exist");
        assert_eq!(loaded.tokens.expect("tokens").access_token, "dummy-access");

        try_update_oauth_state("server", "state-1")
            .await
            .expect("store state");
        assert_eq!(
            try_get_oauth_state("server").await.expect("read state"),
            Some("state-1".to_string())
        );
        try_clear_oauth_state("server").await.expect("clear state");
        assert_eq!(
            try_get_oauth_state("server").await.expect("read state"),
            None
        );
    }

    /// 回帰ガード: 2 並行 `try_set` が互いの変更を失わず、両キーが disk に残ること。
    ///
    /// 旧実装は `read_all() → insert() → write_all()` をロック無しで行うため、
    /// 2 つのフローが同じ元データを読むと後書き込みが先書き込みを上書きし、
    /// 片方のキー（OAuth の verifier/state）が消えていた。
    async fn scenario_concurrent_try_set_keeps_both_keys() {
        let tmp = TempAuthFile::new("concurrent-set");
        tmp.activate();

        // 両タスクを同時に走らせ、read-modify-write の競合を確実に発生させる。
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

        let entry_a = entry(Some(OAuthTokens {
            access_token: "verifier-a".to_owned(),
            refresh_token: None,
            expires_at: None,
            scope: None,
        }));
        let entry_b = entry(Some(OAuthTokens {
            access_token: "verifier-b".to_owned(),
            refresh_token: None,
            expires_at: None,
            scope: None,
        }));

        let first = {
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                try_set("flow-a", entry_a, None).await.expect("set flow-a");
            })
        };
        let second = {
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                try_set("flow-b", entry_b, None).await.expect("set flow-b");
            })
        };

        first.await.expect("flow-a task");
        second.await.expect("flow-b task");

        let data = read_all().await.expect("read store after concurrent sets");
        assert!(data.contains_key("flow-a"), "first flow must survive");
        assert!(data.contains_key("flow-b"), "second flow must survive");
    }

    /// Env-var based scenarios run serially because the variable is global.
    #[tokio::test(flavor = "multi_thread")]
    async fn secure_auth_store_behaviour() {
        #[cfg(unix)]
        scenario_new_file_is_owner_only().await;
        #[cfg(unix)]
        scenario_existing_0644_tightened_on_read().await;
        #[cfg(unix)]
        scenario_existing_0644_tightened_on_write().await;
        #[cfg(unix)]
        scenario_concurrent_writes_never_expose_partial_json().await;
        scenario_missing_file_reads_empty().await;
        scenario_corrupt_store_fails_loud().await;
        #[cfg(unix)]
        scenario_io_failure_fails_loud().await;
        scenario_try_api_preserves_valid_store().await;
        scenario_concurrent_try_set_keeps_both_keys().await;
    }
}
