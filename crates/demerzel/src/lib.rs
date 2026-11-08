//! Harness wiring: builds the providers, the tool registry, and the
//! session store for one run. `main` reads flags and the environment
//! into a [`RunConfig`]; tests inject their own paths and endpoints.
//! [`Runtime`] holds the long-lived pieces; the REPL, the full-screen
//! interface, and one-shot mode open sessions over it with
//! [`Runtime::open`].

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
    ApprovalMode, ApprovalPolicy, Effort, Engine, EngineConfig, EngineError, EngineParts,
    EventSink, NoPrompt, ProviderError, ProviderHost, Replay, ReplayToolClock, ReplayUsage, Route,
    Session, SessionMeta, SessionOpen, SessionStore, Target, ToolAccess, ToolHost, TurnOutcome,
};
use demerzel_providers::anthropic::{self, Anthropic, AnthropicConfig};
use demerzel_providers::auth::AuthStore;
use demerzel_providers::openai_codex::{self, CodexConfig, OpenAiCodex};
use demerzel_providers::openai_compat::{self, CompatConfig, OpenAiCompat};
use demerzel_providers::openai_responses::{self, OpenAiResponses, ResponsesConfig};
use demerzel_providers::{EffortWire, Provider};
use demerzel_tools::Registry;
pub use demerzel_tui::{FirstRun, LoginCommand, Theme, Thinking};

use crate::config::Origin;
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
#[non_exhaustive]
pub struct SessionSettings {
    /// The route of the model calls. Its head target names the
    /// instance and the model.
    pub route: Route,
    /// Which layer named the head target. A resume adopts the
    /// session's own target, except that a flag or a variable keeps
    /// the whole head and `/model` keeps the model.
    pub route_origin: Origin,
    pub effort: Effort,
    /// Which layer set the effort; `Default` when none did. For an
    /// adapter without implicit effort, a file counts only through the
    /// instance's own `[providers.<name>]` table. Such an adapter
    /// receives only a set level; the others receive the default too.
    /// See [`Runtime::effort_wire`]. A resume onto another instance
    /// takes that instance's configured effort, unless `/effort` set
    /// one.
    pub effort_origin: Origin,
    pub approval: ApprovalMode,
}

impl SessionSettings {
    /// The settings a run starts with.
    pub fn from_config(config: &RunConfig) -> Self {
        Self {
            route: Route::new(Target::new(&*config.instance.name, &*config.model)),
            route_origin: config.route_origin,
            effort: config.effort,
            effort_origin: config.effort_origin,
            approval: config.approval,
        }
    }

    /// Whether a layer or `/effort` set the effort.
    pub fn effort_set(&self) -> bool {
        self.effort_origin != Origin::Default
    }

    /// `/model`: switch the head target's model.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.route.head.model = model.into();
        if !matches!(self.route_origin, Origin::Flag | Origin::Env) {
            self.route_origin = Origin::Command;
        }
    }

    /// `/effort`: set the level for the turns that follow.
    pub fn set_effort(&mut self, effort: Effort) {
        self.effort = effort;
        self.effort_origin = Origin::Command;
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
    /// The head instance with its resolved keys: the name, the
    /// adapter id, the endpoint, the key variable, the `thinking`
    /// key, and the context window. A shipped adapter's implicit
    /// instance carries the adapter id as its name. The boot check
    /// reads its model from [`RunConfig::model`].
    pub instance: config::Instance,
    /// The head model.
    pub model: String,
    /// Which layer named the head target; see
    /// [`config::Settings::route_origin`]. A flag or a variable wins
    /// over a resumed session's own target.
    pub route_origin: Origin,
    /// The other instances a resume can adopt, resolved from config;
    /// see [`config::resumable`]. Empty, a resume adopts only the
    /// session's model, and only on the head instance.
    pub resumable: Vec<config::Resolved>,
    pub approval: ApprovalMode,
    /// Which session the first prompt opens.
    pub resume: Resume,
    /// Session data root override; `None` uses the XDG default.
    pub data_root: Option<PathBuf>,
    /// The session's reasoning effort.
    pub effort: Effort,
    /// The layer that set `effort`; `Default` when none did. See
    /// [`SessionSettings::effort_origin`] for the adapter rule. Unset,
    /// a compatible endpoint receives no effort field.
    pub effort_origin: Origin,
    /// Model calls one user turn may make before the loop stops.
    pub max_model_calls: u32,
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
            instance: config::Instance::new(config::DEFAULT_PROVIDER, config::DEFAULT_PROVIDER),
            model: openai_codex::DEFAULT_MODEL.to_string(),
            route_origin: Origin::Default,
            resumable: Vec::new(),
            approval: ApprovalMode::ReadOnly,
            resume: Resume::Fresh,
            data_root: None,
            effort: config::DEFAULT_EFFORT,
            effort_origin: Origin::Default,
            max_model_calls: config::DEFAULT_MAX_MODEL_CALLS,
            auth: None,
            history_path: None,
            mouse: config::DEFAULT_MOUSE,
            markdown: true,
            thinking: Thinking::default(),
            theme: Theme::default(),
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

/// The long-lived pieces of one run. Build it once; open sessions
/// over it.
pub struct Runtime {
    config: RunConfig,
    providers: Providers,
    /// The full tool set, and the read-only subset. `tools(mode)`
    /// picks one; `Engine::set_approval` takes the pick when the mode
    /// changes mid-session.
    registry: Registry,
    read_only_registry: Registry,
    store: SessionStore,
}

/// A session open for its first prompt.
#[non_exhaustive]
pub struct Opened<'r> {
    /// The turn engine. Its history holds the messages the session
    /// continues: read them from [`Engine::history`].
    pub engine: Engine<'r>,
    /// The settings the session runs with: the ones the open took,
    /// with a resumed session's own head target adopted.
    pub settings: SessionSettings,
    /// The usage records of a resumed file, for display; empty for a
    /// fresh session.
    pub usage: Vec<ReplayUsage>,
    /// The tool clocks of a resumed file, for display.
    pub tools: Vec<ReplayToolClock>,
    /// A line for the user when the open did not continue on the
    /// session's own head target: config no longer runs its instance,
    /// and the session continues on the run's head.
    pub notice: Option<String>,
}

/// The run's provider instances: the configured head first, then the
/// instances a resume can adopt. Each has one lazy provider slot, so
/// a provider lives as long as the runtime and a failed build retries
/// at the next call.
struct Providers {
    entries: Vec<Entry>,
    auth: Option<AuthPaths>,
}

struct Entry {
    resolved: config::Resolved,
    /// Built at the first turn: credentials resolve then, not at
    /// boot, so a run that opens without a login — the first-run
    /// screen — finds a login that lands before its first turn.
    slot: OnceLock<Box<dyn Provider>>,
}

impl Providers {
    fn entry(&self, instance: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.resolved.instance.name == instance)
    }

    /// The provider of `instance`, built on the first call. A failed
    /// build is not kept: the next call tries again, so a login that
    /// lands between two prompts takes effect at the next one.
    fn get(&self, instance: &str) -> Result<&dyn Provider, RunError> {
        let entry = self.entry(instance).ok_or_else(|| {
            RunError::Config(format!("no provider instance `{instance}` in this run"))
        })?;
        if let Some(provider) = entry.slot.get() {
            return Ok(provider.as_ref());
        }
        let provider = build_provider(&entry.resolved.instance, self.auth.as_ref())?;
        Ok(entry.slot.get_or_init(|| provider).as_ref())
    }
}

impl ProviderHost for Providers {
    fn provider(&self, instance: &str) -> Result<&dyn Provider, ProviderError> {
        self.get(instance).map_err(|error| match error {
            RunError::Auth(error) => error,
            other => ProviderError::InvalidRequest(other.to_string()),
        })
    }

    fn kind(&self, instance: &str) -> Option<&str> {
        self.entry(instance)
            .map(|entry| entry.resolved.instance.kind.as_str())
    }
}

impl Runtime {
    /// Build the long-lived pieces: the session store, the tool
    /// registries, and the checks that stop a run at boot — a
    /// `--session <id>` with no file, a provider id this build does
    /// not have. The credentials resolve at the first turn; see
    /// `Runtime::provider`.
    pub fn new(mut config: RunConfig) -> Result<Self, RunError> {
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
        let mut head = config.instance.clone();
        head.model = (!config.model.is_empty()).then(|| config.model.clone());
        if let Some(problem) = config::instance_problems(&head).into_iter().next() {
            return Err(RunError::Config(problem));
        }
        let head = config::Resolved {
            instance: head,
            effort: config.effort,
            effort_origin: config.effort_origin,
        };
        let entries = std::iter::once(head)
            .chain(std::mem::take(&mut config.resumable))
            .map(|resolved| Entry {
                resolved,
                slot: OnceLock::new(),
            })
            .collect();
        let providers = Providers {
            entries,
            auth: config.auth.clone(),
        };

        Ok(Self {
            config,
            providers,
            registry,
            read_only_registry,
            store,
        })
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// The provider of the run's head instance, built on the first
    /// call. Credentials resolve here, so a missing login fails with
    /// a clear message. The plain interface calls this at boot, and
    /// [`Runtime::open`] for the head target of the session it opens,
    /// so a missing login stops both before a request goes out or a
    /// record is written.
    pub(crate) fn provider(&self) -> Result<&dyn Provider, RunError> {
        self.providers.get(&self.config.instance.name)
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
        let instance = &self.config.instance;
        // An instance with its own key variable, and an adapter
        // without credentials of its own, have no login to offer: the
        // first turn names the missing variable.
        let own_credentials = demerzel_providers::adapter_rules(&instance.kind)
            .is_some_and(|rules| rules.own_credentials);
        if instance.api_key_env.is_some() || !own_credentials {
            return None;
        }
        let env_key = std::env::var(anthropic::ENV_API_KEY)
            .ok()
            .filter(|key| !key.is_empty());
        let provider = instance.kind.as_str();
        // The environment wins and skips the store, the order the
        // provider build applies.
        if provider == anthropic::PROVIDER_ID && env_key.is_some() {
            return None;
        }
        let store = open_store(self.config.auth.as_ref());
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

    /// Open the session for the first prompt and its engine, the one
    /// call that starts a session. The session is the one `resume`
    /// names when it exists and matches this project, else a fresh
    /// one — except that a session named by id must exist and match,
    /// or the open fails.
    ///
    /// A resume continues on the session's own head target: the
    /// instance and the model of its last meta record. A flag or a
    /// variable keeps the whole head of `settings`, and `/model` keeps
    /// its model (see [`SessionSettings::route_origin`]). When config
    /// can no longer run the recorded instance, the session continues
    /// on the head of `settings` and [`Opened::notice`] says so.
    /// [`Opened::settings`] holds the target the session runs on; a
    /// failed open leaves the caller's settings as they were.
    ///
    /// A session opens only with a provider to run it: a missing
    /// login fails here, before a record is written. A resume appends
    /// a fresh meta record, so the file names the target that
    /// continues it.
    pub fn open(
        &self,
        resume: &Resume,
        settings: &SessionSettings,
    ) -> Result<Opened<'_>, RunError> {
        let mut settings = settings.clone();
        let path = match resume {
            Resume::Fresh => None,
            Resume::Latest => self.store.latest(&self.config.project_dir)?,
            Resume::Id(id) => Some(find_session(&self.store, &self.config.project_dir, id)?),
        };
        let mut resumed = None;
        if let Some(path) = path {
            let (session, replay) = Session::resume(&path)?;
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
                resumed = Some((session, replay));
            } else if let Resume::Id(id) = resume {
                return Err(RunError::Config(format!(
                    "session `{id}` belongs to another project"
                )));
            }
        }
        let mut notice = None;
        let mut adopted = None;
        if let Some((session, replay)) = &resumed
            && let Some(meta) = &replay.meta
        {
            match self.adopt(meta, &mut settings) {
                Ok(true) => adopted = Some((session.id().to_string(), meta.provider.clone())),
                Ok(false) => {}
                Err(why) => notice = Some(fallback_notice(session.id(), meta, &settings, &why)),
            }
        }
        // The credentials of the target the session runs on. A
        // missing login on an adopted instance names the session's
        // instance, so the user knows why this instance runs.
        if let Err(error) = self.providers.get(&settings.route.head.instance) {
            return Err(match adopted {
                Some((id, instance)) => RunError::Config(format!(
                    "session {id} ran on provider `{instance}`: {error}. Pass --provider to \
                     continue it on another provider."
                )),
                None => error,
            });
        }
        let (session, replay) = match resumed {
            Some((mut session, replay)) => {
                session.append_meta(&SessionMeta::now(&self.session_open(&settings)))?;
                (session, replay)
            }
            None => {
                let session = self.store.create(&self.session_open(&settings))?;
                (session, Replay::default())
            }
        };
        if let Some(notice) = &notice {
            log::warn!("{notice}");
        }

        let mut engine_config = EngineConfig::new(settings.route.clone());
        engine_config.effort = self.effort_wire(&settings).request();
        engine_config.max_model_calls = self.config.max_model_calls;
        let mut parts = EngineParts::new(
            &self.providers,
            self.tools(settings.approval),
            ApprovalPolicy::new(settings.approval),
            engine_config,
            session,
        );
        parts.history = replay.messages;
        Ok(Opened {
            engine: Engine::new(parts),
            settings,
            usage: replay.usage,
            tools: replay.tools,
            notice,
        })
    }

    /// Adopt the head target that a resumed session's last meta
    /// record names, as far as `settings.route_origin` allows: a flag
    /// or a variable keeps the whole head, and `/model` keeps the
    /// model. `Ok(true)` when the open adopted another instance. A
    /// switch to another instance takes that instance's configured
    /// effort, unless `/effort` set one. `Err` says why config can no
    /// longer run the recorded instance.
    fn adopt(&self, meta: &SessionMeta, settings: &mut SessionSettings) -> Result<bool, String> {
        let keep_model = match settings.route_origin {
            Origin::Flag | Origin::Env => return Ok(false),
            Origin::Command => true,
            _ => false,
        };
        let head = &settings.route.head;
        let model = if keep_model { &head.model } else { &meta.model };
        if meta.provider == head.instance && *model == head.model {
            return Ok(false);
        }
        let Some(entry) = self.providers.entry(&meta.provider) else {
            return Err(format!(
                "The config does not define provider `{}`.",
                meta.provider
            ));
        };
        let instance = &entry.resolved.instance;
        // The name stayed, the adapter changed: the recorded model
        // belongs to another protocol.
        if let Some(kind) = &meta.kind
            && *kind != instance.kind
        {
            return Err(format!(
                "The config now defines provider `{}` as {}, not {kind}.",
                meta.provider, instance.kind
            ));
        }
        let mut check = instance.clone();
        check.model = Some(model.clone());
        if let Some(problem) = config::instance_problems(&check).into_iter().next() {
            return Err(format!("The config cannot run it: {problem}."));
        }
        let other_instance = meta.provider != head.instance;
        if other_instance && settings.effort_origin != Origin::Command {
            settings.effort = entry.resolved.effort;
            settings.effort_origin = entry.resolved.effort_origin;
        }
        settings.route.head = Target::new(&*meta.provider, model.clone());
        Ok(other_instance)
    }

    /// The instance keys of `instance`; `None` for an instance this
    /// run does not know.
    fn instance(&self, instance: &str) -> Option<&config::Instance> {
        self.providers
            .entry(instance)
            .map(|entry| &entry.resolved.instance)
    }

    /// What the requests to the head target carry for the session's
    /// effort: the one answer for the request, the status, and the
    /// `/effort` note. The rule follows the head target's adapter
    /// and its `thinking` key.
    pub fn effort_wire(&self, settings: &SessionSettings) -> EffortWire {
        let instance = self.instance(&settings.route.head.instance);
        demerzel_providers::effort_wire(
            instance.map_or("", |instance| instance.kind.as_str()),
            instance.and_then(|instance| instance.thinking_mode),
            settings.effort,
            settings.effort_set(),
        )
    }

    /// What a meta record names for this run and these settings.
    fn session_open<'a>(&'a self, settings: &'a SessionSettings) -> SessionOpen<'a> {
        let head = &settings.route.head;
        let kind = self
            .instance(&head.instance)
            .map_or(head.instance.as_str(), |instance| instance.kind.as_str());
        let mut open =
            SessionOpen::new(&self.config.project_dir, &head.instance, kind, &head.model);
        open.effort = self.effort_wire(settings).request();
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

    /// The context window of `target`, in tokens: its instance's
    /// `context_window` key when set — the config wins, so a stale
    /// catalog row has an override — or the provider catalog's
    /// number for the instance's adapter and the target's model.
    pub(crate) fn context_window(&self, target: &Target) -> Option<u64> {
        let instance = self.instance(&target.instance)?;
        instance
            .context_window
            .or_else(|| demerzel_providers::context_window(&instance.kind, &target.model))
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

    /// Run one prompt to completion. Display events go to `sink`; the
    /// returned outcome says how the turn ended. A fallback notice of
    /// the open goes to stderr, as the interfaces show theirs.
    pub async fn one_shot(
        &self,
        prompt: String,
        sink: &dyn EventSink,
    ) -> Result<TurnOutcome, RunError> {
        let settings = SessionSettings::from_config(&self.config);
        let mut opened = self.open(&self.config.resume, &settings)?;
        if let Some(notice) = &opened.notice {
            eprintln!("demerzel: {notice}");
        }
        Ok(opened.engine.user_turn(prompt, sink, &mut NoPrompt).await?)
    }
}

/// The auth store of this run: the configured paths, or the defaults.
fn open_store(auth: Option<&AuthPaths>) -> Result<AuthStore, RunError> {
    match auth {
        Some(paths) => {
            AuthStore::open(paths.store.clone(), paths.codex_cli.clone()).map_err(RunError::Auth)
        }
        None => AuthStore::open_default().map_err(RunError::Auth),
    }
}

/// The stored key of instance `name`, for `base_url`. The read takes
/// the instance's entry alone, so a run without `HOME` or with a
/// section this build cannot read still builds; no data directory
/// means no store and no key. A store that is not JSON stops the
/// turn: it can hold the key the endpoint needs. A key checked at
/// another endpoint stops it too: the key goes only to the endpoint
/// the login named.
fn stored_instance_key(
    name: &str,
    auth: Option<&AuthPaths>,
    base_url: &str,
) -> Result<Option<String>, RunError> {
    let path = match auth {
        Some(paths) => paths.store.clone(),
        None => match AuthStore::default_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        },
    };
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

/// Build the provider of `instance`, with its credentials.
fn build_provider(
    instance: &config::Instance,
    auth: Option<&AuthPaths>,
) -> Result<Box<dyn Provider>, RunError> {
    let name = &instance.name;
    // The instance's own key variable wins over every other source.
    // Named but unset, it stops the turn with its name.
    let named_key = match &instance.api_key_env {
        Some(variable) => Some(
            std::env::var(variable)
                .ok()
                .filter(|key| !key.is_empty())
                .ok_or_else(|| {
                    RunError::Auth(ProviderError::Auth(format!(
                        "provider `{name}` reads its key from {variable}, and {variable} \
                         is unset or empty"
                    )))
                })?,
        ),
        None => None,
    };
    match instance.kind.as_str() {
        openai_codex::PROVIDER_ID => {
            let mut store = open_store(auth)?;
            store.ensure().map_err(RunError::Auth)?;
            let mut codex_config = CodexConfig {
                instance: name.clone(),
                ..CodexConfig::default()
            };
            if let Some(base_url) = &instance.base_url {
                codex_config.base_url = base_url.clone();
            }
            Ok(Box::new(OpenAiCodex::new(codex_config, store)))
        }
        anthropic::PROVIDER_ID => {
            // The environment wins and an empty variable counts as
            // unset — the order `AuthStore::resolve_anthropic_key`
            // owns. The store opens only when the environment misses,
            // so a broken store file cannot block a run that carries
            // its key in the environment.
            let api_key = match named_key.or_else(|| {
                std::env::var(anthropic::ENV_API_KEY)
                    .ok()
                    .filter(|key| !key.is_empty())
            }) {
                Some(key) => key,
                None => open_store(auth)?
                    .resolve_anthropic_key(None)
                    .map(|(key, _)| key)
                    .ok_or_else(|| {
                        RunError::Auth(ProviderError::Auth(
                            anthropic::NO_CREDENTIALS_HELP.to_string(),
                        ))
                    })?,
            };
            let mut anthropic_config = AnthropicConfig {
                instance: name.clone(),
                ..AnthropicConfig::default()
            };
            if let Some(base_url) = &instance.base_url {
                anthropic_config.base_url = base_url.clone();
            }
            anthropic_config.thinking = instance.thinking_mode.unwrap_or_default();
            Ok(Box::new(Anthropic::new(anthropic_config, api_key)))
        }
        kind @ (openai_compat::PROVIDER_ID | openai_responses::PROVIDER_ID) => {
            // The boot check requires the endpoint: these adapters
            // have no default one. The key comes from the instance's
            // own variable, else from the instance's entry in the
            // store, else nowhere: no header.
            let base_url = instance
                .base_url
                .clone()
                .ok_or_else(|| RunError::Config(format!("provider `{name}` needs `base_url`")))?;
            let key = match named_key {
                Some(key) => Some(key),
                None => stored_instance_key(name, auth, &base_url)?,
            };
            let provider: Box<dyn Provider> = if kind == openai_compat::PROVIDER_ID {
                let compat_config = CompatConfig::new(name.clone(), base_url);
                Box::new(OpenAiCompat::new(compat_config, key))
            } else {
                let responses_config = ResponsesConfig::new(name.clone(), base_url);
                Box::new(OpenAiResponses::new(responses_config, key))
            };
            Ok(provider)
        }
        _ => Err(unknown_provider(name)),
    }
}

/// The notice for a resume that continues on the head of `settings`,
/// not on the session's own target.
fn fallback_notice(id: &str, meta: &SessionMeta, settings: &SessionSettings, why: &str) -> String {
    let head = &settings.route.head;
    format!(
        "session {id} ran on provider `{}`, model {}. {why} \
         The session continues on provider `{}`, model {}.",
        meta.provider, meta.model, head.instance, head.model
    )
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
