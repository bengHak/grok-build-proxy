use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

pub const DEFAULT_REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
pub const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Clone, Debug)]
pub struct Credentials {
    pub access_token: String,
    pub account_id: String,
    pub expires_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug)]
pub struct AuthStatus {
    pub path: PathBuf,
    pub auth_mode: String,
    pub account_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub has_refresh_token: bool,
    pub last_refresh: Option<DateTime<Utc>>,
    pub file_mode: u32,
    pub file_size: u64,
}

#[async_trait]
pub trait CredentialProvider: Send + Sync {
    async fn get(&self, force_refresh: bool) -> Result<Credentials>;
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    refresh_url: String,
    client: reqwest::Client,
    refresh_margin: Duration,
    lock: Arc<Mutex<()>>,
}
impl Store {
    pub fn new(path: impl Into<PathBuf>, refresh_url: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            bail!("auth file path is required")
        }
        Ok(Self {
            path,
            refresh_url: refresh_url.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            refresh_margin: Duration::minutes(5),
            lock: Arc::new(Mutex::new(())),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    async fn load(&self) -> Result<(Map<String, Value>, Map<String, Value>)> {
        let data=fs::read(&self.path).await.map_err(|e| if e.kind()==std::io::ErrorKind::NotFound { anyhow!("Codex auth file not found at {}; use a file-backed CODEX_HOME and run `codex login`",self.path.display()) } else { anyhow!(e).context("read Codex auth file") })?;
        let doc: Map<String, Value> =
            serde_json::from_slice(&data).context("parse Codex auth file")?;
        let tokens=doc.get("tokens").and_then(Value::as_object).cloned().ok_or_else(|| if doc.get("OPENAI_API_KEY").and_then(Value::as_str).is_some() { anyhow!("Codex auth file contains an API key, not a ChatGPT session; run `codex login` and choose ChatGPT sign-in") } else { anyhow!("Codex auth file does not contain ChatGPT token data; run `codex login` again") })?;
        Ok((doc, tokens))
    }
    pub async fn inspect(&self) -> Result<AuthStatus> {
        let _guard = self.lock.lock().await;
        let (doc, tokens) = self.load().await?;
        let (creds, refresh) = credentials_from_tokens(&tokens)?;
        let meta = fs::metadata(&self.path).await?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = 0;
        Ok(AuthStatus {
            path: self.path.clone(),
            auth_mode: doc
                .get("auth_mode")
                .and_then(Value::as_str)
                .unwrap_or("chatgpt")
                .into(),
            account_id: creds.account_id,
            expires_at: creds.expires_at,
            has_refresh_token: !refresh.is_empty(),
            last_refresh: doc
                .get("last_refresh")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
            file_mode: mode,
            file_size: meta.len(),
        })
    }
    async fn save(&self, doc: &Map<String, Value>) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(doc)?;
        data.push(b'\n');
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let tmp = dir.join(format!(".auth.json.{}", uuid::Uuid::new_v4()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await?;
        file.write_all(&data).await?;
        file.sync_all().await?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await?;
        }
        fs::rename(&tmp, &self.path).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: &'a str,
}
#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    id_token: String,
    access_token: String,
    #[serde(default)]
    refresh_token: String,
}
#[async_trait]
impl CredentialProvider for Store {
    async fn get(&self, force_refresh: bool) -> Result<Credentials> {
        let _guard = self.lock.lock().await;
        let (mut doc, mut tokens) = self.load().await?;
        let (creds, refresh) = credentials_from_tokens(&tokens)?;
        let should = force_refresh
            || creds
                .expires_at
                .is_some_and(|t| t <= Utc::now() + self.refresh_margin);
        if !should {
            return Ok(creds);
        }
        if refresh.is_empty() {
            if force_refresh {
                bail!(
                    "Codex credentials cannot be refreshed: refresh_token is missing; run `codex login` again"
                )
            }
            return Ok(creds);
        }
        let resp = self
            .client
            .post(&self.refresh_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "grok-build-proxy")
            .json(&RefreshRequest {
                client_id: CODEX_OAUTH_CLIENT_ID,
                grant_type: "refresh_token",
                refresh_token: &refresh,
            })
            .send()
            .await
            .context("refresh Codex access token")?;
        if !resp.status().is_success() {
            bail!(
                "refresh Codex access token: HTTP {}; run `codex login` again if the session was revoked",
                resp.status().as_u16()
            )
        }
        let updated: RefreshResponse =
            resp.json().await.context("decode token refresh response")?;
        if updated.access_token.trim().is_empty() {
            bail!("token refresh response did not include access_token")
        }
        tokens.insert("access_token".into(), updated.access_token.into());
        if !updated.refresh_token.is_empty() {
            tokens.insert("refresh_token".into(), updated.refresh_token.into());
        }
        if !updated.id_token.is_empty() {
            tokens.insert("id_token".into(), updated.id_token.into());
        }
        if tokens
            .get("account_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
            && let Some(id) = tokens
                .get("id_token")
                .and_then(Value::as_str)
                .and_then(account_id_from_jwt)
        {
            tokens.insert("account_id".into(), id.into());
        }
        doc.insert("tokens".into(), Value::Object(tokens.clone()));
        doc.insert(
            "last_refresh".into(),
            Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
                .into(),
        );
        self.save(&doc).await?;
        credentials_from_tokens(&tokens).map(|x| x.0)
    }
}
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}
fn account_id_from_jwt(token: &str) -> Option<String> {
    jwt_claims(token)?
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")?
        .as_str()
        .map(str::to_owned)
}
fn expiration_from_jwt(token: &str) -> Option<DateTime<Utc>> {
    let value = jwt_claims(token)?.get("exp")?.clone();
    let secs = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
    Utc.timestamp_opt(secs, 0).single()
}
fn credentials_from_tokens(tokens: &Map<String, Value>) -> Result<(Credentials, String)> {
    let access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if access.is_empty() {
        bail!("Codex auth file is missing access_token; run `codex login` again")
    };
    let id = tokens.get("id_token").and_then(Value::as_str).unwrap_or("");
    let account = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| account_id_from_jwt(id))
        .or_else(|| account_id_from_jwt(access))
        .unwrap_or_default();
    let expires = expiration_from_jwt(access).or_else(|| expiration_from_jwt(id));
    Ok((
        Credentials {
            access_token: access.into(),
            account_id: account,
            expires_at: expires,
        },
        tokens
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn refreshes_and_preserves_unknown_fields() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/token", post(|| async { Json(serde_json::json!({"access_token":test_jwt(1_893_456_000,"new-account"),"refresh_token":"rotated"})) }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let original = serde_json::json!({"unknown":"keep","tokens":{"access_token":test_jwt(1,"old"),"refresh_token":"refresh","token_extra":7}});
        fs::write(&path, serde_json::to_vec(&original).unwrap())
            .await
            .unwrap();
        let store = Store::new(&path, format!("http://{address}/token")).unwrap();
        let credentials = store.get(false).await.unwrap();
        assert_eq!(credentials.account_id, "new-account");
        let saved: Value = serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(saved["unknown"], "keep");
        assert_eq!(saved["tokens"]["token_extra"], 7);
        assert_eq!(saved["tokens"]["refresh_token"], "rotated");
    }
    fn test_jwt(exp: i64, account: &str) -> String {
        let claims = serde_json::json!({"exp":exp,"https://api.openai.com/auth":{"chatgpt_account_id":account}});
        format!(
            "x.{}.x",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }
}
