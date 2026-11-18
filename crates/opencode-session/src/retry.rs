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
            if let Some(retry_after_ms) = headers.get("retry-after-ms") {
                if let Ok(parsed_ms) = retry_after_ms.parse::<f64>() {
                    if let Some(delay_ms) = clamp_retry_after_ms(parsed_ms) {
                        return delay_ms;
                    }
                }
            }

            if let Some(retry_after) = headers.get("retry-after") {
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

            return saturating_backoff(RETRY_INITIAL_DELAY, RETRY_BACKOFF_FACTOR, exponent);
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

            if let Some(body) = response_body {
                if body.contains("FreeUsageLimitError") {
                    return Some(
                        "Free usage exceeded, add credits https://opencode.ai/zen".to_string(),
                    );
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

        // Headers without a retry-after hint use the uncapped branch.
        let mut headers = HashMap::new();
        headers.insert("x-request-id".to_owned(), "abc".to_owned());
        assert_eq!(delay(u32::MAX, Some(&info(Some(headers)))), u64::MAX);
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
        let error = crate::MessageError::ApiError {
            message: "limit".to_owned(),
            status_code: None,
            is_retryable: true,
            response_headers: None,
            response_body: Some("FreeUsageLimitError".to_owned()),
            metadata: None,
        };
        assert_eq!(
            retryable(&error).as_deref(),
            Some("Free usage exceeded, add credits https://opencode.ai/zen")
        );
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
