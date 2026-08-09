//! Backoff helpers shared by the adapters.

use std::time::Duration;

/// Exponential backoff: `base`, `2*base`, `4*base`, ...
pub(crate) fn backoff(base: Duration, attempt: u32) -> Duration {
    base * 2u32.saturating_pow(attempt.saturating_sub(1))
}

/// The `retry-after` header as a duration, when the server sent one.
pub(crate) fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}
