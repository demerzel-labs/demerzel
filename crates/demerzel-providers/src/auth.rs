//! The dmz credential store: one JSON file with mode 0600, one section
//! per provider. File version 2 adds the `anthropic` section; a version-1
//! file still parses, its `anthropic` section is simply absent. Unknown
//! sections and unknown fields inside known sections ride through a
//! save untouched, and a save never lowers the version stamp, so this
//! build can rewrite a newer file without data loss.
//!
//! The `openai_codex` section carries the OAuth machinery from M1. There
//! is no login flow yet: the store imports the Codex CLI login from
//! `~/.codex/auth.json` into dmz's own file and refreshes tokens with
//! the standard OAuth refresh flow. It writes updates to its own store
//! only, never to the Codex CLI file.
//!
//! Refresh strategy (M1 decision): when the Codex CLI file holds a newer
//! `last_refresh` than our store, re-import it instead of spending our
//! refresh token. This keeps the two programs from invalidating each
//! other while the Codex CLI stays in daily use.
//!
//! The `anthropic` section holds an API key. `dmz login anthropic`
//! (M2 step 4) writes it; until then the `ANTHROPIC_API_KEY` variable
//! is the usual source and the store section stays empty.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProviderError;

/// OAuth client id of the Codex CLI, used for the token refresh flow.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default token refresh endpoint.
pub const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// The message for a run with no credentials. `doctor` prints it too.
pub const NO_CREDENTIALS_HELP: &str = "no credentials: log in with the Codex CLI (`codex login`)";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// RFC 3339 timestamp of the last token update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<String>,
    /// Fields a newer build wrote into this section. They ride
    /// through a save untouched.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Credentials for the anthropic adapter: one API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicCredentials {
    pub api_key: String,
    /// Fields a newer build wrote into this section. They ride
    /// through a save untouched.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// The store file version this build writes.
const STORE_VERSION: u32 = 2;

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openai_codex: Option<CodexCredentials>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anthropic: Option<AnthropicCredentials>,
    /// Sections a newer build wrote. They ride through a save
    /// untouched, so an older binary never data-loses a newer file.
    #[serde(default, flatten)]
    extra: serde_json::Map<String, Value>,
}

/// Where a session's credentials would come from. `doctor` reports
/// it; [`AuthStore::probe`] fills it without writing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialSource {
    /// The dmz store already holds credentials.
    Store,
    /// An environment variable holds the credential.
    Env,
    /// The store is empty; the first run imports the Codex CLI login.
    CodexCliImport,
    /// The Codex CLI file exists but does not parse; a session fails
    /// on it with a parse error, not a missing login.
    CodexCliInvalid,
}

impl CredentialSource {
    /// One report line for `doctor`. The wording lives here because
    /// only this module names the Codex CLI.
    pub fn describe(self) -> &'static str {
        match self {
            CredentialSource::Store => "in the dmz store",
            CredentialSource::Env => "from the environment",
            CredentialSource::CodexCliImport => "ready to import from the Codex CLI",
            CredentialSource::CodexCliInvalid => "the Codex CLI auth file does not parse",
        }
    }

    /// Whether a session can authenticate from this source.
    pub fn usable(self) -> bool {
        matches!(
            self,
            CredentialSource::Store | CredentialSource::Env | CredentialSource::CodexCliImport
        )
    }
}

/// The result of a read-only credential check.
#[derive(Debug)]
#[non_exhaustive]
pub struct CredentialProbe {
    pub source: CredentialSource,
    /// The `exp` claim of the access token, when the token parses as
    /// a JWT.
    pub expires: Option<SystemTime>,
}

/// The dmz credential store, one JSON file with mode 0600.
#[derive(Debug)]
pub struct AuthStore {
    path: PathBuf,
    codex_cli_path: Option<PathBuf>,
    token_url: String,
    creds: Option<CodexCredentials>,
    anthropic: Option<AnthropicCredentials>,
    /// The rest of the parsed file: a newer build's sections and its
    /// version, preserved across saves.
    version: u32,
    extra: serde_json::Map<String, Value>,
}

impl AuthStore {
    /// Open the store at `path`. `codex_cli_path` is the Codex CLI
    /// `auth.json` used for import; pass `None` to turn import off.
    pub fn open(path: PathBuf, codex_cli_path: Option<PathBuf>) -> Result<Self, ProviderError> {
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<StoreFile>(&text)
                .map_err(|e| ProviderError::Auth(format!("bad auth store {path:?}: {e}")))?,
            Err(_) => StoreFile::default(),
        };
        Ok(Self {
            path,
            codex_cli_path,
            token_url: DEFAULT_TOKEN_URL.to_string(),
            creds: file.openai_codex,
            anthropic: file.anthropic,
            version: file.version,
            extra: file.extra,
        })
    }

    /// Open the store at the default paths.
    pub fn open_default() -> Result<Self, ProviderError> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| ProviderError::Auth("HOME is not set".to_string()))?;
        let data_dir = demerzel_core::paths::data_dir()
            .map_err(|error| ProviderError::Auth(error.to_string()))?;
        Self::open(
            data_dir.join("auth.json"),
            Some(Path::new(&home).join(".codex/auth.json")),
        )
    }

    /// Override the token refresh endpoint. Used by tests.
    pub fn with_token_url(mut self, url: String) -> Self {
        self.token_url = url;
        self
    }

    pub fn credentials(&self) -> Option<&CodexCredentials> {
        self.creds.as_ref()
    }

    /// The stored Anthropic API key, when the store holds one.
    pub fn anthropic_api_key(&self) -> Option<&str> {
        self.anthropic.as_ref().map(|creds| creds.api_key.as_str())
    }

    /// Resolve the key an anthropic session uses: the environment
    /// value wins over the store, and an empty value counts as unset.
    /// Pass the `ANTHROPIC_API_KEY` value in; the store never reads
    /// the process environment, so tests stay hermetic. A caller with
    /// a non-empty environment key may skip the store — and this call
    /// — entirely; that is the same order.
    pub fn resolve_anthropic_key(
        &self,
        env_key: Option<&str>,
    ) -> Option<(String, CredentialSource)> {
        if let Some(key) = env_key.filter(|key| !key.is_empty()) {
            return Some((key.to_string(), CredentialSource::Env));
        }
        self.anthropic_api_key()
            .map(|key| (key.to_string(), CredentialSource::Store))
    }

    /// Report where an anthropic session would find its key, without
    /// the key text. `None` means the session cannot authenticate.
    pub fn probe_anthropic(&self, env_key: Option<&str>) -> Option<CredentialProbe> {
        let (_, source) = self.resolve_anthropic_key(env_key)?;
        Some(CredentialProbe {
            source,
            expires: None,
        })
    }

    /// Return credentials, importing from the Codex CLI on first use.
    pub fn ensure(&mut self) -> Result<&CodexCredentials, ProviderError> {
        if self.creds.is_none() {
            self.import_from_codex_cli()?;
        }
        self.creds
            .as_ref()
            .ok_or_else(|| ProviderError::Auth(NO_CREDENTIALS_HELP.to_string()))
    }

    /// Report where [`AuthStore::ensure`] would find credentials.
    /// Reads only; it never writes the store. `None` means a session
    /// cannot authenticate. The probe mirrors what a session does: an
    /// unreadable import file or one without both tokens counts as no
    /// credentials, and a file that fails to parse as JSON reports
    /// [`CredentialSource::CodexCliInvalid`], because the import
    /// errors on it.
    pub fn probe(&self) -> Option<CredentialProbe> {
        if let Some(creds) = &self.creds {
            return Some(CredentialProbe {
                source: CredentialSource::Store,
                expires: jwt_expiry(&creds.access_token),
            });
        }
        let source = self.codex_cli_path.as_ref()?;
        let text = std::fs::read_to_string(source).ok()?;
        let Ok(file) = serde_json::from_str::<Value>(&text) else {
            return Some(CredentialProbe {
                source: CredentialSource::CodexCliInvalid,
                expires: None,
            });
        };
        let access = file["tokens"]["access_token"].as_str()?;
        // The import needs both tokens, so the probe does too.
        file["tokens"]["refresh_token"].as_str()?;
        Some(CredentialProbe {
            source: CredentialSource::CodexCliImport,
            expires: jwt_expiry(access),
        })
    }

    /// The ChatGPT account id, from the store or from the id token.
    pub fn account_id(&self) -> Option<String> {
        let creds = self.creds.as_ref()?;
        if let Some(id) = &creds.account_id {
            return Some(id.clone());
        }
        creds.id_token.as_deref().and_then(jwt_account_id)
    }

    /// Import the Codex CLI credentials into this store.
    pub fn import_from_codex_cli(&mut self) -> Result<(), ProviderError> {
        let Some(source) = &self.codex_cli_path else {
            return Ok(());
        };
        let Ok(text) = std::fs::read_to_string(source) else {
            return Ok(());
        };
        let file: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Auth(format!("bad Codex CLI auth file: {e}")))?;
        let tokens = &file["tokens"];
        let (Some(access), Some(refresh)) = (
            tokens["access_token"].as_str(),
            tokens["refresh_token"].as_str(),
        ) else {
            return Ok(());
        };
        self.creds = Some(CodexCredentials {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            id_token: tokens["id_token"].as_str().map(str::to_string),
            account_id: tokens["account_id"].as_str().map(str::to_string),
            last_refresh: file["last_refresh"].as_str().map(str::to_string),
            extra: serde_json::Map::new(),
        });
        self.save()
    }

    /// Refresh the access token. Prefers a re-import when the Codex CLI
    /// file is newer; otherwise runs the OAuth refresh flow.
    pub async fn refresh(&mut self, http: &reqwest::Client) -> Result<(), ProviderError> {
        if self.codex_cli_file_is_newer() {
            self.import_from_codex_cli()?;
            return Ok(());
        }
        let creds = self
            .creds
            .as_ref()
            .ok_or_else(|| ProviderError::Auth("nothing to refresh".to_string()))?;
        let request = serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": creds.refresh_token,
        });
        let response = http
            .post(&self.token_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| ProviderError::Network(format!("token refresh: {e}")))?;
        if !response.status().is_success() {
            return Err(ProviderError::Auth(format!(
                "token refresh failed with status {}",
                response.status()
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Protocol(format!("token refresh body: {e}")))?;
        let creds = self.creds.as_mut().expect("checked above");
        if let Some(token) = body["access_token"].as_str() {
            creds.access_token = token.to_string();
        }
        if let Some(token) = body["refresh_token"].as_str() {
            creds.refresh_token = token.to_string();
        }
        if let Some(token) = body["id_token"].as_str() {
            creds.id_token = Some(token.to_string());
        }
        creds.last_refresh = Some(humantime::format_rfc3339(SystemTime::now()).to_string());
        self.save()
    }

    fn codex_cli_file_is_newer(&self) -> bool {
        let Some(source) = &self.codex_cli_path else {
            return false;
        };
        let Some(theirs) = std::fs::read_to_string(source)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|file| file["last_refresh"].as_str().map(str::to_string))
            .and_then(|stamp| parse_rfc3339(&stamp))
        else {
            return false;
        };
        let ours = self
            .creds
            .as_ref()
            .and_then(|c| c.last_refresh.as_deref())
            .and_then(parse_rfc3339);
        match ours {
            Some(ours) => theirs > ours,
            None => true,
        }
    }

    fn save(&self) -> Result<(), ProviderError> {
        let file = StoreFile {
            // Never downgrade the stamp of a newer file this build
            // still understands enough to rewrite.
            version: self.version.max(STORE_VERSION),
            openai_codex: self.creds.clone(),
            anthropic: self.anthropic.clone(),
            extra: self.extra.clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|e| ProviderError::Auth(format!("serialize auth store: {e}")))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ProviderError::Auth(format!("create {parent:?}: {e}")))?;
        }
        std::fs::write(&self.path, text)
            .map_err(|e| ProviderError::Auth(format!("write {:?}: {e}", self.path)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| ProviderError::Auth(format!("chmod {:?}: {e}", self.path)))?;
        }
        Ok(())
    }
}

fn parse_rfc3339(stamp: &str) -> Option<SystemTime> {
    humantime::parse_rfc3339(stamp).ok()
}

/// Seconds for 9999-12-31T23:59:59Z. A larger `exp` is malformed —
/// usually a millisecond timestamp — and RFC 3339 cannot render it.
const MAX_EXPIRY_SECONDS: u64 = 253_402_300_799;

/// Read the `exp` claim out of a JWT. Malformed or out-of-range
/// claims are `None`, never a panic: the value comes from a file on
/// disk.
fn jwt_expiry(jwt: &str) -> Option<SystemTime> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let exp = &claims["exp"];
    let seconds = exp
        .as_u64()
        .or_else(|| exp.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))?;
    if seconds > MAX_EXPIRY_SECONDS {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
}

/// Read the ChatGPT account id claim out of an id-token JWT.
fn jwt_account_id(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fake_jwt(claims: Value) -> String {
        let engine = &base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.{}",
            engine.encode(r#"{"alg":"none"}"#),
            engine.encode(claims.to_string()),
            engine.encode("sig"),
        )
    }

    fn write_codex_file(dir: &Path, last_refresh: &str, account_id: Option<&str>) -> PathBuf {
        let jwt = fake_jwt(json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_from_jwt"},
        }));
        let mut tokens = json!({
            "id_token": jwt,
            "access_token": "access-1",
            "refresh_token": "refresh-1",
        });
        if let Some(id) = account_id {
            tokens["account_id"] = json!(id);
        }
        let path = dir.join("codex-auth.json");
        std::fs::write(
            &path,
            json!({"tokens": tokens, "last_refresh": last_refresh}).to_string(),
        )
        .unwrap();
        path
    }

    #[test]
    fn imports_codex_cli_credentials_and_sets_0600() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_codex_file(dir.path(), "2026-08-01T10:00:00Z", Some("acct_1"));
        let store_path = dir.path().join("store/auth.json");
        let mut store = AuthStore::open(store_path.clone(), Some(source)).unwrap();

        let creds = store.ensure().unwrap();
        assert_eq!(creds.access_token, "access-1");
        assert_eq!(store.account_id().as_deref(), Some("acct_1"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let reopened = AuthStore::open(store_path, None).unwrap();
        assert_eq!(reopened.credentials().unwrap().access_token, "access-1");
    }

    #[test]
    fn account_id_falls_back_to_the_id_token_claim() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_codex_file(dir.path(), "2026-08-01T10:00:00Z", None);
        let mut store = AuthStore::open(dir.path().join("auth.json"), Some(source)).unwrap();
        store.ensure().unwrap();
        assert_eq!(store.account_id().as_deref(), Some("acct_from_jwt"));
    }

    #[test]
    fn newer_codex_cli_file_wins_on_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_codex_file(dir.path(), "2026-08-02T10:00:00Z", Some("acct_1"));
        let mut store = AuthStore::open(dir.path().join("auth.json"), Some(source)).unwrap();
        store.creds = Some(CodexCredentials {
            access_token: "stale".into(),
            refresh_token: "stale".into(),
            id_token: None,
            account_id: None,
            last_refresh: Some("2026-08-01T00:00:00Z".into()),
            extra: serde_json::Map::new(),
        });
        assert!(store.codex_cli_file_is_newer());

        store.creds.as_mut().unwrap().last_refresh = Some("2026-08-03T00:00:00Z".into());
        assert!(!store.codex_cli_file_is_newer());
    }

    #[test]
    fn probe_reports_the_source_without_writing_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_codex_file(dir.path(), "2026-08-01T10:00:00Z", Some("acct_1"));
        let store_path = dir.path().join("auth.json");

        // Import available: the probe sees it and writes nothing.
        let store = AuthStore::open(store_path.clone(), Some(source)).unwrap();
        let probe = store.probe().unwrap();
        assert_eq!(probe.source, CredentialSource::CodexCliImport);
        assert!(!store_path.exists());

        // Filled store: the probe reads it directly.
        let mut store = AuthStore::open(store_path.clone(), None).unwrap();
        store.creds = Some(CodexCredentials {
            access_token: fake_jwt(json!({"exp": 4102444800u64})),
            refresh_token: "refresh".into(),
            id_token: None,
            account_id: None,
            last_refresh: None,
            extra: serde_json::Map::new(),
        });
        let probe = store.probe().unwrap();
        assert_eq!(probe.source, CredentialSource::Store);
        let expires = probe.expires.unwrap();
        let seconds = expires
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(seconds, 4102444800);

        // Neither: no credentials.
        let empty = AuthStore::open(dir.path().join("other.json"), None).unwrap();
        assert!(empty.probe().is_none());
    }

    #[test]
    fn probe_separates_an_unparsable_codex_cli_file_from_a_missing_login() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("codex-auth.json");

        std::fs::write(&source, "not json {").unwrap();
        let store = AuthStore::open(dir.path().join("auth.json"), Some(source.clone())).unwrap();
        let probe = store.probe().unwrap();
        assert_eq!(probe.source, CredentialSource::CodexCliInvalid);
        assert!(!probe.source.usable());

        // Valid JSON without both tokens is a missing login, like the
        // import treats it.
        std::fs::write(&source, r#"{"tokens": {"access_token": "a"}}"#).unwrap();
        let store = AuthStore::open(dir.path().join("auth.json"), Some(source)).unwrap();
        assert!(store.probe().is_none());
    }

    #[test]
    fn jwt_expiry_rejects_malformed_claims_without_panicking() {
        assert!(jwt_expiry("not-a-jwt").is_none());
        assert!(jwt_expiry(&fake_jwt(json!({}))).is_none());
        assert!(jwt_expiry(&fake_jwt(json!({"exp": u64::MAX}))).is_none());
        // A millisecond timestamp is past the year 9999.
        assert!(jwt_expiry(&fake_jwt(json!({"exp": 1_754_654_400_000u64}))).is_none());
        assert!(jwt_expiry(&fake_jwt(json!({"exp": -5.0}))).is_none());

        let expires = jwt_expiry(&fake_jwt(json!({"exp": 4_102_444_800.5}))).unwrap();
        let seconds = expires
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(seconds, 4_102_444_800);
    }

    #[test]
    fn the_anthropic_probe_prefers_the_environment_over_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            json!({"version": 2, "anthropic": {"api_key": "sk-test"}}).to_string(),
        )
        .unwrap();
        let store = AuthStore::open(path, None).unwrap();
        assert_eq!(store.anthropic_api_key(), Some("sk-test"));

        assert_eq!(
            store.resolve_anthropic_key(None),
            Some(("sk-test".into(), CredentialSource::Store))
        );
        assert_eq!(
            store.resolve_anthropic_key(Some("sk-env")),
            Some(("sk-env".into(), CredentialSource::Env))
        );
        // An empty variable counts as unset.
        assert_eq!(
            store.resolve_anthropic_key(Some("")),
            Some(("sk-test".into(), CredentialSource::Store))
        );
        assert_eq!(
            store.probe_anthropic(None).unwrap().source,
            CredentialSource::Store
        );

        let empty = AuthStore::open(dir.path().join("other.json"), None).unwrap();
        assert!(empty.probe_anthropic(None).is_none());
        assert!(empty.resolve_anthropic_key(Some("")).is_none());
        assert_eq!(
            empty.probe_anthropic(Some("sk-env")).unwrap().source,
            CredentialSource::Env
        );
    }

    #[test]
    fn a_codex_save_preserves_the_anthropic_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            json!({"version": 2, "anthropic": {"api_key": "sk-keep"}}).to_string(),
        )
        .unwrap();
        let source = write_codex_file(dir.path(), "2026-08-01T10:00:00Z", Some("acct_1"));
        let mut store = AuthStore::open(path.clone(), Some(source)).unwrap();
        store.ensure().unwrap();

        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], 2);
        assert_eq!(saved["anthropic"]["api_key"], "sk-keep");
        assert_eq!(saved["openai_codex"]["access_token"], "access-1");
    }

    #[test]
    fn a_save_preserves_a_newer_files_sections_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            json!({
                "version": 3,
                "anthropic": {"api_key": "sk-keep", "oauth": {"refresh": "r-9"}},
                "openai_codex": {
                    "access_token": "a",
                    "refresh_token": "r",
                    "future_field": "keep-me-too",
                },
                "future_provider": {"token": "keep-me"},
            })
            .to_string(),
        )
        .unwrap();
        let mut store = AuthStore::open(path.clone(), None).unwrap();
        store.creds.as_mut().unwrap().access_token = "a2".into();
        store.save().unwrap();

        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], 3, "never downgrade the version stamp");
        assert_eq!(saved["future_provider"]["token"], "keep-me");
        // Unknown fields inside known sections survive too.
        assert_eq!(saved["anthropic"]["api_key"], "sk-keep");
        assert_eq!(saved["anthropic"]["oauth"]["refresh"], "r-9");
        assert_eq!(saved["openai_codex"]["access_token"], "a2");
        assert_eq!(saved["openai_codex"]["future_field"], "keep-me-too");
    }

    #[test]
    fn a_version_1_file_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            json!({
                "version": 1,
                "openai_codex": {"access_token": "a", "refresh_token": "r"},
            })
            .to_string(),
        )
        .unwrap();
        let store = AuthStore::open(path, None).unwrap();
        assert_eq!(store.credentials().unwrap().access_token, "a");
        assert!(store.anthropic_api_key().is_none());
    }

    #[test]
    fn missing_codex_cli_file_reports_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AuthStore::open(
            dir.path().join("auth.json"),
            Some(dir.path().join("does-not-exist.json")),
        )
        .unwrap();
        let error = store.ensure().unwrap_err();
        assert!(error.to_string().contains("codex login"));
    }
}
