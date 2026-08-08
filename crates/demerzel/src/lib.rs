//! Harness wiring: builds the provider, the tool registry, and the
//! session store for one run. `main` reads flags and the environment
//! into a [`RunConfig`]; tests inject their own paths and endpoints.
//! [`Runtime`] holds the long-lived pieces; the REPL and one-shot mode
//! build engines over it.

pub mod repl;

use std::path::PathBuf;

use demerzel_core::{
    ApprovalMode, ApprovalPolicy, Engine, EngineConfig, EngineError, EventSink, Message, NoPrompt,
    ProviderError, Session, SessionMeta, SessionStore, ToolAccess, TurnOutcome,
};
use demerzel_providers::Provider;
use demerzel_providers::openai_codex::{AuthStore, CodexConfig, OpenAiCodex};
use demerzel_tools::Registry;

/// Default model until per-role model routing arrives (M2 config).
pub const DEFAULT_MODEL: &str = "gpt-5.6-sol";

/// First line the REPL prints. The PTY boot probe waits for it.
pub const READY_MARKER: &str = "demerzel ready";

/// Auth store locations for one run.
#[derive(Debug, Clone)]
pub struct AuthPaths {
    /// The dmz auth store file.
    pub store: PathBuf,
    /// The Codex CLI `auth.json` to import from; `None` turns the
    /// import off.
    pub codex_cli: Option<PathBuf>,
}

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
    /// Auth store override; `None` uses the default paths.
    pub auth: Option<AuthPaths>,
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
            auth: None,
        }
    }
}

/// What stopped a run before or during a turn.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error("{0}")]
    Auth(ProviderError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("session: {0}")]
    Session(#[from] std::io::Error),
}

/// The long-lived pieces of one run. Build it once; open sessions and
/// engines over it.
pub struct Runtime {
    config: RunConfig,
    provider: OpenAiCodex,
    registry: Registry,
    store: SessionStore,
}

impl Runtime {
    pub fn new(config: RunConfig) -> Result<Self, RunError> {
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

        let auth = match &config.auth {
            Some(paths) => AuthStore::open(paths.store.clone(), paths.codex_cli.clone())
                .map_err(RunError::Auth)?,
            None => AuthStore::open_default().map_err(RunError::Auth)?,
        };
        let mut codex_config = CodexConfig::default();
        if let Some(base_url) = &config.base_url {
            codex_config.base_url = base_url.clone();
        }
        let provider = OpenAiCodex::new(codex_config, auth);

        Ok(Self {
            config,
            provider,
            registry,
            store,
        })
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// Open the session for this run: the latest one when
    /// `continue_last` asks for it and one matches this project, else
    /// a fresh one. Resume appends a fresh meta record, so the file
    /// names the model and provider that continue it.
    pub fn open_session(&self) -> Result<(Session, Vec<Message>), RunError> {
        if self.config.continue_last
            && let Some(path) = self.store.latest(&self.config.project_dir)?
        {
            let (mut session, replay) = Session::resume(&path)?;
            // A hash collision in the project directory name is the
            // only way another project's file lands here; refuse to
            // mix them. Both sides compare canonical, so a relative or
            // symlinked spelling of the same project still matches.
            let canonical = self
                .config
                .project_dir
                .canonicalize()
                .unwrap_or_else(|_| self.config.project_dir.clone());
            let same_project = replay
                .meta
                .as_ref()
                .is_none_or(|meta| meta.cwd == canonical.display().to_string());
            if same_project {
                session.append_meta(&SessionMeta::now(
                    &self.config.project_dir,
                    self.provider.id(),
                    &self.config.model,
                ))?;
                return Ok((session, replay.messages));
            }
        }
        let session = self.store.create(
            &self.config.project_dir,
            self.provider.id(),
            &self.config.model,
        )?;
        Ok((session, Vec::new()))
    }

    /// Build the turn engine over an open session.
    pub fn engine(&self, session: Session, history: Vec<Message>) -> Engine<'_> {
        Engine::with_history(
            &self.provider,
            &self.registry,
            ApprovalPolicy::new(self.config.approval),
            EngineConfig::new(self.config.model.clone()),
            session,
            history,
        )
    }
}

/// Run one prompt to completion and exit. Display events go to `sink`;
/// the returned outcome says how the turn ended.
pub async fn run_one_shot(
    config: &RunConfig,
    prompt: String,
    sink: &dyn EventSink,
) -> Result<TurnOutcome, RunError> {
    let runtime = Runtime::new(config.clone())?;
    let (session, history) = runtime.open_session()?;
    let mut engine = runtime.engine(session, history);
    Ok(engine.user_turn(prompt, sink, &mut NoPrompt).await?)
}
