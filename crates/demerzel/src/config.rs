//! Layered TOML configuration for the demerzel bin.
//!
//! Three layers plus defaults, highest wins: command-line flags, then
//! their environment variables, then the project file
//! `.demerzel/config.toml`, then the user file
//! `$XDG_CONFIG_HOME/demerzel/config.toml`, then built-in defaults.
//! Resolution happens once at boot into [`crate::RunConfig`]; the
//! engine and the tools never read config files.
//!
//! Unknown keys warn and do not stop the run, so an old binary stays
//! usable on a newer config file. A known key with a bad value is an
//! error: a typo in `approval` must not silently fall back to a more
//! permissive mode.
//!
//! The resolver takes the environment as a closure, so the precedence
//! tests run hermetically, with no process-global environment writes.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use demerzel_core::{ApprovalMode, Effort};

/// The provider id a run uses when nothing picks one.
pub const DEFAULT_PROVIDER: &str = demerzel_providers::openai_codex::PROVIDER_ID;
/// The reasoning effort when nothing sets one. High: the Messages
/// API's own default, and the level a coding agent wants.
pub const DEFAULT_EFFORT: Effort = Effort::High;

/// Environment variables, one per flag.
pub const ENV_PROVIDER: &str = "DEMERZEL_PROVIDER";
pub const ENV_MODEL: &str = "DEMERZEL_MODEL";
pub const ENV_BASE_URL: &str = "DEMERZEL_BASE_URL";
pub const ENV_EFFORT: &str = "DEMERZEL_EFFORT";

/// Which layer set a key. `doctor` prints it next to each value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Flag,
    Env,
    Project,
    User,
    Default,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Origin::Flag => "flag",
            Origin::Env => "environment",
            Origin::Project => "project file",
            Origin::User => "user file",
            Origin::Default => "default",
        })
    }
}

/// One resolved key and the layer that set it.
#[derive(Debug, Clone)]
pub struct Setting<T> {
    pub value: T,
    pub origin: Origin,
}

impl<T> Setting<T> {
    fn new(value: T, origin: Origin) -> Self {
        Self { value, origin }
    }
}

/// The resolved configuration for one run.
#[derive(Debug, Clone)]
pub struct Settings {
    pub provider: Setting<String>,
    pub model: Setting<String>,
    /// `None` keeps the provider's default endpoint.
    pub base_url: Setting<Option<String>>,
    /// `None` keeps the M1 rule: the REPL asks, one-shot is read-only.
    pub approval: Setting<Option<ApprovalMode>>,
    /// The reasoning effort every request carries.
    pub effort: Setting<Effort>,
    pub history_file: Setting<bool>,
    /// The `[tui]` table.
    pub tui: TuiSettings,
}

/// The resolved `[tui]` keys.
#[derive(Debug, Clone)]
pub struct TuiSettings {
    /// Mouse capture in the full-screen interface, for wheel scroll.
    pub mouse: Setting<bool>,
    /// Markdown rendering of model replies in the full-screen
    /// interface; off shows the raw text.
    pub markdown: Setting<bool>,
}

impl Settings {
    /// The approval mode for one run: the configured mode when one is
    /// set, else the M1 rule. A one-shot run has nobody to ask, so
    /// `ask` clamps to read-only there — the M1 behavior.
    pub fn approval_mode(&self, interactive: bool) -> ApprovalMode {
        match self.approval.value {
            Some(ApprovalMode::Ask) | None if !interactive => ApprovalMode::ReadOnly,
            Some(mode) => mode,
            None => ApprovalMode::Ask,
        }
    }
}

/// Values from command-line flags. `main` fills it from clap.
#[derive(Debug, Default)]
pub struct Flags {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub approval: Option<ApprovalMode>,
    pub effort: Option<Effort>,
}

/// Per-provider keys from a `[providers.<id>]` table.
#[derive(Debug, Default, PartialEq)]
struct ProviderKeys {
    base_url: Option<String>,
    model: Option<String>,
    effort: Option<Effort>,
}

/// Interface keys from the `[tui]` table.
#[derive(Debug, Default, PartialEq)]
struct TuiKeys {
    mouse: Option<bool>,
    markdown: Option<bool>,
}

/// One parsed config file.
#[derive(Debug, Default, PartialEq)]
pub struct Layer {
    provider: Option<String>,
    model: Option<String>,
    approval: Option<ApprovalMode>,
    effort: Option<Effort>,
    history_file: Option<bool>,
    providers: BTreeMap<String, ProviderKeys>,
    tui: TuiKeys,
}

impl Layer {
    /// Parse one config file. Unknown keys land in the returned
    /// warnings; a known key with a bad value is an error.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        let table = text
            .parse::<toml::Table>()
            .map_err(|error| error.to_string())?;
        Self::from_table(table)
    }

    /// Build a layer from an already parsed table. `doctor` parses
    /// once and scans the same table for secret shapes.
    pub fn from_table(table: toml::Table) -> Result<(Self, Vec<String>), String> {
        let mut layer = Self::default();
        let mut warnings = Vec::new();
        for (key, value) in table {
            match key.as_str() {
                "provider" => layer.provider = Some(expect_string(&key, value)?),
                "model" => layer.model = Some(expect_string(&key, value)?),
                "effort" => layer.effort = Some(expect_effort(&key, value)?),
                "approval" => {
                    layer.approval = Some(parse_approval(&expect_string(&key, value)?)?);
                }
                "history_file" => match value {
                    toml::Value::Boolean(flag) => layer.history_file = Some(flag),
                    other => {
                        return Err(format!(
                            "`history_file` must be a boolean, not {}",
                            other.type_str()
                        ));
                    }
                },
                "providers" => match value {
                    toml::Value::Table(tables) => {
                        parse_providers(tables, &mut layer.providers, &mut warnings)?;
                    }
                    other => {
                        return Err(format!(
                            "`providers` must be a table, not {}",
                            other.type_str()
                        ));
                    }
                },
                "tui" => match value {
                    toml::Value::Table(table) => {
                        parse_tui(table, &mut layer.tui, &mut warnings)?;
                    }
                    other => {
                        return Err(format!("`tui` must be a table, not {}", other.type_str()));
                    }
                },
                _ => warnings.push(format!("unknown key `{key}`")),
            }
        }
        Ok((layer, warnings))
    }
}

fn parse_providers(
    tables: toml::Table,
    into: &mut BTreeMap<String, ProviderKeys>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    for (id, value) in tables {
        let toml::Value::Table(table) = value else {
            return Err(format!(
                "`providers.{id}` must be a table, not {}",
                value.type_str()
            ));
        };
        let mut keys = ProviderKeys::default();
        for (key, value) in table {
            let path = format!("providers.{id}.{key}");
            match key.as_str() {
                "base_url" => keys.base_url = Some(expect_string(&path, value)?),
                "model" => keys.model = Some(expect_string(&path, value)?),
                "effort" => keys.effort = Some(expect_effort(&path, value)?),
                _ => warnings.push(format!("unknown key `{path}`")),
            }
        }
        into.insert(id, keys);
    }
    Ok(())
}

fn parse_tui(
    table: toml::Table,
    into: &mut TuiKeys,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    for (key, value) in table {
        let path = format!("tui.{key}");
        match key.as_str() {
            "mouse" => into.mouse = Some(expect_bool(&path, value)?),
            "markdown" => into.markdown = Some(expect_bool(&path, value)?),
            _ => warnings.push(format!("unknown key `{path}`")),
        }
    }
    Ok(())
}

fn expect_effort(key: &str, value: toml::Value) -> Result<Effort, String> {
    expect_string(key, value)?
        .parse()
        .map_err(|error| format!("`{key}` {error}"))
}

fn expect_bool(key: &str, value: toml::Value) -> Result<bool, String> {
    match value {
        toml::Value::Boolean(flag) => Ok(flag),
        other => Err(format!(
            "`{key}` must be a boolean, not {}",
            other.type_str()
        )),
    }
}

fn expect_string(key: &str, value: toml::Value) -> Result<String, String> {
    match value {
        toml::Value::String(text) if text.is_empty() => Err(format!("`{key}` must not be empty")),
        toml::Value::String(text) => Ok(text),
        other => Err(format!(
            "`{key}` must be a string, not {}",
            other.type_str()
        )),
    }
}

fn parse_approval(text: &str) -> Result<ApprovalMode, String> {
    match text {
        "read-only" => Ok(ApprovalMode::ReadOnly),
        "ask" => Ok(ApprovalMode::Ask),
        "auto" => Ok(ApprovalMode::Auto),
        other => Err(format!(
            "`approval` must be `read-only`, `ask`, or `auto`, not `{other}`"
        )),
    }
}

/// A config file the run cannot use.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{}: {message}", .path.display())]
    Invalid { path: PathBuf, message: String },
    #[error("cannot read {}: {error}", .path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
}

/// Read and parse the file at `path`. A missing file is `Ok(None)`.
pub fn load(path: &Path) -> Result<Option<(Layer, Vec<String>)>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                error,
            });
        }
    };
    match Layer::parse(&text) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(message) => Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            message,
        }),
    }
}

/// The config file locations for one run.
#[derive(Debug, Clone)]
pub struct Locations {
    /// `None` when no home directory resolves.
    pub user: Option<PathBuf>,
    pub project: PathBuf,
}

impl Locations {
    pub fn locate(project_dir: &Path) -> Self {
        Self {
            user: demerzel_core::paths::config_dir()
                .ok()
                .map(|dir| dir.join("config.toml")),
            project: project_dir.join(".demerzel").join("config.toml"),
        }
    }
}

/// Resolve flags, environment, and file layers into settings. `env`
/// is injected, so tests need no process environment. The returned
/// warnings name project-file keys the trust rule ignored; the caller
/// prints them.
///
/// The trust rule: a project file travels with the repository, so it
/// must not raise privilege or move credentials. It cannot raise the
/// approval mode, it cannot set an endpoint, and it cannot turn the
/// history file or mouse capture on against a user who turned them
/// off.
pub fn resolve(
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<String>,
    project: Option<&Layer>,
    user: Option<&Layer>,
) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    // An empty flag or variable counts as unset, like an empty
    // `XDG_DATA_HOME` in the path rules.
    let flag = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
    let env = |name: &str| env(name).filter(|v| !v.is_empty());

    // The provider resolves first: the model and base_url lookups
    // below key off the active provider's table.
    let provider = flag(&flags.provider)
        .map(|v| Setting::new(v, Origin::Flag))
        .or_else(|| env(ENV_PROVIDER).map(|v| Setting::new(v, Origin::Env)))
        .or_else(|| layer_key(project, Origin::Project, |l| l.provider.clone()))
        .or_else(|| layer_key(user, Origin::User, |l| l.provider.clone()))
        .unwrap_or_else(|| Setting::new(DEFAULT_PROVIDER.to_string(), Origin::Default));

    // Within one layer the `[providers.<id>]` table is more specific
    // than the top-level `model`, so it wins. A top-level `model`
    // names one provider's model id: when its layer also names a
    // provider, and that provider is not the active one, the resolver
    // skips the key — a provider switch must not pair the other
    // provider's model with the active adapter (step-3 decision).
    let layer_model = |layer: Option<&Layer>, origin| {
        layer_key(layer, origin, |l| {
            l.providers
                .get(&provider.value)
                .and_then(|p| p.model.clone())
                .or_else(|| match &l.provider {
                    Some(named) if *named != provider.value => None,
                    _ => l.model.clone(),
                })
        })
    };
    let model = flag(&flags.model)
        .map(|v| Setting::new(v, Origin::Flag))
        .or_else(|| env(ENV_MODEL).map(|v| Setting::new(v, Origin::Env)))
        .or_else(|| layer_model(project, Origin::Project))
        .or_else(|| layer_model(user, Origin::User))
        // Each provider carries its own default model. An unknown
        // provider gets the shipped default; the run fails on the
        // provider id before the model matters.
        .unwrap_or_else(|| {
            let default = demerzel_providers::default_model(&provider.value)
                .unwrap_or(demerzel_providers::openai_codex::DEFAULT_MODEL);
            Setting::new(default.to_string(), Origin::Default)
        });

    // Trust rule: the project file cannot pick the endpoint requests
    // — and the credentials on them — go to.
    let flag_or_env_base_url = flag(&flags.base_url)
        .map(|v| Setting::new(v, Origin::Flag))
        .or_else(|| env(ENV_BASE_URL).map(|v| Setting::new(v, Origin::Env)));
    let project_base_url = project.and_then(|l| {
        l.providers
            .get(&provider.value)
            .and_then(|p| p.base_url.as_deref())
    });
    if let Some(url) = project_base_url
        && flag_or_env_base_url.is_none()
    {
        warnings.push(format!(
            "ignored: the project file sets base_url = {url}; \
             set the endpoint with --base-url, {ENV_BASE_URL}, or the user file"
        ));
    }
    let base_url = flag_or_env_base_url
        .map(some_setting)
        .or_else(|| {
            layer_key(user, Origin::User, |l| {
                l.providers
                    .get(&provider.value)
                    .and_then(|p| p.base_url.clone())
            })
            .map(some_setting)
        })
        .unwrap_or_else(|| Setting::new(None, Origin::Default));

    // Trust rule: the project file can lower the approval mode, never
    // raise it. The baseline is the user file, or the default `ask`.
    // A flag decides the mode outright, so no warning then.
    let user_approval = layer_key(user, Origin::User, |l| l.approval);
    let baseline = user_approval
        .as_ref()
        .map(|s| s.value)
        .unwrap_or(ApprovalMode::Ask);
    let project_approval = match layer_key(project, Origin::Project, |l| l.approval) {
        Some(setting) if rank(setting.value) > rank(baseline) => {
            if flags.approval.is_none() {
                warnings.push(format!(
                    "ignored: the project file sets approval = {}; a project file \
                     cannot raise the approval mode; use a flag or the user file",
                    mode_name(setting.value)
                ));
            }
            None
        }
        other => other,
    };
    let approval = flags
        .approval
        .map(|mode| Setting::new(Some(mode), Origin::Flag))
        .or_else(|| project_approval.map(some_setting))
        .or_else(|| user_approval.map(some_setting))
        .unwrap_or_else(|| Setting::new(None, Origin::Default));

    // Within one layer the provider table's `effort` wins over the
    // top-level one; unlike `model`, a level belongs to no provider,
    // so a layer that names another provider still lends its
    // top-level `effort`. Trust rule: the project file can lower the
    // effort, never raise it above the user file's level or the
    // default — a high effort costs more, and a project file travels
    // with the repository. A flag or the environment decides
    // outright, so no warning then.
    let layer_effort = |layer: Option<&Layer>, origin| {
        layer_key(layer, origin, |l| {
            l.providers
                .get(&provider.value)
                .and_then(|p| p.effort)
                .or(l.effort)
        })
    };
    let flag_or_env_effort = flags
        .effort
        .map(|v| Setting::new(v, Origin::Flag))
        .or_else(|| {
            env(ENV_EFFORT).and_then(|value| match value.parse() {
                Ok(effort) => Some(Setting::new(effort, Origin::Env)),
                Err(error) => {
                    warnings.push(format!("ignored: {ENV_EFFORT}: {error}"));
                    None
                }
            })
        });
    let user_effort = layer_effort(user, Origin::User);
    let effort_baseline = user_effort
        .as_ref()
        .map_or(DEFAULT_EFFORT, |setting| setting.value);
    let project_effort = match layer_effort(project, Origin::Project) {
        Some(setting) if setting.value > effort_baseline => {
            if flag_or_env_effort.is_none() {
                warnings.push(format!(
                    "ignored: the project file sets effort = {}; a project file \
                     cannot raise the effort; use --effort, {ENV_EFFORT}, or the user file",
                    setting.value
                ));
            }
            None
        }
        other => other,
    };
    let effort = flag_or_env_effort
        .or(project_effort)
        .or(user_effort)
        .unwrap_or_else(|| Setting::new(DEFAULT_EFFORT, Origin::Default));

    // Trust rule: the project file can turn the history file off,
    // never on against a user who turned it off.
    let user_history = layer_key(user, Origin::User, |l| l.history_file);
    let project_history = match layer_key(project, Origin::Project, |l| l.history_file) {
        Some(setting) if setting.value && user_history.as_ref().is_some_and(|user| !user.value) => {
            warnings.push("ignored: the project file cannot turn the history file on".to_string());
            None
        }
        other => other,
    };
    let history_file = project_history
        .or(user_history)
        .unwrap_or_else(|| Setting::new(true, Origin::Default));

    // Trust rule: the project file can turn mouse capture off, never
    // on against a user who turned it off — capture takes the
    // terminal's own text selection, and the user knows their
    // terminal.
    let user_mouse = layer_key(user, Origin::User, |l| l.tui.mouse);
    let project_mouse = match layer_key(project, Origin::Project, |l| l.tui.mouse) {
        Some(setting) if setting.value && user_mouse.as_ref().is_some_and(|user| !user.value) => {
            warnings.push("ignored: the project file cannot turn mouse capture on".to_string());
            None
        }
        other => other,
    };
    let mouse = project_mouse
        .or(user_mouse)
        .unwrap_or_else(|| Setting::new(true, Origin::Default));

    // Cosmetic: either file can set it, the project file first.
    let markdown = layer_key(project, Origin::Project, |l| l.tui.markdown)
        .or_else(|| layer_key(user, Origin::User, |l| l.tui.markdown))
        .unwrap_or_else(|| Setting::new(true, Origin::Default));

    (
        Settings {
            provider,
            model,
            base_url,
            approval,
            effort,
            history_file,
            tui: TuiSettings { mouse, markdown },
        },
        warnings,
    )
}

/// Privilege order of the approval modes.
fn rank(mode: ApprovalMode) -> u8 {
    match mode {
        ApprovalMode::ReadOnly => 0,
        ApprovalMode::Ask => 1,
        ApprovalMode::Auto => 2,
    }
}

/// The config spelling of an approval mode.
pub fn mode_name(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::ReadOnly => "read-only",
        ApprovalMode::Ask => "ask",
        ApprovalMode::Auto => "auto",
    }
}

fn layer_key<T>(
    layer: Option<&Layer>,
    origin: Origin,
    key: impl Fn(&Layer) -> Option<T>,
) -> Option<Setting<T>> {
    layer.and_then(key).map(|value| Setting::new(value, origin))
}

fn some_setting<T>(setting: Setting<T>) -> Setting<Option<T>> {
    Setting::new(Some(setting.value), setting.origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use demerzel_providers::{anthropic, openai_codex};

    fn layer(text: &str) -> Layer {
        let (layer, warnings) = Layer::parse(text).unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        layer
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// Resolve and assert the trust rule ignored nothing.
    fn resolve_clean(
        flags: &Flags,
        env: &dyn Fn(&str) -> Option<String>,
        project: Option<&Layer>,
        user: Option<&Layer>,
    ) -> Settings {
        let (settings, warnings) = resolve(flags, env, project, user);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        settings
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let settings = resolve_clean(&Flags::default(), &no_env, None, None);
        assert_eq!(settings.provider.value, DEFAULT_PROVIDER);
        assert_eq!(settings.provider.origin, Origin::Default);
        assert_eq!(settings.model.value, openai_codex::DEFAULT_MODEL);
        assert_eq!(settings.base_url.value, None);
        assert_eq!(settings.approval.value, None);
        assert!(settings.history_file.value);
        assert!(settings.tui.mouse.value);
        assert_eq!(settings.tui.mouse.origin, Origin::Default);
        assert_eq!(settings.approval_mode(true), ApprovalMode::Ask);
        assert_eq!(settings.approval_mode(false), ApprovalMode::ReadOnly);
    }

    #[test]
    fn the_project_file_cannot_turn_mouse_capture_on() {
        let user = layer("[tui]\nmouse = false");
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert!(!settings.tui.mouse.value);
        assert_eq!(settings.tui.mouse.origin, Origin::User);

        let project = layer("[tui]\nmouse = true");
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert!(!settings.tui.mouse.value);
        assert_eq!(settings.tui.mouse.origin, Origin::User);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("mouse capture"));

        // Off is always accepted; on over the default is a no-op.
        let project = layer("[tui]\nmouse = false");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), None);
        assert!(!settings.tui.mouse.value);
        assert_eq!(settings.tui.mouse.origin, Origin::Project);

        let project = layer("[tui]\nmouse = true");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), None);
        assert!(settings.tui.mouse.value);
    }

    #[test]
    fn the_markdown_key_resolves_project_over_user_over_default() {
        let settings = resolve_clean(&Flags::default(), &no_env, None, None);
        assert!(settings.tui.markdown.value);
        assert_eq!(settings.tui.markdown.origin, Origin::Default);

        let user = layer("[tui]\nmarkdown = false");
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert!(!settings.tui.markdown.value);
        assert_eq!(settings.tui.markdown.origin, Origin::User);

        // Cosmetic: the project file can turn it either way.
        let project = layer("[tui]\nmarkdown = true");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert!(settings.tui.markdown.value);
        assert_eq!(settings.tui.markdown.origin, Origin::Project);
    }

    #[test]
    fn effort_resolves_by_layer_and_the_project_file_cannot_raise_it() {
        let settings = resolve_clean(&Flags::default(), &no_env, None, None);
        assert_eq!(settings.effort.value, Effort::High);
        assert_eq!(settings.effort.origin, Origin::Default);

        let user = layer("effort = \"low\"\n[providers.openai-codex]\neffort = \"high\"");
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert_eq!(
            settings.effort.value,
            Effort::High,
            "the provider table wins"
        );
        let flags = Flags {
            provider: Some("anthropic".into()),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, None, Some(&user));
        assert_eq!(
            settings.effort.value,
            Effort::Low,
            "another provider takes the top-level key"
        );

        // The project file lowers, never raises: not above the user
        // file's level, and not above the default when the user file
        // is silent.
        let user = layer("effort = \"low\"");
        let project = layer("effort = \"low\"");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(settings.effort.value, Effort::Low);
        assert_eq!(settings.effort.origin, Origin::Project);
        let project = layer("effort = \"medium\"");
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(settings.effort.value, Effort::Low);
        assert_eq!(settings.effort.origin, Origin::User);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("effort"));
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), None);
        assert_eq!(settings.effort.value, Effort::Medium);
        assert_eq!(settings.effort.origin, Origin::Project);

        // A flag or the environment decides outright; a bad variable
        // is ignored with a warning.
        let flags = Flags {
            effort: Some(Effort::High),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, Some(&layer("effort = \"low\"")), None);
        assert_eq!(settings.effort.value, Effort::High);
        assert_eq!(settings.effort.origin, Origin::Flag);
        let env = |name: &str| (name == ENV_EFFORT).then(|| "high".to_string());
        let settings = resolve_clean(&Flags::default(), &env, None, None);
        assert_eq!(settings.effort.origin, Origin::Env);
        let env = |name: &str| (name == ENV_EFFORT).then(|| "extreme".to_string());
        let (settings, warnings) = resolve(&Flags::default(), &env, None, None);
        assert_eq!(settings.effort.value, Effort::High);
        assert!(warnings[0].contains(ENV_EFFORT));

        assert!(
            Layer::parse("effort = \"extreme\"")
                .unwrap_err()
                .contains("effort")
        );
        assert_eq!(
            layer("effort = \"ultra\"").effort,
            Some(Effort::Ultra),
            "every level parses"
        );
    }

    #[test]
    fn flags_beat_environment() {
        let flags = Flags {
            model: Some("from-flag".into()),
            ..Flags::default()
        };
        let env = |name: &str| (name == ENV_MODEL).then(|| "from-env".to_string());
        let settings = resolve_clean(&flags, &env, None, None);
        assert_eq!(settings.model.value, "from-flag");
        assert_eq!(settings.model.origin, Origin::Flag);
    }

    #[test]
    fn environment_beats_the_project_file() {
        let project = layer("model = \"from-project\"");
        let env = |name: &str| (name == ENV_MODEL).then(|| "from-env".to_string());
        let settings = resolve_clean(&Flags::default(), &env, Some(&project), None);
        assert_eq!(settings.model.value, "from-env");
        assert_eq!(settings.model.origin, Origin::Env);
    }

    #[test]
    fn the_project_file_beats_the_user_file() {
        let project = layer("model = \"from-project\"");
        let user = layer("model = \"from-user\"");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(settings.model.value, "from-project");
        assert_eq!(settings.model.origin, Origin::Project);
    }

    #[test]
    fn the_user_file_beats_the_defaults() {
        let user = layer("model = \"from-user\"\napproval = \"auto\"\nhistory_file = false");
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert_eq!(settings.model.value, "from-user");
        assert_eq!(settings.model.origin, Origin::User);
        assert_eq!(settings.approval.value, Some(ApprovalMode::Auto));
        assert!(!settings.history_file.value);
    }

    #[test]
    fn empty_flags_and_variables_count_as_unset() {
        let flags = Flags {
            model: Some(String::new()),
            ..Flags::default()
        };
        let env = |name: &str| (name == ENV_MODEL).then(String::new);
        let settings = resolve_clean(&flags, &env, None, None);
        assert_eq!(settings.model.value, openai_codex::DEFAULT_MODEL);
        assert_eq!(settings.model.origin, Origin::Default);
    }

    #[test]
    fn each_provider_has_its_own_default_model() {
        let flags = Flags {
            provider: Some("anthropic".into()),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, None, None);
        assert_eq!(settings.model.value, anthropic::DEFAULT_MODEL);
        assert_ne!(settings.model.value, openai_codex::DEFAULT_MODEL);
        assert_eq!(settings.model.origin, Origin::Default);
    }

    #[test]
    fn a_provider_switch_skips_the_other_providers_top_level_model() {
        // The user file pairs a provider with a top-level model. A flag
        // switches the provider; the paired model must not follow.
        let user = layer(
            "provider = \"anthropic\"\n\
             model = \"claude-x\"",
        );
        let flags = Flags {
            provider: Some(DEFAULT_PROVIDER.into()),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, None, Some(&user));
        assert_eq!(settings.model.value, openai_codex::DEFAULT_MODEL);
        assert_eq!(settings.model.origin, Origin::Default);

        // A layer that names no provider keeps its top-level model:
        // the author wrote it for whatever provider is active.
        let user = layer("model = \"unscoped-model\"");
        let flags = Flags {
            provider: Some("anthropic".into()),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, None, Some(&user));
        assert_eq!(settings.model.value, "unscoped-model");

        // When the layer's provider matches the active one, its
        // top-level model applies.
        let user = layer(
            "provider = \"anthropic\"\n\
             model = \"claude-x\"",
        );
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert_eq!(settings.provider.value, "anthropic");
        assert_eq!(settings.model.value, "claude-x");
    }

    #[test]
    fn the_provider_table_beats_the_top_level_model_in_one_layer() {
        let project = layer(
            "model = \"top-level\"\n\
             [providers.openai-codex]\n\
             model = \"from-table\"",
        );
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), None);
        assert_eq!(settings.model.value, "from-table");
        assert_eq!(settings.model.origin, Origin::Project);
    }

    #[test]
    fn the_active_provider_picks_its_own_table() {
        let user = layer(
            "provider = \"anthropic\"\n\
             model = \"top-level\"\n\
             [providers.openai-codex]\n\
             model = \"codex-model\"\n\
             [providers.anthropic]\n\
             model = \"anthropic-model\"",
        );
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert_eq!(settings.provider.value, "anthropic");
        assert_eq!(settings.model.value, "anthropic-model");
    }

    #[test]
    fn a_provider_flag_switches_the_table_lookup() {
        let user = layer(
            "[providers.other]\n\
             base_url = \"http://other.example\"",
        );
        let flags = Flags {
            provider: Some("other".into()),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, None, Some(&user));
        assert_eq!(settings.provider.origin, Origin::Flag);
        assert_eq!(
            settings.base_url.value.as_deref(),
            Some("http://other.example")
        );
        assert_eq!(settings.base_url.origin, Origin::User);
    }

    #[test]
    fn the_project_file_cannot_raise_approval() {
        let project = layer("approval = \"auto\"");
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), None);
        assert_eq!(settings.approval.value, None);
        assert_eq!(settings.approval_mode(true), ApprovalMode::Ask);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("cannot raise the approval mode"));

        // `ask` cannot override a user who chose read-only.
        let project = layer("approval = \"ask\"");
        let user = layer("approval = \"read-only\"");
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(settings.approval.value, Some(ApprovalMode::ReadOnly));
        assert_eq!(settings.approval.origin, Origin::User);
        assert_eq!(warnings.len(), 1);

        // A flag decides the mode outright; the warning goes away.
        let project = layer("approval = \"auto\"");
        let flags = Flags {
            approval: Some(ApprovalMode::Auto),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, Some(&project), None);
        assert_eq!(settings.approval.value, Some(ApprovalMode::Auto));
        assert_eq!(settings.approval.origin, Origin::Flag);
    }

    #[test]
    fn the_project_file_can_lower_approval() {
        let project = layer("approval = \"read-only\"");
        let user = layer("approval = \"auto\"");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(settings.approval.value, Some(ApprovalMode::ReadOnly));
        assert_eq!(settings.approval.origin, Origin::Project);
    }

    #[test]
    fn the_project_file_cannot_set_base_url() {
        let project = layer(
            "[providers.openai-codex]\n\
             base_url = \"http://project.example\"",
        );
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), None);
        assert_eq!(settings.base_url.value, None);
        assert_eq!(settings.base_url.origin, Origin::Default);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("http://project.example"));

        // The user file still wins, and a flag silences the warning.
        let user = layer(
            "[providers.openai-codex]\n\
             base_url = \"http://user.example\"",
        );
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(
            settings.base_url.value.as_deref(),
            Some("http://user.example")
        );
        assert_eq!(settings.base_url.origin, Origin::User);
        assert_eq!(warnings.len(), 1);

        let flags = Flags {
            base_url: Some("http://flag.example".into()),
            ..Flags::default()
        };
        let settings = resolve_clean(&flags, &no_env, Some(&project), None);
        assert_eq!(
            settings.base_url.value.as_deref(),
            Some("http://flag.example")
        );
    }

    #[test]
    fn the_project_file_cannot_turn_history_on() {
        let project = layer("history_file = true");
        let user = layer("history_file = false");
        let (settings, warnings) = resolve(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert!(!settings.history_file.value);
        assert_eq!(settings.history_file.origin, Origin::User);
        assert_eq!(warnings.len(), 1);

        // Off is always accepted; on over the default is a no-op.
        let project = layer("history_file = false");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), None);
        assert!(!settings.history_file.value);
        assert_eq!(settings.history_file.origin, Origin::Project);

        let project = layer("history_file = true");
        let settings = resolve_clean(&Flags::default(), &no_env, Some(&project), None);
        assert!(settings.history_file.value);
    }

    #[test]
    fn one_shot_clamps_ask_to_read_only() {
        let user = layer("approval = \"ask\"");
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert_eq!(settings.approval_mode(true), ApprovalMode::Ask);
        assert_eq!(settings.approval_mode(false), ApprovalMode::ReadOnly);

        let user = layer("approval = \"auto\"");
        let settings = resolve_clean(&Flags::default(), &no_env, None, Some(&user));
        assert_eq!(settings.approval_mode(false), ApprovalMode::Auto);
    }

    #[test]
    fn unknown_keys_warn_and_do_not_stop_the_parse() {
        let (layer, warnings) = Layer::parse(
            "model = \"m\"\n\
             future_key = 1\n\
             [providers.openai-codex]\n\
             future_sub_key = \"x\"\n\
             [tui]\n\
             future_tui_key = true",
        )
        .unwrap();
        assert_eq!(layer.model.as_deref(), Some("m"));
        assert_eq!(
            warnings,
            vec![
                "unknown key `future_key`",
                "unknown key `providers.openai-codex.future_sub_key`",
                "unknown key `tui.future_tui_key`",
            ]
        );
    }

    #[test]
    fn bad_values_for_known_keys_are_errors() {
        assert!(
            Layer::parse("approval = \"yolo\"")
                .unwrap_err()
                .contains("approval")
        );
        assert!(Layer::parse("model = 3").unwrap_err().contains("string"));
        assert!(
            Layer::parse("history_file = \"yes\"")
                .unwrap_err()
                .contains("boolean")
        );
        assert!(Layer::parse("providers = 1").unwrap_err().contains("table"));
        assert!(Layer::parse("tui = 1").unwrap_err().contains("table"));
        assert!(
            Layer::parse("[tui]\nmouse = \"off\"")
                .unwrap_err()
                .contains("boolean")
        );
        assert!(Layer::parse("model = \"\"").unwrap_err().contains("empty"));
        assert!(
            Layer::parse("[providers.openai-codex]\nbase_url = \"\"")
                .unwrap_err()
                .contains("empty")
        );
        assert!(Layer::parse("not toml at all [").is_err());
    }

    #[test]
    fn approval_values_parse() {
        assert_eq!(parse_approval("read-only"), Ok(ApprovalMode::ReadOnly));
        assert_eq!(parse_approval("ask"), Ok(ApprovalMode::Ask));
        assert_eq!(parse_approval("auto"), Ok(ApprovalMode::Auto));
        assert!(parse_approval("Auto").is_err());
    }

    #[test]
    fn load_distinguishes_missing_from_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(load(&path).unwrap().is_none());

        std::fs::write(&path, "model = 3").unwrap();
        let error = load(&path).unwrap_err();
        assert!(error.to_string().contains("config.toml"));

        std::fs::write(&path, "model = \"m\"").unwrap();
        let (layer, _) = load(&path).unwrap().unwrap();
        assert_eq!(layer.model.as_deref(), Some("m"));
    }
}
