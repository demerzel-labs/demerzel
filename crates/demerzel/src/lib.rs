//! Harness wiring: builds the provider, the tool registry, and the
//! session for one run. `main` reads flags and the environment into a
//! [`RunConfig`]; tests inject their own paths and endpoints.

use std::path::PathBuf;

use demerzel_core::{
    ApprovalMode, ApprovalPolicy, Engine, EngineConfig, EventSink, NoPrompt, Session, SessionStore,
    ToolAccess, TurnOutcome,
};
use demerzel_providers::Provider;
use demerzel_providers::openai_codex::{AuthStore, CodexConfig, OpenAiCodex};
use demerzel_tools::Registry;

/// Default model until per-role model routing arrives (M2 config).
pub const DEFAULT_MODEL: &str = "gpt-5.6-sol";

/// Everything one run needs. `main` fills it from flags and the
/// environment; tests fill it directly.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The project directory tools and sessions are scoped to.
    pub project_dir: PathBuf,
    pub model: String,
    pub approval: ApprovalMode,
    /// Continue the latest session in this project.
    pub continue_last: bool,
    /// Session data root override; `None` uses the XDG default.
    pub data_root: Option<PathBuf>,
    /// Endpoint override; `None` uses the Codex default.
    pub base_url: Option<String>,
    /// Auth store override as `(store path, Codex CLI import path)`;
    /// `None` uses the default paths.
    pub auth_paths: Option<(PathBuf, Option<PathBuf>)>,
}

impl RunConfig {
    /// The defaults for a non-interactive run: read-only, fresh
    /// session, default endpoints.
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            model: DEFAULT_MODEL.to_string(),
            approval: ApprovalMode::ReadOnly,
            continue_last: false,
            data_root: None,
            base_url: None,
            auth_paths: None,
        }
    }
}

/// Run one prompt to completion and exit. Display events go to `sink`;
/// the returned outcome says how the turn ended.
pub async fn run_one_shot(
    config: &RunConfig,
    prompt: String,
    sink: &mut dyn EventSink,
) -> Result<TurnOutcome, Box<dyn std::error::Error>> {
    let store = match &config.data_root {
        Some(root) => SessionStore::at(root),
        None => SessionStore::open_default()?,
    };

    let mut registry = Registry::full(&config.project_dir);
    if config.approval == ApprovalMode::ReadOnly {
        // A read-only session drops the write tools instead of
        // advertising and refusing them.
        registry.retain(|tool| tool.access() == ToolAccess::ReadOnly);
    }

    let auth = match &config.auth_paths {
        Some((path, codex_cli)) => AuthStore::open(path.clone(), codex_cli.clone())?,
        None => AuthStore::open_default()?,
    };
    let mut codex_config = CodexConfig::default();
    if let Some(base_url) = &config.base_url {
        codex_config.base_url = base_url.clone();
    }
    let provider = OpenAiCodex::new(codex_config, auth);

    let (session, history) = open_session(&store, config, provider.id())?;
    let policy = ApprovalPolicy::new(config.approval);
    let mut engine = Engine::with_history(
        &provider,
        &registry,
        policy,
        EngineConfig::new(config.model.clone()),
        session,
        history,
    );
    Ok(engine.user_turn(prompt, sink, &mut NoPrompt).await?)
}

/// Open the session for this run: the latest one when `continue_last`
/// asks for it and one exists, else a fresh one.
fn open_session(
    store: &SessionStore,
    config: &RunConfig,
    provider_id: &str,
) -> Result<(Session, Vec<demerzel_core::Message>), Box<dyn std::error::Error>> {
    if config.continue_last
        && let Some(path) = store.latest(&config.project_dir)?
    {
        let (session, replay) = Session::resume(&path)?;
        return Ok((session, replay.messages));
    }
    let session = store.create(&config.project_dir, provider_id, &config.model)?;
    Ok((session, Vec::new()))
}
