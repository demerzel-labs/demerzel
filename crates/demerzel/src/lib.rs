//! Harness wiring: builds the provider, the tool registry, and the
//! session store for one run. `main` reads flags and the environment
//! into a [`RunConfig`]; tests inject their own paths and endpoints.
//! [`Runtime`] holds the long-lived pieces; the REPL and one-shot mode
//! build engines over it.

pub mod config;
pub mod doctor;
pub mod repl;

use std::path::PathBuf;

use demerzel_core::{
    ApprovalMode, ApprovalPolicy, Engine, EngineConfig, EngineError, EventSink, Message, NoPrompt,
    ProviderError, Session, SessionMeta, SessionStore, ToolAccess, TurnOutcome,
};
use demerzel_providers::Provider;
use demerzel_providers::anthropic::{self, Anthropic, AnthropicConfig};
use demerzel_providers::auth::AuthStore;
use demerzel_providers::openai_codex::{self, CodexConfig, OpenAiCodex};
use demerzel_tools::Registry;

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
    /// The active provider id. M2 builds ship `openai-codex` and
    /// `anthropic`.
    pub provider: String,
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
    /// Input-history file for the REPL; `None` keeps history in
    /// memory only.
    pub history_path: Option<PathBuf>,
}

impl RunConfig {
    /// The defaults for a non-interactive run: read-only, fresh
    /// session, default endpoints.
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            provider: config::DEFAULT_PROVIDER.to_string(),
            model: openai_codex::DEFAULT_MODEL.to_string(),
            approval: ApprovalMode::ReadOnly,
            continue_last: false,
            data_root: None,
            base_url: None,
            auth: None,
            history_path: None,
        }
    }
}

/// What stopped a run before or during a turn.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error("{0}")]
    Auth(ProviderError),
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// The long-lived pieces of one run. Build it once; open sessions and
/// engines over it.
pub struct Runtime {
    config: RunConfig,
    engine_config: EngineConfig,
    provider: Box<dyn Provider>,
    /// The full tool set, and the read-only subset. The engine picks
    /// one per session, so a mode change is an engine rebuild, not a
    /// runtime rebuild.
    registry: Registry,
    read_only_registry: Registry,
    store: SessionStore,
}

impl Runtime {
    pub fn new(config: RunConfig) -> Result<Self, RunError> {
        let store = match &config.data_root {
            Some(root) => SessionStore::at(root),
            None => SessionStore::open_default()?,
        };

        let registry = Registry::full(&config.project_dir);
        let mut read_only_registry = Registry::full(&config.project_dir);
        read_only_registry.retain(|tool| tool.access() == ToolAccess::ReadOnly);

        let provider = Self::build_provider(&config)?;

        Ok(Self {
            engine_config: EngineConfig::new(config.model.clone()),
            config,
            provider,
            registry,
            read_only_registry,
            store,
        })
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// Build the provider the config names. Credentials resolve here,
    /// so a missing login fails with a clear message instead of inside
    /// the first turn.
    fn build_provider(config: &RunConfig) -> Result<Box<dyn Provider>, RunError> {
        let open_store = || match &config.auth {
            Some(paths) => AuthStore::open(paths.store.clone(), paths.codex_cli.clone())
                .map_err(RunError::Auth),
            None => AuthStore::open_default().map_err(RunError::Auth),
        };
        match config.provider.as_str() {
            openai_codex::PROVIDER_ID => {
                let mut auth = open_store()?;
                auth.ensure().map_err(RunError::Auth)?;
                let mut codex_config = CodexConfig::default();
                if let Some(base_url) = &config.base_url {
                    codex_config.base_url = base_url.clone();
                }
                Ok(Box::new(OpenAiCodex::new(codex_config, auth)))
            }
            anthropic::PROVIDER_ID => {
                // The environment wins and an empty variable counts as
                // unset — the order `AuthStore::resolve_anthropic_key`
                // owns. The store opens only when the environment
                // misses, so a broken store file cannot block a run
                // that carries its key in the environment.
                let api_key = match std::env::var(anthropic::ENV_API_KEY)
                    .ok()
                    .filter(|key| !key.is_empty())
                {
                    Some(key) => key,
                    None => open_store()?
                        .resolve_anthropic_key(None)
                        .map(|(key, _)| key)
                        .ok_or_else(|| {
                            RunError::Auth(ProviderError::Auth(
                                anthropic::NO_CREDENTIALS_HELP.to_string(),
                            ))
                        })?,
                };
                let mut anthropic_config = AnthropicConfig::default();
                if let Some(base_url) = &config.base_url {
                    anthropic_config.base_url = base_url.clone();
                }
                Ok(Box::new(Anthropic::new(anthropic_config, api_key)))
            }
            other => Err(RunError::Config(format!(
                "unknown provider `{other}` — this build has: {}",
                demerzel_providers::PROVIDER_IDS.join(", ")
            ))),
        }
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

    /// Build the turn engine over an open session. A read-only run
    /// advertises only the read tools.
    pub fn engine(&self, session: Session, history: Vec<Message>) -> Engine<'_> {
        let registry = if self.config.approval == ApprovalMode::ReadOnly {
            &self.read_only_registry
        } else {
            &self.registry
        };
        Engine::with_history(
            self.provider.as_ref(),
            registry,
            ApprovalPolicy::new(self.config.approval),
            self.engine_config.clone(),
            session,
            history,
        )
    }

    /// Run one prompt to completion. Display events go to `sink`; the
    /// returned outcome says how the turn ended.
    pub async fn one_shot(
        &self,
        prompt: String,
        sink: &dyn EventSink,
    ) -> Result<TurnOutcome, RunError> {
        let (session, history) = self.open_session()?;
        let mut engine = self.engine(session, history);
        Ok(engine.user_turn(prompt, sink, &mut NoPrompt).await?)
    }
}

/// Build a runtime and run one prompt. A repeated caller builds one
/// [`Runtime`] and calls [`Runtime::one_shot`], so the HTTP connection
/// pool persists between calls.
pub async fn run_one_shot(
    config: &RunConfig,
    prompt: String,
    sink: &dyn EventSink,
) -> Result<TurnOutcome, RunError> {
    Runtime::new(config.clone())?.one_shot(prompt, sink).await
}
