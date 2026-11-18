//! Auth bridge — connects TS plugin auth hooks to the Rust AuthManager
//! and provides HTTP fetch proxying for plugins with custom `fetch`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use super::client::{AuthMeta, AuthMethodMeta, PluginSubprocess, PluginSubprocessError};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PluginAuthError {
    #[error("plugin subprocess error: {0}")]
    Subprocess(#[from] PluginSubprocessError),

    #[error("plugin {0} does not provide auth")]
    NoAuth(String),

    #[error("invalid auth method index {index} for plugin {plugin}")]
    InvalidMethodIndex { plugin: String, index: usize },

    #[error("no custom fetch available for plugin {0}")]
    NoCustomFetch(String),
}

// ---------------------------------------------------------------------------
// PluginAuthBridge
// ---------------------------------------------------------------------------

/// Bridges a single TS plugin's auth hooks to the Rust side.
///
/// Each plugin that declares `auth` in its initialize response gets one of
/// these. It wraps the subprocess RPC calls and tracks whether the plugin
/// provides a custom fetch proxy.
pub struct PluginAuthBridge {
    client: Arc<PluginSubprocess>,
    meta: AuthMeta,
    has_custom_fetch: std::sync::atomic::AtomicBool,
    /// Cached API key from the last `auth.load` call.
    cached_api_key: tokio::sync::RwLock<Option<String>>,
}

impl PluginAuthBridge {
    pub fn new(client: Arc<PluginSubprocess>, meta: AuthMeta) -> Self {
        Self {
            client,
            meta,
            has_custom_fetch: std::sync::atomic::AtomicBool::new(false),
            cached_api_key: tokio::sync::RwLock::new(None),
        }
    }

    // -- Accessors ----------------------------------------------------------

    /// The provider ID this auth plugin serves (e.g. "openai", "anthropic").
    pub fn provider(&self) -> &str {
        &self.meta.provider
    }

    /// Available auth methods (OAuth, API key, etc.).
    pub fn methods(&self) -> &[AuthMethodMeta] {
        &self.meta.methods
    }

    /// Whether the plugin provides a custom fetch that must proxy LLM requests.
    pub fn has_custom_fetch(&self) -> bool {
        self.has_custom_fetch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The cached API key from the last `load()` call, if any.
    pub async fn cached_api_key(&self) -> Option<String> {
        self.cached_api_key.read().await.clone()
    }

    // -- Auth flow ----------------------------------------------------------

    /// Start an OAuth authorization flow for the given method index.
    ///
    /// Returns the authorization URL and instructions for the user.
    pub async fn authorize(
        &self,
        method_index: usize,
        inputs: Option<HashMap<String, String>>,
    ) -> Result<PluginAuthorizeResult, PluginAuthError> {
        if method_index >= self.meta.methods.len() {
            return Err(PluginAuthError::InvalidMethodIndex {
                plugin: self.client.name().to_string(),
                index: method_index,
            });
        }

        let inputs_value = inputs.map(|m| serde_json::to_value(m).unwrap_or(Value::Null));
        let result = self
            .client
            .auth_authorize(method_index, inputs_value)
            .await?;

        Ok(PluginAuthorizeResult {
            url: result.url,
            instructions: result.instructions,
            method: result.method,
        })
    }

    /// Complete the OAuth callback with an optional authorization code.
    pub async fn callback(&self, code: Option<&str>) -> Result<Value, PluginAuthError> {
        let result = self.client.auth_callback(code).await?;
        Ok(result)
    }

    /// Load the auth provider configuration.
    ///
    /// This calls `auth.load` on the plugin, which may return an API key
    /// and/or indicate that a custom fetch proxy is available.
    pub async fn load(&self) -> Result<PluginAuthLoadResult, PluginAuthError> {
        let result = self.client.auth_load(self.provider()).await?;

        // Cache the API key
        {
            let mut cached = self.cached_api_key.write().await;
            *cached = result.api_key.clone();
        }

        // Track custom fetch availability
        self.has_custom_fetch.store(
            result.has_custom_fetch,
            std::sync::atomic::Ordering::Relaxed,
        );

        Ok(PluginAuthLoadResult {
            api_key: result.api_key,
            has_custom_fetch: result.has_custom_fetch,
        })
    }

    // -- Fetch proxy --------------------------------------------------------

    /// Proxy an HTTP request through the plugin's custom fetch.
    ///
    /// Only valid when `has_custom_fetch()` is true (after a successful `load()`).
    pub async fn fetch_proxy(
        &self,
        request: PluginFetchRequest,
    ) -> Result<PluginFetchResponse, PluginAuthError> {
        if !self.has_custom_fetch() {
            return Err(PluginAuthError::NoCustomFetch(
                self.client.name().to_string(),
            ));
        }

        let result = self
            .client
            .auth_fetch(
                &request.url,
                &request.method,
                &request.headers,
                request.body.as_deref(),
            )
            .await?;

        Ok(PluginFetchResponse {
            status: result.status,
            headers: result.headers,
            body: result.body,
        })
    }

    /// Proxy an HTTP request through plugin custom fetch as a real-time stream.
    pub async fn fetch_proxy_stream(
        &self,
        request: PluginFetchRequest,
    ) -> Result<PluginFetchStreamResponse, PluginAuthError> {
        if !self.has_custom_fetch() {
            return Err(PluginAuthError::NoCustomFetch(
                self.client.name().to_string(),
            ));
        }

        let result = self
            .client
            .auth_fetch_stream(
                &request.url,
                &request.method,
                &request.headers,
                request.body.as_deref(),
            )
            .await?;

        Ok(PluginFetchStreamResponse {
            status: result.status,
            headers: result.headers,
            chunks: result.chunks,
        })
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthorizeResult {
    pub url: Option<String>,
    pub instructions: Option<String>,
    pub method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthLoadResult {
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(rename = "hasCustomFetch")]
    pub has_custom_fetch: bool,
}

/// An HTTP request to be proxied through the plugin's custom fetch.
#[derive(Debug, Clone)]
pub struct PluginFetchRequest {
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

/// The response from a proxied HTTP request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginFetchResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// Real-time streamed response from plugin custom fetch.
pub struct PluginFetchStreamResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub chunks: mpsc::Receiver<Result<String, PluginSubprocessError>>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // 実際の認証情報・トークン値・API キーは一切使わない。
    // 以下の定数はすべて検証用のダミー文字列である。
    const DUMMY_PLUGIN: &str = "test-plugin";
    const DUMMY_KEY: &str = "dummy-placeholder-not-a-real-key";

    #[test]
    fn error_display_no_auth() {
        let err = PluginAuthError::NoAuth(DUMMY_PLUGIN.to_string());
        assert_eq!(err.to_string(), "plugin test-plugin does not provide auth");
    }

    #[test]
    fn error_display_invalid_method_index() {
        let err = PluginAuthError::InvalidMethodIndex {
            plugin: DUMMY_PLUGIN.to_string(),
            index: 3,
        };
        assert_eq!(
            err.to_string(),
            "invalid auth method index 3 for plugin test-plugin"
        );
    }

    #[test]
    fn error_display_no_custom_fetch() {
        let err = PluginAuthError::NoCustomFetch(DUMMY_PLUGIN.to_string());
        assert_eq!(
            err.to_string(),
            "no custom fetch available for plugin test-plugin"
        );
    }

    #[test]
    fn error_display_subprocess_wraps_inner_error() {
        let err = PluginAuthError::from(PluginSubprocessError::NotRunning);
        assert_eq!(
            err.to_string(),
            "plugin subprocess error: plugin subprocess not running"
        );

        let err = PluginAuthError::Subprocess(PluginSubprocessError::Rpc {
            code: -32000,
            message: "dummy rpc failure".to_string(),
        });
        assert_eq!(
            err.to_string(),
            "plugin subprocess error: plugin RPC error (-32000): dummy rpc failure"
        );
    }

    #[test]
    fn from_subprocess_error_preserves_inner_variant() {
        let err = PluginAuthError::from(PluginSubprocessError::NotRunning);
        assert!(matches!(
            err,
            PluginAuthError::Subprocess(PluginSubprocessError::NotRunning)
        ));
    }

    #[test]
    fn auth_load_result_deserializes_camel_case_and_round_trips() {
        let json = serde_json::json!({
            "apiKey": DUMMY_KEY,
            "hasCustomFetch": true,
        });
        let parsed: PluginAuthLoadResult = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(parsed.api_key.as_deref(), Some(DUMMY_KEY));
        assert!(parsed.has_custom_fetch);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), json);
    }

    #[test]
    fn auth_load_result_allows_missing_api_key() {
        let parsed: PluginAuthLoadResult =
            serde_json::from_value(serde_json::json!({ "hasCustomFetch": false })).unwrap();
        assert!(parsed.api_key.is_none());
        assert!(!parsed.has_custom_fetch);

        let value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "apiKey": null, "hasCustomFetch": false })
        );
    }

    #[test]
    fn authorize_result_round_trips_with_dummy_values() {
        let result = PluginAuthorizeResult {
            url: Some("https://example.invalid/authorize".to_string()),
            instructions: Some("dummy instructions".to_string()),
            method: Some("oauth".to_string()),
        };
        let value = serde_json::to_value(&result).unwrap();
        let parsed: PluginAuthorizeResult = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.url, result.url);
        assert_eq!(parsed.instructions, result.instructions);
        assert_eq!(parsed.method, result.method);
    }

    #[test]
    fn fetch_response_round_trips_with_dummy_values() {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());
        let response = PluginFetchResponse {
            status: 200,
            headers,
            body: "dummy body".to_string(),
        };
        let value = serde_json::to_value(&response).unwrap();
        let parsed: PluginFetchResponse = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, "dummy body");
        assert_eq!(
            parsed.headers.get("content-type").map(String::as_str),
            Some("text/plain")
        );
    }

    #[test]
    fn fetch_request_holds_dummy_values_without_side_effects() {
        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), "application/json".to_string());
        let request = PluginFetchRequest {
            url: "https://example.invalid/echo".to_string(),
            method: "GET".to_string(),
            headers: headers.clone(),
            body: None,
        };
        assert_eq!(request.url, "https://example.invalid/echo");
        assert_eq!(request.method, "GET");
        assert_eq!(request.headers, headers);
        assert!(request.body.is_none());
    }
}
