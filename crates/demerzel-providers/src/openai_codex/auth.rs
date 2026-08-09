//! Credential store for the openai-codex adapter.
//!
//! M1 implements no login flow. The store imports the Codex CLI login
//! from `~/.codex/auth.json` into dmz's own file and refreshes tokens
//! with the standard OAuth refresh flow. It writes updates to its own
//! store only, never to the Codex CLI file.
//!
//! Refresh strategy (M1 decision): when the Codex CLI file holds a newer
//! `last_refresh` than our store, re-import it instead of spending our
//! refresh token. This keeps the two programs from invalidating each
//! other while the Codex CLI stays in daily use.

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
pub const NO_CREDENTIALS_HELP: &str =
    "no credentials: log in with the Codex CLI (`codex login`); `dmz login` arrives in M2";

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
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openai_codex: Option<CodexCredentials>,
}

/// Where a session's credentials would come from. `doctor` reports
/// it; [`AuthStore::probe`] fills it without writing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    /// The dmz store already holds credentials.
    Store,
    /// The store is empty; the first run imports the Codex CLI login.
    CodexCliImport,
}

/// The result of a read-only credential check.
#[derive(Debug)]
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
}

impl AuthStore {
    /// Open the store at `path`. `codex_cli_path` is the Codex CLI
    /// `auth.json` used for import; pass `None` to turn import off.
    pub fn open(path: PathBuf, codex_cli_path: Option<PathBuf>) -> Result<Self, ProviderError> {
        let creds = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let file: StoreFile = serde_json::from_str(&text)
                    .map_err(|e| ProviderError::Auth(format!("bad auth store {path:?}: {e}")))?;
                file.openai_codex
            }
            Err(_) => None,
        };
        Ok(Self {
            path,
            codex_cli_path,
            token_url: DEFAULT_TOKEN_URL.to_string(),
            creds,
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
    /// cannot authenticate.
    pub fn probe(&self) -> Option<CredentialProbe> {
        if let Some(creds) = &self.creds {
            return Some(CredentialProbe {
                source: CredentialSource::Store,
                expires: jwt_expiry(&creds.access_token),
            });
        }
        let source = self.codex_cli_path.as_ref()?;
        let text = std::fs::read_to_string(source).ok()?;
        let file: Value = serde_json::from_str(&text).ok()?;
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
            version: 1,
            openai_codex: self.creds.clone(),
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

/// Read the `exp` claim out of a JWT.
fn jwt_expiry(jwt: &str) -> Option<SystemTime> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let seconds = claims["exp"].as_u64()?;
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
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
