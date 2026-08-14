//! End-to-end login tests against the mock endpoints. Hermetic: the
//! browser is a plain HTTP request from the test, the token exchange
//! hits the mock server, the store lands in a temp dir.

use std::path::Path;

use demerzel::login::{CodexFlow, Options};
use demerzel_mock::MockServer;
use demerzel_providers::auth::AuthStore;
use serde_json::json;

fn options(dir: &Path, mock: &MockServer) -> Options {
    Options {
        store_path: dir.join("auth.json"),
        authorize_endpoint: format!("{}/oauth/authorize", mock.url()),
        token_url: format!("{}/oauth/token", mock.url()),
        redirect_ports: vec![0],
        open_browser: false,
        anthropic_base_url: mock.url(),
    }
}

/// One query-parameter value out of a URL. The login values are
/// URL-safe, so no decoding is needed.
fn param(url: &str, name: &str) -> String {
    let (_, query) = url.split_once('?').expect("the URL has a query");
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no `{name}` in {url}"))
        .to_string()
}

#[tokio::test]
async fn the_loopback_flow_stores_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(
        Vec::new(),
        vec![json!({"access_token": "at-1", "refresh_token": "rt-1"})],
    )
    .await;
    let options = options(dir.path(), &mock);

    let mut flow = CodexFlow::start(&options).await.unwrap();
    assert!(flow.listening());
    let url = flow.authorize_url();
    let state = param(&url, "state");
    let port = flow.port();

    // The fake browser: a stray request first — the wait must ride
    // over it — then the real redirect.
    let browser = tokio::spawn(async move {
        let http = reqwest::Client::new();
        let stray = http
            .get(format!("http://127.0.0.1:{port}/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(stray.status().as_u16(), 404);
        let reply = http
            .get(format!(
                "http://127.0.0.1:{port}/auth/callback?code=code-1&state={state}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status().as_u16(), 200);
    });

    let code = flow.wait_for_callback().await.unwrap();
    assert_eq!(code, "code-1");
    browser.await.unwrap();
    flow.finish(&options, &reqwest::Client::new(), &code)
        .await
        .unwrap();

    let store = AuthStore::open(options.store_path.clone(), None).unwrap();
    let creds = store.credentials().unwrap();
    assert_eq!(creds.access_token, "at-1");
    assert_eq!(creds.refresh_token, "rt-1");
    assert!(creds.last_refresh.is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&options.store_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let received = mock.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].path, "/oauth/token");
    assert_eq!(received[0].body["grant_type"], "authorization_code");
    assert_eq!(received[0].body["code"], "code-1");
}

#[tokio::test]
async fn a_wrong_state_fails_the_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let options = options(dir.path(), &mock);

    let mut flow = CodexFlow::start(&options).await.unwrap();
    let port = flow.port();
    let browser = tokio::spawn(async move {
        let reply = reqwest::Client::new()
            .get(format!(
                "http://127.0.0.1:{port}/auth/callback?code=code-1&state=foreign"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status().as_u16(), 200);
        assert!(reply.text().await.unwrap().contains("Login failed"));
    });

    let error = flow.wait_for_callback().await.unwrap_err();
    assert!(error.to_string().contains("state"), "{error}");
    browser.await.unwrap();
    assert!(!options.store_path.exists());
}

#[tokio::test]
async fn paste_mode_parses_the_redirect_url() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut options = options(dir.path(), &mock);
    options.redirect_ports = Vec::new();

    let flow = CodexFlow::start(&options).await.unwrap();
    assert!(!flow.listening());
    let state = param(&flow.authorize_url(), "state");
    let pasted = format!("http://localhost:1455/auth/callback?code=code-7&state={state}\n");
    assert_eq!(flow.parse_pasted(&pasted).unwrap(), "code-7");
}

#[tokio::test]
async fn a_taken_port_falls_through_to_the_next() {
    let taken = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let taken_port = taken.local_addr().unwrap().port();

    let dir = tempfile::tempdir().unwrap();
    let mock = MockServer::start(Vec::new(), Vec::new()).await;
    let mut options = options(dir.path(), &mock);
    options.redirect_ports = vec![taken_port, 0];

    let flow = CodexFlow::start(&options).await.unwrap();
    assert!(flow.listening());
    assert_ne!(flow.port(), taken_port);
}
