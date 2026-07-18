pub mod auth;
pub mod client;
pub mod request;
pub mod stream;

pub const WIRE_MODEL: &str = "kimi-for-coding";

pub fn is_model(id: &str) -> bool {
    matches!(id, WIRE_MODEL | "kimi-k2.6" | "k2.6")
}

pub fn validate_upstream_url(value: &str) -> anyhow::Result<()> {
    validate_sensitive_url(
        value,
        "api.kimi.com",
        Some("/coding/v1/chat/completions"),
        "Kimi upstream URL",
    )
}

fn validate_oauth_host(value: &str) -> anyhow::Result<()> {
    validate_sensitive_url(value, "auth.kimi.com", Some("/"), "Kimi OAuth host")
}

fn validate_sensitive_url(
    value: &str,
    official_host: &str,
    official_path: Option<&str>,
    label: &str,
) -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use std::net::IpAddr;

    let url = url::Url::parse(value).with_context(|| format!("invalid {label}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("{label} must not contain credentials, a query, or a fragment")
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{label} must include a host"))?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if loopback && matches!(url.scheme(), "http" | "https") {
        return Ok(());
    }
    if url.scheme() != "https"
        || !host.eq_ignore_ascii_case(official_host)
        || url.port().is_some_and(|port| port != 443)
        || official_path.is_some_and(|path| url.path() != path)
    {
        bail!("{label} must use the official HTTPS endpoint or a loopback test endpoint")
    }
    Ok(())
}
