use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

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
        // 4 要素目は「レガシーなトップレベル `message` を残すか」のフラグ。
        // 共通スキーマは `{"error":{"message","type"}}` だが、旧 401 レスポンスは
        // `{"error":"unauthorized","message":"..."}` を返していた。互換性のため
        // `Unauthorized` のみトップレベル `message` も併記する。旧 `error` の文字列値
        // "unauthorized" は `error.type` へ移行して保持する（詳細は report 参照）。
        let (status, message, error_type, include_legacy_message) = match self {
            ApiError::SessionNotFound(id) => (
                StatusCode::NOT_FOUND,
                format!("Session not found: {}", id),
                "session_not_found",
                false,
            ),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "not_found", false),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, "bad_request", false),
            ApiError::ProviderError(msg) => (
                StatusCode::BAD_GATEWAY,
                format!("Provider error: {}", msg),
                "provider_error",
                false,
            ),
            ApiError::InvalidRequest(msg) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid request: {}", msg),
                "invalid_request",
                false,
            ),
            ApiError::InternalError(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Internal error: {}", msg),
                "internal_error",
                false,
            ),
            ApiError::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                if msg.is_empty() {
                    "Invalid or missing authentication token".to_owned()
                } else {
                    msg
                },
                "unauthorized",
                true,
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
    async fn internal_error_maps_to_500() {
        assert_response(
            ApiError::InternalError("boom".to_owned()),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Internal error: boom",
        )
        .await;
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
