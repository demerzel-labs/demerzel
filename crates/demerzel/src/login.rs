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
//! no HTTP-server dependency. Each connection is served on its own
//! task inside one time budget, so a stalled client cannot block the
//! real redirect. Only a request that carries this login's state ends
//! the wait; everything else gets a 404 and the wait continues, so a
//! stray or hostile local request cannot end the login.
//!
//! [`CodexFlow`] is the tested seam: the end-to-end tests drive the
//! listener with their own fake browser through it. Keep it public.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use demerzel_providers::auth::{self, AuthStore};
use demerzel_providers::{ProviderError, anthropic, oauth, openai_codex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::RunError;

/// The whole time budget for one loopback connection: read the
/// request, write the reply.
const CONNECTION_BUDGET: Duration = Duration::from_secs(10);

/// Everything a login run needs. `main` fills it with the defaults;
/// tests point the endpoints at a mock and the store at a temp dir.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Options {
    /// The dmz auth store file the login writes.
    pub store_path: PathBuf,
    pub authorize_endpoint: String,
    pub token_url: String,
    /// Loopback ports to try in order. The authorization server
    /// accepts redirects only on the registered ports. Tests bind
    /// port 0 to get an ephemeral one.
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

/// Run one login. `provider` is the resolved provider id: `None` or
/// `openai-codex` for the ChatGPT subscription, `anthropic` for the
/// API key.
pub async fn run(provider: Option<&str>, options: &Options) -> Result<(), RunError> {
    match provider {
        None | Some(openai_codex::PROVIDER_ID) => codex(options).await,
        Some(anthropic::PROVIDER_ID) => anthropic_login(options).await,
        Some(other) => {
            let known: Vec<String> = demerzel_providers::PROVIDER_IDS
                .iter()
                .map(|id| format!("`{id}`"))
                .collect();
            Err(RunError::Config(format!(
                "unknown provider `{other}` — this build has: {}",
                known.join(", ")
            )))
        }
    }
}

/// The PKCE flow, split into steps so tests can drive the listener
/// with their own fake browser.
pub struct CodexFlow {
    request: Arc<oauth::AuthorizeRequest>,
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
            None => options
                .redirect_ports
                .first()
                .copied()
                .unwrap_or(oauth::REDIRECT_PORTS[0]),
        };
        let request =
            oauth::AuthorizeRequest::with_endpoint(options.authorize_endpoint.clone(), port)
                .map_err(RunError::Auth)?;
        Ok(Self {
            request: Arc::new(request),
            listener,
        })
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
    /// code. The first request that carries this login's state ends
    /// the wait — with the code, or with the server's error. The
    /// wait consumes the listener; the port frees before the token
    /// exchange runs.
    pub async fn wait_for_callback(&mut self) -> Result<String, RunError> {
        let listener = self.listener.take().ok_or_else(|| {
            RunError::Config("no loopback listener — paste the redirect URL instead".to_string())
        })?;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String, RunError>>(1);
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let request = Arc::clone(&self.request);
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let served =
                            tokio::time::timeout(CONNECTION_BUDGET, serve_connection(stream, request))
                                .await;
                        if let Ok(Some(outcome)) = served {
                            let _ = tx.send(outcome).await;
                        }
                    });
                }
                outcome = rx.recv() => {
                    return outcome.expect("the loop holds a sender");
                }
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
            "The redirect ports are all busy. Complete the login in the \
             browser, then paste the redirect URL here."
        );
        let line = read_stdin_line("Redirect URL: ").await?;
        flow.parse_pasted(&line)?
    };
    flow.finish(options, &http_client(), &code).await?;
    println!("Login complete. The credentials are in the dmz auth store.");
    Ok(())
}

async fn anthropic_login(options: &Options) -> Result<(), RunError> {
    let key = prompt_for_key("Anthropic API key: ").await?;
    if key.is_empty() {
        return Err(RunError::Auth(ProviderError::Auth(
            "the key is empty".to_string(),
        )));
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
/// means the request was not this login's callback.
async fn serve_connection(
    mut stream: TcpStream,
    request: Arc<oauth::AuthorizeRequest>,
) -> Option<Result<String, RunError>> {
    let target = read_request_target(&mut stream).await?;
    let path = target
        .split_once('?')
        .map_or(target.as_str(), |(path, _)| path);
    if path != oauth::CALLBACK_PATH || !request.state_matches(&target) {
        respond(
            &mut stream,
            "404 Not Found",
            "<p>Not the login callback.</p>",
        )
        .await;
        return None;
    }
    match request.parse_callback(&target) {
        Ok(code) => {
            respond(
                &mut stream,
                "200 OK",
                "<p>Login complete. Close this tab and return to the terminal.</p>",
            )
            .await;
            Some(Ok(code))
        }
        Err(error) => {
            let body = format!("<p>Login failed: {}</p>", escape_html(&error.to_string()));
            respond(&mut stream, "200 OK", &body).await;
            Some(Err(RunError::Auth(error)))
        }
    }
}

/// Read the target out of an HTTP request line. Reads until the
/// headers end, so the reply never races unread request bytes.
/// `None` for anything that does not deliver a request line within
/// the buffer; the caller's time budget bounds the wait.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = [0u8; 8192];
    let mut len = 0;
    loop {
        if len == buf.len() {
            return None;
        }
        let read = stream.read(&mut buf[len..]).await.ok()?;
        if read == 0 {
            break;
        }
        // Rescan only the new bytes, plus the overlap a split
        // delimiter needs.
        let scan_from = len.saturating_sub(3);
        len += read;
        if buf[scan_from..len]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
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
    read_blocking_line().await
}

/// One stdin line, read on the blocking pool. The runtime thread
/// stays free to poll the Ctrl-C branch of the caller's select.
async fn read_blocking_line() -> Result<String, RunError> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map(|_| line)
    })
    .await
    .map_err(|error| RunError::Io(std::io::Error::other(error)))?
    .map_err(RunError::Io)
}

/// Prompt for the API key with echo off. The guard restores the
/// terminal on every exit path, including the drop of an aborted
/// future. A piped stdin — tests, scripts — reads plainly.
async fn prompt_for_key(prompt: &str) -> Result<String, RunError> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let guard = EchoGuard::disable();
    let line = read_blocking_line().await?;
    let was_off = guard.active();
    drop(guard);
    if was_off {
        // The user's Enter did not echo; end the prompt line.
        println!();
    }
    Ok(line.trim().to_string())
}

/// Terminal echo, off for the lifetime of the guard. A process kill
/// by an untrapped signal skips every destructor, this one included;
/// the shell restores its own terminal at the next prompt.
#[cfg(unix)]
struct EchoGuard(Option<libc::termios>);

#[cfg(unix)]
impl EchoGuard {
    fn disable() -> Self {
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) != 1 {
                return Self(None);
            }
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term) != 0 {
                return Self(None);
            }
            let saved = term;
            term.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) != 0 {
                return Self(None);
            }
            Self(Some(saved))
        }
    }

    fn active(&self) -> bool {
        self.0.is_some()
    }
}

#[cfg(unix)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        if let Some(saved) = &self.0 {
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved) };
        }
    }
}

/// Echo stays on where termios does not exist.
#[cfg(not(unix))]
struct EchoGuard;

#[cfg(not(unix))]
impl EchoGuard {
    fn disable() -> Self {
        Self
    }

    fn active(&self) -> bool {
        false
    }
}
