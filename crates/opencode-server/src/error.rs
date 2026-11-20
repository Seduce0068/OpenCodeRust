use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// 内部エラーの詳細を tracing ログへ出す前に、パスとトークンらしい値を伏せる。
///
/// クライアントには汎用メッセージと相関 ID だけを返し、内部詳細はログへ残す。
/// その際にファイルパス（`/...` / `C:\...` / URL）や認証トークン値がログへ
/// 流出しないよう、該当する単語を `<redacted>` に置換する。
fn redact_sensitive_detail(detail: &str) -> String {
    const REDACTED: &str = "<redacted>";
    let mut words: Vec<&str> = Vec::new();
    let mut redact_next = false;
    for word in detail.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| matches!(c, ',' | ';' | ')' | ']'));
        let key = trimmed
            .split(['=', ':'])
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .to_ascii_lowercase();
        let is_secret_key = matches!(
            key.as_str(),
            "token" | "secret" | "password" | "authorization" | "bearer" | "api_key" | "apikey"
        );
        let looks_like_path = trimmed.contains('/') || trimmed.contains('\\');
        let looks_like_token = trimmed.len() >= 24
            && trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '='));
        // 機微キーを含む語は「値が 24 文字未満」でも漏洩し得るため、
        // 現在語そのものも伏せる。後続語は redact_next で長さに関わらず伏せる。
        if redact_next || is_secret_key || looks_like_path || looks_like_token {
            words.push(REDACTED);
        } else {
            words.push(word);
        }
        redact_next = is_secret_key;
    }
    words.join(" ")
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Provider error: {0}")]
    ProviderError(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Internal error: {0}")]
    InternalError(String),

    /// 認証失敗（401）。
    ///
    /// `auth_middleware` の 401 レスポンスを `ApiError` の共通スキーマへ
    /// 集約するために追加したバリアント。共通スキーマに加え、既存クライアント
    /// 互換のためトップレベル `message` も保持する（詳細は `IntoResponse`）。
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 4 要素目は「レガシーなトップレベル `message` を残すか」のフラグ、
        // 5 要素目は InternalError の相関 ID（他バリアントは None）。
        // 共通スキーマは `{"error":{"message","type"}}` だが、旧 401 レスポンスは
        // `{"error":"unauthorized","message":"..."}` を返していた。互換性のため
        // `Unauthorized` のみトップレベル `message` も併記する。旧 `error` の文字列値
        // "unauthorized" は `error.type` へ移行して保持する（詳細は report 参照）。
        let (status, message, error_type, include_legacy_message, correlation_id) = match self {
            ApiError::SessionNotFound(id) => (
                StatusCode::NOT_FOUND,
                format!("Session not found: {}", id),
                "session_not_found",
                false,
                None,
            ),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "not_found", false, None),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, "bad_request", false, None),
            ApiError::ProviderError(msg) => (
                StatusCode::BAD_GATEWAY,
                format!("Provider error: {}", msg),
                "provider_error",
                false,
                None,
            ),
            ApiError::InvalidRequest(msg) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid request: {}", msg),
                "invalid_request",
                false,
                None,
            ),
            // 内部詳細はボディへ出さず、相関 ID 付きで tracing ログへ残す。
            // ログにもパス / トークン値が出ないよう redact してから記録する。
            ApiError::InternalError(msg) => {
                let correlation_id = uuid::Uuid::new_v4().to_string();
                tracing::error!(
                    correlation_id = %correlation_id,
                    detail = %redact_sensitive_detail(&msg),
                    "internal error"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_owned(),
                    "internal_error",
                    false,
                    Some(correlation_id),
                )
            }
            ApiError::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                if msg.is_empty() {
                    "Invalid or missing authentication token".to_owned()
                } else {
                    msg
                },
                "unauthorized",
                true,
                None,
            ),
        };

        let body = if include_legacy_message {
            let legacy_message = message.clone();
            json!({
                "error": {
                    "message": legacy_message,
                    "type": error_type
                },
                "message": message
            })
        } else if let Some(correlation_id) = correlation_id {
            json!({
                "error": {
                    "message": message,
                    "type": error_type,
                    "correlation_id": correlation_id
                }
            })
        } else {
            json!({
                "error": {
                    "message": message,
                    "type": error_type
                }
            })
        };

        (status, Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    async fn assert_response(
        error: ApiError,
        expected_status: StatusCode,
        expected_type: &str,
        expected_message: &str,
    ) {
        let response = error.into_response();
        assert_eq!(response.status(), expected_status);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], expected_type);
        assert_eq!(body["error"]["message"], expected_message);
    }

    #[tokio::test]
    async fn session_not_found_maps_to_404() {
        assert_response(
            ApiError::SessionNotFound("abc".to_owned()),
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session not found: abc",
        )
        .await;
    }

    #[tokio::test]
    async fn not_found_maps_to_404() {
        assert_response(
            ApiError::NotFound("missing".to_owned()),
            StatusCode::NOT_FOUND,
            "not_found",
            "missing",
        )
        .await;
    }

    #[tokio::test]
    async fn bad_request_maps_to_400() {
        assert_response(
            ApiError::BadRequest("bad".to_owned()),
            StatusCode::BAD_REQUEST,
            "bad_request",
            "bad",
        )
        .await;
    }

    #[tokio::test]
    async fn provider_error_maps_to_502() {
        assert_response(
            ApiError::ProviderError("down".to_owned()),
            StatusCode::BAD_GATEWAY,
            "provider_error",
            "Provider error: down",
        )
        .await;
    }

    #[tokio::test]
    async fn invalid_request_maps_to_400() {
        assert_response(
            ApiError::InvalidRequest("oops".to_owned()),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request: oops",
        )
        .await;
    }

    #[tokio::test]
    async fn internal_error_maps_to_500_without_internal_detail() {
        // 回帰ガード: 内部メッセージはボディへ出ず、汎用メッセージ＋相関 ID のみ。
        let response = ApiError::InternalError("boom".to_owned()).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "internal_error");
        assert_eq!(body["error"]["message"], "Internal server error");
        let correlation_id = body["error"]["correlation_id"]
            .as_str()
            .expect("correlation id must be returned");
        assert!(
            uuid::Uuid::parse_str(correlation_id).is_ok(),
            "correlation id must be a UUID: {correlation_id}"
        );
        assert!(!body.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn internal_error_never_leaks_paths_or_tokens_to_body() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let response = ApiError::InternalError(format!(
            "read /home/user/.secrets/auth.json with token={secret} failed"
        ))
        .into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let text = body.to_string();
        assert!(!text.contains("/home/user"), "path must not leak: {text}");
        assert!(!text.contains("auth.json"), "path must not leak: {text}");
        assert!(!text.contains(secret), "token value must not leak: {text}");
    }

    #[test]
    fn redacted_internal_detail_hides_paths_and_token_values() {
        let redacted = redact_sensitive_detail(
            "read /home/user/.secrets/auth.json with token=sk-abcdefghijklmnopqrstuvwxyz012345 failed",
        );
        assert!(!redacted.contains("/home/user"), "{redacted}");
        assert!(!redacted.contains("auth.json"), "{redacted}");
        assert!(
            !redacted.contains("sk-abcdefghijklmnopqrstuvwxyz012345"),
            "{redacted}"
        );
        assert!(redacted.contains("<redacted>"), "{redacted}");
        // 無害な詳細はログに残る（トレース可能性を維持）。
        assert_eq!(
            redact_sensitive_detail("find_text scan task failed: JoinError"),
            "find_text scan task failed: JoinError"
        );
    }

    #[test]
    fn redacted_internal_detail_hides_inline_secret_key_values() {
        // 回帰ガード (R4): "token=abc123" のようにキーと値が同一語にある場合、
        // 値が 24 文字未満でもキー語自体を <redacted> に置換する。
        let redacted = redact_sensitive_detail("provider rejected token=abc123 request");
        assert!(!redacted.contains("abc123"), "{redacted}");
        assert!(!redacted.contains("token"), "{redacted}");
        assert!(redacted.contains("<redacted>"), "{redacted}");
    }

    #[test]
    fn redacted_internal_detail_hides_bearer_header_and_short_values() {
        // 回帰ガード (R4): "Authorization: Bearer xyz" のトークン値と、
        // 24 文字未満の機微値をログへ出さない。
        let redacted = redact_sensitive_detail("Authorization: Bearer xyz was rejected");
        assert!(!redacted.contains("xyz"), "{redacted}");
        assert!(!redacted.contains("Authorization"), "{redacted}");
        assert!(!redacted.contains("Bearer"), "{redacted}");

        let redacted = redact_sensitive_detail("secret hunter2 leaked");
        assert!(!redacted.contains("hunter2"), "{redacted}");

        let redacted = redact_sensitive_detail("password: short2 failed");
        assert!(!redacted.contains("short2"), "{redacted}");
    }

    #[tokio::test]
    async fn unauthorized_maps_to_401_with_common_schema_and_legacy_message() {
        // 回帰ガード: 共通スキーマ `error.{message,type}` に加え、
        // 旧クライアントが読むトップレベル `message` が残ること。
        let response = ApiError::Unauthorized("Invalid or missing authentication token".to_owned())
            .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "unauthorized");
        assert_eq!(
            body["error"]["message"],
            "Invalid or missing authentication token"
        );
        assert_eq!(
            body["message"], "Invalid or missing authentication token",
            "legacy top-level message must be preserved for older clients"
        );
    }

    #[tokio::test]
    async fn unauthorized_empty_message_uses_safe_default() {
        // 空文字でも TypeError 名称だけは安全に返し、トークン値を露出しない。
        let response = ApiError::Unauthorized(String::new()).into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "unauthorized");
        assert_eq!(
            body["error"]["message"],
            "Invalid or missing authentication token"
        );
    }
}
