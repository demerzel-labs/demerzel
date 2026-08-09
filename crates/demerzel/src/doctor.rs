//! `demerzel doctor`: report the state a session depends on. The
//! report shows presence, not values — never token or key text.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use demerzel_core::{ApprovalMode, SessionStore};
use demerzel_providers::openai_codex::AuthStore;
use demerzel_providers::openai_codex::auth::NO_CREDENTIALS_HELP;

use crate::config::{self, Flags, Layer, Locations, Origin, mode_name};

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
    print_setting(out, "model", &settings.model.value, settings.model.origin)?;
    let approval = match settings.approval.value {
        Some(mode) => mode_name(mode).to_string(),
        None => "ask in the REPL, read-only for one-shot".to_string(),
    };
    print_setting(out, "approval", &approval, settings.approval.origin)?;
    let history = if settings.history_file.value {
        "on"
    } else {
        "off"
    };
    print_setting(out, "history_file", history, settings.history_file.origin)?;
    let base_url = settings
        .base_url
        .value
        .as_deref()
        .unwrap_or("provider default");
    print_setting(out, "base_url", base_url, settings.base_url.origin)?;
    if settings.provider.value != config::DEFAULT_PROVIDER {
        problems += 1;
        writeln!(
            out,
            "  problem: provider `{}` is not in this build; `{}` is",
            settings.provider.value,
            config::DEFAULT_PROVIDER
        )?;
    }

    report_credentials(&mut problems, out)?;
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

fn report_credentials(problems: &mut u32, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "credentials")?;
    let store = match AuthStore::open_default() {
        Ok(store) => store,
        Err(error) => {
            *problems += 1;
            writeln!(out, "  problem: {error}")?;
            return Ok(());
        }
    };
    match store.probe() {
        Some(probe) if probe.source.usable() => {
            let expiry = match probe.expires {
                Some(when) if when <= SystemTime::now() => {
                    " — the access token is expired; the next run refreshes it".to_string()
                }
                Some(when) => format!(
                    " — the access token expires {}",
                    humantime::format_rfc3339_seconds(when)
                ),
                None => String::new(),
            };
            writeln!(out, "  openai-codex: {}{expiry}", probe.source.describe())?;
        }
        Some(probe) => {
            *problems += 1;
            writeln!(out, "  problem: openai-codex: {}", probe.source.describe())?;
        }
        None => {
            *problems += 1;
            writeln!(out, "  openai-codex: none — {NO_CREDENTIALS_HELP}")?;
        }
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
        Ok(()) => writeln!(out, "  project root: writable — {}", project_dir.display())?,
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
                Ok(()) => writeln!(out, "  session directory: writable — {}", dir.display())?,
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
fn probe_writable(dir: &Path) -> std::io::Result<()> {
    let mut target = dir;
    while !target.exists() {
        target = target
            .parent()
            .ok_or_else(|| std::io::Error::other("no existing ancestor"))?;
    }
    let probe = target.join(".dmz-doctor-probe");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
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
             a = \"sk-live\"\n\
             b = \"ghp_abc\"\n\
             c = \"eyJhbGci\"\n\
             extras = [\"xoxb-1\", \"plain\"]",
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
    fn writable_probe_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b");
        probe_writable(&target).unwrap();
        assert!(!target.exists());
        assert!(!dir.path().join("a").exists());
        assert!(!dir.path().join(".dmz-doctor-probe").exists());
    }

    #[test]
    fn the_report_runs_in_process() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        let problems = report(dir.path(), &Flags::default(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("active settings"), "{text}");
        assert!(problems <= 1, "{text}");
    }
}
