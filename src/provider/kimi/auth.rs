use std::{path::PathBuf, sync::Arc};

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
const KIMI_CLI_VERSION: &str = "1.37.0";
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
        let auth = file::load_auth(&self.path).await?;
        let metadata = tokio::fs::metadata(&self.path).await?;
        #[cfg(unix)]
        let file_mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let file_mode = 0;
        let expires_at = DateTime::from_timestamp_millis(auth.expires as i64)
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
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-msh-platform", "kimi_cli".to_owned()),
            ("x-msh-version", KIMI_CLI_VERSION.to_owned()),
            ("x-msh-device-name", ascii(&hostname)),
            (
                "x-msh-device-model",
                ascii(&format!(
                    "{} {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )),
            ),
            ("x-msh-os-version", ascii(std::env::consts::ARCH)),
            ("x-msh-device-id", device_id),
        ] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            );
        }
        headers.insert(USER_AGENT, HeaderValue::from_static("KimiCLI/1.37.0"));
        Ok(headers)
    }
}

#[async_trait]
impl CredentialProvider for Store {
    async fn get(&self, force_refresh: bool) -> Result<Credentials> {
        let _guard = self.lock.lock().await;
        let auth = file::load_auth(&self.path).await?;
        let now = Utc::now().timestamp_millis().max(0) as u64;
        let auth = if force_refresh || auth.expires <= now + REFRESH_MARGIN_MS {
            self.refresh(&auth).await?
        } else {
            auth
        };
        Ok(credentials(&auth))
    }
}

fn credentials(auth: &StoredAuth) -> Credentials {
    Credentials {
        access_token: auth.access.clone(),
        account_id: auth.user_id.clone().unwrap_or_default(),
        expires_at: DateTime::from_timestamp_millis(auth.expires as i64),
    }
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

const fn default_device_expiry() -> u64 {
    900
}

const fn default_poll_interval() -> u64 {
    5
}
