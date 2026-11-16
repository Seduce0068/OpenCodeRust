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
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message, error_type) = match self {
            ApiError::SessionNotFound(id) => (
                StatusCode::NOT_FOUND,
                format!("Session not found: {}", id),
                "session_not_found",
            ),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "not_found"),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, "bad_request"),
            ApiError::ProviderError(msg) => (
                StatusCode::BAD_GATEWAY,
                format!("Provider error: {}", msg),
                "provider_error",
            ),
            ApiError::InvalidRequest(msg) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid request: {}", msg),
                "invalid_request",
            ),
            ApiError::InternalError(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Internal error: {}", msg),
                "internal_error",
            ),
        };

        let body = Json(json!({
            "error": {
                "message": message,
                "type": error_type
            }
        }));

        (status, body).into_response()
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
}
