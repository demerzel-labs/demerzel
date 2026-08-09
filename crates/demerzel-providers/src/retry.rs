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

/// The wait before the next attempt: the server's `retry-after` when
/// it sent one, else exponential backoff. The cap keeps a bad header
/// from parking a turn for hours; the sleep stays abortable either
/// way.
pub(crate) fn retry_delay(response: &reqwest::Response, base: Duration, attempt: u32) -> Duration {
    const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
    match retry_after(response) {
        Some(delay) => delay.min(MAX_RETRY_AFTER),
        None => backoff(base, attempt),
    }
}
