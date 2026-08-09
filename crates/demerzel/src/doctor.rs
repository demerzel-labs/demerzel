//! `demerzel doctor`: report the state a session depends on. The
//! report shows presence, not values — never token or key text.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use demerzel_core::{ApprovalMode, SessionStore};
use demerzel_providers::openai_codex::auth::NO_CREDENTIALS_HELP;
use demerzel_providers::openai_codex::{AuthStore, CredentialSource};

use crate::config::{self, Flags, Layer, Locations, Origin};

/// Run the checks and print the report to stdout. Returns the process
/// exit code: 0 when every check passes, 1 otherwise.
pub fn run(project_dir: &Path, flags: &Flags) -> i32 {
    let mut problems = 0u32;
    let locations = Locations::locate(project_dir);

    println!("config files");
    let user = report_file("user", locations.user.as_deref(), &mut problems);
    let project = report_file("project", Some(&locations.project), &mut problems);

    let settings = config::resolve(
        flags,
        &|name| std::env::var(name).ok(),
        project.as_ref(),
        user.as_ref(),
    );
    println!("active settings");
    print_setting(
        "provider",
        &settings.provider.value,
        settings.provider.origin,
    );
    print_setting("model", &settings.model.value, settings.model.origin);
    let approval = match settings.approval.value {
        Some(mode) => mode_name(mode).to_string(),
        None => "ask in the REPL, read-only for one-shot".to_string(),
    };
    print_setting("approval", &approval, settings.approval.origin);
    let history = if settings.history_file.value {
        "on"
    } else {
        "off"
    };
    print_setting("history_file", history, settings.history_file.origin);
    let base_url = settings
        .base_url
        .value
        .as_deref()
        .unwrap_or("provider default");
    print_setting("base_url", base_url, settings.base_url.origin);
    if settings.provider.value != config::DEFAULT_PROVIDER {
        problems += 1;
        println!(
            "  problem: provider `{}` is not in this build; `{}` is",
            settings.provider.value,
            config::DEFAULT_PROVIDER
        );
    }

    report_credentials(&mut problems);
    report_tools(project_dir, &mut problems);

    match problems {
        0 => {
            println!("result: ok — a session can start");
            0
        }
        1 => {
            println!("result: 1 problem");
            1
        }
        n => {
            println!("result: {n} problems");
            1
        }
    }
}

fn print_setting(name: &str, value: &str, origin: Origin) {
    println!("  {name} = {value} ({origin})");
}

fn mode_name(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::ReadOnly => "read-only",
        ApprovalMode::Ask => "ask",
        ApprovalMode::Auto => "auto",
    }
}

/// Report one config file and return its parsed layer.
fn report_file(label: &str, path: Option<&Path>, problems: &mut u32) -> Option<Layer> {
    let Some(path) = path else {
        println!("  {label}: none — HOME is not set");
        return None;
    };
    match config::load(path) {
        Ok(None) => {
            println!("  {label}: {} — none", path.display());
            None
        }
        Ok(Some((layer, warnings))) => {
            println!("  {label}: {} — found", path.display());
            for warning in warnings {
                println!("  warning: {label} file: {warning}");
            }
            for warning in secret_warnings(path) {
                println!("  warning: {label} file: {warning}");
            }
            Some(layer)
        }
        Err(error) => {
            *problems += 1;
            println!("  problem: {error}");
            None
        }
    }
}

fn report_credentials(problems: &mut u32) {
    println!("credentials");
    let store = match AuthStore::open_default() {
        Ok(store) => store,
        Err(error) => {
            *problems += 1;
            println!("  problem: {error}");
            return;
        }
    };
    match store.probe() {
        Some(probe) => {
            let source = match probe.source {
                CredentialSource::Store => "in the dmz store",
                CredentialSource::CodexCliImport => "ready to import at the first run",
            };
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
            println!("  openai-codex: {source}{expiry}");
        }
        None => {
            *problems += 1;
            println!("  openai-codex: none — {NO_CREDENTIALS_HELP}");
        }
    }
}

fn report_tools(project_dir: &Path, problems: &mut u32) {
    println!("tools");
    match find_in_path("bash") {
        Some(path) => println!("  bash: {}", path.display()),
        None => {
            *problems += 1;
            println!("  problem: bash is not on PATH");
        }
    }
    check_writable("project root", project_dir, problems);
    match SessionStore::open_default() {
        Ok(store) => {
            check_writable(
                "session directory",
                &store.sessions_dir(project_dir),
                problems,
            );
        }
        Err(error) => {
            *problems += 1;
            println!("  problem: session directory: {error}");
        }
    }
}

fn check_writable(label: &str, dir: &Path, problems: &mut u32) {
    match probe_writable(dir) {
        Ok(()) => println!("  {label}: writable — {}", dir.display()),
        Err(error) => {
            *problems += 1;
            println!(
                "  problem: {label} {} is not writable: {error}",
                dir.display()
            );
        }
    }
}

/// Mirror a session start: create the directory, then a file in it.
fn probe_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".doctor-probe");
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

/// Warn on values that look like credentials. The auth store is the
/// only place for secrets.
fn secret_warnings(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    scan_table(&table, "", &mut warnings);
    warnings
}

fn scan_table(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(sub) => scan_table(sub, &path, out),
            toml::Value::String(text) if secret_name(key) || secret_value(text) => {
                out.push(format!(
                    "`{path}` looks like a secret; move it to the auth store"
                ));
            }
            _ => {}
        }
    }
}

fn secret_name(key: &str) -> bool {
    ["key", "token", "secret", "password"]
        .iter()
        .any(|word| key.contains(word))
}

fn secret_value(text: &str) -> bool {
    text.starts_with("sk-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_shapes_are_flagged_by_name_or_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "model = \"gpt-5.6-sol\"\n\
             api_key = \"abc\"\n\
             [providers.anthropic]\n\
             base_url = \"sk-oops\"",
        )
        .unwrap();
        let warnings = secret_warnings(&path);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("`api_key`"));
        assert!(warnings[1].contains("`providers.anthropic.base_url`"));
    }

    #[test]
    fn bash_resolves_on_this_machine() {
        assert!(find_in_path("bash").is_some());
    }

    #[test]
    fn writable_probe_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh");
        probe_writable(&target).unwrap();
        assert!(target.exists());
        assert!(!target.join(".doctor-probe").exists());
    }
}
