//! The PKCE authorization-code login for the Codex endpoint.
//!
//! Contract reference: the open-source Codex CLI login flow. The
//! authorization server accepts the loopback redirect only on the
//! registered ports, so a login binds one of [`REDIRECT_PORTS`] —
//! never an ephemeral port. The token exchange is form-encoded; the
//! refresh flow in [`crate::auth`] posts JSON. Both shapes copy the
//! reference client.
//!
//! This module owns the wire contract: challenge, URLs, parsing, and
//! the exchange. The interactive flow — listener, browser, prompts —
//! lives in the bin crate.

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ProviderError;
use crate::auth::{CLIENT_ID, CodexCredentials, jwt_account_id};

/// Default authorization endpoint.
pub const DEFAULT_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";

/// The loopback ports the authorization server accepts redirects on.
/// The login binds the first free one.
pub const REDIRECT_PORTS: &[u16] = &[1455, 1457];

/// The path of the loopback redirect.
pub const CALLBACK_PATH: &str = "/auth/callback";

/// The scopes a login asks for. `offline_access` grants the refresh
/// token.
const SCOPE: &str = "openid profile email offline_access";

/// One prepared authorization request: endpoint, state, and PKCE pair.
/// Build it once per login; the state and the verifier tie the
/// callback and the exchange to this request.
#[derive(Debug)]
pub struct AuthorizeRequest {
    endpoint: String,
    client_id: String,
    port: u16,
    state: String,
    verifier: String,
    challenge: String,
}

impl AuthorizeRequest {
    /// A request against the default endpoint, redirecting to `port`
    /// on the loopback interface.
    pub fn new(port: u16) -> Result<Self, ProviderError> {
        Self::with_endpoint(DEFAULT_AUTHORIZE_URL.to_string(), port)
    }

    /// A request against a custom authorization endpoint. Tests use
    /// it; the default flow never does.
    pub fn with_endpoint(endpoint: String, port: u16) -> Result<Self, ProviderError> {
        let verifier = base64url(&random_bytes::<64>()?);
        let challenge = challenge_s256(&verifier);
        Ok(Self {
            endpoint,
            client_id: CLIENT_ID.to_string(),
            port,
            state: base64url(&random_bytes::<32>()?),
            verifier,
            challenge,
        })
    }

    /// The loopback port this request redirects to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The registered redirect target. The host is `localhost` per the
    /// reference client; the listener binds `127.0.0.1`.
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}{CALLBACK_PATH}", self.port)
    }

    /// The full authorization URL the browser opens.
    pub fn url(&self) -> String {
        format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}\
             &code_challenge={}&code_challenge_method=S256\
             &id_token_add_organizations=true&codex_cli_simplified_flow=true\
             &state={}&originator=codex_cli_rs",
            self.endpoint,
            self.client_id,
            percent_encode(&self.redirect_uri()),
            percent_encode(SCOPE),
            self.challenge,
            self.state,
        )
    }

    /// Extract the authorization code from a redirect. `target`
    /// accepts the full redirect URL or just its path and query, so
    /// the loopback request line and a pasted URL parse the same way.
    /// The state must match this request.
    pub fn parse_callback(&self, target: &str) -> Result<String, ProviderError> {
        let query = target
            .split_once('?')
            .map(|(_, query)| query)
            .ok_or_else(|| ProviderError::Auth("the redirect carries no query".to_string()))?;
        let mut code = None;
        let mut state = None;
        let mut error = None;
        let mut description = None;
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value);
            match key {
                "code" => code = Some(value),
                "state" => state = Some(value),
                "error" => error = Some(value),
                "error_description" => description = Some(value),
                _ => {}
            }
        }
        if let Some(error) = error {
            let detail = description.unwrap_or_default();
            let sep = if detail.is_empty() { "" } else { ": " };
            return Err(ProviderError::Auth(format!(
                "the authorization server returned `{error}`{sep}{detail}"
            )));
        }
        let code = code
            .filter(|code| !code.is_empty())
            .ok_or_else(|| ProviderError::Auth("the redirect carries no code".to_string()))?;
        if state.as_deref() != Some(self.state.as_str()) {
            return Err(ProviderError::Auth(
                "the redirect state does not match this login".to_string(),
            ));
        }
        Ok(code)
    }

    /// Exchange the authorization code for tokens. Returns credentials
    /// ready for the auth store; the caller writes them.
    pub async fn exchange(
        &self,
        http: &reqwest::Client,
        token_url: &str,
        code: &str,
    ) -> Result<CodexCredentials, ProviderError> {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &self.redirect_uri()),
            ("client_id", &self.client_id),
            ("code_verifier", &self.verifier),
        ];
        let response = http
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| ProviderError::Network(format!("token exchange: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Auth(format!(
                "token exchange failed with status {status}"
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Protocol(format!("token exchange body: {e}")))?;
        let (Some(access), Some(refresh)) = (
            body["access_token"].as_str(),
            body["refresh_token"].as_str(),
        ) else {
            return Err(ProviderError::Protocol(
                "token exchange reply misses a token".to_string(),
            ));
        };
        let id_token = body["id_token"].as_str().map(str::to_string);
        Ok(CodexCredentials {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            account_id: id_token.as_deref().and_then(jwt_account_id),
            id_token,
            last_refresh: Some(humantime::format_rfc3339(std::time::SystemTime::now()).to_string()),
            extra: serde_json::Map::new(),
        })
    }
}

/// The S256 code challenge for a verifier.
fn challenge_s256(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn random_bytes<const N: usize>() -> Result<[u8; N], ProviderError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes)
        .map_err(|e| ProviderError::Auth(format!("cannot draw login randomness: {e}")))?;
    Ok(bytes)
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Percent-encode everything outside the URL unreserved set.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decode percent escapes and `+` in a query value. Malformed escapes
/// pass through untouched: the input comes from a browser or a paste.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use demerzel_mock::MockServer;
    use serde_json::json;

    fn request() -> AuthorizeRequest {
        AuthorizeRequest::with_endpoint("http://auth.local/oauth/authorize".to_string(), 1455)
            .unwrap()
    }

    #[test]
    fn the_challenge_matches_the_rfc_7636_vector() {
        assert_eq!(
            challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_and_state_are_url_safe_and_fresh() {
        let a = request();
        let b = request();
        assert_eq!(a.verifier.len(), 86);
        assert_eq!(a.state.len(), 43);
        let url_safe = |text: &str| {
            text.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        assert!(url_safe(&a.verifier) && url_safe(&a.state));
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn the_authorize_url_carries_the_contract_parameters() {
        let request = request();
        let url = request.url();
        assert!(url.starts_with("http://auth.local/oauth/authorize?"));
        assert!(url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
        assert!(url.contains(&format!("code_challenge={}", request.challenge)));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains(&format!("state={}", request.state)));
    }

    #[test]
    fn parse_callback_accepts_a_full_url_or_a_path() {
        let request = request();
        let query = format!("code=abc-123&state={}", request.state);
        let full = format!("http://localhost:1455/auth/callback?{query}");
        assert_eq!(request.parse_callback(&full).unwrap(), "abc-123");
        let path = format!("/auth/callback?{query}");
        assert_eq!(request.parse_callback(&path).unwrap(), "abc-123");
    }

    #[test]
    fn parse_callback_rejects_a_foreign_state() {
        let request = request();
        let error = request
            .parse_callback("/auth/callback?code=abc&state=wrong")
            .unwrap_err();
        assert!(error.to_string().contains("state"));
    }

    #[test]
    fn parse_callback_reports_the_server_error() {
        let request = request();
        let error = request
            .parse_callback(
                "/auth/callback?error=access_denied&error_description=the+user%20said%20no",
            )
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("access_denied"), "{text}");
        assert!(text.contains("the user said no"), "{text}");
    }

    #[test]
    fn parse_callback_needs_a_query_and_a_code() {
        let request = request();
        assert!(request.parse_callback("/auth/callback").is_err());
        let no_code = format!("/auth/callback?state={}", request.state);
        assert!(request.parse_callback(&no_code).is_err());
    }

    #[test]
    fn percent_decoding_handles_escapes_and_malformed_input() {
        assert_eq!(percent_decode("a%2Bb+c"), "a+b c");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
    }

    fn fake_id_token() -> String {
        let engine = &base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let claims = json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_9"},
        });
        format!(
            "{}.{}.{}",
            engine.encode(r#"{"alg":"none"}"#),
            engine.encode(claims.to_string()),
            engine.encode("sig"),
        )
    }

    #[tokio::test]
    async fn exchange_posts_the_form_and_builds_credentials() {
        let server = MockServer::start(
            Vec::new(),
            vec![json!({
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "id_token": fake_id_token(),
            })],
        )
        .await;
        let request = request();
        let creds = request
            .exchange(
                &reqwest::Client::new(),
                &format!("{}/oauth/token", server.url()),
                "code-9",
            )
            .await
            .unwrap();
        assert_eq!(creds.access_token, "at-1");
        assert_eq!(creds.refresh_token, "rt-1");
        assert_eq!(creds.account_id.as_deref(), Some("acct_9"));
        assert!(creds.last_refresh.is_some());

        let received = server.received();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].path, "/oauth/token");
        assert_eq!(received[0].body["grant_type"], "authorization_code");
        assert_eq!(received[0].body["code"], "code-9");
        assert_eq!(received[0].body["code_verifier"], json!(request.verifier));
        assert_eq!(
            received[0].body["redirect_uri"],
            "http://localhost:1455/auth/callback"
        );
        assert_eq!(received[0].body["client_id"], CLIENT_ID);
    }

    #[tokio::test]
    async fn exchange_reports_a_failed_status() {
        // An empty token script replies 410 to the exchange.
        let server = MockServer::start(Vec::new(), Vec::new()).await;
        let request = request();
        let error = request
            .exchange(
                &reqwest::Client::new(),
                &format!("{}/oauth/token", server.url()),
                "code",
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("token exchange"));
    }

    #[tokio::test]
    async fn exchange_rejects_a_reply_without_both_tokens() {
        let server = MockServer::start(Vec::new(), vec![json!({"access_token": "at-only"})]).await;
        let request = request();
        let error = request
            .exchange(
                &reqwest::Client::new(),
                &format!("{}/oauth/token", server.url()),
                "code",
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("token"));
    }
}
