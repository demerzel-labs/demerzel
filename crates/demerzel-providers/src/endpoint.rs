//! The request loop for adapters that serve a user-named endpoint with
//! an optional bearer key: `openai-compat` and `openai-responses`, and
//! the key check that `dmz login <instance>` runs for both.
//!
//! A key has no refresh flow, so a 401 fails at once and names the
//! instance. Status 429, a 5xx, and a network failure retry with
//! backoff before the stream opens; once it opens, nothing retries, so
//! no event goes out twice. A 200 reply that is not an event stream
//! fails with its content type, because a wrong `base_url` can answer
//! with a page.

use std::time::Duration;

use crate::ProviderError;
use crate::retry::{backoff, redact, request_id, retry_after, retry_delay};

/// What a key check found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyCheck {
    /// The models route accepted the key.
    Accepted,
    /// The check did not confirm the key: the route answered with
    /// another status, or the request failed. Many compatible servers
    /// do not serve the route. The text says what happened.
    Unconfirmed(String),
}

/// Check a key with `GET {base_url}/models`. A 2xx reply accepts it.
/// A 401 refuses it with an error, and so does a request that does not
/// build — a `base_url` that is no URL, or a key that is no header
/// value — because no session could send it either. Any other outcome
/// is [`KeyCheck::Unconfirmed`]. `instance` names the instance in the
/// text.
pub async fn verify_key(
    http: &reqwest::Client,
    instance: &str,
    base_url: &str,
    key: &str,
) -> Result<KeyCheck, ProviderError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let request = http.get(&url).bearer_auth(key).build().map_err(|error| {
        ProviderError::InvalidRequest(redact(
            &format!(
                "the key check of instance `{instance}` at `{url}` does not build: {error}; \
                 check its base_url and key"
            ),
            key,
        ))
    })?;
    let response = match http.execute(request).await {
        Ok(response) => response,
        Err(error) => {
            return Ok(KeyCheck::Unconfirmed(redact(
                &format!("GET {url} failed: {error}"),
                key,
            )));
        }
    };
    let status = response.status();
    if status.is_success() {
        return Ok(KeyCheck::Accepted);
    }
    if status.as_u16() == 401 {
        return Err(ProviderError::Auth(format!(
            "the endpoint of instance `{instance}` rejected the key (status 401 from GET {url})"
        )));
    }
    Ok(KeyCheck::Unconfirmed(format!(
        "GET {url} answered with status {status}"
    )))
}

/// One user-named endpoint of one instance.
pub(crate) struct KeyedEndpoint<'a> {
    pub http: &'a reqwest::Client,
    /// The instance name, for the error text.
    pub instance: &'a str,
    /// The full request URL.
    pub url: &'a str,
    /// The route under the base URL, for the trace.
    pub route: &'a str,
    /// The bearer key; `None` sends no `Authorization` header.
    pub key: Option<&'a str>,
    /// Total attempts for retryable failures.
    pub max_attempts: u32,
    pub base_delay: Duration,
}

impl KeyedEndpoint<'_> {
    /// POST `body` and return the response once it opens an event
    /// stream. The request is built once: each attempt shares the one
    /// copy of the body.
    pub(crate) async fn post(&self, body: Vec<u8>) -> Result<reqwest::Response, ProviderError> {
        let route = self.route;
        let instance = self.instance;
        let max_attempts = self.max_attempts;
        let secret = self.key.unwrap_or("");
        log::debug!("request POST {route} body_bytes={}", body.len());
        log::trace!("request body {}", String::from_utf8_lossy(&body));
        let mut builder = self
            .http
            .post(self.url)
            .header("accept", "text/event-stream")
            .header("content-type", "application/json")
            .body(body);
        if let Some(key) = self.key {
            builder = builder.bearer_auth(key);
        }
        // A URL that does not parse fails here, once: no retry mends it.
        let request = builder.build().map_err(|error| {
            ProviderError::Network(redact(
                &format!(
                    "the request of instance `{instance}` to `{}` does not build: {error}; \
                     check its base_url and key",
                    self.url
                ),
                secret,
            ))
        })?;
        let mut attempt: u32 = 0;
        loop {
            let attempt_request = request.try_clone().expect("a byte body clones");
            let response = match self.http.execute(attempt_request).await {
                Ok(response) => response,
                Err(error) => {
                    attempt += 1;
                    if attempt >= max_attempts {
                        log::error!(
                            "request failed after {attempt} of {max_attempts} attempts: \
                             instance={instance} network error: {error}"
                        );
                        return Err(ProviderError::Network(error.to_string()));
                    }
                    let delay = backoff(self.base_delay, attempt);
                    log::info!(
                        "retry in {}ms after network error: {error} (attempt {attempt} of \
                         {max_attempts}) instance={instance}",
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
            };
            let status = response.status();
            log::info!(
                "POST {route} attempt {} of {max_attempts}: status {} request-id={} \
                 instance={instance}",
                attempt + 1,
                status.as_u16(),
                request_id(&response)
            );
            if status.is_success() {
                // A wrong `base_url` can answer 200 with a page or a
                // whole JSON reply; the SSE parser would hold all of
                // it. Refuse it now and show its start.
                if let Some(kind) = not_event_stream(&response) {
                    let head = redact(&read_head(response).await, secret);
                    return Err(ProviderError::Protocol(format!(
                        "the endpoint of instance `{instance}` answered with {kind}, not an \
                         event stream; check its base_url: {head}"
                    )));
                }
                return Ok(response);
            }
            if status.as_u16() == 401 {
                return Err(ProviderError::Auth(format!(
                    "the endpoint of instance `{instance}` rejected the request (status 401): \
                     check the instance's key — its `api_key_env`, or `dmz login {instance}`"
                )));
            }
            if status.as_u16() == 429 || status.is_server_error() {
                attempt += 1;
                if attempt >= max_attempts {
                    log::error!(
                        "request failed after {attempt} of {max_attempts} attempts: status {} \
                         instance={instance}",
                        status.as_u16()
                    );
                    return Err(if status.as_u16() == 429 {
                        ProviderError::RateLimited {
                            retry_after: retry_after(&response),
                        }
                    } else {
                        ProviderError::Server(format!("status {status}"))
                    });
                }
                let delay = retry_delay(&response, self.base_delay, attempt);
                log::info!(
                    "retry in {}ms after status {} (attempt {attempt} of {max_attempts}) \
                     instance={instance}",
                    delay.as_millis(),
                    status.as_u16()
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            // The body is the server's text; it must not echo the key
            // into the record.
            let text = redact(&response.text().await.unwrap_or_default(), secret);
            return Err(ProviderError::InvalidRequest(format!(
                "status {status}: {text}"
            )));
        }
    }
}

/// The content type of a reply that is not an event stream; `None` for
/// an event stream, and for a reply without the header, which the
/// parser then judges.
fn not_event_stream(response: &reqwest::Response) -> Option<String> {
    let value = response.headers().get("content-type")?.to_str().ok()?;
    let media = value.split(';').next().unwrap_or("").trim();
    (!media.eq_ignore_ascii_case("text/event-stream")).then(|| media.to_string())
}

/// The first bytes of a reply body, at most 512, for an error text.
async fn read_head(mut response: reqwest::Response) -> String {
    const HEAD: usize = 512;
    let mut head = Vec::new();
    while head.len() < HEAD {
        match response.chunk().await {
            Ok(Some(bytes)) => head.extend_from_slice(&bytes),
            _ => break,
        }
    }
    head.truncate(HEAD);
    String::from_utf8_lossy(&head).into_owned()
}
