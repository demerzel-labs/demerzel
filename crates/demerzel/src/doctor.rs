//! `demerzel doctor`: report the state a session depends on. The
//! report shows presence, not values — never token or key text.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use demerzel_core::{ApprovalMode, SessionStore};
use demerzel_providers::auth::{AuthStore, CredentialSource, NO_CREDENTIALS_HELP};
use demerzel_providers::{AdapterRules, anthropic, openai_codex};

use crate::config::{self, Flags, Layer, Locations, Origin};

/// Run the checks and print the report to stdout. Returns the process
/// exit code: 0 when every check passes, 1 otherwise.
pub fn run(project_dir: &Path, flags: &Flags) -> i32 {
    let mut out = std::io::stdout().lock();
    // A closed pipe must not turn a healthy machine into exit 101.
    match report(project_dir, flags, &mut out) {
        Ok(0) => 0,
        _ => 1,
    }
}

/// Write the full report to `out` and return the problem count.
pub fn report(project_dir: &Path, flags: &Flags, out: &mut dyn Write) -> std::io::Result<u32> {
    let mut problems = 0u32;
    let locations = Locations::locate(project_dir);

    writeln!(out, "config files")?;
    let user = report_file("user", locations.user.as_deref(), &mut problems, out)?;
    let project = report_file("project", Some(&locations.project), &mut problems, out)?;

    let (settings, ignored) = config::resolve(
        flags,
        &|name| std::env::var(name).ok(),
        project.as_ref(),
        user.as_ref(),
    );
    writeln!(out, "active settings")?;
    for warning in &ignored {
        writeln!(out, "  warning: {warning}")?;
    }
    print_setting(
        out,
        "provider",
        &settings.provider.value,
        settings.provider.origin,
    )?;
    print_setting(
        out,
        "kind",
        settings.kind.value.as_deref().unwrap_or("unknown"),
        settings.kind.origin,
    )?;
    print_setting(
        out,
        "model",
        settings.model.value.as_deref().unwrap_or("none"),
        settings.model.origin,
    )?;
    if let Some(variable) = &settings.api_key_env.value {
        print_setting(out, "api_key_env", variable, settings.api_key_env.origin)?;
    }
    let approval = match settings.approval.value {
        Some(mode) => mode.to_string(),
        None => "ask in an interactive session, read-only for one-shot".to_string(),
    };
    print_setting(out, "approval", &approval, settings.approval.origin)?;
    let rules = settings
        .kind
        .value
        .as_deref()
        .and_then(demerzel_providers::adapter_rules);
    // The request shape: an adapter without implicit effort receives
    // only a set level, and an instance with thinking off receives
    // none.
    let thinking = settings.thinking_mode.value;
    let effort_sent = settings.effort_set || rules.is_none_or(|rules| rules.implicit_effort);
    let effort = if thinking.is_some_and(|mode| !mode.sends_effort()) {
        format!(
            "{} — not sent: this instance turns thinking off",
            settings.effort.value
        )
    } else if effort_sent {
        settings.effort.value.as_str().to_string()
    } else {
        "default — not sent to this adapter".to_string()
    };
    print_setting(out, "effort", &effort, settings.effort.origin)?;
    if thinking.is_some() || rules.is_some_and(|rules| rules.thinking_switch) {
        let mode = thinking.unwrap_or_default();
        let shape = if mode.sends_effort() {
            format!("{mode} — sends {mode} thinking and the effort")
        } else {
            format!("{mode} — sends disabled thinking and no effort")
        };
        print_setting(out, "thinking", &shape, settings.thinking_mode.origin)?;
    }
    print_setting(
        out,
        "max_model_calls",
        &settings.max_model_calls.value.to_string(),
        settings.max_model_calls.origin,
    )?;
    print_setting(
        out,
        "history_file",
        on_off(settings.history_file.value),
        settings.history_file.origin,
    )?;
    print_setting(
        out,
        "tui.mouse",
        on_off(settings.tui.mouse.value),
        settings.tui.mouse.origin,
    )?;
    print_setting(
        out,
        "tui.markdown",
        on_off(settings.tui.markdown.value),
        settings.tui.markdown.origin,
    )?;
    print_setting(
        out,
        "tui.thinking",
        settings.tui.thinking.value.as_str(),
        settings.tui.thinking.origin,
    )?;
    print_setting(
        out,
        "tui.theme",
        &settings.tui.theme.value,
        settings.tui.theme.origin,
    )?;
    if let Err(error) = config::theme(&settings.tui.theme.value, user.as_ref()) {
        problems += 1;
        writeln!(out, "  problem: {error}")?;
    }
    let base_url = settings
        .base_url
        .value
        .as_deref()
        .unwrap_or("provider default");
    print_setting(out, "base_url", base_url, settings.base_url.origin)?;
    let context_window = settings.context_window.value.or_else(|| {
        demerzel_providers::context_window(
            settings.kind.value.as_deref()?,
            settings.model.value.as_deref()?,
        )
    });
    let context_window = match context_window {
        Some(window) => window.to_string(),
        None => "unknown".to_string(),
    };
    print_setting(
        out,
        "context_window",
        &context_window,
        settings.context_window.origin,
    )?;
    for problem in settings.problems() {
        problems += 1;
        writeln!(out, "  problem: {problem}")?;
    }

    // The instance keys read the store's path, the way a session does,
    // so a run without HOME still finds them.
    let store_path = AuthStore::default_path().ok();
    report_instances(
        &config::instances(user.as_ref()),
        store_path.as_deref(),
        out,
    )?;
    report_credentials(&settings, store_path.as_deref(), &mut problems, out)?;
    // A read-only session never writes the project root, so a
    // non-writable root is only a note there.
    let read_only = settings.approval.value == Some(ApprovalMode::ReadOnly);
    report_tools(project_dir, read_only, &mut problems, out)?;

    match problems {
        0 => writeln!(out, "result: ok — a session can start")?,
        1 => writeln!(out, "result: 1 problem")?,
        n => writeln!(out, "result: {n} problems")?,
    }
    Ok(problems)
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn print_setting(
    out: &mut dyn Write,
    name: &str,
    value: &str,
    origin: Origin,
) -> std::io::Result<()> {
    writeln!(out, "  {name} = {value} ({origin})")
}

/// Report one config file and return its parsed layer. The file is
/// read and parsed once; the secret scan walks the same table.
fn report_file(
    label: &str,
    path: Option<&Path>,
    problems: &mut u32,
    out: &mut dyn Write,
) -> std::io::Result<Option<Layer>> {
    let Some(path) = path else {
        writeln!(out, "  {label}: none — HOME is not set")?;
        return Ok(None);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writeln!(out, "  {label}: {} — none", path.display())?;
            return Ok(None);
        }
        Err(error) => {
            *problems += 1;
            writeln!(out, "  problem: cannot read {}: {error}", path.display())?;
            return Ok(None);
        }
    };
    let table = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => {
            *problems += 1;
            writeln!(out, "  problem: {}: {error}", path.display())?;
            return Ok(None);
        }
    };
    writeln!(out, "  {label}: {} — found", path.display())?;
    let mut secrets = Vec::new();
    scan_table(&table, "", &mut secrets);
    match Layer::from_table(table) {
        Ok((layer, warnings)) => {
            for warning in warnings.iter().chain(&secrets) {
                writeln!(out, "  warning: {label} file: {warning}")?;
            }
            Ok(Some(layer))
        }
        Err(message) => {
            *problems += 1;
            for warning in &secrets {
                writeln!(out, "  warning: {label} file: {warning}")?;
            }
            writeln!(out, "  problem: {}: {message}", path.display())?;
            Ok(None)
        }
    }
}

/// One provider's credential state: the wording `doctor` reports
/// and the first-run screen shows, from a read-only probe.
pub(crate) struct CredentialState {
    pub provider: &'static str,
    /// The state text after the provider's name.
    pub state: String,
    /// Where the credential comes from; `None` when none resolves.
    pub source: Option<CredentialSource>,
}

impl CredentialState {
    /// Whether a session with this provider can authenticate.
    pub fn usable(&self) -> bool {
        self.source.is_some_and(CredentialSource::usable)
    }
}

/// The credential state of each provider from one store. The
/// environment key wins for the API-key state and skips the store,
/// the order a session applies. Reads only: the store stays
/// unwritten, and no key text leaves it.
pub(crate) fn credential_states(store: &AuthStore, env_key: Option<&str>) -> [CredentialState; 2] {
    let codex = match store.probe() {
        Some(probe) => {
            let mut state = probe.source.describe().to_string();
            if probe.source.usable() {
                match probe.expires {
                    Some(when) if when <= SystemTime::now() => {
                        state.push_str(" — the access token is expired; the next run refreshes it")
                    }
                    Some(when) => {
                        let _ = write!(
                            state,
                            " — the access token expires {}",
                            humantime::format_rfc3339_seconds(when)
                        );
                    }
                    None => {}
                }
            }
            CredentialState {
                provider: openai_codex::PROVIDER_ID,
                state,
                source: Some(probe.source),
            }
        }
        None => CredentialState {
            provider: openai_codex::PROVIDER_ID,
            state: format!("none — {NO_CREDENTIALS_HELP}"),
            source: None,
        },
    };
    let anthropic_source = match env_key.filter(|key| !key.is_empty()) {
        Some(_) => Some(CredentialSource::Env),
        None => store.anthropic_api_key().map(|_| CredentialSource::Store),
    };
    let anthropic = match anthropic_source {
        Some(source) => {
            let mut state = source.describe().to_string();
            if source == CredentialSource::Env {
                let _ = write!(state, " ({})", anthropic::ENV_API_KEY);
            }
            CredentialState {
                provider: anthropic::PROVIDER_ID,
                state,
                source: Some(source),
            }
        }
        None => CredentialState {
            provider: anthropic::PROVIDER_ID,
            state: format!("none — {}", anthropic::NO_CREDENTIALS_HELP),
            source: None,
        },
    };
    [codex, anthropic]
}

/// List the instances the user file defines: the adapter, the
/// endpoint, the model, and where the key comes from. Presence, not
/// values: a key variable shows its name and whether it is set, a
/// stored key only that the store holds one for the endpoint. An
/// instance that could not run gets a warning; the active one's
/// problems count below.
fn report_instances(
    instances: &[config::Instance],
    store_path: Option<&Path>,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "provider instances")?;
    if instances.is_empty() {
        writeln!(
            out,
            "  none in the user file; the adapters {} stand for themselves",
            demerzel_providers::PROVIDER_IDS.join(", ")
        )?;
        return Ok(());
    }
    for instance in instances {
        let Some(rules) = demerzel_providers::adapter_rules(&instance.kind) else {
            continue;
        };
        let endpoint = match (&instance.base_url, rules.needs_endpoint) {
            (Some(url), _) => url.as_str(),
            (None, true) => "none — set base_url",
            (None, false) => "the adapter's default",
        };
        let has_default = demerzel_providers::default_model(&instance.kind).is_some();
        let model = match (&instance.model, has_default) {
            (Some(model), _) => model.as_str(),
            (None, false) => "none — set model",
            (None, true) => "the adapter's default",
        };
        let stored = stored_key(store_path, &instance.name, instance.base_url.as_deref());
        let thinking = match instance.thinking_mode {
            Some(mode) => format!(", thinking {mode}"),
            None => String::new(),
        };
        writeln!(
            out,
            "  {}: {} — endpoint {endpoint}, model {model}, key {}{thinking}",
            instance.name,
            instance.kind,
            key_state(
                &instance.name,
                instance.api_key_env.as_deref(),
                rules,
                stored
            )
        )?;
        for problem in config::instance_problems(instance) {
            writeln!(out, "  warning: {problem}")?;
        }
    }
    Ok(())
}

/// What the store holds for one instance, read the way a session
/// reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    None,
    /// A key for the instance's endpoint.
    Serves,
    /// A key checked at another endpoint: a session stops on it.
    OtherEndpoint,
    /// The store does not read: a session stops on it.
    Unreadable,
}

fn stored_key(store_path: Option<&Path>, name: &str, base_url: Option<&str>) -> Stored {
    let Some(path) = store_path else {
        return Stored::None;
    };
    match AuthStore::read_instance_key(path, name) {
        Err(_) => Stored::Unreadable,
        Ok(None) => Stored::None,
        // Without an endpoint the run stops on that first, with its own
        // problem line.
        Ok(Some(key)) if base_url.is_none_or(|url| key.serves(url)) => Stored::Serves,
        Ok(Some(_)) => Stored::OtherEndpoint,
    }
}

/// Where an instance's key comes from, by name and presence. The
/// variable wins over the store, the order a session applies.
fn key_state(name: &str, api_key_env: Option<&str>, rules: AdapterRules, stored: Stored) -> String {
    match api_key_env {
        Some(variable) if env_set(variable) => format!("from {variable} (set)"),
        Some(variable) => format!("from {variable} (unset)"),
        None if rules.own_credentials => "from the adapter's credentials below".to_string(),
        None => match stored {
            Stored::Serves => "in the dmz store".to_string(),
            Stored::OtherEndpoint => format!(
                "in the dmz store for another endpoint — run `dmz login {name}` for this one"
            ),
            Stored::Unreadable => "unknown — the auth store does not read".to_string(),
            Stored::None => "none — requests carry no Authorization header".to_string(),
        },
    }
}

fn env_set(variable: &str) -> bool {
    std::env::var(variable).is_ok_and(|value| !value.is_empty())
}

/// Report every provider's credentials. Missing credentials count as
/// a problem only for the active provider: the other provider's state
/// is information, not a blocker.
fn report_credentials(
    settings: &config::Settings,
    store_path: Option<&Path>,
    problems: &mut u32,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "credentials")?;
    let name = &settings.provider.value;
    let kind = settings.kind.value.as_deref().unwrap_or("");
    let rules = demerzel_providers::adapter_rules(kind);
    // An instance with its own key variable, or an adapter without
    // credentials of its own, reports its own line; the adapter lines
    // below stay information.
    let own_line =
        settings.api_key_env.value.is_some() || rules.is_some_and(|rules| !rules.own_credentials);
    if let Some(rules) = rules
        && own_line
    {
        let variable = settings.api_key_env.value.as_deref();
        let stored = match variable {
            Some(_) => Stored::None,
            None => stored_key(store_path, name, settings.base_url.value.as_deref()),
        };
        // The session stops on an unset variable, a key for another
        // endpoint, and a store that does not read.
        let stops = variable.is_some_and(|variable| !env_set(variable))
            || matches!(stored, Stored::OtherEndpoint | Stored::Unreadable);
        if stops {
            *problems += 1;
        }
        let prefix = if stops { "problem: " } else { "" };
        writeln!(
            out,
            "  {prefix}{name}: key {}",
            key_state(name, variable, rules, stored)
        )?;
    }
    let active_provider = if own_line { "" } else { kind };
    let store = match AuthStore::open_default() {
        Ok(store) => store,
        // An instance with its own key line reads its entry alone, so
        // the adapters' store is information then.
        Err(error) if own_line => {
            writeln!(out, "  {error}")?;
            return Ok(());
        }
        Err(error) => {
            *problems += 1;
            writeln!(out, "  problem: {error}")?;
            return Ok(());
        }
    };
    let env_key = std::env::var(anthropic::ENV_API_KEY).ok();
    for state in credential_states(&store, env_key.as_deref()) {
        let active = state.provider == active_provider;
        if active && !state.usable() {
            *problems += 1;
        }
        // A source that is there but cannot authenticate — an import
        // file that does not parse — names the problem on its line.
        let prefix = if active && state.source.is_some() && !state.usable() {
            "problem: "
        } else {
            ""
        };
        writeln!(out, "  {prefix}{}: {}", state.provider, state.state)?;
    }
    Ok(())
}

fn report_tools(
    project_dir: &Path,
    read_only: bool,
    problems: &mut u32,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "tools")?;
    match find_in_path("bash") {
        Some(path) => writeln!(out, "  bash: {}", path.display())?,
        None => {
            *problems += 1;
            writeln!(out, "  problem: bash is not on PATH")?;
        }
    }
    match probe_writable(project_dir) {
        Ok(_) => writeln!(out, "  project root: writable — {}", project_dir.display())?,
        Err(error) if read_only => {
            writeln!(
                out,
                "  project root: not writable ({error}) — fine for a read-only session"
            )?;
        }
        Err(error) => {
            *problems += 1;
            writeln!(
                out,
                "  problem: project root {} is not writable: {error}",
                project_dir.display()
            )?;
        }
    }
    match SessionStore::open_default() {
        Ok(store) => {
            let dir = store.sessions_dir(project_dir);
            match probe_writable(&dir) {
                Ok(probed) if probed == dir => {
                    writeln!(out, "  session directory: writable — {}", dir.display())?;
                }
                Ok(probed) => {
                    writeln!(
                        out,
                        "  session directory: writable — {} (checked {})",
                        dir.display(),
                        probed.display()
                    )?;
                }
                Err(error) => {
                    *problems += 1;
                    writeln!(
                        out,
                        "  problem: session directory {} is not writable: {error}",
                        dir.display()
                    )?;
                }
            }
        }
        Err(error) => {
            *problems += 1;
            writeln!(out, "  problem: session directory: {error}")?;
        }
    }
    Ok(())
}

/// Check that a session could write under `dir` without creating it:
/// probe the nearest existing ancestor with a short-lived file.
/// Returns the directory the probe wrote to, so the report names
/// what was checked.
fn probe_writable(dir: &Path) -> std::io::Result<&Path> {
    let mut target = dir;
    while !target.exists() {
        target = target
            .parent()
            .ok_or_else(|| std::io::Error::other("no existing ancestor"))?;
    }
    let probe = target.join(".dmz-doctor-probe");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)?;
    Ok(target)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Key names that suggest a credential.
const SECRET_WORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "credential",
    "auth",
    "bearer",
];

/// Value prefixes of common credential formats.
const SECRET_PREFIXES: &[&str] = &["sk-", "sk_", "ghp_", "gho_", "xox", "eyJ"];

fn scan_table(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        scan_value(key, &path, value, out);
    }
}

fn scan_value(key: &str, path: &str, value: &toml::Value, out: &mut Vec<String>) {
    match value {
        toml::Value::Table(sub) => scan_table(sub, path, out),
        toml::Value::Array(items) => {
            for item in items {
                scan_value(key, path, item, out);
            }
        }
        // `api_key_env` holds a variable's name, never the key; the
        // config parser refuses any value that is not a name.
        toml::Value::String(text) if key == "api_key_env" && !secret_value(text) => {}
        toml::Value::String(text) if secret_name(key) || secret_value(text) => {
            out.push(format!(
                "`{path}` looks like a secret; move it to the auth store"
            ));
        }
        _ => {}
    }
}

fn secret_name(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| key.contains(word))
}

fn secret_value(text: &str) -> bool {
    SECRET_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warnings(text: &str) -> Vec<String> {
        let table = text.parse::<toml::Table>().unwrap();
        let mut out = Vec::new();
        scan_table(&table, "", &mut out);
        out
    }

    #[test]
    fn secret_names_are_flagged_in_any_case_and_nesting() {
        let found = warnings(
            "API_KEY = \"abc\"\n\
             AuthToken = \"abc\"\n\
             [providers.anthropic]\n\
             Secret = \"x\"",
        );
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].contains("`API_KEY`"));
        assert!(found[1].contains("`AuthToken`"));
        assert!(found[2].contains("`providers.anthropic.Secret`"));
    }

    #[test]
    fn secret_values_are_flagged_by_shape_and_inside_arrays() {
        let found = warnings(
            "model = \"gpt-5.6-sol\"\n\
             a = \"sk-fake-live\"\n\
             b = \"ghp_fake\"\n\
             c = \"eyJhbGci\"\n\
             extras = [\"xoxb-fake\", \"plain\"]",
        );
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found.iter().any(|w| w.contains("`extras`")));
    }

    #[test]
    fn plain_settings_are_not_flagged() {
        assert!(
            warnings(
                "model = \"gpt-5.6-sol\"\n\
                 provider = \"openai-codex\"\n\
                 history_file = true\n\
                 [providers.openai-codex]\n\
                 base_url = \"http://localhost:1\"",
            )
            .is_empty()
        );
    }

    #[test]
    fn bash_resolves_on_this_machine() {
        assert!(find_in_path("bash").is_some());
    }

    #[test]
    fn writable_probe_creates_nothing_and_names_the_checked_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b");
        let probed = probe_writable(&target).unwrap();
        assert_eq!(probed, dir.path());
        assert!(!target.exists());
        assert!(!dir.path().join("a").exists());
        assert!(!dir.path().join(".dmz-doctor-probe").exists());
    }
}
