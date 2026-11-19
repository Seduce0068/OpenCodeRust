//! OAuth discovery for remote MCP servers.
//!
//! Implements the discovery steps the MCP authorization spec requires before a
//! client can start an authorization-code flow:
//!
//! 1. Protected resource metadata (RFC 9728) published by the MCP server.
//! 2. Authorization server metadata (RFC 8414) for each advertised issuer.
//! 3. Dynamic client registration (RFC 7591) when no client id is configured.
//!
//! References:
//! - <https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization>
//! - <https://datatracker.ietf.org/doc/html/rfc9728>
//! - <https://datatracker.ietf.org/doc/html/rfc8414>
//! - <https://datatracker.ietf.org/doc/html/rfc7591>
//! - <https://datatracker.ietf.org/doc/html/rfc8707> (resource indicator)

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Errors raised while discovering OAuth endpoints.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("invalid URL: {0}")]
    Url(String),
    #[error("server returned HTTP {status} for {url}")]
    Status { status: u16, url: String },
    #[error("failed to parse {context}: {message}")]
    Parse { context: String, message: String },
    #[error("no authorization server advertised by the resource metadata")]
    NoAuthorizationServer,
}

/// Protected resource metadata (RFC 9728).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProtectedResourceMetadata {
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// Authorization server metadata (RFC 8414 subset used by MCP).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizationServerMetadata {
    pub issuer: Option<String>,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

/// Result of dynamic client registration (RFC 7591).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredClient {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// The effective OAuth endpoints for one MCP server.
#[derive(Debug, Clone, Default)]
pub struct OAuthEndpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes: Vec<String>,
    /// RFC 8707 resource indicator advertised by the protected resource metadata.
    /// `None` keeps the legacy behavior of omitting the `resource` parameter.
    pub resource: Option<String>,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Builds the well-known URL for `base` with `suffix` appended to the path.
///
/// For `https://host/path` and suffix `.well-known/oauth-protected-resource`
/// this returns `https://host/.well-known/oauth-protected-resource/path`, which
/// is the RFC 9728 form used for resources with a path component.
pub fn well_known_url(base: &str, suffix: &str) -> Result<String, DiscoveryError> {
    let url = url::Url::parse(base).map_err(|e| DiscoveryError::Url(e.to_string()))?;
    let mut result = url.clone();
    let mut path = format!("/{suffix}");
    let trimmed = url.path().trim_end_matches('/');
    if !trimmed.is_empty() && trimmed != "/" {
        path.push_str(trimmed);
    }
    result.set_path(&path);
    result.set_query(None);
    result.set_fragment(None);
    Ok(result.to_string())
}

/// Builds the well-known URL for `base`, ignoring its path component.
///
/// Used as the fallback for servers that publish the metadata document at the
/// host root instead of the RFC 9728 path-qualified location.
pub fn well_known_url_root(base: &str, suffix: &str) -> Result<String, DiscoveryError> {
    let url = url::Url::parse(base).map_err(|e| DiscoveryError::Url(e.to_string()))?;
    let mut result = url.clone();
    result.set_path(&format!("/{suffix}"));
    result.set_query(None);
    result.set_fragment(None);
    Ok(result.to_string())
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    url: &str,
    context: &str,
) -> Result<T, DiscoveryError> {
    let response = http_client()
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| DiscoveryError::Http(e.to_string()))?;

    if !response.status().is_success() {
        return Err(DiscoveryError::Status {
            status: response.status().as_u16(),
            url: url.to_string(),
        });
    }

    response
        .json::<T>()
        .await
        .map_err(|e| DiscoveryError::Parse {
            context: context.to_string(),
            message: e.to_string(),
        })
}

/// Fetches protected resource metadata, trying the RFC 9728 path form first and
/// the root form as a fallback.
pub async fn fetch_resource_metadata(
    server_url: &str,
) -> Result<ProtectedResourceMetadata, DiscoveryError> {
    // RFC 9728 places the resource path after the well-known segment, but some
    // servers publish only the root form; try both.
    let candidates = [
        well_known_url(server_url, ".well-known/oauth-protected-resource")?,
        well_known_url_root(server_url, ".well-known/oauth-protected-resource")?,
    ];

    let mut last_error = None;
    for candidate in candidates {
        match get_json::<ProtectedResourceMetadata>(&candidate, "protected resource metadata").await
        {
            Ok(metadata) => return Ok(metadata),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or(DiscoveryError::NoAuthorizationServer))
}

/// Fetches authorization server metadata, trying the OIDC and OAuth well-known
/// suffixes.
pub async fn fetch_authorization_server_metadata(
    issuer: &str,
) -> Result<AuthorizationServerMetadata, DiscoveryError> {
    let candidates = [
        well_known_url(issuer, ".well-known/oauth-authorization-server")?,
        well_known_url(issuer, ".well-known/openid-configuration")?,
    ];

    let mut last_error = None;
    for candidate in candidates {
        match get_json::<AuthorizationServerMetadata>(&candidate, "authorization server metadata")
            .await
        {
            Ok(metadata) => return Ok(metadata),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or(DiscoveryError::NoAuthorizationServer))
}

/// Registers a client dynamically (RFC 7591).
pub async fn register_client(
    registration_endpoint: &str,
    redirect_uri: &str,
    scopes: &[String],
) -> Result<RegisteredClient, DiscoveryError> {
    let mut body = serde_json::json!({
        "client_name": "OpenCodeRust",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if !scopes.is_empty() {
        body["scope"] = serde_json::Value::String(scopes.join(" "));
    }

    let response = http_client()
        .post(registration_endpoint)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| DiscoveryError::Http(e.to_string()))?;

    if !response.status().is_success() {
        return Err(DiscoveryError::Status {
            status: response.status().as_u16(),
            url: registration_endpoint.to_string(),
        });
    }

    response
        .json::<RegisteredClient>()
        .await
        .map_err(|e| DiscoveryError::Parse {
            context: "client registration response".to_string(),
            message: e.to_string(),
        })
}

/// Discovers the endpoints for an MCP server, falling back to the legacy
/// `{server}/oauth/authorize` convention when metadata is unavailable.
///
/// Explicitly configured endpoints win over discovered ones; discovery still
/// runs to fill in whatever was not configured (including the registration
/// endpoint used for dynamic client registration).
pub async fn discover_endpoints(
    server_url: &str,
    explicit_authorization_url: Option<&str>,
    explicit_token_url: Option<&str>,
) -> Result<OAuthEndpoints, DiscoveryError> {
    if let (Some(authorization_endpoint), Some(token_endpoint)) =
        (explicit_authorization_url, explicit_token_url)
    {
        return Ok(OAuthEndpoints {
            authorization_endpoint: authorization_endpoint.to_string(),
            token_endpoint: token_endpoint.to_string(),
            registration_endpoint: None,
            scopes: Vec::new(),
            // No metadata was fetched, so there is no resource indicator.
            resource: None,
        });
    }

    let resource = fetch_resource_metadata(server_url).await?;
    let issuer = resource
        .authorization_servers
        .first()
        .cloned()
        .ok_or(DiscoveryError::NoAuthorizationServer)?;

    let metadata = fetch_authorization_server_metadata(&issuer).await?;
    let authorization_endpoint = metadata
        .authorization_endpoint
        .ok_or(DiscoveryError::NoAuthorizationServer)?;
    let token_endpoint = metadata
        .token_endpoint
        .ok_or(DiscoveryError::NoAuthorizationServer)?;

    Ok(OAuthEndpoints {
        authorization_endpoint: explicit_authorization_url
            .map(str::to_string)
            .unwrap_or(authorization_endpoint),
        token_endpoint: explicit_token_url
            .map(str::to_string)
            .unwrap_or(token_endpoint),
        registration_endpoint: metadata.registration_endpoint,
        scopes: metadata.scopes_supported,
        // Carry the RFC 8707 resource from the protected resource metadata so
        // the OAuth flow can bind issued tokens to this MCP resource.
        resource: resource.resource,
    })
}

#[cfg(test)]
mod tests {
    //! ループバック HTTP モックによる discovery の回帰テスト。
    //!
    //! 新しい依存を追加せず、`std::net::TcpListener` と `std::thread` だけで
    //! 複数接続を順番に捌く。reqwest がプロキシ経由でループバックへ接続しない
    //! よう、各 HTTP テスト冒頭の `ensure_no_proxy()` が
    //! `NO_PROXY=127.0.0.1,localhost` をプロセス内に設定する（外部送信なし）。
    //! ループバック bind / accept の失敗は `SKIPPED` で成功扱いにせず、
    //! 必ずテスト失敗として扱う。
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex, Once};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    #[test]
    fn builds_path_aware_well_known_urls() {
        assert_eq!(
            well_known_url(
                "https://mcp.example.com/mcp",
                ".well-known/oauth-protected-resource"
            )
            .unwrap(),
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(
            well_known_url(
                "https://auth.example.com",
                ".well-known/oauth-authorization-server"
            )
            .unwrap(),
            "https://auth.example.com/.well-known/oauth-authorization-server"
        );
        assert_eq!(
            well_known_url(
                "https://auth.example.com/",
                ".well-known/oauth-authorization-server"
            )
            .unwrap(),
            "https://auth.example.com/.well-known/oauth-authorization-server"
        );
    }

    #[test]
    fn builds_root_well_known_url_without_resource_path() {
        // Regression guard: the fallback candidate must not concatenate the
        // resource path onto the well-known segment.
        assert_eq!(
            well_known_url_root(
                "https://mcp.example.com/mcp",
                ".well-known/oauth-protected-resource"
            )
            .unwrap(),
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            well_known_url_root(
                "http://127.0.0.1:9999/some/deep/path",
                ".well-known/oauth-protected-resource"
            )
            .unwrap(),
            "http://127.0.0.1:9999/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn rejects_invalid_urls() {
        assert!(matches!(
            well_known_url("not a url", ".well-known/x"),
            Err(DiscoveryError::Url(_))
        ));
        assert!(matches!(
            well_known_url_root("not a url", ".well-known/x"),
            Err(DiscoveryError::Url(_))
        ));
    }

    #[test]
    fn well_known_url_root_ignores_path() {
        let url = well_known_url_root("https://host/a/b?x=1#frag", ".well-known/x").unwrap();
        assert_eq!(url, "https://host/.well-known/x");
    }

    #[test]
    fn well_known_url_strips_query_and_fragment() {
        let url = well_known_url("https://host/path?x=1#frag", ".well-known/x").unwrap();
        assert!(!url.contains('?'));
        assert!(!url.contains('#'));
        assert!(url.contains("/path"));
    }

    #[test]
    fn resource_metadata_deserializes_resource_indicator() {
        // Pure serde test; no network I/O against a real OAuth server.
        let metadata: ProtectedResourceMetadata = serde_json::from_str(
            r#"{
                "resource": "https://mcp.example.invalid/mcp",
                "authorization_servers": ["https://auth.example.invalid"],
                "scopes_supported": ["mcp:read"]
            }"#,
        )
        .expect("protected resource metadata must parse");

        assert_eq!(
            metadata.resource.as_deref(),
            Some("https://mcp.example.invalid/mcp")
        );
    }

    #[test]
    fn oauth_endpoints_default_has_no_resource() {
        // `None` must keep the legacy behavior of omitting the resource param.
        let endpoints = OAuthEndpoints::default();
        assert!(endpoints.resource.is_none());
    }

    /// reqwest が 127.0.0.1 への接続をプロキシに回さないようにする。
    ///
    /// HTTP テストは並列実行されるため、プロセス内で一度だけ設定する。
    fn ensure_no_proxy() {
        static SET_NO_PROXY: Once = Once::new();
        SET_NO_PROXY.call_once(|| {
            std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
            std::env::set_var("no_proxy", "127.0.0.1,localhost");
        });
    }

    /// (status line, body) の組を作るヘルパー。
    fn canned(status: &str, body: impl Into<String>) -> (String, String) {
        (status.to_string(), body.into())
    }

    /// HTTP リクエスト（リクエスト行・ヘッダ・ボディ）を生文字列で読み取る。
    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("mock must read request");
            assert!(read > 0, "unexpected EOF while reading request headers");
            buf.extend_from_slice(&chunk[..read]);
            if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
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
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// ループバック HTTP モックのハンドル。
    struct MockServer {
        base_url: String,
        seen: Arc<Mutex<Vec<String>>>,
        thread: JoinHandle<()>,
    }

    impl MockServer {
        /// `responses` の数だけ順番に接続を受け付け、対応するレスポンスを返す。
        fn spawn(responses: Vec<(String, String)>) -> Self {
            Self::spawn_with(|_| responses)
        }

        /// `base_url` を渡された後にレスポンスを組み立てられる版の `spawn`。
        fn spawn_with<F>(build: F) -> Self
        where
            F: FnOnce(&str) -> Vec<(String, String)>,
        {
            let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind must succeed");
            let addr = listener.local_addr().expect("loopback addr must succeed");
            let base_url = format!("http://{addr}");
            let responses = build(&base_url);
            let seen = Arc::new(Mutex::new(Vec::new()));
            let seen_for_thread = Arc::clone(&seen);
            let thread = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("mock listener must be nonblocking");
                let deadline = Instant::now() + Duration::from_secs(10);
                for (status_line, body) in responses {
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
                        .expect("mock stream read timeout");
                    seen_for_thread
                        .lock()
                        .expect("mock seen lock")
                        .push(read_request(&mut stream));
                    let headers = format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream
                        .write_all(headers.as_bytes())
                        .expect("mock must write headers");
                    stream
                        .write_all(body.as_bytes())
                        .expect("mock must write body");
                }
            });
            Self {
                base_url,
                seen,
                thread,
            }
        }

        /// モックスレッドの完了を待ち、記録されたリクエストを返す。
        /// bind / accept 失敗は `join()` の panic として伝播し、テストを失敗させる。
        fn requests(self) -> Vec<String> {
            self.thread.join().expect("mock thread must not panic");
            self.seen.lock().expect("mock seen lock").clone()
        }
    }

    #[tokio::test]
    async fn resource_metadata_falls_back_from_path_form_to_root_form() {
        ensure_no_proxy();
        let mock = MockServer::spawn(vec![
            canned("404 Not Found", "not found"),
            canned(
                "200 OK",
                r#"{"resource":"resource-indicator","authorization_servers":["issuer"],"scopes_supported":["mcp:read"]}"#,
            ),
        ]);
        let base = mock.base_url.clone();

        let metadata = fetch_resource_metadata(&format!("{base}/mcp"))
            .await
            .expect("root form must be used as fallback");

        assert_eq!(metadata.resource.as_deref(), Some("resource-indicator"));
        assert_eq!(metadata.authorization_servers, ["issuer"]);
        assert_eq!(metadata.scopes_supported, ["mcp:read"]);

        let requests = mock.requests();
        assert_eq!(requests.len(), 2, "both candidates must be attempted");
        let first = &requests[0];
        let second = &requests[1];
        assert!(
            first.contains("/.well-known/oauth-protected-resource/mcp "),
            "first candidate must be the RFC 9728 path form: {first}"
        );
        assert!(
            second.contains(" /.well-known/oauth-protected-resource "),
            "second candidate must be the root form: {second}"
        );
        assert!(
            requests.iter().all(|request| request
                .to_ascii_lowercase()
                .contains("accept: application/json")),
            "discovery GETs must advertise JSON: {requests:?}"
        );
    }

    #[tokio::test]
    async fn authorization_server_metadata_falls_back_to_openid_configuration() {
        ensure_no_proxy();
        let mock = MockServer::spawn(vec![
            canned("404 Not Found", "not found"),
            canned(
                "200 OK",
                r#"{"issuer":"issuer","authorization_endpoint":"/authorize","token_endpoint":"/token","code_challenge_methods_supported":["S256"]}"#,
            ),
        ]);
        let base = mock.base_url.clone();

        let metadata = fetch_authorization_server_metadata(&format!("{base}/tenant"))
            .await
            .expect("openid configuration must be used as fallback");

        assert_eq!(metadata.issuer.as_deref(), Some("issuer"));
        assert_eq!(
            metadata.authorization_endpoint.as_deref(),
            Some("/authorize")
        );
        assert_eq!(metadata.token_endpoint.as_deref(), Some("/token"));
        assert_eq!(metadata.code_challenge_methods_supported, ["S256"]);

        let requests = mock.requests();
        assert_eq!(
            requests.len(),
            2,
            "both well-known candidates must be attempted"
        );
        let first = &requests[0];
        let second = &requests[1];
        assert!(
            first.contains("/.well-known/oauth-authorization-server/tenant "),
            "first candidate must be the OAuth well-known form: {first}"
        );
        assert!(
            second.contains(" /.well-known/openid-configuration/tenant "),
            "second candidate must be the OIDC form: {second}"
        );
    }

    #[tokio::test]
    async fn resource_metadata_reports_non_success_status_from_last_candidate() {
        ensure_no_proxy();
        let mock = MockServer::spawn(vec![
            canned("500 Internal Server Error", "boom"),
            canned("403 Forbidden", "denied"),
        ]);
        let base = mock.base_url.clone();

        let error = fetch_resource_metadata(&format!("{base}/mcp"))
            .await
            .expect_err("no candidate may succeed");

        match error {
            DiscoveryError::Status { status, url } => {
                assert_eq!(status, 403, "the last candidate error is propagated");
                assert!(
                    url.ends_with("/.well-known/oauth-protected-resource"),
                    "status error must name the root candidate: {url}"
                );
            }
            other => panic!("expected Status error, got {other:?}"),
        }

        assert_eq!(mock.requests().len(), 2);
    }

    #[tokio::test]
    async fn resource_metadata_reports_json_parse_error() {
        ensure_no_proxy();
        let mock = MockServer::spawn(vec![
            canned("200 OK", "not-json"),
            canned("200 OK", "still-not-json"),
        ]);
        let base = mock.base_url.clone();

        let error = fetch_resource_metadata(&format!("{base}/mcp"))
            .await
            .expect_err("invalid JSON must not be accepted");

        match error {
            DiscoveryError::Parse { context, message } => {
                assert_eq!(context, "protected resource metadata");
                assert!(!message.is_empty(), "parse error must carry a message");
            }
            other => panic!("expected Parse error, got {other:?}"),
        }

        assert_eq!(mock.requests().len(), 2);
    }

    #[tokio::test]
    async fn register_client_posts_expected_registration_body() {
        ensure_no_proxy();
        let mock = MockServer::spawn(vec![canned(
            "201 Created",
            r#"{"client_id":"cid-123","client_secret":"csecret-456"}"#,
        )]);
        let base = mock.base_url.clone();

        let client = register_client(
            &format!("{base}/register"),
            "http://127.0.0.1:19876/mcp/oauth/callback",
            &["mcp:read".to_string(), "mcp:write".to_string()],
        )
        .await
        .expect("registration must succeed");

        assert_eq!(client.client_id, "cid-123");
        assert_eq!(client.client_secret.as_deref(), Some("csecret-456"));

        let requests = mock.requests();
        assert_eq!(requests.len(), 1, "registration must be a single POST");
        let request = &requests[0];
        assert!(
            request.starts_with("POST /register HTTP/1.1"),
            "unexpected request line: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "registration body must be JSON: {request}"
        );
        let (_, request_body) = request
            .split_once("\r\n\r\n")
            .expect("request must contain a header/body separator");
        let body_json: serde_json::Value =
            serde_json::from_str(request_body).expect("registration body must be valid JSON");
        assert_eq!(body_json["client_name"], "OpenCodeRust");
        assert_eq!(
            body_json["redirect_uris"][0],
            "http://127.0.0.1:19876/mcp/oauth/callback"
        );
        assert_eq!(body_json["grant_types"][0], "authorization_code");
        assert_eq!(body_json["grant_types"][1], "refresh_token");
        assert_eq!(body_json["response_types"][0], "code");
        assert_eq!(body_json["token_endpoint_auth_method"], "none");
        assert_eq!(body_json["scope"], "mcp:read mcp:write");
    }

    #[tokio::test]
    async fn register_client_without_scopes_omits_scope_field() {
        ensure_no_proxy();
        let mock = MockServer::spawn(vec![canned("200 OK", r#"{"client_id":"cid-456"}"#)]);
        let base = mock.base_url.clone();

        let client = register_client(
            &format!("{base}/register"),
            "http://127.0.0.1:19876/mcp/oauth/callback",
            &[],
        )
        .await
        .expect("registration must succeed");

        assert_eq!(client.client_id, "cid-456");
        assert!(client.client_secret.is_none());

        let requests = mock.requests();
        let request = &requests[0];
        let (_, request_body) = request
            .split_once("\r\n\r\n")
            .expect("request must contain a header/body separator");
        let body_json: serde_json::Value =
            serde_json::from_str(request_body).expect("registration body must be valid JSON");
        assert!(
            body_json.get("scope").is_none(),
            "scope must be omitted when there are no scopes"
        );
    }

    #[tokio::test]
    async fn discover_endpoints_combines_metadata_from_loopback_server() {
        ensure_no_proxy();
        let mock = MockServer::spawn_with(|base| {
            vec![
                canned("404 Not Found", "not found"),
                canned(
                    "200 OK",
                    serde_json::json!({
                        "resource": format!("{base}/mcp"),
                        "authorization_servers": [base],
                        "scopes_supported": ["mcp:read"],
                    })
                    .to_string(),
                ),
                canned(
                    "200 OK",
                    serde_json::json!({
                        "issuer": base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register"),
                        "scopes_supported": ["mcp:read", "mcp:write"],
                    })
                    .to_string(),
                ),
            ]
        });
        let base = mock.base_url.clone();

        let endpoints = discover_endpoints(&format!("{base}/mcp"), None, None)
            .await
            .expect("discovery must succeed from loopback metadata");

        assert_eq!(
            endpoints.authorization_endpoint,
            format!("{base}/authorize")
        );
        assert_eq!(endpoints.token_endpoint, format!("{base}/token"));
        assert_eq!(
            endpoints.registration_endpoint,
            Some(format!("{base}/register"))
        );
        assert_eq!(endpoints.scopes, ["mcp:read", "mcp:write"]);
        assert_eq!(endpoints.resource, Some(format!("{base}/mcp")));

        assert_eq!(
            mock.requests().len(),
            3,
            "resource path fallback plus authorization metadata fetch"
        );
    }
}
