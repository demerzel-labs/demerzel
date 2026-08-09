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

use demerzel_core::ApprovalMode;

use crate::DEFAULT_MODEL;

/// The provider id every build ships.
pub const DEFAULT_PROVIDER: &str = "openai-codex";

/// Environment variables, one per flag.
pub const ENV_PROVIDER: &str = "DEMERZEL_PROVIDER";
pub const ENV_MODEL: &str = "DEMERZEL_MODEL";
pub const ENV_BASE_URL: &str = "DEMERZEL_BASE_URL";

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
    pub history_file: Setting<bool>,
}

impl Settings {
    /// The approval mode for one run: the configured mode when one is
    /// set, else the M1 rule.
    pub fn approval_mode(&self, interactive: bool) -> ApprovalMode {
        self.approval.value.unwrap_or(if interactive {
            ApprovalMode::Ask
        } else {
            ApprovalMode::ReadOnly
        })
    }
}

/// Values from command-line flags. `main` fills it from clap.
#[derive(Debug, Default)]
pub struct Flags {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub approval: Option<ApprovalMode>,
}

/// Per-provider keys from a `[providers.<id>]` table.
#[derive(Debug, Default, PartialEq)]
struct ProviderKeys {
    base_url: Option<String>,
    model: Option<String>,
}

/// One parsed config file.
#[derive(Debug, Default, PartialEq)]
pub struct Layer {
    provider: Option<String>,
    model: Option<String>,
    approval: Option<ApprovalMode>,
    history_file: Option<bool>,
    providers: BTreeMap<String, ProviderKeys>,
}

impl Layer {
    /// Parse one config file. Unknown keys land in the returned
    /// warnings; a known key with a bad value is an error.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        let table = text
            .parse::<toml::Table>()
            .map_err(|error| error.to_string())?;
        let mut layer = Self::default();
        let mut warnings = Vec::new();
        for (key, value) in table {
            match key.as_str() {
                "provider" => layer.provider = Some(expect_string(&key, value)?),
                "model" => layer.model = Some(expect_string(&key, value)?),
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
                _ => warnings.push(format!("unknown key `{path}`")),
            }
        }
        into.insert(id, keys);
    }
    Ok(())
}

fn expect_string(key: &str, value: toml::Value) -> Result<String, String> {
    match value {
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
/// is injected, so tests need no process environment.
pub fn resolve(
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<String>,
    project: Option<&Layer>,
    user: Option<&Layer>,
) -> Settings {
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
    // than the top-level `model`, so it wins.
    let layer_model = |layer: Option<&Layer>, origin| {
        layer_key(layer, origin, |l| {
            l.providers
                .get(&provider.value)
                .and_then(|p| p.model.clone())
                .or_else(|| l.model.clone())
        })
    };
    let model = flag(&flags.model)
        .map(|v| Setting::new(v, Origin::Flag))
        .or_else(|| env(ENV_MODEL).map(|v| Setting::new(v, Origin::Env)))
        .or_else(|| layer_model(project, Origin::Project))
        .or_else(|| layer_model(user, Origin::User))
        // The anthropic default model lands with its adapter.
        .unwrap_or_else(|| Setting::new(DEFAULT_MODEL.to_string(), Origin::Default));

    let layer_base_url = |layer: Option<&Layer>, origin| {
        layer_key(layer, origin, |l| {
            l.providers
                .get(&provider.value)
                .and_then(|p| p.base_url.clone())
        })
    };
    let base_url = flag(&flags.base_url)
        .map(|v| Setting::new(Some(v), Origin::Flag))
        .or_else(|| env(ENV_BASE_URL).map(|v| Setting::new(Some(v), Origin::Env)))
        .or_else(|| layer_base_url(project, Origin::Project).map(some_setting))
        .or_else(|| layer_base_url(user, Origin::User).map(some_setting))
        .unwrap_or_else(|| Setting::new(None, Origin::Default));

    let approval = flags
        .approval
        .map(|mode| Setting::new(Some(mode), Origin::Flag))
        .or_else(|| layer_key(project, Origin::Project, |l| l.approval).map(some_setting))
        .or_else(|| layer_key(user, Origin::User, |l| l.approval).map(some_setting))
        .unwrap_or_else(|| Setting::new(None, Origin::Default));

    let history_file = layer_key(project, Origin::Project, |l| l.history_file)
        .or_else(|| layer_key(user, Origin::User, |l| l.history_file))
        .unwrap_or_else(|| Setting::new(true, Origin::Default));

    Settings {
        provider,
        model,
        base_url,
        approval,
        history_file,
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

    fn layer(text: &str) -> Layer {
        let (layer, warnings) = Layer::parse(text).unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        layer
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let settings = resolve(&Flags::default(), &no_env, None, None);
        assert_eq!(settings.provider.value, DEFAULT_PROVIDER);
        assert_eq!(settings.provider.origin, Origin::Default);
        assert_eq!(settings.model.value, DEFAULT_MODEL);
        assert_eq!(settings.base_url.value, None);
        assert_eq!(settings.approval.value, None);
        assert!(settings.history_file.value);
        assert_eq!(settings.approval_mode(true), ApprovalMode::Ask);
        assert_eq!(settings.approval_mode(false), ApprovalMode::ReadOnly);
    }

    #[test]
    fn flags_beat_environment() {
        let flags = Flags {
            model: Some("from-flag".into()),
            ..Flags::default()
        };
        let env = |name: &str| (name == ENV_MODEL).then(|| "from-env".to_string());
        let settings = resolve(&flags, &env, None, None);
        assert_eq!(settings.model.value, "from-flag");
        assert_eq!(settings.model.origin, Origin::Flag);
    }

    #[test]
    fn environment_beats_the_project_file() {
        let project = layer("model = \"from-project\"");
        let env = |name: &str| (name == ENV_MODEL).then(|| "from-env".to_string());
        let settings = resolve(&Flags::default(), &env, Some(&project), None);
        assert_eq!(settings.model.value, "from-env");
        assert_eq!(settings.model.origin, Origin::Env);
    }

    #[test]
    fn the_project_file_beats_the_user_file() {
        let project = layer("model = \"from-project\"");
        let user = layer("model = \"from-user\"");
        let settings = resolve(&Flags::default(), &no_env, Some(&project), Some(&user));
        assert_eq!(settings.model.value, "from-project");
        assert_eq!(settings.model.origin, Origin::Project);
    }

    #[test]
    fn the_user_file_beats_the_defaults() {
        let user = layer("model = \"from-user\"\napproval = \"auto\"\nhistory_file = false");
        let settings = resolve(&Flags::default(), &no_env, None, Some(&user));
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
        let settings = resolve(&flags, &env, None, None);
        assert_eq!(settings.model.value, DEFAULT_MODEL);
        assert_eq!(settings.model.origin, Origin::Default);
    }

    #[test]
    fn the_provider_table_beats_the_top_level_model_in_one_layer() {
        let project = layer(
            "model = \"top-level\"\n\
             [providers.openai-codex]\n\
             model = \"from-table\"\n\
             base_url = \"http://project.example\"",
        );
        let settings = resolve(&Flags::default(), &no_env, Some(&project), None);
        assert_eq!(settings.model.value, "from-table");
        assert_eq!(
            settings.base_url.value.as_deref(),
            Some("http://project.example")
        );
        assert_eq!(settings.base_url.origin, Origin::Project);
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
        let settings = resolve(&Flags::default(), &no_env, None, Some(&user));
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
        let settings = resolve(&flags, &no_env, None, Some(&user));
        assert_eq!(settings.provider.origin, Origin::Flag);
        assert_eq!(
            settings.base_url.value.as_deref(),
            Some("http://other.example")
        );
    }

    #[test]
    fn unknown_keys_warn_and_do_not_stop_the_parse() {
        let (layer, warnings) = Layer::parse(
            "model = \"m\"\n\
             future_key = 1\n\
             [providers.openai-codex]\n\
             future_sub_key = \"x\"",
        )
        .unwrap();
        assert_eq!(layer.model.as_deref(), Some("m"));
        assert_eq!(
            warnings,
            vec![
                "unknown key `future_key`",
                "unknown key `providers.openai-codex.future_sub_key`",
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
