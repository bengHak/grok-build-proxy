use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE};

use crate::auth::CredentialProvider;

use super::{auth::Store, request::translate_request};

pub async fn send(
    client: &reqwest::Client,
    upstream_url: &str,
    store: &Store,
    raw: &[u8],
    session_id: &str,
    force_refresh: bool,
) -> Result<reqwest::Response> {
    let credentials = store
        .get(force_refresh)
        .await
        .context("load Kimi credentials")?;
    let body = translate_request(raw, session_id)?;
    let response = client
        .post(upstream_url)
        .headers(store.headers().await?)
        .bearer_auth(credentials.access_token)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "text/event-stream")
        .json(&body)
        .send()
        .await
        .context("send Kimi chat completion")?;
    Ok(response)
}
