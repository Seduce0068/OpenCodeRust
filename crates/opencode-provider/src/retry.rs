use std::collections::HashMap;
use tokio::sync::watch;

// ---------------------------------------------------------------------------
// Constants (ported from SessionRetry namespace)
// ---------------------------------------------------------------------------

pub const RETRY_INITIAL_DELAY: u64 = 2000;
pub const RETRY_BACKOFF_FACTOR: u64 = 2;
pub const RETRY_MAX_DELAY_NO_HEADERS: u64 = 30_000;
pub const RETRY_MAX_DELAY: u64 = 2_147_483_647;

// ---------------------------------------------------------------------------
// IsRetryable trait
// ---------------------------------------------------------------------------

/// Implement this on error types to expose whether an error is transient.
/// Return `Some(message)` when the error is retryable, `None` otherwise.
pub trait IsRetryable {
    fn is_retryable(&self) -> Option<String>;
}
// ---------------------------------------------------------------------------
// sleep – cancellable async sleep
// ---------------------------------------------------------------------------

/// Sleep for `ms` milliseconds, returning early if the `cancel` receiver
/// signals `true`.
pub async fn sleep(ms: u64, mut cancel: watch::Receiver<bool>) {
    let capped = ms.min(RETRY_MAX_DELAY);
    let duration = std::time::Duration::from_millis(capped);

    tokio::select! {
        _ = tokio::time::sleep(duration) => {}
        _ = async {
            while !*cancel.borrow_and_update() {
                if cancel.changed().await.is_err() {
                    // Sender dropped – treat as cancellation.
                    return;
                }
            }
        } => {}
    }
}

// ---------------------------------------------------------------------------
// delay – compute how long to wait before the next retry
// ---------------------------------------------------------------------------

/// Calculate the retry delay in milliseconds.
///
/// Priority:
/// 1. `retry-after-ms` header (milliseconds)
/// 2. `retry-after` header (seconds as float, or HTTP-date)
/// 3. Exponential backoff: `initial_delay * backoff_factor^(attempt-1)`
///
/// When headers are present the delay is uncapped (up to `RETRY_MAX_DELAY`).
/// Without headers the delay is capped at `RETRY_MAX_DELAY_NO_HEADERS`.
pub fn delay(attempt: u32, response_headers: Option<&HashMap<String, String>>) -> u64 {
    if let Some(headers) = response_headers {
        // 1. retry-after-ms
        if let Some(val) = headers.get("retry-after-ms") {
            if let Ok(ms) = val.parse::<f64>() {
                if !ms.is_nan() {
                    return ms as u64;
                }
            }
        }

        // 2. retry-after (seconds or HTTP-date)
        if let Some(val) = headers.get("retry-after") {
            // Try as seconds first
            if let Ok(secs) = val.parse::<f64>() {
                if !secs.is_nan() {
                    return (secs * 1000.0).ceil() as u64;
                }
            }
            // Try as HTTP-date via chrono
            if let Ok(date) = chrono::DateTime::parse_from_rfc2822(val) {
                let now = chrono::Utc::now();
                let diff_ms = (date.signed_duration_since(now)).num_milliseconds();
                if diff_ms > 0 {
                    return diff_ms as u64;
                }
            }
        }

        // Headers present but no usable retry-after – uncapped backoff
        return exponential_backoff(
            attempt,
            RETRY_INITIAL_DELAY,
            RETRY_BACKOFF_FACTOR,
            RETRY_MAX_DELAY,
        );
    }

    // No headers at all – capped backoff
    exponential_backoff(
        attempt,
        RETRY_INITIAL_DELAY,
        RETRY_BACKOFF_FACTOR,
        RETRY_MAX_DELAY_NO_HEADERS,
    )
}

fn exponential_backoff(attempt: u32, initial: u64, factor: u64, max: u64) -> u64 {
    let exp = factor.saturating_pow(attempt.saturating_sub(1));
    initial.saturating_mul(exp).min(max)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff_no_headers() {
        // Without headers the delay is capped at 30 000 ms.
        assert_eq!(delay(1, None), 2000); // 2000 * 2^0
        assert_eq!(delay(2, None), 4000); // 2000 * 2^1
        assert_eq!(delay(3, None), 8000); // 2000 * 2^2
        assert_eq!(delay(4, None), 16000); // 2000 * 2^3
        assert_eq!(delay(5, None), 30000); // capped
    }

    #[test]
    fn test_retry_after_ms_header() {
        let mut headers = HashMap::new();
        headers.insert("retry-after-ms".to_string(), "1234".to_string());
        assert_eq!(delay(1, Some(&headers)), 1234);
    }

    #[test]
    fn test_retry_after_seconds_header() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "3.5".to_string());
        assert_eq!(delay(1, Some(&headers)), 3500);
    }

    #[test]
    fn test_headers_present_but_no_retry_after() {
        let headers = HashMap::new();
        // Empty headers → uncapped backoff
        assert_eq!(delay(1, Some(&headers)), 2000);
        assert_eq!(delay(5, Some(&headers)), 32000); // 2000 * 2^4, under 2B cap
    }
}
