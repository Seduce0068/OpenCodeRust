use std::time::Duration;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

pub const RETRY_INITIAL_DELAY: u64 = 2000;
pub const RETRY_BACKOFF_FACTOR: u64 = 2;
pub const RETRY_MAX_DELAY_NO_HEADERS: u64 = 30_000;
pub const RETRY_MAX_DELAY: u64 = 2_147_483_647;

#[derive(Debug, thiserror::Error)]
#[error("Sleep cancelled")]
pub struct SleepCancelled;

pub async fn sleep_with_cancel(ms: u64, cancel: CancellationToken) -> Result<(), SleepCancelled> {
    let duration = Duration::from_millis(ms.min(RETRY_MAX_DELAY));

    tokio::select! {
        _ = sleep(duration) => Ok(()),
        _ = cancel.cancelled() => Err(SleepCancelled),
    }
}

/// Compute `base * factor^exponent` using saturating arithmetic.
///
/// `u64::pow` and plain `*` panic on overflow in debug builds (and silently
/// wrap in release builds). A large retry attempt must do neither — the value
/// is eventually clamped by the caller or by [`sleep_with_cancel`].
fn saturating_backoff(base: u64, factor: u64, exponent: u32) -> u64 {
    let power = factor.checked_pow(exponent).unwrap_or(u64::MAX);
    base.saturating_mul(power)
}

/// Convert a parsed `retry-after` value expressed in milliseconds into a
/// bounded delay.
///
/// Returns `None` for negative, zero, NaN, or infinite values so the caller
/// falls back to exponential backoff. Rust's `f64 -> u64` casts saturate
/// (negative/NaN become `0`, infinity becomes `u64::MAX`), so validation must
/// happen *before* the cast. Clamping is performed in `f64` space, which also
/// covers `parsed_seconds * 1000.0` overflowing to infinity.
fn clamp_retry_after_ms(ms: f64) -> Option<u64> {
    if !ms.is_finite() || ms <= 0.0 {
        return None;
    }
    let bounded = ms.min(RETRY_MAX_DELAY as f64);
    Some(bounded.ceil() as u64)
}

pub fn delay(attempt: u32, error: Option<&ApiErrorInfo>) -> u64 {
    // `attempt` is 1-based at every call site; saturating_sub also keeps this
    // safe if a future caller passes 0.
    let exponent = attempt.saturating_sub(1);

    if let Some(err) = error {
        if let Some(ref headers) = err.response_headers {
            // HTTP ヘッダ名は大小文字非依存。provider.rs の
            // parse_retry_after_seconds と同じ eq_ignore_ascii_case で探す。
            // 完全一致（小文字）の既存挙動も変わらない。
            let retry_after_ms = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("retry-after-ms"))
                .map(|(_, value)| value);
            if let Some(retry_after_ms) = retry_after_ms {
                if let Ok(parsed_ms) = retry_after_ms.parse::<f64>() {
                    if let Some(delay_ms) = clamp_retry_after_ms(parsed_ms) {
                        return delay_ms;
                    }
                }
            }

            let retry_after = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                .map(|(_, value)| value);
            if let Some(retry_after) = retry_after {
                if let Ok(parsed_seconds) = retry_after.parse::<f64>() {
                    if let Some(delay_ms) = clamp_retry_after_ms(parsed_seconds * 1000.0) {
                        return delay_ms;
                    }
                }

                if let Ok(parsed_date) = chrono::DateTime::parse_from_rfc2822(retry_after) {
                    let now = chrono::Utc::now();
                    let diff = parsed_date.with_timezone(&chrono::Utc) - now;
                    if let Some(delay_ms) = clamp_retry_after_ms(diff.num_milliseconds() as f64) {
                        return delay_ms;
                    }
                }
            }

            // Finding 6: ヘッダはあるが Retry-After を解釈できなかった場合も、
            // ヘッダ無しと同じ 30 秒上限を適用する（従来は u64::MAX まで伸びていた）。
            return saturating_backoff(RETRY_INITIAL_DELAY, RETRY_BACKOFF_FACTOR, exponent)
                .min(RETRY_MAX_DELAY_NO_HEADERS);
        }
    }

    let calculated = saturating_backoff(RETRY_INITIAL_DELAY, RETRY_BACKOFF_FACTOR, exponent);
    calculated.min(RETRY_MAX_DELAY_NO_HEADERS)
}

#[derive(Debug, Clone)]
pub struct ApiErrorInfo {
    pub message: String,
    pub is_retryable: bool,
    pub response_headers: Option<std::collections::HashMap<String, String>>,
    pub response_body: Option<String>,
}

/// Detect the free-usage-limit error without a dedicated error variant.
///
/// The provider reports this as a plain string in `response_body`, often
/// embedded in a JSON error payload, so detection stays string-based. The
/// body is normalized to upper-case ASCII alphanumerics to tolerate case and
/// separator differences (`FreeUsageLimitError`, `free_usage_limit_error`,
/// `Free Usage Limit Error`, ...).
fn body_mentions_free_usage_limit(body: &str) -> bool {
    let normalized: String = body
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    normalized.contains("FREEUSAGELIMITERROR")
}

pub fn retryable(error: &crate::MessageError) -> Option<String> {
    match error {
        crate::MessageError::ContextOverflowError { .. } => None,
        crate::MessageError::ApiError {
            message,
            is_retryable,
            response_body,
            ..
        } => {
            if !is_retryable {
                return None;
            }

            // M-50: exhausted free credits cannot be restored by waiting.
            // Returning None makes the llm.rs retry loop skip the entire retry
            // branch (no backoff sleep, no additional attempts).
            if let Some(body) = response_body {
                if body_mentions_free_usage_limit(body) {
                    return None;
                }
            }

            if message.contains("Overloaded") {
                Some("Provider is overloaded".to_string())
            } else {
                Some(message.clone())
            }
        }
        crate::MessageError::Unknown { message } => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(message) {
                if let Some(obj) = json.as_object() {
                    let code = obj.get("code").and_then(|c| c.as_str()).unwrap_or("");

                    if obj.get("type").and_then(|t| t.as_str()) == Some("error") {
                        if let Some(error_obj) = obj.get("error").and_then(|e| e.as_object()) {
                            if error_obj.get("type").and_then(|t| t.as_str())
                                == Some("too_many_requests")
                            {
                                return Some("Too Many Requests".to_string());
                            }
                            if error_obj
                                .get("code")
                                .and_then(|c| c.as_str())
                                .map(|c| c.contains("rate_limit"))
                                .unwrap_or(false)
                            {
                                return Some("Rate Limited".to_string());
                            }
                        }
                    }

                    if code.contains("exhausted") || code.contains("unavailable") {
                        return Some("Provider is overloaded".to_string());
                    }

                    return Some(message.clone());
                }
            }
            None
        }
        _ => None,
    }
}

pub struct RetryState {
    pub attempt: u32,
    pub max_attempts: u32,
    pub last_error: Option<String>,
}

impl RetryState {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            attempt: 0,
            max_attempts,
            last_error: None,
        }
    }

    pub fn should_retry(&self) -> bool {
        self.attempt < self.max_attempts
    }

    pub fn increment(&mut self, error: String) {
        self.attempt += 1;
        self.last_error = Some(error);
    }

    pub fn next_delay(&self, error_info: Option<&ApiErrorInfo>) -> u64 {
        delay(self.attempt, error_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn info(headers: Option<HashMap<String, String>>) -> ApiErrorInfo {
        ApiErrorInfo {
            message: "error".to_owned(),
            is_retryable: true,
            response_headers: headers,
            response_body: None,
        }
    }

    #[test]
    fn delay_without_headers_uses_exponential_backoff() {
        assert_eq!(delay(1, None), 2_000);
        assert_eq!(delay(2, None), 4_000);
        assert_eq!(delay(3, None), 8_000);
    }

    #[test]
    fn delay_without_headers_is_capped() {
        assert_eq!(delay(10, None), RETRY_MAX_DELAY_NO_HEADERS);
    }

    #[test]
    fn delay_saturates_for_large_attempt() {
        // 2^70 would overflow u64; saturating arithmetic must not panic/wrap.
        assert_eq!(delay(u32::MAX, None), RETRY_MAX_DELAY_NO_HEADERS);

        // Finding 6: a header-bearing error without a usable Retry-After must
        // use the same 30s cap as the no-headers branch (previously u64::MAX).
        // Saturating arithmetic is still exercised: 2^u32::MAX saturates before
        // the cap is applied, so this must not panic or wrap.
        let mut headers = HashMap::new();
        headers.insert("x-request-id".to_owned(), "abc".to_owned());
        assert_eq!(
            delay(u32::MAX, Some(&info(Some(headers)))),
            RETRY_MAX_DELAY_NO_HEADERS
        );
    }

    #[test]
    fn delay_retry_after_ms_header_takes_precedence() {
        let mut headers = HashMap::new();
        headers.insert("retry-after-ms".to_owned(), "1500".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), 1_500);
    }

    #[test]
    fn delay_retry_after_seconds_header_is_converted() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_owned(), "2.5".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), 2_500);
    }

    #[test]
    fn delay_invalid_header_falls_back_to_backoff() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_owned(), "not-a-number".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), 2_000);
    }

    #[test]
    fn delay_retry_after_headers_are_case_insensitive() {
        // R2: "Retry-After: 5" を返す server でもバックオフが無視されないこと。
        for name in ["Retry-After", "RETRY-AFTER", "rEtRy-aFtEr"] {
            let mut headers = HashMap::new();
            headers.insert(name.to_owned(), "5".to_owned());
            assert_eq!(delay(1, Some(&info(Some(headers)))), 5_000, "name={name}");
        }
    }

    #[test]
    fn delay_retry_after_ms_headers_are_case_insensitive() {
        for name in ["Retry-After-Ms", "RETRY-AFTER-MS", "rEtRy-aFtEr-Ms"] {
            let mut headers = HashMap::new();
            headers.insert(name.to_owned(), "1500".to_owned());
            assert_eq!(delay(1, Some(&info(Some(headers)))), 1_500, "name={name}");
        }
    }

    #[test]
    fn delay_lowercase_headers_keep_existing_behavior() {
        // 小文字キーの既存挙動は不変。
        let mut headers = HashMap::new();
        headers.insert("retry-after-ms".to_owned(), "1500".to_owned());
        headers.insert("retry-after".to_owned(), "5".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), 1_500);
    }

    #[test]
    fn delay_retry_after_with_unrelated_headers_is_honored() {
        // (b) ヘッダが存在し、解釈可能な Retry-After があれば delay へ反映される。
        let mut headers = HashMap::new();
        headers.insert("x-request-id".to_owned(), "abc".to_owned());
        headers.insert("retry-after".to_owned(), "2".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), 2_000);
    }

    #[test]
    fn delay_unrelated_headers_only_never_exceeds_no_headers_cap() {
        // (c) 無関係ヘッダだけでは 30 秒を超えないこと。
        for attempt in [1, 5, 10, 20, u32::MAX] {
            let mut headers = HashMap::new();
            headers.insert("x-request-id".to_owned(), "abc".to_owned());
            headers.insert("content-type".to_owned(), "application/json".to_owned());
            let actual = delay(attempt, Some(&info(Some(headers))));
            assert!(
                actual <= RETRY_MAX_DELAY_NO_HEADERS,
                "attempt={attempt}, delay={actual}"
            );
        }
    }

    #[test]
    fn delay_retry_after_ms_at_max_is_exact() {
        let mut headers = HashMap::new();
        headers.insert("retry-after-ms".to_owned(), RETRY_MAX_DELAY.to_string());
        assert_eq!(delay(1, Some(&info(Some(headers)))), RETRY_MAX_DELAY);
    }

    #[test]
    fn delay_retry_after_ms_over_max_is_clamped() {
        let mut headers = HashMap::new();
        headers.insert(
            "retry-after-ms".to_owned(),
            (RETRY_MAX_DELAY + 1).to_string(),
        );
        assert_eq!(delay(1, Some(&info(Some(headers)))), RETRY_MAX_DELAY);
    }

    #[test]
    fn delay_retry_after_ms_huge_is_clamped() {
        let mut headers = HashMap::new();
        headers.insert("retry-after-ms".to_owned(), "1e18".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), RETRY_MAX_DELAY);
    }

    #[test]
    fn delay_retry_after_ms_non_positive_falls_back() {
        for value in ["-1", "0", "-0.5"] {
            let mut headers = HashMap::new();
            headers.insert("retry-after-ms".to_owned(), value.to_owned());
            assert_eq!(delay(1, Some(&info(Some(headers)))), 2_000, "value={value}");
        }
    }

    #[test]
    fn delay_retry_after_ms_non_finite_falls_back() {
        for value in ["NaN", "nan", "inf", "Infinity", "-inf"] {
            let mut headers = HashMap::new();
            headers.insert("retry-after-ms".to_owned(), value.to_owned());
            assert_eq!(delay(1, Some(&info(Some(headers)))), 2_000, "value={value}");
        }
    }

    #[test]
    fn delay_retry_after_seconds_is_clamped() {
        let mut headers = HashMap::new();
        // 1e9 seconds -> 1e12 ms, far above RETRY_MAX_DELAY.
        headers.insert("retry-after".to_owned(), "1e9".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), RETRY_MAX_DELAY);
    }

    #[test]
    fn delay_retry_after_seconds_overflow_to_inf_falls_back() {
        let mut headers = HashMap::new();
        // 1e308 * 1000.0 overflows f64 to infinity; must not become u64::MAX.
        headers.insert("retry-after".to_owned(), "1e308".to_owned());
        assert_eq!(delay(1, Some(&info(Some(headers)))), 2_000);
    }

    #[test]
    fn delay_retry_after_seconds_invalid_falls_back() {
        for value in ["-2.5", "NaN", "inf"] {
            let mut headers = HashMap::new();
            headers.insert("retry-after".to_owned(), value.to_owned());
            assert_eq!(delay(1, Some(&info(Some(headers)))), 2_000, "value={value}");
        }
    }

    #[test]
    fn retryable_context_overflow_returns_none() {
        let error = crate::MessageError::ContextOverflowError {
            message: "x".to_owned(),
            response_body: None,
        };
        assert!(retryable(&error).is_none());
    }

    #[test]
    fn retryable_non_retryable_api_error_returns_none() {
        let error = crate::MessageError::ApiError {
            message: "fail".to_owned(),
            status_code: None,
            is_retryable: false,
            response_headers: None,
            response_body: None,
            metadata: None,
        };
        assert!(retryable(&error).is_none());
    }

    #[test]
    fn retryable_free_usage_limit_returns_hint() {
        // M-50: the historical test name is kept for traceability, but the
        // expectation changed from a retry hint (Some) to non-retryable (None)
        // because waiting cannot restore exhausted free credits.
        let error = crate::MessageError::ApiError {
            message: "limit".to_owned(),
            status_code: None,
            is_retryable: true,
            response_headers: None,
            response_body: Some("FreeUsageLimitError".to_owned()),
            metadata: None,
        };
        assert!(retryable(&error).is_none());
    }

    #[test]
    fn retryable_free_usage_limit_detection_is_normalized() {
        // Case and separator variants must all be treated as non-retryable.
        for body in [
            "FreeUsageLimitError",
            "freeusagelimiterror",
            "FREEUSAGELIMITERROR",
            "free_usage_limit_error",
            "Free-Usage-Limit-Error",
            "Free Usage Limit Error",
        ] {
            let error = crate::MessageError::ApiError {
                message: "request failed".to_owned(),
                status_code: None,
                is_retryable: true,
                response_headers: None,
                response_body: Some(body.to_owned()),
                metadata: None,
            };
            assert!(retryable(&error).is_none(), "body={body}");
        }
    }

    #[test]
    fn retryable_free_usage_limit_inside_json_body_is_non_retryable() {
        // Real payloads embed the error name in JSON with surrounding text.
        let body = r#"{"type":"error","error":{"type":"FreeUsageLimitError"}}"#;
        let error = crate::MessageError::ApiError {
            message: "API call failed".to_owned(),
            status_code: None,
            is_retryable: true,
            response_headers: None,
            response_body: Some(body.to_owned()),
            metadata: None,
        };
        assert!(retryable(&error).is_none());
    }

    #[test]
    fn retryable_unrelated_body_keeps_retry_hint() {
        // Boundary: normalization must not swallow unrelated error bodies, and
        // the Overloaded classification must stay intact when a body is present.
        for body in ["", "{}", "daily request limit reached", "free tier"] {
            let error = crate::MessageError::ApiError {
                message: "Provider Overloaded now".to_owned(),
                status_code: None,
                is_retryable: true,
                response_headers: None,
                response_body: Some(body.to_owned()),
                metadata: None,
            };
            assert_eq!(
                retryable(&error).as_deref(),
                Some("Provider is overloaded"),
                "body={body}"
            );
        }
    }

    #[test]
    fn retryable_free_usage_marker_outside_body_stays_retryable() {
        // Boundary: M-50 scopes detection to response_body only; a marker in
        // `message` alone does not change the ApiError classification.
        let error = crate::MessageError::ApiError {
            message: "FreeUsageLimitError".to_owned(),
            status_code: None,
            is_retryable: true,
            response_headers: None,
            response_body: None,
            metadata: None,
        };
        assert_eq!(retryable(&error).as_deref(), Some("FreeUsageLimitError"));
    }

    #[test]
    fn retryable_free_usage_with_non_retryable_flag_returns_none() {
        // is_retryable=false short-circuits before body inspection; confirm the
        // new early return does not alter that existing behavior.
        let error = crate::MessageError::ApiError {
            message: "limit".to_owned(),
            status_code: None,
            is_retryable: false,
            response_headers: None,
            response_body: Some("FreeUsageLimitError".to_owned()),
            metadata: None,
        };
        assert!(retryable(&error).is_none());
    }

    #[test]
    fn retryable_overloaded_message_is_normalized() {
        let error = crate::MessageError::ApiError {
            message: "Provider Overloaded now".to_owned(),
            status_code: None,
            is_retryable: true,
            response_headers: None,
            response_body: None,
            metadata: None,
        };
        assert_eq!(retryable(&error).as_deref(), Some("Provider is overloaded"));
    }

    #[test]
    fn retryable_unknown_json_rate_limit() {
        let message = r#"{"type":"error","error":{"type":"too_many_requests"}}"#;
        let error = crate::MessageError::Unknown {
            message: message.to_owned(),
        };
        assert_eq!(retryable(&error).as_deref(), Some("Too Many Requests"));
    }

    /// provider 側の parse がリトライ可と判定したエラーが session 側の
    /// `retryable()` でも retry hint を返すことを固定する回帰テスト。
    /// source 由来の一時エラーも同じ `is_retryable` フラグで伝播する。
    #[test]
    fn retryable_propagates_provider_retryable_flag() {
        use opencode_provider::{parse_api_call_error, ParsedAPICallError, ProviderError};

        let parsed = parse_api_call_error("some-provider", &ProviderError::Timeout);
        let is_retryable = match parsed {
            ParsedAPICallError::ApiError { is_retryable, .. } => is_retryable,
            other => panic!("expected ApiError, got {other:?}"),
        };
        assert!(is_retryable, "provider parse 側でリトライ可であること");

        let error = crate::MessageError::ApiError {
            message: "API error: request failed".to_owned(),
            status_code: None,
            is_retryable,
            response_headers: None,
            response_body: None,
            metadata: None,
        };
        assert_eq!(
            retryable(&error).as_deref(),
            Some("API error: request failed"),
            "トークン値を増幅せず、元のメッセージだけを retry hint に使う"
        );
    }

    #[test]
    fn retry_state_tracks_attempts() {
        let mut state = RetryState::new(2);
        assert!(state.should_retry());
        state.increment("first".to_owned());
        assert!(state.should_retry());
        state.increment("second".to_owned());
        assert!(!state.should_retry());
        assert_eq!(state.last_error.as_deref(), Some("second"));
    }
}
