use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// 機微情報のキー名かどうかを判定する。
///
/// 大文字小文字を無視し、`-` と `_` を同一視して正規化したうえで **suffix 一致** を行う。
/// 完全一致ではなく suffix 一致にすることで、OAuth 系の `client_secret` /
/// `client-secret` / `access_token` / `api-key` / `refresh_token` / `id_token` など、
/// 既知の語に接頭辞が付いた形も検出できる。
fn is_secret_key(key: &str) -> bool {
    const SENSITIVE_KEY_SUFFIXES: &[&str] = &[
        "token",
        "secret",
        "password",
        "authorization",
        "bearer",
        "api_key",
        "apikey",
        "credential",
        "credentials",
        "secret_key",
        "private_key",
        "access_key",
    ];

    let normalized = key
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .replace('-', "_");
    // camelCase の境界（small→large）を _ へ変換し、secretKey → secret_key に正規化する。
    let mut with_boundaries = String::with_capacity(normalized.len() + 4);
    let mut previous: Option<char> = None;
    for ch in normalized.chars() {
        if ch.is_uppercase() && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            with_boundaries.push('_');
        }
        with_boundaries.push(ch);
        previous = Some(ch);
    }
    let normalized = with_boundaries.to_ascii_lowercase();
    SENSITIVE_KEY_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

/// 内部エラーの詳細を tracing ログへ出す前に、パスとトークンらしい値を伏せる。
///
/// クライアントには汎用メッセージと相関 ID だけを返し、内部詳細はログへ残す。
/// その際にファイルパス（`/...` / `C:\...` / URL）や認証トークン値がログへ
/// 流出しないよう、該当する単語を `<redacted>` に置換する。
pub(crate) fn redact_sensitive_detail(detail: &str) -> String {
    // ログ側のマスクは山括弧形式 `<redacted>` に固定する。
    // UI 表示側（opencode-tui）は `[redacted]` を用いる。共有定数を新設しない
    // スコープのため、テストで値を固定して形式の漂移を検出する。
    const REDACTED: &str = "<redacted>";
    let mut words: Vec<&str> = Vec::new();
    let mut redact_next = false;
    let mut redaction_budget = 0usize;
    const MAX_REDACTED_VALUE_WORDS: usize = 1;
    for word in detail.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| matches!(c, ',' | ';' | ')' | ']'));
        let key = trimmed.split(['=', ':']).next().unwrap_or("");
        let secret_key = is_secret_key(key);
        let looks_like_path = trimmed.contains('/') || trimmed.contains('\\');
        let looks_like_token = trimmed.len() >= 24
            && trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '='));
        // 機微キーを含む語は値が 24 文字未満でも漏洩し得るため語全体を伏せる。
        // 後続語は redact_next で長さに関わらず常に伏せる（キーと値が別語でも安全）。
        if redact_next || secret_key || looks_like_path || looks_like_token {
            words.push(REDACTED);
        } else {
            words.push(word);
        }
        // 機微キー検出後は、次の `key=` / `key:` による新しいKeyValueが出るまで
        // 値の一部として伏せ続ける（"secret: ab cd ef failed" の複数語に対応）。
        let starts_new_assignment = word.contains('=') || word.contains(':');
        if secret_key {
            redact_next = true;
            redaction_budget = MAX_REDACTED_VALUE_WORDS;
        } else if redact_next {
            redaction_budget = redaction_budget.saturating_sub(1);
            if redaction_budget == 0 || (starts_new_assignment && !is_secret_key(key)) {
                redact_next = false;
            }
        }
    }
    words.join(" ")
}

/// 相関 ID を払い出し、redact 済みの構造化フィールド付きでサーバエラーを記録する。
///
/// `InternalError` / `ProviderError` / セッション永続化失敗で共用する。
/// 呼び出し側は必ず redact 済みの詳細だけを渡し、生エラー文字列を渡さない。
/// クライアントには相関 ID だけを返し、内部詳細はログへ残す。
pub(crate) fn log_error_with_correlation_id(
    event: &'static str,
    kind: &'static str,
    safe_detail: &str,
) -> String {
    let correlation_id = uuid::Uuid::new_v4().to_string();
    tracing::error!(
        correlation_id = %correlation_id,
        error_kind = %kind,
        detail = %safe_detail,
        event = %event,
        "server error"
    );
    correlation_id
}

/// ProviderError のログ詳細を `provider_id` と `status` だけに絞る。
///
/// プロバイダのレスポンス本文・トークン値はこの関数へ渡されないため、
/// tracing ログへ漏れない。識別子も念のため redact を通す。
fn provider_log_detail(provider_id: &str, status: Option<u16>) -> String {
    format!(
        "provider_id={} status={}",
        redact_sensitive_detail(provider_id),
        status
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".to_owned())
    )
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    /// プロバイダ層の失敗。
    ///
    /// `provider_id` / `status` は tracing ログへ構造化フィールドとして残す。
    /// `detail`（プロバイダの本文）は redact してクライアント文言に載せるが、
    /// ログには渡さない（本文・トークン値の漏洩防止）。
    #[error("Provider error: {detail}")]
    ProviderError {
        provider_id: String,
        status: Option<u16>,
        detail: String,
    },

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
        // 5 要素目は InternalError / ProviderError の相関 ID（他バリアントは None）。
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
            // 相関 ID を返し、tracing には provider_id と status だけを残す。
            // プロバイダの本文は redact してボディへ載せるが、ログへは渡さない。
            ApiError::ProviderError {
                provider_id,
                status,
                detail,
            } => {
                let safe_detail = redact_sensitive_detail(&detail);
                let log_detail = provider_log_detail(&provider_id, status);
                let correlation_id =
                    log_error_with_correlation_id("provider error", "provider_error", &log_detail);
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Provider error: {safe_detail}"),
                    "provider_error",
                    false,
                    Some(correlation_id),
                )
            }
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
                let safe_detail = redact_sensitive_detail(&msg);
                let correlation_id =
                    log_error_with_correlation_id("internal error", "internal_error", &safe_detail);
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

        let body = if error_type == "unauthorized" {
            // 旧 401 契約 `{"error":"unauthorized","message":"..."}` を維持する。
            // 新スキーマの `error.type` と同等の値をトップレベル `error` にも載せる。
            json!({
                "error": error_type,
                "message": message
            })
        } else if include_legacy_message {
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
            ApiError::ProviderError {
                provider_id: "openai".to_owned(),
                status: Some(502),
                detail: "down".to_owned(),
            },
            StatusCode::BAD_GATEWAY,
            "provider_error",
            "Provider error: down",
        )
        .await;
    }

    #[tokio::test]
    async fn provider_error_returns_correlation_id_without_leaking_token() {
        // 回帰ガード (OBS-02): ProviderError も相関 ID を返し、
        // プロバイダ本文に含まれるトークン値をボディへ出さない。
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let response = ApiError::ProviderError {
            provider_id: "openai".to_owned(),
            status: Some(401),
            detail: format!("upstream rejected token={secret}"),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "provider_error");
        let correlation_id = body["error"]["correlation_id"]
            .as_str()
            .expect("provider error must return a correlation id");
        assert!(
            uuid::Uuid::parse_str(correlation_id).is_ok(),
            "correlation id must be a UUID: {correlation_id}"
        );
        let text = body.to_string();
        assert!(!text.contains(secret), "token value must not leak: {text}");
    }

    #[test]
    fn provider_log_detail_includes_only_provider_id_and_status() {
        // 回帰ガード (OBS-02): tracing へ渡す詳細は provider_id と status のみで、
        // レスポンス本文やトークン値は含まれない。
        let detail = provider_log_detail("openai", Some(429));
        assert!(detail.contains("provider_id=openai"), "{detail}");
        assert!(detail.contains("status=429"), "{detail}");
        assert!(!detail.contains("body"), "{detail}");
        assert!(!detail.contains("token"), "{detail}");

        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let redacted = provider_log_detail(secret, Some(500));
        assert!(!redacted.contains(secret), "{redacted}");
        assert!(redacted.contains("<redacted>"), "{redacted}");

        let unspecified = provider_log_detail("openai", None);
        assert!(unspecified.contains("status=none"), "{unspecified}");
    }

    /// 回帰テスト（R2）: 内部エラーのログマスクは山括弧形式 `<redacted>` に固定する。
    /// UI 表示側の `[redacted]` に漂移しないことを検出する。
    #[test]
    fn redact_sensitive_detail_uses_log_angle_bracket_marker() {
        let masked = redact_sensitive_detail("token=abc123");
        assert_eq!(
            masked, "<redacted>",
            "log mask must stay in angle-bracket form"
        );
        assert!(!masked.contains("[redacted]"));
        assert!(!masked.contains("abc123"), "token value leaked: {masked}");
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

    #[test]
    fn is_secret_key_normalizes_case_and_separators_and_uses_suffix_match() {
        for key in [
            "token",
            "TOKEN",
            "client_secret",
            "client-secret",
            "CLIENT_SECRET",
            "access_token",
            "access-token",
            "ACCESS_TOKEN",
            "api-key",
            "api_key",
            "API_KEY",
            "apikey",
            "refresh_token",
            "id_token",
            "authorization",
            "Bearer",
            "password",
            "x-amz-security-token",
            "private_key",
        ] {
            assert!(is_secret_key(key), "expected secret key: {key}");
        }

        for safe in [
            "find_text",
            "model",
            "provider",
            "status",
            "duration",
            "JoinError",
        ] {
            assert!(!is_secret_key(safe), "unexpected secret key: {safe}");
        }
    }

    #[test]
    fn redacted_internal_detail_hides_camel_case_keys_and_multiword_values() {
        let detail =
            "oauth secretKey=abc123 accessKey: xyz privateKey=q7 client_secret: ab cd ef failed";
        let redacted = redact_sensitive_detail(detail);
        for leaked in [
            "abc123",
            "xyz",
            "q7",
            "secretKey",
            "accessKey",
            "privateKey",
        ] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
        assert!(redacted.contains("<redacted>"), "{redacted}");
    }

    #[test]
    fn redacted_internal_detail_hides_oauth_keys_even_with_short_values() {
        // 回帰ガード (R2): OAuth 系のキーは完全一致でなくてもマスクし、
        // 値が短くてもキー語と後続値を <redacted> に置換する。
        let detail = concat!(
            "oauth client_secret=abcdef access_token: ghijkl ",
            "api-key=mnopqr refresh_token=stuvwx id_token=yzabcd client-secret=efghij"
        );
        let redacted = redact_sensitive_detail(detail);
        for leaked in [
            "abcdef",
            "ghijkl",
            "mnopqr",
            "stuvwx",
            "yzabcd",
            "efghij",
            "client_secret",
            "access_token",
            "api-key",
            "refresh_token",
            "id_token",
        ] {
            assert!(!redacted.contains(leaked), "leaked {leaked}: {redacted}");
        }
        assert!(redacted.contains("<redacted>"), "{redacted}");
        // 無害な語はトレース可能性のため残る。
        assert!(redacted.contains("oauth"), "{redacted}");

        // キーと値が別語でも、値が 2 文字でも伏せる。
        let short = redact_sensitive_detail("client_secret: q7 failed");
        assert!(!short.contains("q7"), "{short}");
        assert!(!short.contains("client_secret"), "{short}");
        assert!(short.ends_with("failed"), "{short}");
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
        assert_eq!(
            body["error"], "unauthorized",
            "legacy clients read error as the string \"unauthorized\""
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
        assert_eq!(body["error"], "unauthorized");
        assert_eq!(body["message"], "Invalid or missing authentication token");
    }
}
