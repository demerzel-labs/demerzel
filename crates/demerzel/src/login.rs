//! `dmz login`: interactive credential setup.
//!
//! Two forms. `dmz login` runs the PKCE flow for the ChatGPT
//! subscription: a loopback listener takes the browser redirect and
//! the token exchange writes the auth store. `dmz login anthropic`
//! prompts for an API key with echo off, verifies it with one cheap
//! call, and stores it. The wire contract lives in
//! `demerzel_providers::oauth`; this module owns the interaction.
//!
//! The loopback listener hand-rolls its HTTP reply, so the flow needs
//! no HTTP-server dependency. It serves one browser redirect; every
//! other request gets a 404 and the wait continues.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use demerzel_providers::auth::{self, AuthStore};
use demerzel_providers::{anthropic, oauth};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::RunError;

/// Everything a login run needs. `main` fills it with the defaults;
/// tests point the endpoints at a mock and the store at a temp dir.
#[derive(Debug, Clone)]
pub struct Options {
    /// The dmz auth store file the login writes.
    pub store_path: PathBuf,
    pub authorize_endpoint: String,
    pub token_url: String,
    /// Loopback ports to try in order. Port 0 binds an ephemeral
    /// port; only tests use that, the authorization server accepts
    /// redirects on the registered ports alone.
    pub redirect_ports: Vec<u16>,
    pub open_browser: bool,
    /// Endpoint for the API-key check.
    pub anthropic_base_url: String,
}

impl Options {
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            authorize_endpoint: oauth::DEFAULT_AUTHORIZE_URL.to_string(),
            token_url: auth::DEFAULT_TOKEN_URL.to_string(),
            redirect_ports: oauth::REDIRECT_PORTS.to_vec(),
            open_browser: true,
            anthropic_base_url: anthropic::DEFAULT_BASE_URL.to_string(),
        }
    }
}

/// Run one login. `provider` is the subcommand argument: `None` for
/// the ChatGPT subscription, `anthropic` for the API key.
pub async fn run(provider: Option<&str>, options: &Options) -> Result<(), RunError> {
    match provider {
        None => codex(options).await,
        Some(anthropic::PROVIDER_ID) => anthropic_login(options).await,
        Some(other) => Err(RunError::Config(format!(
            "unknown login form `{other}` — run `dmz login` for the ChatGPT \
             subscription, or `dmz login anthropic` for an API key"
        ))),
    }
}

/// The PKCE flow, split into steps so tests can drive the listener
/// with their own fake browser.
pub struct CodexFlow {
    request: oauth::AuthorizeRequest,
    listener: Option<TcpListener>,
}

impl CodexFlow {
    /// Bind the first free redirect port and prepare the request.
    /// No free port means paste-only mode.
    pub async fn start(options: &Options) -> Result<Self, RunError> {
        let mut listener = None;
        for port in &options.redirect_ports {
            if let Ok(bound) = TcpListener::bind(("127.0.0.1", *port)).await {
                listener = Some(bound);
                break;
            }
        }
        let port = match &listener {
            Some(bound) => bound.local_addr()?.port(),
            None => oauth::REDIRECT_PORTS[0],
        };
        let request =
            oauth::AuthorizeRequest::with_endpoint(options.authorize_endpoint.clone(), port)
                .map_err(RunError::Auth)?;
        Ok(Self { request, listener })
    }

    /// The URL the browser opens.
    pub fn authorize_url(&self) -> String {
        self.request.url()
    }

    /// Whether a loopback listener is up.
    pub fn listening(&self) -> bool {
        self.listener.is_some()
    }

    /// The port the redirect lands on.
    pub fn port(&self) -> u16 {
        self.request.port()
    }

    /// Wait for the browser redirect and return the authorization
    /// code. Requests to other paths get a 404 and the wait goes on;
    /// the first callback request — success or error — ends it.
    pub async fn wait_for_callback(&mut self) -> Result<String, RunError> {
        let listener = self
            .listener
            .as_ref()
            .expect("the caller checked listening()");
        loop {
            let (mut stream, _) = listener.accept().await?;
            if let Some(result) = serve_connection(&mut stream, &self.request).await {
                return result;
            }
        }
    }

    /// Parse a pasted redirect URL into the authorization code.
    pub fn parse_pasted(&self, line: &str) -> Result<String, RunError> {
        self.request
            .parse_callback(line.trim())
            .map_err(RunError::Auth)
    }

    /// Exchange the code and write the credentials to the store.
    pub async fn finish(
        self,
        options: &Options,
        http: &reqwest::Client,
        code: &str,
    ) -> Result<(), RunError> {
        let creds = self
            .request
            .exchange(http, &options.token_url, code)
            .await
            .map_err(RunError::Auth)?;
        let mut store =
            AuthStore::open(options.store_path.clone(), None).map_err(RunError::Auth)?;
        store.set_codex_credentials(creds).map_err(RunError::Auth)?;
        Ok(())
    }
}

async fn codex(options: &Options) -> Result<(), RunError> {
    let mut flow = CodexFlow::start(options).await?;
    let url = flow.authorize_url();
    println!("Open this URL to log in:\n\n  {url}\n");
    if options.open_browser {
        open_browser(&url);
    }
    let code = if flow.listening() {
        println!("Waiting for the browser on 127.0.0.1:{} ...", flow.port());
        flow.wait_for_callback().await?
    } else {
        println!(
            "No redirect port is free. Complete the login in the browser, \
             then paste the redirect URL here."
        );
        let line = read_stdin_line("Redirect URL: ").await?;
        flow.parse_pasted(&line)?
    };
    flow.finish(options, &http_client(), &code).await?;
    println!("Login complete. The credentials are in the dmz auth store.");
    Ok(())
}

async fn anthropic_login(options: &Options) -> Result<(), RunError> {
    let key = prompt_for_key("Anthropic API key: ")?;
    if key.is_empty() {
        return Err(RunError::Config("the key is empty".to_string()));
    }
    anthropic::verify_key(&http_client(), &options.anthropic_base_url, &key)
        .await
        .map_err(RunError::Auth)?;
    let mut store = AuthStore::open(options.store_path.clone(), None).map_err(RunError::Auth)?;
    store.set_anthropic_key(key).map_err(RunError::Auth)?;
    println!("Login complete. The key is in the dmz auth store.");
    Ok(())
}

/// Serve one connection. `Some` carries the flow's outcome; `None`
/// means the request was not the callback and the wait continues.
async fn serve_connection(
    stream: &mut TcpStream,
    request: &oauth::AuthorizeRequest,
) -> Option<Result<String, RunError>> {
    let target = read_request_target(stream).await?;
    if !target.starts_with(oauth::CALLBACK_PATH) {
        respond(stream, "404 Not Found", "<p>Not the login callback.</p>").await;
        return None;
    }
    match request.parse_callback(&target) {
        Ok(code) => {
            respond(
                stream,
                "200 OK",
                "<p>Login complete. Close this tab and return to the terminal.</p>",
            )
            .await;
            Some(Ok(code))
        }
        Err(error) => {
            let body = format!("<p>Login failed: {}</p>", escape_html(&error.to_string()));
            respond(stream, "200 OK", &body).await;
            Some(Err(RunError::Auth(error)))
        }
    }
}

/// Read the target out of an HTTP request line. `None` for anything
/// that does not deliver one line within the limits.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = [0u8; 8192];
    let mut len = 0;
    loop {
        if len == buf.len() {
            return None;
        }
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf[len..]))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            break;
        }
        len += read;
        if buf[..len].windows(2).any(|pair| pair == b"\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf[..len]);
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    parts.next().map(str::to_string)
}

/// One hand-rolled HTTP reply. Write errors are fine: the token
/// exchange, not the browser tab, decides the outcome.
async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let page = format!("<!doctype html><html><body>{body}</body></html>");
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{page}",
        page.len(),
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// Open the URL in the default browser. Failure is fine: the URL is
/// already printed.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build the login http client")
}

/// Prompt on stdout and read one stdin line off the runtime thread,
/// so Ctrl-C stays responsive while the user types.
async fn read_stdin_line(prompt: &str) -> Result<String, RunError> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map(|_| line)
    })
    .await
    .map_err(|error| RunError::Config(format!("stdin reader: {error}")))??;
    Ok(line)
}

/// Prompt for the API key with echo off. A piped stdin — no
/// terminal — reads plainly, which keeps tests and scripts working.
fn prompt_for_key(prompt: &str) -> Result<String, RunError> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let line = read_line_echo_off()?;
    Ok(line.trim().to_string())
}

#[cfg(unix)]
fn read_line_echo_off() -> Result<String, std::io::Error> {
    /// Restore the saved terminal state on every exit path.
    struct Restore(Option<libc::termios>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(saved) = &self.0 {
                unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved) };
            }
        }
    }

    let saved = unsafe {
        if libc::isatty(libc::STDIN_FILENO) == 1 {
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term) == 0 {
                let saved = term;
                term.c_lflag &= !libc::ECHO;
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term);
                Some(saved)
            } else {
                None
            }
        } else {
            None
        }
    };
    let echo_off = saved.is_some();
    let _restore = Restore(saved);
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if echo_off {
        // The user's Enter did not echo; end the prompt line.
        println!();
    }
    Ok(line)
}

#[cfg(not(unix))]
fn read_line_echo_off() -> Result<String, std::io::Error> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}
