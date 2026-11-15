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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
