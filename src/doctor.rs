use crate::{auth::Store, codexcli, grokconfig::GrokConfig, modelmap::ModelMap};
use anyhow::Result;
use serde_json::Value;
use std::{net::SocketAddr, path::Path, time::Duration};

pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub warning: bool,
    pub detail: String,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: true,
            warning: false,
            detail: detail.into(),
        }
    }
    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: true,
            warning: true,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: false,
            warning: false,
            detail: detail.into(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_full(
    auth_file: &Path,
    grok_config: &Path,
    codex_home: &Path,
    listen: &str,
    client_token: &str,
    codex_binary: &str,
    grok_binary: &str,
    model_map: &str,
    timeout: Duration,
) -> Vec<Check> {
    let mut checks = Vec::new();
    if cfg!(target_os = "macos") && matches!(std::env::consts::ARCH, "aarch64" | "x86_64") {
        checks.push(Check::pass(
            "Platform",
            format!("macOS {}", std::env::consts::ARCH),
        ));
    } else {
        checks.push(Check::fail(
            "Platform",
            format!(
                "unsupported {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ));
    }

    match ModelMap::parse(model_map) {
        Ok(map) => checks.push(Check::pass(
            "Model substitutions",
            if map.is_empty() {
                "none".into()
            } else {
                map.stable_string()
            },
        )),
        Err(error) => checks.push(Check::fail("Model substitutions", error.to_string())),
    }
    for (name, binary) in [("Codex CLI", codex_binary), ("Grok Build CLI", grok_binary)] {
        match codexcli::find_binary(binary) {
            Some(path) => checks.push(Check::pass(name, path.display().to_string())),
            None => checks.push(Check::fail(name, format!("{binary} not found in PATH"))),
        }
    }

    let codex_config = codex_home.join("config.toml");
    match tokio::fs::read_to_string(&codex_config).await {
        Ok(text)
            if text
                .lines()
                .any(|line| line.trim() == "cli_auth_credentials_store = \"file\"") =>
        {
            checks.push(Check::pass(
                "Codex credential configuration",
                codex_config.display().to_string(),
            ));
        }
        Ok(_) => checks.push(Check::fail(
            "Codex credential configuration",
            "cli_auth_credentials_store must be \"file\"",
        )),
        Err(error) => checks.push(Check::fail(
            "Codex credential configuration",
            format!("{}: {error}", codex_config.display()),
        )),
    }

    match Store::new(auth_file, crate::auth::DEFAULT_REFRESH_URL) {
        Ok(store) => match store.inspect().await {
            Ok(status) => {
                #[cfg(unix)]
                let secure = status.file_mode & 0o077 == 0;
                #[cfg(not(unix))]
                let secure = true;
                if secure {
                    checks.push(Check::pass(
                        "ChatGPT auth",
                        format!("{} ({})", status.path.display(), status.auth_mode),
                    ));
                } else {
                    checks.push(Check::fail(
                        "ChatGPT auth",
                        "credential file is group/world accessible",
                    ));
                }
                if !status.has_refresh_token {
                    checks.push(Check::warn(
                        "Refresh token",
                        "missing; login may need to be repeated",
                    ));
                }
            }
            Err(error) => checks.push(Check::fail("ChatGPT auth", error.to_string())),
        },
        Err(error) => checks.push(Check::fail("ChatGPT auth", error.to_string())),
    }

    match GrokConfig::load(grok_config) {
        Ok(config) => {
            let records = config.records();
            if records.is_empty() {
                checks.push(Check::fail(
                    "Grok config",
                    "no loopback Responses model found",
                ));
            } else if records.iter().any(|record| !record.valid) {
                let invalid = records
                    .iter()
                    .filter(|record| !record.valid)
                    .map(|record| format!("{}: {}", record.alias, record.errors.join(", ")))
                    .collect::<Vec<_>>()
                    .join("; ");
                checks.push(Check::fail("Grok config", invalid));
            } else {
                checks.push(Check::pass(
                    "Grok config",
                    format!("{} ({} proxy models)", grok_config.display(), records.len()),
                ));
            }
        }
        Err(error) => checks.push(Check::fail("Grok config", error.to_string())),
    }

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(client) => client,
        Err(error) => {
            checks.push(Check::fail("Proxy readiness", error.to_string()));
            return checks;
        }
    };
    let base = format!("http://{listen}");
    match client.get(format!("{base}/healthz")).send().await {
        Ok(response) => match response.json::<Value>().await {
            Ok(body) if body.get("service").and_then(Value::as_str) == Some("grok-build-proxy") => {
                let mut request = client.get(format!("{base}/readyz"));
                if !client_token.trim().is_empty() {
                    request = request.bearer_auth(client_token.trim());
                }
                match request.send().await {
                    Ok(response) if response.status().is_success() => {
                        checks.push(Check::pass("Proxy readiness", format!("ready at {base}")))
                    }
                    Ok(response) => checks.push(Check::fail(
                        "Proxy readiness",
                        format!("readiness returned HTTP {}", response.status()),
                    )),
                    Err(error) => checks.push(Check::fail("Proxy readiness", error.to_string())),
                }
            }
            _ => checks.push(Check::fail(
                "Proxy readiness",
                "health endpoint is not grok-build-proxy",
            )),
        },
        Err(error) => {
            match listen
                .parse::<SocketAddr>()
                .ok()
                .and_then(|address| std::net::TcpListener::bind(address).ok())
            {
                Some(listener) => {
                    drop(listener);
                    checks.push(Check::warn(
                        "Proxy readiness",
                        format!("not running at {base}: {error}"),
                    ));
                }
                None => checks.push(Check::fail(
                    "Proxy readiness",
                    format!("unreachable and address unavailable: {error}"),
                )),
            }
        }
    }
    checks
}

pub async fn run(auth_file: &Path, grok_config: &Path, codex_home: &Path) -> Vec<Check> {
    run_full(
        auth_file,
        grok_config,
        codex_home,
        "127.0.0.1:18765",
        "",
        "codex",
        "grok",
        "",
        Duration::from_secs(1),
    )
    .await
}

pub fn ensure_ok(checks: &[Check]) -> Result<()> {
    if checks.iter().all(|check| check.ok) {
        Ok(())
    } else {
        anyhow::bail!("one or more doctor checks failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_credentials_are_reported_without_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let checks = run(
            &directory.path().join("missing-auth.json"),
            &directory.path().join("missing-grok.toml"),
            directory.path(),
        )
        .await;
        assert!(
            checks
                .iter()
                .any(|check| check.name == "ChatGPT auth" && !check.ok)
        );
        assert!(ensure_ok(&checks).is_err());
        assert!(
            !checks
                .iter()
                .any(|check| check.detail.contains("access_token"))
        );
    }
}
