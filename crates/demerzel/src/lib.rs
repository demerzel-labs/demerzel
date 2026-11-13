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
mod prompt;
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
    /// The route of the model calls: the head target, then the
    /// fallback targets of the default role.
    pub route: Route,
    /// What a resume keeps of `route.head`; see [`HeadPin`].
    pub pin: HeadPin,
    /// The level `/effort` set for the session. Every target of the
    /// route receives it. `None`: each target receives the level that
    /// config gives its instance, under its adapter's rule; see
    /// [`Runtime::effort_wire_for`].
    pub effort: Option<Effort>,
    pub approval: ApprovalMode,
}

/// What a resume keeps of the settings' head target. A resume
/// adopts the head target of the session's last meta record, as far
/// as the pin allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HeadPin {
    /// Nothing: config named the head. A resume adopts the session's
    /// instance and model.
    None,
    /// The model: `/model <model>` named it. A resume adopts the
    /// session's instance and keeps the model.
    Model,
    /// The whole head: a flag or a variable, or
    /// `/model <provider> <model>`, named it. A resume keeps it.
    Target,
}

impl SessionSettings {
    /// The settings a run starts with.
    pub fn from_config(config: &RunConfig) -> Self {
        let mut route = Route::new(Target::new(&*config.instance.name, &*config.model));
        route.tail.clone_from(&config.tail);
        let pin = match config.route_origin {
            Origin::Flag | Origin::Env => HeadPin::Target,
            _ => HeadPin::None,
        };
        Self {
            route,
            pin,
            effort: None,
            approval: config.approval,
        }
    }

    /// `/model`: switch the head target's model.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.route.head.model = model.into();
        if self.pin == HeadPin::None {
            self.pin = HeadPin::Model;
        }
    }

    /// `/effort`: set the level for the turns that follow, on every
    /// target.
    pub fn set_effort(&mut self, effort: Effort) {
        self.effort = Some(effort);
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
    /// The fallback targets of the default role, after the head.
    /// [`Runtime::new`] refuses a target on an instance the run does
    /// not know.
    pub tail: Vec<Target>,
    /// Which layer named the head target; see
    /// [`config::Settings::route_origin`]. A flag or a variable wins
    /// over a resumed session's own target.
    pub route_origin: Origin,
    /// Every configured instance other than the head, resolved; see
    /// [`config::run_instances`]. A fallback target, a resume, and
    /// `/model <provider> <model>` can name only the head instance
    /// and these.
    pub other_instances: Vec<config::Resolved>,
    pub approval: ApprovalMode,
    /// Which session the first prompt opens.
    pub resume: Resume,
    /// Session data root override; `None` uses the XDG default.
    pub data_root: Option<PathBuf>,
    /// The session's reasoning effort.
    pub effort: Effort,
    /// The layer that set `effort`; `Default` when none did. An
    /// adapter without implicit effort receives only a set level, and
    /// a file sets one for it only through the instance's own
    /// `[providers.<name>]` table.
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
    /// The user's config directory. The system prompt takes the
    /// user's profile and the user's instruction file from it.
    /// `None` reads no user file: the prompt holds the base prompt
    /// and the project's instruction file alone.
    pub config_dir: Option<PathBuf>,
    /// The file whose content replaces the base prompt, the path
    /// `prompt.base_file` resolves to. `None` keeps the harness
    /// default. [`Runtime::new`] refuses a file it cannot read.
    pub prompt_base_file: Option<PathBuf>,
}

impl RunConfig {
    /// The defaults for a non-interactive run: read-only, fresh
    /// session, default endpoints.
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            instance: config::Instance::new(config::DEFAULT_PROVIDER, config::DEFAULT_PROVIDER),
            model: openai_codex::DEFAULT_MODEL.to_string(),
            tail: Vec::new(),
            route_origin: Origin::Default,
            other_instances: Vec::new(),
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
            config_dir: None,
            prompt_base_file: None,
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
    /// The content of `prompt.base_file`, read at boot. `None` keeps
    /// the harness default.
    base_prompt: Option<String>,
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
    /// One line for the user per instruction file or profile that
    /// the system prompt cut at a size limit or left out. Empty when
    /// the prompt holds every file whole.
    pub prompt_notes: Vec<String>,
}

/// The run's provider instances: the configured head first, then
/// every other configured instance. Each has one lazy provider slot,
/// so a provider lives as long as the runtime and a failed build
/// retries at the next call.
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

impl Entry {
    /// What requests to this instance carry when the session sets no
    /// level: the level config gives the instance, under the
    /// instance's adapter rule.
    fn effort_wire(&self) -> EffortWire {
        let resolved = &self.resolved;
        demerzel_providers::effort_wire(
            &resolved.instance.kind,
            resolved.instance.thinking_mode,
            resolved.effort,
            resolved.effort_origin != Origin::Default,
        )
    }
}

impl ProviderHost for Providers {
    /// The boot check refuses a route target or a `/model` instance
    /// that the run does not know, so a lookup finds every name the
    /// engine asks for. A build that fails on its config, such as a
    /// missing endpoint, maps to an invalid request.
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

    fn effort(&self, instance: &str) -> Option<demerzel_core::Effort> {
        self.entry(instance)
            .and_then(|entry| entry.effort_wire().request())
    }
}

impl Runtime {
    /// Build the long-lived pieces: the session store, the tool
    /// registries, and the checks that stop a run at boot — a
    /// `--session <id>` with no file, a provider id this build does
    /// not have, a fallback target on an instance the run does not
    /// know, a base prompt file that does not read. The credentials
    /// resolve at the first turn; see
    /// `Runtime::check_credentials`.
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
            .chain(std::mem::take(&mut config.other_instances))
            .map(|resolved| Entry {
                resolved,
                slot: OnceLock::new(),
            })
            .collect();
        let providers = Providers {
            entries,
            auth: config.auth.clone(),
        };
        // A fallback target on an instance the run does not know stops
        // the run here, so the host never meets an unknown name.
        for target in &config.tail {
            let Some(entry) = providers.entry(&target.instance) else {
                return Err(RunError::Config(format!(
                    "the fallback chain names provider `{}`, which the config does not define",
                    target.instance
                )));
            };
            let mut check = entry.resolved.instance.clone();
            check.model = Some(target.model.clone());
            if let Some(problem) = config::instance_problems(&check).into_iter().next() {
                return Err(RunError::Config(problem));
            }
        }

        // The user named the base prompt file: a file that does not
        // read stops the run here, not at the first prompt. The
        // content stays fixed for the run.
        let base_prompt = match &config.prompt_base_file {
            Some(path) => Some(prompt::read_base(path).map_err(RunError::Config)?),
            None => None,
        };

        Ok(Self {
            config,
            providers,
            registry,
            read_only_registry,
            store,
            base_prompt,
        })
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// Check that a target of `route` builds its provider: the head,
    /// or a fallback target. Credentials resolve here, so a missing
    /// key fails with a clear message: the head's error when no
    /// target builds. A build does not prove a login: an adapter
    /// whose login refreshes at the first request builds without one,
    /// and that request names the failure. The plain interface checks
    /// the configured route at boot, and [`Runtime::open`] the route
    /// of the session it opens, before a request goes out or a
    /// record is written. A tail target builds only when the head
    /// does not.
    pub(crate) fn check_credentials(&self, route: &Route) -> Result<(), RunError> {
        let Err(error) = self.providers.get(&route.head.instance) else {
            return Ok(());
        };
        let tail_runs = route
            .tail
            .iter()
            .any(|target| self.providers.get(&target.instance).is_ok());
        if tail_runs {
            log::warn!(
                "provider `{}` does not build: {error}; the calls fall back",
                route.head.instance
            );
            return Ok(());
        }
        Err(error)
    }

    /// The first-run screen's text when no credential resolves for
    /// any target of the configured route; `None` when one does. The
    /// decision comes first and the text second: a run that can
    /// authenticate pays the probe alone, and an Anthropic session
    /// with its key in the environment opens no store at all. The
    /// state lines come from the read-only probe `doctor` runs, with
    /// its wording: the store stays unwritten, and a Codex CLI login
    /// that the first turn would import counts as a credential. A
    /// store that does not open is a state to show, not a stop: the
    /// first turn fails on it with the same error. A resumed session
    /// can continue on another instance; [`Runtime::open`] checks
    /// that one and names it.
    pub fn first_run(&self) -> Option<FirstRun> {
        let env_key = std::env::var(anthropic::ENV_API_KEY)
            .ok()
            .filter(|key| !key.is_empty());
        let mut store = None;
        let route = SessionSettings::from_config(&self.config).route;
        for target in route.targets() {
            let instance = self.instance(&target.instance)?;
            // An instance with its own key variable runs when the
            // variable holds a key; unset, it cannot run, and no login
            // helps it. An adapter without credentials of its own can
            // run without a key.
            if let Some(variable) = &instance.api_key_env {
                if std::env::var(variable).is_ok_and(|key| !key.is_empty()) {
                    return None;
                }
                continue;
            }
            let own_credentials = demerzel_providers::adapter_rules(&instance.kind)
                .is_some_and(|rules| rules.own_credentials);
            if !own_credentials {
                return None;
            }
            // The environment wins and skips the store, the order the
            // provider build applies.
            if instance.kind == anthropic::PROVIDER_ID && env_key.is_some() {
                return None;
            }
            let store = store.get_or_insert_with(|| open_store(self.config.auth.as_ref()));
            let resolves = match (instance.kind.as_str(), &*store) {
                (_, Err(_)) => false,
                (openai_codex::PROVIDER_ID, Ok(store)) => {
                    store.probe().is_some_and(|probe| probe.source.usable())
                }
                (anthropic::PROVIDER_ID, Ok(store)) => store.anthropic_api_key().is_some(),
                // The boot check refuses any other id; the build's
                // error says so at the first turn.
                _ => return None,
            };
            if resolves {
                return None;
            }
        }
        let provider = self.config.instance.kind.as_str();
        let store = store.unwrap_or_else(|| open_store(self.config.auth.as_ref()));
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
    /// The open composes the system prompt of the session, once: the
    /// base prompt, the user's profile, and the instruction files of
    /// the user and the project. `first_prompt` is the prompt that
    /// starts the session; the composer retrieves for it. Every model
    /// call of the session sends the same text, to every target of
    /// the route. A resume composes again from the profile and the
    /// instruction files as they are now; the base prompt stays as
    /// [`Runtime::new`] read it. [`Opened::prompt_notes`] names each
    /// file that the prompt cut or left out.
    ///
    /// A resume continues on the session's own head target: the
    /// instance and the model of its last meta record, as far as
    /// [`SessionSettings::pin`] allows. When config
    /// can no longer run the recorded instance, the session continues
    /// on the head of `settings` and [`Opened::notice`] says so.
    /// [`Opened::settings`] holds the target the session runs on; a
    /// failed open leaves the caller's settings as they were.
    ///
    /// A session opens only when a target of its route builds its
    /// provider: a missing key on every target fails here, before a
    /// record is written. A resume appends
    /// a fresh meta record, so the file names the target that
    /// continues it.
    pub fn open(
        &self,
        resume: &Resume,
        settings: &SessionSettings,
        first_prompt: &str,
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
        // The credentials of the route the session runs on: the head
        // target, or a fallback target. A missing login on an adopted
        // instance names the session's instance, so the user knows why
        // this instance runs.
        if let Err(error) = self.check_credentials(&settings.route) {
            return Err(match adopted {
                Some((id, instance)) => RunError::Config(format!(
                    "session {id} ran on provider `{instance}`: {error}. Pass --provider to \
                     continue it on another provider."
                )),
                None => error,
            });
        }
        // The prompt comes before the session's record, so a later
        // record can name the prompt the session runs under.
        let mut composed = prompt::compose(self.prompt_sources(), first_prompt);
        let prompt_notes = composed.notes();
        log::info!(
            "prompt composed: {} bytes, base {}, {}",
            composed.text.len(),
            if self.base_prompt.is_some() {
                "replaced"
            } else {
                "default"
            },
            prompt::file_list(&composed)
        );
        let system_prompt = std::mem::take(&mut composed.text);
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
        engine_config.system_prompt = system_prompt;
        // A level from `/effort` reaches every target. Otherwise each
        // target gets the level config gives its instance, from the
        // host.
        engine_config.effort = settings.effort;
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
            prompt_notes,
        })
    }

    /// Adopt the head target that a resumed session's last meta
    /// record names, as far as `settings.pin` allows. `Ok(true)` when
    /// the open adopted another instance; its requests then carry the
    /// effort config gives it, unless `/effort` set one. `Err` says
    /// why config can no longer run the recorded instance.
    fn adopt(&self, meta: &SessionMeta, settings: &mut SessionSettings) -> Result<bool, String> {
        let keep_model = match settings.pin {
            HeadPin::Target => return Ok(false),
            HeadPin::Model => true,
            HeadPin::None => false,
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
        settings.route.head = Target::new(&*meta.provider, model.clone());
        Ok(other_instance)
    }

    /// The instance keys of `instance`; `None` for an instance this
    /// run does not know.
    pub(crate) fn instance(&self, instance: &str) -> Option<&config::Instance> {
        self.providers
            .entry(instance)
            .map(|entry| &entry.resolved.instance)
    }

    /// What the requests to the head target carry for the session's
    /// effort: the one answer for the request, the status, and the
    /// `/effort` note. The rule follows the head target's adapter
    /// and its `thinking` key.
    pub fn effort_wire(&self, settings: &SessionSettings) -> EffortWire {
        self.effort_wire_for(settings, &settings.route.head.instance)
    }

    /// What the requests to `instance` carry in a session with
    /// `settings`: the level `/effort` set, else the level config
    /// gives the instance, each under the instance's adapter rule.
    /// Each attempt of a fallback chain follows the rule of its own
    /// target, as the engine does through [`ProviderHost::effort`].
    pub fn effort_wire_for(&self, settings: &SessionSettings, instance: &str) -> EffortWire {
        let Some(entry) = self.providers.entry(instance) else {
            // The boot check and `/model` refuse an unknown instance.
            return EffortWire::Default;
        };
        match settings.effort {
            Some(effort) => demerzel_providers::effort_wire(
                &entry.resolved.instance.kind,
                entry.resolved.instance.thinking_mode,
                effort,
                true,
            ),
            None => entry.effort_wire(),
        }
    }

    /// `/model <provider> <model>`: `settings` with the head target
    /// on instance `instance` and model `model`. The tail stays. The
    /// requests to another instance carry the effort config gives it,
    /// unless `/effort` set one, as after a resume. `Err` says why
    /// the run cannot switch: an instance config does not define, or
    /// one the boot check refuses with this model. Credentials
    /// resolve at the next model call, where a missing login falls
    /// back.
    pub fn switch_head(
        &self,
        settings: &SessionSettings,
        instance: &str,
        model: &str,
    ) -> Result<SessionSettings, String> {
        let Some(entry) = self.providers.entry(instance) else {
            let mut known: Vec<&str> = self
                .providers
                .entries
                .iter()
                .map(|entry| entry.resolved.instance.name.as_str())
                .collect();
            known.sort_unstable();
            return Err(format!(
                "the config does not define provider `{instance}`; the run knows: {}",
                known.join(", ")
            ));
        };
        let mut check = entry.resolved.instance.clone();
        check.model = Some(model.to_string());
        if let Some(problem) = config::instance_problems(&check).into_iter().next() {
            return Err(problem);
        }
        let mut next = settings.clone();
        next.route.head = Target::new(instance, model);
        next.pin = HeadPin::Target;
        Ok(next)
    }

    /// What the composer reads for a session of this run.
    fn prompt_sources(&self) -> prompt::Sources<'_> {
        prompt::Sources {
            base: self.base_prompt.as_deref(),
            user_dir: self.config.config_dir.as_deref(),
            project_dir: &self.config.project_dir,
        }
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
    /// the open and its prompt notes go to stderr, as the interfaces
    /// show theirs.
    pub async fn one_shot(
        &self,
        prompt: String,
        sink: &dyn EventSink,
    ) -> Result<TurnOutcome, RunError> {
        let settings = SessionSettings::from_config(&self.config);
        let mut opened = self.open(&self.config.resume, &settings, &prompt)?;
        for notice in opened.notice.iter().chain(&opened.prompt_notes) {
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
