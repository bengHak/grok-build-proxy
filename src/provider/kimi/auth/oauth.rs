use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;

use super::{CLIENT_ID, DeviceAuthorization, Store, StoredAuth, TokenResponse, file};

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

impl Store {
    pub async fn begin_device_login(&self) -> Result<DeviceAuthorization> {
        let response = self
            .client
            .post(format!(
                "{}/api/oauth/device_authorization",
                self.oauth_host
            ))
            .headers(self.headers().await?)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .form(&[("client_id", CLIENT_ID)])
            .send()
            .await
            .context("start Kimi device authorization")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("start Kimi device authorization: HTTP {status}: {body}")
        }
        response
            .json()
            .await
            .context("decode Kimi device authorization")
    }

    pub async fn finish_device_login(&self, authorization: &DeviceAuthorization) -> Result<()> {
        let _guard = self.lock.lock().await;
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(authorization.expires_in);
        let mut interval = authorization.interval.max(1);
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!("Kimi device code expired; run login again")
            }
            let response = self
                .client
                .post(format!("{}/api/oauth/token", self.oauth_host))
                .headers(self.headers().await?)
                .form(&[
                    ("client_id", CLIENT_ID),
                    ("device_code", authorization.device_code.as_str()),
                    ("grant_type", DEVICE_GRANT),
                ])
                .send()
                .await
                .context("poll Kimi device authorization")?;
            if response.status().is_success() {
                let tokens: TokenResponse =
                    response.json().await.context("decode Kimi device token")?;
                let auth = stored_auth(tokens, None)?;
                return file::save_auth(&self.path, &auth).await;
            }
            let status = response.status();
            let error: Value = response.json().await.unwrap_or_default();
            match error.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += 5,
                Some("expired_token") => bail!("Kimi device code expired; run login again"),
                Some(code) => bail!("Kimi device authorization failed ({status}): {code}"),
                None => bail!("Kimi device authorization failed: HTTP {status}"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        }
    }

    pub(super) async fn refresh(&self, current: &StoredAuth) -> Result<StoredAuth> {
        if current.refresh.trim().is_empty() {
            bail!("Kimi credentials cannot be refreshed; run `grok-build-proxy kimi auth login`")
        }
        let response = self
            .client
            .post(format!("{}/api/oauth/token", self.oauth_host))
            .headers(self.headers().await?)
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", current.refresh.as_str()),
            ])
            .send()
            .await
            .context("refresh Kimi access token")?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "refresh Kimi access token: HTTP {status}; run `grok-build-proxy kimi auth login` if access was revoked"
            )
        }
        let tokens: TokenResponse = response.json().await.context("decode Kimi token refresh")?;
        let auth = stored_auth(tokens, Some(current))?;
        file::save_auth(&self.path, &auth).await?;
        Ok(auth)
    }
}

fn stored_auth(tokens: TokenResponse, current: Option<&StoredAuth>) -> Result<StoredAuth> {
    if tokens.access_token.trim().is_empty() {
        bail!("Kimi token response did not include an access token")
    }
    let now = Utc::now().timestamp_millis().max(0) as u64;
    let user_id =
        user_id(&tokens.access_token).or_else(|| current.and_then(|auth| auth.user_id.clone()));
    Ok(StoredAuth {
        access: tokens.access_token,
        refresh: tokens
            .refresh_token
            .filter(|token| !token.is_empty())
            .or_else(|| current.map(|auth| auth.refresh.clone()))
            .unwrap_or_default(),
        expires: now + tokens.expires_in.unwrap_or(900) * 1000,
        scope: tokens
            .scope
            .or_else(|| current.and_then(|auth| auth.scope.clone())),
        user_id,
    })
}

fn user_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims.get("sub")?.as_str().map(str::to_owned)
}
