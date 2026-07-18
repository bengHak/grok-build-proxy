use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};

use crate::auth::{CredentialProvider, Credentials};

mod file;
mod oauth;

pub const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
pub const DEFAULT_UPSTREAM: &str = "https://api.kimi.com/coding/v1/chat/completions";
pub const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const KIMI_CLI_VERSION: &str = "1.49.0";
const REFRESH_MARGIN_MS: u64 = 5 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredAuth {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(rename = "userId", default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AuthStatus {
    pub path: PathBuf,
    pub user_id: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub has_refresh_token: bool,
    pub file_mode: u32,
    pub file_size: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceAuthorization {
    pub user_code: String,
    device_code: String,
    #[serde(default)]
    pub verification_uri: Option<String>,
    pub verification_uri_complete: String,
    #[serde(default = "default_device_expiry")]
    expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    oauth_host: String,
    client: reqwest::Client,
    upstream_client: reqwest::Client,
    lock: Arc<Mutex<()>>,
    device_id: Arc<OnceCell<String>>,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>, oauth_host: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            bail!("Kimi auth file path is required")
        }
        let oauth_host = oauth_host.into().trim_end_matches('/').to_owned();
        if oauth_host.is_empty() {
            bail!("Kimi OAuth host is required")
        }
        super::validate_oauth_host(&oauth_host)?;
        Ok(Self {
            path,
            oauth_host,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            upstream_client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .pool_max_idle_per_host(20)
                .build()?,
            lock: Arc::new(Mutex::new(())),
            device_id: Arc::new(OnceCell::new()),
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(super) fn http_client(&self) -> &reqwest::Client {
        &self.upstream_client
    }

    pub async fn inspect(&self) -> Result<AuthStatus> {
        let _guard = self.lock.lock().await;
        let auth = file::load_auth_for_inspection(&self.path).await?;
        let metadata = tokio::fs::metadata(&self.path).await?;
        #[cfg(unix)]
        let file_mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let file_mode = 0;
        let expires =
            i64::try_from(auth.expires).context("Kimi credential expiry is out of range")?;
        let expires_at = DateTime::from_timestamp_millis(expires)
            .context("Kimi credential expiry is out of range")?;
        Ok(AuthStatus {
            path: self.path.clone(),
            user_id: auth.user_id,
            expires_at,
            has_refresh_token: !auth.refresh.is_empty(),
            file_mode,
            file_size: metadata.len(),
        })
    }

    pub async fn logout(&self) -> Result<()> {
        let _guard = self.lock.lock().await;
        file::remove_auth(&self.path).await
    }

    pub async fn headers(&self) -> Result<HeaderMap> {
        let device_id = self
            .device_id
            .get_or_try_init(|| file::device_id(&self.path))
            .await?
            .clone();
        let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into());
        let (os_version, device_model) = platform_metadata();
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-msh-platform", "kimi_cli".to_owned()),
            ("x-msh-version", KIMI_CLI_VERSION.to_owned()),
            ("x-msh-device-name", ascii(&hostname)),
            ("x-msh-device-model", ascii(device_model)),
            ("x-msh-os-version", ascii(os_version)),
            ("x-msh-device-id", device_id),
        ] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            );
        }
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("KimiCLI/{KIMI_CLI_VERSION}"))?,
        );
        Ok(headers)
    }
}

#[async_trait]
impl CredentialProvider for Store {
    async fn get(&self, force_refresh: bool) -> Result<Credentials> {
        let _guard = self.lock.lock().await;
        let auth = file::load_auth(&self.path).await?;
        let now = Utc::now().timestamp_millis().max(0) as u64;
        let refresh_at = now.saturating_add(REFRESH_MARGIN_MS);
        let auth = if force_refresh || auth.expires <= refresh_at {
            self.refresh(&auth).await?
        } else {
            auth
        };
        credentials(&auth)
    }
}

fn credentials(auth: &StoredAuth) -> Result<Credentials> {
    let expires = i64::try_from(auth.expires).context("Kimi credential expiry is out of range")?;
    let expires_at = DateTime::from_timestamp_millis(expires)
        .context("Kimi credential expiry is out of range")?;
    Ok(Credentials {
        access_token: auth.access.clone(),
        account_id: auth.user_id.clone().unwrap_or_default(),
        expires_at: Some(expires_at),
    })
}

fn ascii(value: &str) -> String {
    let value: String = value
        .chars()
        .filter(|character| character.is_ascii() && !character.is_control())
        .collect();
    let value = value.trim();
    if value.is_empty() {
        "unknown".into()
    } else {
        value.into()
    }
}

fn platform_metadata() -> &'static (String, String) {
    static METADATA: OnceLock<(String, String)> = OnceLock::new();
    METADATA.get_or_init(|| {
        let os_version = platform_version();
        let release = platform_release().unwrap_or_else(|| os_version.clone());
        let system = if cfg!(target_os = "macos") {
            "macOS"
        } else {
            std::env::consts::OS
        };
        (
            os_version,
            format!("{system} {release} {}", std::env::consts::ARCH),
        )
    })
}

#[cfg(unix)]
fn platform_version() -> String {
    command_output("uname", &["-v"]).unwrap_or_else(|| "unknown".into())
}

#[cfg(not(unix))]
fn platform_version() -> String {
    std::env::consts::OS.to_owned()
}

#[cfg(target_os = "macos")]
fn platform_release() -> Option<String> {
    command_output("/usr/bin/sw_vers", &["-productVersion"])
        .or_else(|| command_output("uname", &["-r"]))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_release() -> Option<String> {
    command_output("uname", &["-r"])
}

#[cfg(not(unix))]
fn platform_release() -> Option<String> {
    None
}

#[cfg(unix)]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

const fn default_device_expiry() -> u64 {
    900
}

const fn default_poll_interval() -> u64 {
    5
}
