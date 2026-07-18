use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};

use crate::auth::CredentialProvider;

use super::{auth::Store, request::translate_request};

pub async fn send(
    upstream_url: &str,
    store: &Store,
    api_key: &str,
    raw: &[u8],
    prompt_cache_key: Option<&str>,
    force_refresh: bool,
) -> Result<reqwest::Response> {
    let body = translate_request(raw, prompt_cache_key)?;
    let api_key = api_key.trim();
    let token = if api_key.is_empty() {
        let credentials = store
            .get(force_refresh)
            .await
            .context("load Kimi credentials")?;
        credentials.access_token
    } else {
        api_key.to_owned()
    };
    let request = store.http_client().post(upstream_url).bearer_auth(token);
    let request = if api_key.is_empty() {
        request.headers(store.headers().await?)
    } else {
        request.header(
            USER_AGENT,
            concat!("grok-build-proxy/", env!("CARGO_PKG_VERSION")),
        )
    };
    let response = request
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "text/event-stream")
        .json(&body)
        .send()
        .await
        .context("send Kimi chat completion")?;
    Ok(response)
}
