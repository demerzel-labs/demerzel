//! Harness wiring: builds the provider, the tool registry, and the
//! session store for one run. `main` reads flags and the environment
//! into a [`RunConfig`]; tests inject their own paths and endpoints.
//! [`Runtime`] holds the long-lived pieces; the REPL and one-shot mode
//! build engines over it.

mod commands;
pub mod config;
pub mod doctor;
mod git;
pub mod login;
mod preview;
pub mod repl;
mod replay;
pub mod trace;
pub mod tui;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use demerzel_core::{
    ApprovalMode, ApprovalPolicy, Effort, Engine, EngineConfig, EngineError, EventSink, Message,
    NoPrompt, ProviderError, Replay, Session, SessionMeta, SessionOpen, SessionStore, ToolAccess,
    ToolHost, TurnOutcome,
};
use demerzel_providers::Provider;
use demerzel_providers::ThinkingMode;
use demerzel_providers::anthropic::{self, Anthropic, AnthropicConfig};
use demerzel_providers::auth::AuthStore;
use demerzel_providers::openai_codex::{self, CodexConfig, OpenAiCodex};
use demerzel_providers::openai_compat::{self, CompatConfig, OpenAiCompat};
use demerzel_providers::openai_responses::{self, OpenAiResponses, ResponsesConfig};
use demerzel_tools::Registry;
pub use demerzel_tui::{FirstRun, LoginCommand, Theme, Thinking};

use crate::doctor::credential_states;

/// First line an interactive interface prints on stderr. The PTY boot
/// probe waits for it; the TUI prints it before the alternate screen.
pub const READY_MARKER: &str = "demerzel ready";

/// Which session a run opens at its first prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resume {
    /// A fresh session.
    #[default]
    Fresh,
    /// The latest session in the project, `-c`. With none, a fresh
    /// one.
    Latest,
    /// The session with this id in the project, `--session <id>`.
    /// With no such file the run stops at boot with a clear error.
    Id(String),
}

/// The session knobs the interfaces own: `/model`, `/effort`, and
/// `/approval` switch them mid-session, so they travel apart from
/// the boot-time [`RunConfig`].
#[derive(Debug, Clone)]
pub struct SessionSettings {
    pub model: String,
    pub effort: Effort,
    /// Whether a flag, the environment, a config file, or `/effort`
    /// set the effort. For an adapter without implicit effort, a file
    /// counts only through the instance's own `[providers.<name>]`
    /// table. Such an adapter receives only a set level; the others
    /// receive the default too.
    pub effort_set: bool,
    /// Whether the active adapter receives the default level, from
    /// its rules.
    pub implicit_effort: bool,
    /// Whether the active instance keeps the effort off the wire at
    /// every level: an instance with thinking off. The session keeps
    /// its level, and the effort notes say the instance omits it.
    pub effort_omitted: bool,
    pub approval: ApprovalMode,
}

impl SessionSettings {
    /// The settings a run starts with.
    pub fn from_config(config: &RunConfig) -> Self {
        Self {
            model: config.model.clone(),
            effort: config.effort,
            effort_set: config.effort_set,
            implicit_effort: demerzel_providers::adapter_rules(config.kind())
                .is_some_and(|rules| rules.implicit_effort),
            effort_omitted: config
                .thinking_mode
                .is_some_and(|mode| !mode.sends_effort()),
            approval: config.approval,
        }
    }

    /// The effort the requests carry. An adapter without implicit
    /// effort, such as a compatible endpoint, gets a level only when
    /// one was set: many servers refuse a field they do not know.
    pub fn request_effort(&self) -> Option<Effort> {
        (self.effort_set || self.implicit_effort).then_some(self.effort)
    }
}

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
#[non_exhaustive]
pub struct RunConfig {
    /// The project directory tools and sessions are scoped to.
    pub project_dir: PathBuf,
    /// The active provider instance name. A shipped adapter's implicit
    /// instance carries the adapter id.
    pub provider: String,
    /// The adapter id behind the instance: `openai-codex`,
    /// `anthropic`, `openai-compat`, or `openai-responses`. `None` when the instance name
    /// is the adapter id, the meaning a config table without `kind`
    /// keeps. See [`RunConfig::kind`].
    pub kind: Option<String>,
    pub model: String,
    /// The environment variable that holds the instance's key; `None`
    /// keeps the adapter's own credential source.
    pub api_key_env: Option<String>,
    pub approval: ApprovalMode,
    /// Which session the first prompt opens.
    pub resume: Resume,
    /// Session data root override; `None` uses the XDG default.
    pub data_root: Option<PathBuf>,
    /// Endpoint override; `None` uses the Codex default.
    pub base_url: Option<String>,
    /// The reasoning effort every request carries.
    pub effort: Effort,
    /// Whether a flag, the environment, or a config file set `effort`;
    /// see [`SessionSettings::effort_set`] for the adapter rule. Unset,
    /// a compatible endpoint receives no effort field.
    pub effort_set: bool,
    /// Model calls one user turn may make before the loop stops.
    pub max_model_calls: u32,
    /// The context window, in tokens. Set, it wins over the provider
    /// catalog; `None` leaves the catalog in charge.
    pub context_window: Option<u64>,
    /// The instance's `thinking` key; `None` keeps the adapter's
    /// default. Only an adapter with a thinking switch takes a value;
    /// the boot check refuses it on another.
    pub thinking_mode: Option<ThinkingMode>,
    /// Auth store override; `None` uses the default paths.
    pub auth: Option<AuthPaths>,
    /// Input-history file for the REPL; `None` keeps history in
    /// memory only.
    pub history_path: Option<PathBuf>,
    /// Capture the mouse in the full-screen interface: the wheel
    /// scrolls the transcript and a drag selects text. On by
    /// default; off gives the terminal its own text selection back.
    pub mouse: bool,
    /// Render model replies as markdown in the full-screen interface.
    pub markdown: bool,
    /// How a thinking block starts in the full-screen interface.
    pub thinking: Thinking,
    /// The colors of the full-screen interface.
    pub theme: Theme,
}

impl RunConfig {
    /// The defaults for a non-interactive run: read-only, fresh
    /// session, default endpoints.
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            provider: config::DEFAULT_PROVIDER.to_string(),
            kind: None,
            model: openai_codex::DEFAULT_MODEL.to_string(),
            api_key_env: None,
            approval: ApprovalMode::ReadOnly,
            resume: Resume::Fresh,
            data_root: None,
            base_url: None,
            effort: config::DEFAULT_EFFORT,
            effort_set: false,
            max_model_calls: config::DEFAULT_MAX_MODEL_CALLS,
            context_window: None,
            thinking_mode: None,
            auth: None,
            history_path: None,
            mouse: config::DEFAULT_MOUSE,
            markdown: true,
            thinking: Thinking::default(),
            theme: Theme::default(),
        }
    }

    /// The adapter id behind the active instance.
    pub fn kind(&self) -> &str {
        self.kind.as_deref().unwrap_or(&self.provider)
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
    /// The provider, built at the first turn: credentials resolve
    /// then, not at boot, so a run that opens without a login — the
    /// first-run screen — finds a login that lands before its first
    /// turn. See `Runtime::provider`.
    provider: OnceLock<Box<dyn Provider>>,
    /// The full tool set, and the read-only subset. `tools(mode)`
    /// picks one; `Engine::set_approval` takes the pick when the mode
    /// changes mid-session.
    registry: Registry,
    read_only_registry: Registry,
    store: SessionStore,
}

impl Runtime {
    /// Build the long-lived pieces: the session store, the tool
    /// registries, and the checks that stop a run at boot — a
    /// `--session <id>` with no file, a provider id this build does
    /// not have. The credentials resolve at the first turn; see
    /// `Runtime::provider`.
    pub fn new(config: RunConfig) -> Result<Self, RunError> {
        let store = match &config.data_root {
            Some(root) => SessionStore::at(root),
            None => SessionStore::open_default()?,
        };
        // A named session must exist at boot, not at the first
        // prompt: one stat, and the run stops before the interface
        // opens or a request goes out.
        if let Resume::Id(id) = &config.resume {
            find_session(&store, &config.project_dir, id)?;
        }

        let registry = Registry::full(&config.project_dir);
        let mut read_only_registry = Registry::full(&config.project_dir);
        read_only_registry.retain(|tool| tool.access() == ToolAccess::ReadOnly);

        // The instance is a config value: an unknown adapter, a
        // missing endpoint or model, or a key that would reach the
        // wrong endpoint stops the run at boot, before the interface
        // opens. The same check serves `doctor`. The credentials
        // resolve at the first turn.
        let mut instance = config::Instance::new(config.provider.clone(), config.kind());
        instance.base_url = config.base_url.clone();
        instance.model = (!config.model.is_empty()).then(|| config.model.clone());
        instance.api_key_env = config.api_key_env.clone();
        instance.thinking_mode = config.thinking_mode;
        if let Some(problem) = config::instance_problems(&instance).into_iter().next() {
            return Err(RunError::Config(problem));
        }

        let mut engine_config = EngineConfig::new(config.model.clone());
        engine_config.instance = Some(config.provider.clone());
        engine_config.max_model_calls = config.max_model_calls;

        Ok(Self {
            engine_config,
            config,
            provider: OnceLock::new(),
            registry,
            read_only_registry,
            store,
        })
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// The provider, built on the first call. Credentials resolve
    /// here, so a missing login fails with a clear message. A failed
    /// build is not kept: the next call tries again, so a login that
    /// lands between two prompts takes effect at the next one. The
    /// plain interface calls this at boot and a one-shot run inside
    /// the open of its one session, so a missing login stops both
    /// before a request goes out or a session file is touched.
    pub(crate) fn provider(&self) -> Result<&dyn Provider, RunError> {
        if let Some(provider) = self.provider.get() {
            return Ok(provider.as_ref());
        }
        let provider = Self::build_provider(&self.config)?;
        Ok(self.provider.get_or_init(|| provider).as_ref())
    }

    /// The first-run screen's text when no credential resolves for
    /// the session's provider; `None` when one does. The decision
    /// comes first and the text second: a run that can authenticate
    /// pays the probe alone, and an Anthropic session with its key in
    /// the environment opens no store at all. The state lines come
    /// from the read-only probe `doctor` runs, with its wording: the
    /// store stays unwritten, and a Codex CLI login that the first
    /// turn would import counts as a credential. A store that does
    /// not open is a state to show, not a stop: the first turn fails
    /// on it with the same error.
    pub fn first_run(&self) -> Option<FirstRun> {
        // An instance with its own key variable, and an adapter
        // without credentials of its own, have no login to offer: the
        // first turn names the missing variable.
        let own_credentials = demerzel_providers::adapter_rules(self.config.kind())
            .is_some_and(|rules| rules.own_credentials);
        if self.config.api_key_env.is_some() || !own_credentials {
            return None;
        }
        let env_key = std::env::var(anthropic::ENV_API_KEY)
            .ok()
            .filter(|key| !key.is_empty());
        let provider = self.config.kind();
        // The environment wins and skips the store, the order the
        // provider build applies.
        if provider == anthropic::PROVIDER_ID && env_key.is_some() {
            return None;
        }
        let store = Self::open_store(&self.config);
        let resolves = match (provider, &store) {
            (_, Err(_)) => false,
            (openai_codex::PROVIDER_ID, Ok(store)) => {
                store.probe().is_some_and(|probe| probe.source.usable())
            }
            (anthropic::PROVIDER_ID, Ok(store)) => store.anthropic_api_key().is_some(),
            // The boot check refuses any other id; the build's error
            // says so at the first turn.
            _ => return None,
        };
        if resolves {
            return None;
        }
        let credentials = match &store {
            Ok(store) => credential_states(store, env_key.as_deref())
                .map(|state| format!("{}: {}", state.provider, state.state))
                .to_vec(),
            Err(error) => {
                let state = error.to_string();
                vec![
                    format!("{}: {state}", openai_codex::PROVIDER_ID),
                    format!("{}: {state}", anthropic::PROVIDER_ID),
                ]
            }
        };
        let codex_login =
            LoginCommand::new("dmz login", "the ChatGPT subscription, through the browser");
        let anthropic_login =
            LoginCommand::new("dmz login anthropic", "an API key, prompted and verified");
        let mut screen = FirstRun::default();
        screen.provider = provider.to_string();
        screen.credentials = credentials;
        // The session's provider first.
        screen.commands = if provider == anthropic::PROVIDER_ID {
            vec![anthropic_login, codex_login]
        } else {
            vec![codex_login, anthropic_login]
        };
        Some(screen)
    }

    /// The auth store of this run: the configured paths, or the
    /// defaults.
    fn open_store(config: &RunConfig) -> Result<AuthStore, RunError> {
        match &config.auth {
            Some(paths) => AuthStore::open(paths.store.clone(), paths.codex_cli.clone())
                .map_err(RunError::Auth),
            None => AuthStore::open_default().map_err(RunError::Auth),
        }
    }

    /// The stored key of the active instance, for `base_url`. The read
    /// takes the instance's entry alone, so a run without `HOME` or
    /// with a section this build cannot read still builds; no data
    /// directory means no store and no key. A store that is not JSON
    /// stops the turn: it can hold the key the endpoint needs. A key
    /// checked at another endpoint stops it too: the key goes only to
    /// the endpoint the login named.
    fn stored_instance_key(config: &RunConfig, base_url: &str) -> Result<Option<String>, RunError> {
        let path = match &config.auth {
            Some(paths) => paths.store.clone(),
            None => match AuthStore::default_path() {
                Ok(path) => path,
                Err(_) => return Ok(None),
            },
        };
        let name = &config.provider;
        match AuthStore::read_instance_key(&path, name).map_err(RunError::Auth)? {
            None => Ok(None),
            Some(stored) if stored.serves(base_url) => Ok(Some(stored.api_key)),
            Some(stored) => Err(RunError::Auth(ProviderError::Auth(format!(
                "the stored key of provider `{name}` was checked at {}, not at {base_url}; \
                 run `dmz login {name}` to store a key for this endpoint",
                stored.base_url.as_deref().unwrap_or("no endpoint")
            )))),
        }
    }

    /// Build the provider the config names, with its credentials.
    fn build_provider(config: &RunConfig) -> Result<Box<dyn Provider>, RunError> {
        let open_store = || Self::open_store(config);
        // The instance's own key variable wins over every other
        // source. Named but unset, it stops the turn with its name.
        let named_key = match &config.api_key_env {
            Some(variable) => Some(
                std::env::var(variable)
                    .ok()
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| {
                        RunError::Auth(ProviderError::Auth(format!(
                            "provider `{}` reads its key from {variable}, and {variable} \
                             is unset or empty",
                            config.provider
                        )))
                    })?,
            ),
            None => None,
        };
        match config.kind() {
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
                let api_key = match named_key.or_else(|| {
                    std::env::var(anthropic::ENV_API_KEY)
                        .ok()
                        .filter(|key| !key.is_empty())
                }) {
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
                anthropic_config.thinking = config.thinking_mode.unwrap_or_default();
                Ok(Box::new(Anthropic::new(anthropic_config, api_key)))
            }
            kind @ (openai_compat::PROVIDER_ID | openai_responses::PROVIDER_ID) => {
                // The boot check requires the endpoint: these adapters
                // have no default one. The key comes from the
                // instance's own variable, else from the instance's
                // entry in the store, else nowhere: no header.
                let base_url = config.base_url.clone().ok_or_else(|| {
                    RunError::Config(format!("provider `{}` needs `base_url`", config.provider))
                })?;
                let key = match named_key {
                    Some(key) => Some(key),
                    None => Self::stored_instance_key(config, &base_url)?,
                };
                let instance = config.provider.clone();
                let provider: Box<dyn Provider> = if kind == openai_compat::PROVIDER_ID {
                    let compat_config = CompatConfig::new(instance, base_url);
                    Box::new(OpenAiCompat::new(compat_config, key))
                } else {
                    let responses_config = ResponsesConfig::new(instance, base_url);
                    Box::new(OpenAiResponses::new(responses_config, key))
                };
                Ok(provider)
            }
            _ => Err(unknown_provider(&config.provider)),
        }
    }

    /// Open the session for this run: the one `resume` names when it
    /// exists and matches this project, else a fresh one — except
    /// that a session named by id must exist and match, or the open
    /// fails. Resume appends a fresh meta record, so the file names
    /// the model and provider that continue it. The interfaces own
    /// the session model — `/model` switches it — so it arrives as
    /// an argument. The replay holds the history the engine
    /// continues, and the records the full-screen interface shows
    /// again; a fresh session's is empty. A session opens only with
    /// a provider to run it: a missing login fails here, before a
    /// file is created or touched.
    pub fn open_session(
        &self,
        resume: &Resume,
        settings: &SessionSettings,
    ) -> Result<(Session, Replay), RunError> {
        self.provider()?;
        let path = match resume {
            Resume::Fresh => None,
            Resume::Latest => self.store.latest(&self.config.project_dir)?,
            Resume::Id(id) => Some(find_session(&self.store, &self.config.project_dir, id)?),
        };
        if let Some(path) = path {
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
                session.append_meta(&SessionMeta::now(&self.session_open(settings)))?;
                return Ok((session, replay));
            }
            if let Resume::Id(id) = resume {
                return Err(RunError::Config(format!(
                    "session `{id}` belongs to another project"
                )));
            }
        }
        let session = self.store.create(&self.session_open(settings))?;
        Ok((session, Replay::default()))
    }

    /// What a meta record names for this run and these settings.
    fn session_open<'a>(&'a self, settings: &'a SessionSettings) -> SessionOpen<'a> {
        let mut open = SessionOpen::new(
            &self.config.project_dir,
            &self.config.provider,
            self.config.kind(),
            &settings.model,
        );
        open.effort = settings.request_effort();
        open.approval = settings.approval;
        open
    }

    /// Whether `id` names the project's latest session, the one `-c`
    /// opens. The exit line asks at the end of a run, so it names a
    /// command that opens the session it names; a store it cannot
    /// read answers no, and the line falls back to the id.
    pub fn is_latest_session(&self, id: &str) -> bool {
        self.store
            .latest(&self.config.project_dir)
            .ok()
            .flatten()
            .is_some_and(|path| path.file_stem().is_some_and(|stem| stem == id))
    }

    /// The context window of `model`, in tokens: the
    /// `context_window` key when set — the config wins, so a stale
    /// catalog row has an override — or the provider catalog's
    /// number.
    pub(crate) fn context_window(&self, model: &str) -> Option<u64> {
        self.config
            .context_window
            .or_else(|| demerzel_providers::context_window(self.config.kind(), model))
    }

    /// The tool set for an approval mode: `read-only` advertises the
    /// read tools alone, the other modes the full registry.
    pub(crate) fn tools(&self, approval: ApprovalMode) -> &dyn ToolHost {
        if approval == ApprovalMode::ReadOnly {
            &self.read_only_registry
        } else {
            &self.registry
        }
    }

    /// Build the turn engine over an open session, with the
    /// interface's current session settings. The provider builds
    /// here when nothing built it yet; see `Runtime::provider`.
    pub fn engine(
        &self,
        session: Session,
        history: Vec<Message>,
        settings: &SessionSettings,
    ) -> Result<Engine<'_>, RunError> {
        let mut engine_config = self.engine_config.clone();
        engine_config.model = settings.model.clone();
        engine_config.effort = settings.request_effort();
        Ok(Engine::with_history(
            self.provider()?,
            self.tools(settings.approval),
            ApprovalPolicy::new(settings.approval),
            engine_config,
            session,
            history,
        ))
    }

    /// Run one prompt to completion. Display events go to `sink`; the
    /// returned outcome says how the turn ended.
    pub async fn one_shot(
        &self,
        prompt: String,
        sink: &dyn EventSink,
    ) -> Result<TurnOutcome, RunError> {
        let settings = SessionSettings::from_config(&self.config);
        let (session, replay) = self.open_session(&self.config.resume, &settings)?;
        let mut engine = self.engine(session, replay.messages, &settings)?;
        Ok(engine.user_turn(prompt, sink, &mut NoPrompt).await?)
    }
}

/// The error for a provider id this build does not have.
fn unknown_provider(other: &str) -> RunError {
    let known: Vec<String> = demerzel_providers::PROVIDER_IDS
        .iter()
        .map(|id| format!("`{id}`"))
        .collect();
    RunError::Config(format!(
        "unknown provider `{other}` — this build has: {}",
        known.join(", ")
    ))
}

/// The session file `id` names in the project, or the clear error
/// `--session` promises: no file, no run.
fn find_session(store: &SessionStore, project: &Path, id: &str) -> Result<PathBuf, RunError> {
    store
        .find(project, id)?
        .ok_or_else(|| RunError::Config(format!("no session `{id}` in this project")))
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
