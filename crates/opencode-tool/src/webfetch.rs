use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::{Tool, ToolContext, ToolError, ToolResult};

const MAX_RESPONSE_SIZE: usize = 5 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
/// 手動で追跡するリダイレクトの上限。reqwest の自動追跡は無効化する。
const MAX_REDIRECTS: usize = 10;

pub struct WebFetchTool {
    client: Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36")
                .timeout(std::time::Duration::from_secs(MAX_TIMEOUT_SECS))
                // リダイレクトは自動追跡せず、追跡先を毎回 SSRF 検証してから個別に送信する。
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WebFetchInput {
    url: String,
    #[serde(default = "default_format")]
    format: String,
    #[serde(default)]
    timeout: Option<u64>,
}

fn default_format() -> String {
    "markdown".to_string()
}

/// IPv4 アドレスが SSRF 保護のために拒否すべき範囲かどうかを判定する（純粋関数）。
fn ipv4_is_forbidden(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        // 169.254.169.254 は link-local に含まれるが、クラウドメタデータ
        // エンドポイントとして明示的に拒否する意図を残す。
        || *ip == Ipv4Addr::new(169, 254, 169, 254)
        // CGNAT (100.64.0.0/10)、multicast (224.0.0.0/4)、
        // reserved (240.0.0.0/4) も SSRF の宛先として拒否する。
        || (octets[0] == 100 && (octets[1] & 0xc0) == 0x40)
        || (octets[0] & 0xf0) == 0xe0
        || (octets[0] & 0xf0) == 0xf0
}

/// IPv6 アドレスが拒否すべき範囲かどうかを判定する（純粋関数）。
fn ipv6_is_forbidden(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // fc00::/7（unique local）、fe80::/10（link-local）、
    // ff00::/8（multicast）をビット列で拒否する。
    if (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xff00) == 0xff00
    {
        return true;
    }
    // IPv4-mapped（::ffff:a.b.c.d）と IPv4-compatible（::a.b.c.d）は
    // IPv6 側の範囲検査をすり抜けてプライベート IPv4 を運べるため、
    // 埋め込まれた IPv4 を取り出して IPv4 の規則で再検査する。
    if segments[..5].iter().all(|&segment| segment == 0)
        && (segments[5] == 0 || segments[5] == 0xffff)
    {
        let embedded = Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        );
        return ipv4_is_forbidden(&embedded);
    }
    // 6to4（2002::/16）は bits 16-47 に IPv4 を埋め込む。
    if segments[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        );
        return ipv4_is_forbidden(&embedded);
    }
    // Teredo（2001::/32）は bits 32-63 にサーバ IPv4、
    // bits 96-127 にクライアント IPv4 のビット反転値を埋め込む。
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        let server = Ipv4Addr::new(
            (segments[2] >> 8) as u8,
            segments[2] as u8,
            (segments[3] >> 8) as u8,
            segments[3] as u8,
        );
        if ipv4_is_forbidden(&server) {
            return true;
        }
        let client = Ipv4Addr::new(
            ((!segments[6]) >> 8) as u8,
            (!segments[6]) as u8,
            ((!segments[7]) >> 8) as u8,
            (!segments[7]) as u8,
        );
        return ipv4_is_forbidden(&client);
    }
    false
}

/// IP アドレスが安全でない範囲（loopback / private / link-local / 未指定 /
/// メタデータエンドポイント / CGNAT / multicast / reserved /
/// トンネル埋め込み IPv4 等）にあるかどうかを判定する（純粋関数）。
fn ip_is_forbidden(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_forbidden(v4),
        IpAddr::V6(v6) => ipv6_is_forbidden(v6),
    }
}

/// URL のホスト文字列から IP リテラルを取り出す。IPv6 の括弧を除去する。
fn literal_ip_from_host(host: &str) -> Option<IpAddr> {
    let candidate = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    candidate.parse::<IpAddr>().ok()
}

fn forbidden_target_error(url: &Url) -> ToolError {
    ToolError::PermissionDenied(format!(
        "Refusing to fetch '{}': host resolves to a loopback, private, link-local, or otherwise unsafe IP address",
        url.host_str().unwrap_or("<no host>")
    ))
}

/// URL のスキームと IP リテラルを検証する（DNS 問い合わせを行わない）。
fn validate_url_literal(url: &Url) -> Result<(), ToolError> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(ToolError::InvalidArguments(format!(
                "Unsupported URL scheme '{other}'; only http and https are allowed"
            )))
        }
    }

    let host = url
        .host_str()
        .ok_or_else(|| ToolError::InvalidArguments("URL must contain a host".to_string()))?;

    match literal_ip_from_host(host) {
        Some(ip) if ip_is_forbidden(&ip) => return Err(forbidden_target_error(url)),
        _ => {}
    }
    Ok(())
}

/// ホスト名を解決し、解決された「すべての」IP を検査する。
///
/// 失敗時は fail-closed（解決不能・拒否どちらもエラー）とする。
async fn validate_resolved_host(url: &Url) -> Result<(), ToolError> {
    let host = match url.host_str() {
        Some(host) => host,
        None => {
            return Err(ToolError::InvalidArguments(
                "URL must contain a host".to_string(),
            ))
        }
    };
    if literal_ip_from_host(host).is_some() {
        // IP リテラルは validate_url_literal で検査済み。
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let addresses: Vec<IpAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ToolError::execution_with_source("Failed to resolve URL host", e))?
        .map(|socket| socket.ip())
        .collect();

    if addresses.is_empty() {
        return Err(ToolError::ExecutionError(
            "URL host resolved to no IP addresses".to_string(),
        ));
    }
    if addresses.iter().any(ip_is_forbidden) {
        return Err(forbidden_target_error(url));
    }
    Ok(())
}

/// フェッチ対象（初期 URL と各リダイレクト先）の完全な検証。
async fn validate_url_target(url: &Url) -> Result<(), ToolError> {
    validate_url_literal(url)?;
    validate_resolved_host(url).await
}

#[async_trait]
impl Tool for WebFetchTool {
    fn id(&self) -> &str {
        "webfetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL. Returns the content in the specified format (text, markdown, or html). Defaults to markdown."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch content from"
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "markdown", "html"],
                    "default": "markdown",
                    "description": "The format to return the content in (text, markdown, or html). Defaults to markdown."
                },
                "timeout": {
                    "type": "number",
                    "description": "Optional timeout in seconds (max 120)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: WebFetchInput =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let url = input.url.clone();

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ToolError::InvalidArguments(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        let parsed_url = Url::parse(&url)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid URL: {e}")))?;
        // 権限確認の前に SSRF 検証を行い、危険な宛先はユーザー承認対象にすらしない。
        validate_url_target(&parsed_url).await?;

        ctx.ask_permission(
            crate::PermissionRequest::new("webfetch")
                .with_pattern(&url)
                .always_allow(),
        )
        .await?;

        let timeout_secs = input
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        let accept_header = match input.format.as_str() {
            "markdown" => "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
            "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
            "html" => "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1",
            _ => "*/*",
        };

        let response = tokio::select! {
            result = self.fetch_with_retry(&parsed_url, accept_header, timeout_secs) => result,
            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)) => {
                return Err(ToolError::Timeout(format!("Request timed out after {} seconds", timeout_secs)));
            }
            _ = ctx.abort.cancelled() => {
                return Err(ToolError::Cancelled);
            }
        };

        let response = response?;

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());

        if let Some(len) = content_length {
            if len > MAX_RESPONSE_SIZE {
                return Err(ToolError::ExecutionError(
                    "Response too large (exceeds 5MB limit)".to_string(),
                ));
            }
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ToolError::execution_with_source("Failed to read response", e))?;

        if bytes.len() > MAX_RESPONSE_SIZE {
            return Err(ToolError::ExecutionError(
                "Response too large (exceeds 5MB limit)".to_string(),
            ));
        }

        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let title = format!("{} ({})", url, content_type);

        let is_image = mime.starts_with("image/")
            && mime != "image/svg+xml"
            && mime != "image/vnd.fastbidsheet";

        if is_image {
            let base64_content =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
            let data_url = format!("data:{};base64,{}", mime, base64_content);
            let output = format!(
                "Image fetched successfully.\n\n<attachment type=\"image\" mimeType=\"{}\" url=\"{}\" size=\"{}\" data=\"{}\" />",
                mime, url, bytes.len(), data_url
            );
            let mut metadata = std::collections::HashMap::new();
            metadata.insert("url".to_string(), serde_json::json!(url));
            metadata.insert("mimeType".to_string(), serde_json::json!(mime));
            metadata.insert("size".to_string(), serde_json::json!(bytes.len()));
            metadata.insert("data".to_string(), serde_json::json!(data_url));
            metadata.insert(
                "attachment".to_string(),
                serde_json::json!({
                    "type": "image",
                    "mimeType": mime,
                    "url": url,
                    "size": bytes.len(),
                    "data": data_url
                }),
            );
            return Ok(ToolResult {
                title,
                output,
                metadata,
                truncated: false,
            });
        }

        let content = String::from_utf8_lossy(&bytes).to_string();

        let output = match input.format.as_str() {
            "markdown" => {
                if content_type.contains("text/html") {
                    convert_html_to_markdown(&content)
                } else {
                    content
                }
            }
            "text" => {
                if content_type.contains("text/html") {
                    strip_html(&content)
                } else {
                    content
                }
            }
            "html" | _ => content,
        };

        let mut metadata = std::collections::HashMap::new();
        metadata.insert("url".to_string(), serde_json::json!(url));
        metadata.insert("format".to_string(), serde_json::json!(input.format));
        metadata.insert("mimeType".to_string(), serde_json::json!(mime));
        metadata.insert("size".to_string(), serde_json::json!(output.len()));

        Ok(ToolResult {
            title,
            output,
            metadata,
            truncated: false,
        })
    }
}

impl WebFetchTool {
    async fn fetch_with_retry(
        &self,
        url: &Url,
        accept_header: &str,
        _timeout_secs: u64,
    ) -> Result<reqwest::Response, ToolError> {
        let mut current = url.clone();

        for _ in 0..=MAX_REDIRECTS {
            // 初期 URL は execute() で検証済み。リダイレクト先は追跡のたびに再検証する。
            let mut response = self
                .client
                .get(current.clone())
                .header("Accept", accept_header)
                .header("Accept-Language", "en-US,en;q=0.9")
                .send()
                .await
                .map_err(|e| ToolError::execution_with_source("Failed to fetch URL", e))?;

            if response.status() == 403 {
                let cf_mitigated = response
                    .headers()
                    .get("cf-mitigated")
                    .and_then(|v| v.to_str().ok());

                if cf_mitigated == Some("challenge") {
                    response = self
                        .client
                        .get(current.clone())
                        .header("Accept", accept_header)
                        .header("User-Agent", "opencode")
                        .send()
                        .await
                        .map_err(|e| ToolError::execution_with_source("Failed to fetch URL", e))?;
                }
            }

            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ToolError::ExecutionError(format!(
                            "Redirect from '{current}' has no valid Location header"
                        ))
                    })?;
                let next = current.join(location).map_err(|e| {
                    ToolError::ExecutionError(format!("Invalid redirect URL '{location}': {e}"))
                })?;
                validate_url_target(&next).await?;
                current = next;
                continue;
            }

            if !response.status().is_success() {
                return Err(ToolError::ExecutionError(format!(
                    "Request failed with status code: {}",
                    response.status()
                )));
            }

            return Ok(response);
        }

        Err(ToolError::ExecutionError(format!(
            "Exceeded maximum of {MAX_REDIRECTS} redirects"
        )))
    }
}

fn convert_html_to_markdown(html: &str) -> String {
    strip_html(html)
}

fn strip_html(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let chars: Vec<char> = html.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];

        if c == '<' {
            if i + 7 <= len {
                let tag: String = chars[i..i + 7].iter().collect();
                let tag_lower = tag.to_lowercase();
                if tag_lower.starts_with("<script") {
                    in_script = true;
                } else if tag_lower.starts_with("<style") {
                    in_style = true;
                }
            }
            in_tag = true;
            i += 1;
            continue;
        }

        if c == '>' {
            if in_script {
                if i >= 8 {
                    let end_tag: String = chars[i - 8..=i].iter().collect();
                    if end_tag.to_lowercase() == "</script>" {
                        in_script = false;
                    }
                }
            } else if in_style && i >= 7 {
                let end_tag: String = chars[i - 7..=i].iter().collect();
                if end_tag.to_lowercase() == "</style>" {
                    in_style = false;
                }
            }
            in_tag = false;
            i += 1;
            continue;
        }

        if !in_tag && !in_script && !in_style {
            if c == '&' {
                if i + 4 <= len {
                    let entity: String = chars[i..i + 4].iter().collect();
                    match entity.as_str() {
                        "&lt;" => {
                            result.push('<');
                            i += 4;
                            continue;
                        }
                        "&gt;" => {
                            result.push('>');
                            i += 4;
                            continue;
                        }
                        "&amp;" => {
                            result.push('&');
                            i += 5;
                            continue;
                        }
                        _ => {}
                    }
                }
                if i + 6 <= len {
                    let entity: String = chars[i..i + 6].iter().collect();
                    if entity == "&nbsp;" {
                        result.push(' ');
                        i += 6;
                        continue;
                    }
                }
            }
            result.push(c);
        }

        i += 1;
    }

    let result = result
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    result
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_schema() {
        let tool = WebFetchTool::new();
        assert_eq!(tool.id(), "webfetch");
        assert_eq!(tool.parameters()["required"][0], "url");
    }

    #[test]
    fn default_format_is_markdown() {
        assert_eq!(default_format(), "markdown");
    }

    #[test]
    fn input_applies_defaults() {
        let input: WebFetchInput =
            serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert_eq!(input.url, "https://example.com");
        assert_eq!(input.format, "markdown");
        assert!(input.timeout.is_none());
    }

    #[test]
    fn input_accepts_explicit_options() {
        let input: WebFetchInput = serde_json::from_value(serde_json::json!({
            "url": "https://example.com",
            "format": "text",
            "timeout": 10
        }))
        .unwrap();
        assert_eq!(input.format, "text");
        assert_eq!(input.timeout, Some(10));
    }

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(
            strip_html("<p>hello <b>world</b></p>").trim(),
            "hello world"
        );
    }

    #[test]
    fn convert_html_to_markdown_delegates_to_strip_html() {
        let html = "<div>text</div>";
        assert_eq!(convert_html_to_markdown(html), strip_html(html));
    }

    #[test]
    fn ipv4_forbidden_ranges_are_rejected() {
        for literal in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.1.1",
            "0.0.0.0",
            "255.255.255.255",
            "100.64.0.1",
            "100.127.255.254",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.254",
        ] {
            let ip: IpAddr = literal.parse().unwrap();
            assert!(ip_is_forbidden(&ip), "{literal} must be rejected");
        }
    }

    #[test]
    fn ipv4_public_addresses_are_allowed() {
        for literal in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            // CGNAT は 100.64.0.0/10 のみ。前後の /8 は許可する。
            "100.0.0.1",
            "100.63.255.255",
            "100.128.0.1",
            // multicast (224/4) と reserved (240/4) の直前まで許可する。
            "223.255.255.255",
        ] {
            let ip: IpAddr = literal.parse().unwrap();
            assert!(!ip_is_forbidden(&ip), "{literal} must be allowed");
        }
    }

    #[test]
    fn ipv6_forbidden_ranges_and_embedded_ipv4_are_rejected() {
        for literal in [
            "::1",
            "fc00::1",
            "fd00::1",
            "fe80::1",
            "::",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "ff02::1",
            // 6to4（2002::/16）の bits 16-47 に loopback / private を埋め込む。
            "2002:7f00:1::",
            "2002:a00:1::",
            // Teredo（2001::/32）の末尾 32bit はクライアント IPv4 の反転値。
            // 0x80ff:fffe -> 127.0.0.1
            "2001:0:4136:e378:8000:63bf:80ff:fffe",
            // bits 32-63 の Teredo サーバ IPv4 が loopback の場合も拒否する。
            "2001:0:7f00:1:8000:63bf:c000:22d",
        ] {
            let ip: IpAddr = literal.parse().unwrap();
            assert!(ip_is_forbidden(&ip), "{literal} must be rejected");
        }
    }

    #[test]
    fn ipv6_public_addresses_are_allowed() {
        for literal in [
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            // Teredo でも埋め込み IPv4 とサーバ IPv4 が public なら許可する。
            "2001:0:4136:e378:8000:63bf:c000:22d",
        ] {
            let ip: IpAddr = literal.parse().unwrap();
            assert!(!ip_is_forbidden(&ip), "{literal} must be allowed");
        }
    }

    #[test]
    fn literal_ip_from_host_strips_ipv6_brackets() {
        assert_eq!(
            literal_ip_from_host("127.0.0.1"),
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)))
        );
        assert_eq!(
            literal_ip_from_host("[::1]"),
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST))
        );
        assert_eq!(literal_ip_from_host("example.com"), None);
    }

    #[test]
    fn validate_url_literal_rejects_unsafe_targets_without_network() {
        let cases = [
            ("http://127.0.0.1/", true),
            ("http://[::1]/", true),
            ("http://169.254.169.254/latest/meta-data/", true),
            ("http://[::ffff:192.168.1.1]/", true),
            ("http://[2002:7f00:1::]/", true),
            ("file:///etc/passwd", true),
            ("https://example.com/", false),
        ];
        for (raw, should_reject) in cases {
            let parsed = Url::parse(raw).unwrap();
            assert_eq!(
                validate_url_literal(&parsed).is_err(),
                should_reject,
                "unexpected validation result for {raw}"
            );
        }
    }
}
