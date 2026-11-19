//! Persistent storage for MCP OAuth credentials.
//!
//! Mirrors the TypeScript `McpAuth` namespace – stores tokens, client info,
//! code verifiers and OAuth state in a JSON file inside the user data directory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

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

/// Read the entire auth store from disk.
async fn read_all() -> HashMap<String, AuthEntry> {
    let path = auth_file_path();
    match fs::read_to_string(&path).await {
        Ok(contents) => {
            // On Unix, tighten a pre-existing over-permissive store on access.
            // This is best-effort and never changes the read result.
            #[cfg(unix)]
            tighten_permissions(&path).await;
            serde_json::from_str(&contents).unwrap_or_default()
        }
        Err(_) => HashMap::new(),
    }
}

/// Bring an existing credential file back to owner-only permissions (Unix).
#[cfg(unix)]
async fn tighten_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Ok(meta) = fs::metadata(path).await {
        if meta.permissions().mode() & 0o777 != 0o600 {
            let _ = fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
        }
    }
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

/// Get the auth entry for a given MCP server name.
pub async fn get(mcp_name: &str) -> Option<AuthEntry> {
    let data = read_all().await;
    data.get(mcp_name).cloned()
}

/// Get the auth entry only if it was stored for the same `server_url`.
pub async fn get_for_url(mcp_name: &str, server_url: &str) -> Option<AuthEntry> {
    let entry = get(mcp_name).await?;
    match &entry.server_url {
        Some(url) if url == server_url => Some(entry),
        _ => None,
    }
}
/// Persist an auth entry (optionally updating the server URL).
pub async fn set(
    mcp_name: &str,
    entry: AuthEntry,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    let mut data = read_all().await;
    let mut entry = entry;
    if let Some(url) = server_url {
        entry.server_url = Some(url.to_string());
    }
    data.insert(mcp_name.to_string(), entry);
    write_all(&data).await
}

/// Remove all stored auth data for a server.
pub async fn remove(mcp_name: &str) -> Result<(), std::io::Error> {
    let mut data = read_all().await;
    data.remove(mcp_name);
    write_all(&data).await
}

/// Update only the tokens portion of an entry.
pub async fn update_tokens(
    mcp_name: &str,
    tokens: OAuthTokens,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    let mut entry = get(mcp_name).await.unwrap_or_default();
    entry.tokens = Some(tokens);
    set(mcp_name, entry, server_url).await
}

/// Store the discovered authorization/token endpoints for a server.
pub async fn update_endpoints(
    mcp_name: &str,
    auth_url: &str,
    token_url: &str,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    let mut entry = get(mcp_name).await.unwrap_or_default();
    entry.auth_url = Some(auth_url.to_string());
    entry.token_url = Some(token_url.to_string());
    set(mcp_name, entry, server_url).await
}

/// Update only the client info portion of an entry.
pub async fn update_client_info(
    mcp_name: &str,
    info: OAuthClientInfo,
    server_url: Option<&str>,
) -> Result<(), std::io::Error> {
    let mut entry = get(mcp_name).await.unwrap_or_default();
    entry.client_info = Some(info);
    set(mcp_name, entry, server_url).await
}

/// Store the PKCE code verifier.
pub async fn update_code_verifier(
    mcp_name: &str,
    code_verifier: &str,
) -> Result<(), std::io::Error> {
    let mut entry = get(mcp_name).await.unwrap_or_default();
    entry.code_verifier = Some(code_verifier.to_string());
    set(mcp_name, entry, None).await
}

/// Clear the stored code verifier.
pub async fn clear_code_verifier(mcp_name: &str) -> Result<(), std::io::Error> {
    if let Some(mut entry) = get(mcp_name).await {
        entry.code_verifier = None;
        set(mcp_name, entry, None).await?;
    }
    Ok(())
}
/// Store the OAuth state parameter.
pub async fn update_oauth_state(mcp_name: &str, state: &str) -> Result<(), std::io::Error> {
    let mut entry = get(mcp_name).await.unwrap_or_default();
    entry.oauth_state = Some(state.to_string());
    set(mcp_name, entry, None).await
}

/// Read the stored OAuth state.
pub async fn get_oauth_state(mcp_name: &str) -> Option<String> {
    get(mcp_name).await.and_then(|e| e.oauth_state)
}

/// Clear the stored OAuth state.
pub async fn clear_oauth_state(mcp_name: &str) -> Result<(), std::io::Error> {
    if let Some(mut entry) = get(mcp_name).await {
        entry.oauth_state = None;
        set(mcp_name, entry, None).await?;
    }
    Ok(())
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

        let _ = read_all().await;

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
        assert!(read_all().await.is_empty());
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
    }
}
