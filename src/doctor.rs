use crate::{
    auth::Store,
    catalog::{Catalog, normalize_id},
    codexcli,
    grokconfig::GrokConfig,
    modelmap::ModelMap,
    provider::Provider,
    provider::kimi::auth::{DEFAULT_OAUTH_HOST as DEFAULT_KIMI_OAUTH_HOST, Store as KimiStore},
};
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
    kimi_auth_file: Option<&Path>,
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

    let mappings = match ModelMap::parse(model_map) {
        Ok(map) => {
            checks.push(Check::pass(
                "Model substitutions",
                if map.is_empty() {
                    "none".into()
                } else {
                    map.stable_string()
                },
            ));
            Some(map)
        }
        Err(error) => {
            checks.push(Check::fail("Model substitutions", error.to_string()));
            None
        }
    };
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

    if let Some(path) = kimi_auth_file {
        match KimiStore::new(path, DEFAULT_KIMI_OAUTH_HOST) {
            Ok(store) => match store.inspect().await {
                Ok(status) => {
                    #[cfg(unix)]
                    let secure = status.file_mode & 0o077 == 0;
                    #[cfg(not(unix))]
                    let secure = true;
                    if secure {
                        let now = chrono::Utc::now();
                        if status.expires_at <= now && !status.has_refresh_token {
                            checks.push(Check::fail(
                                "Kimi auth",
                                "credential is expired and has no refresh token; run `grok-build-proxy kimi auth login`",
                            ));
                        } else {
                            checks
                                .push(Check::pass("Kimi auth", status.path.display().to_string()));
                            if !status.has_refresh_token {
                                checks.push(Check::warn(
                                    "Kimi refresh token",
                                    "missing; login may need to be repeated when the access token expires",
                                ));
                            }
                        }
                    } else {
                        checks.push(Check::fail(
                            "Kimi auth",
                            "credential file is group/world accessible",
                        ));
                    }
                }
                Err(error) => checks.push(Check::fail("Kimi auth", error.to_string())),
            },
            Err(error) => checks.push(Check::fail("Kimi auth", error.to_string())),
        }
    }

    let mut required_codex = true;
    let mut required_kimi = true;
    match GrokConfig::load(grok_config) {
        Ok(config) => {
            let records = config.records();
            let catalog = Catalog::default();
            required_codex = records.is_empty();
            required_kimi = records.is_empty();
            for record in &records {
                let resolved = mappings
                    .as_ref()
                    .map(|map| map.resolve(&record.model).model)
                    .unwrap_or_else(|| record.model.clone());
                let (model, _) = normalize_id(&resolved);
                match catalog.lookup(&model).0.provider {
                    Provider::Codex => required_codex = true,
                    Provider::Kimi => required_kimi = true,
                }
            }
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

    for check in &mut checks {
        let unused_provider = (!required_codex
            && matches!(
                check.name,
                "Codex CLI" | "Codex credential configuration" | "ChatGPT auth"
            ))
            || (!required_kimi && check.name == "Kimi auth");
        if unused_provider && !check.ok && !check.detail.contains("group/world") {
            check.ok = true;
            check.warning = true;
        }
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
                let mut providers = Vec::new();
                if required_codex {
                    providers.push("codex");
                }
                if required_kimi {
                    providers.push("kimi");
                }
                if providers.is_empty() {
                    providers.push("");
                }
                let mut failures = Vec::new();
                for provider in &providers {
                    let url = if provider.is_empty() {
                        format!("{base}/readyz")
                    } else {
                        format!("{base}/readyz?provider={provider}")
                    };
                    let mut request = client.get(url);
                    if !client_token.trim().is_empty() {
                        request = request.bearer_auth(client_token.trim());
                    }
                    match request.send().await {
                        Ok(response) if response.status().is_success() => {}
                        Ok(response) => failures.push(format!(
                            "{} readiness returned HTTP {}",
                            if provider.is_empty() {
                                "proxy"
                            } else {
                                provider
                            },
                            response.status()
                        )),
                        Err(error) => failures.push(format!(
                            "{} readiness: {error}",
                            if provider.is_empty() {
                                "proxy"
                            } else {
                                provider
                            }
                        )),
                    }
                }
                if failures.is_empty() {
                    checks.push(Check::pass(
                        "Proxy readiness",
                        format!("ready at {base} ({})", providers.join(", ")),
                    ));
                } else {
                    checks.push(Check::fail("Proxy readiness", failures.join("; ")));
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
        None,
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

    #[tokio::test]
    async fn unusable_grok_config_keeps_kimi_auth_required() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing-grok.toml");
        let empty = directory.path().join("empty-grok.toml");
        tokio::fs::write(&empty, "").await.unwrap();

        for grok_config in [&missing, &empty] {
            let checks = run_full(
                &directory.path().join("missing-codex.json"),
                Some(&directory.path().join("missing-kimi.json")),
                grok_config,
                directory.path(),
                "127.0.0.1:0",
                "",
                "missing-codex",
                "missing-grok",
                "",
                Duration::from_millis(50),
            )
            .await;
            assert!(
                checks
                    .iter()
                    .any(|check| { check.name == "Kimi auth" && !check.ok && !check.warning })
            );
        }
    }

    #[tokio::test]
    async fn kimi_credentials_make_missing_codex_setup_optional() {
        let directory = tempfile::tempdir().unwrap();
        let kimi_auth = directory.path().join("kimi-auth.json");
        let grok_config = directory.path().join("grok.toml");
        tokio::fs::write(
            &kimi_auth,
            br#"{"access":"token","refresh":"refresh","expires":4102444800000}"#,
        )
        .await
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&kimi_auth, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        tokio::fs::write(
            &grok_config,
            "[model.kimi]\nmodel = \"mapped-kimi\"\nname = \"Mapped Kimi\"\nbase_url = \"http://127.0.0.1:18765/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 256000\n",
        )
        .await
        .unwrap();

        let checks = run_full(
            &directory.path().join("missing-codex-auth.json"),
            Some(&kimi_auth),
            &grok_config,
            directory.path(),
            "127.0.0.1:0",
            "",
            "missing-codex",
            "missing-grok",
            "mapped-kimi=kimi-for-coding",
            Duration::from_millis(50),
        )
        .await;
        assert!(
            checks
                .iter()
                .any(|check| check.name == "Kimi auth" && check.ok)
        );
        for name in [
            "Codex CLI",
            "Codex credential configuration",
            "ChatGPT auth",
        ] {
            assert!(
                checks
                    .iter()
                    .any(|check| { check.name == name && check.ok && check.warning })
            );
        }

        tokio::fs::write(
            &grok_config,
            "[model.kimi]\nmodel = \"mapped-kimi\"\nname = \"Mapped Kimi\"\nbase_url = \"http://127.0.0.1:18765/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 256000\n\n[model.codex]\nmodel = \"gpt-5.6-sol\"\nname = \"Codex Sol\"\nbase_url = \"http://127.0.0.1:18765/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 372000\n",
        )
        .await
        .unwrap();
        let mixed_checks = run_full(
            &directory.path().join("missing-codex-auth.json"),
            Some(&kimi_auth),
            &grok_config,
            directory.path(),
            "127.0.0.1:0",
            "",
            "missing-codex",
            "missing-grok",
            "mapped-kimi=kimi-for-coding",
            Duration::from_millis(50),
        )
        .await;
        assert!(
            mixed_checks
                .iter()
                .any(|check| check.name == "ChatGPT auth" && !check.ok)
        );
    }

    #[tokio::test]
    async fn expired_kimi_credentials_without_refresh_fail_doctor() {
        let directory = tempfile::tempdir().unwrap();
        let kimi_auth = directory.path().join("kimi-auth.json");
        let grok_config = directory.path().join("grok.toml");
        tokio::fs::write(
            &kimi_auth,
            br#"{"access":"expired","refresh":"","expires":1}"#,
        )
        .await
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&kimi_auth, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        tokio::fs::write(
            &grok_config,
            "[model.kimi]\nmodel = \"kimi-for-coding\"\nname = \"Kimi\"\nbase_url = \"http://127.0.0.1:18765/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 256000\n",
        )
        .await
        .unwrap();

        let checks = run_full(
            &directory.path().join("missing-codex.json"),
            Some(&kimi_auth),
            &grok_config,
            directory.path(),
            "127.0.0.1:0",
            "",
            "missing-codex",
            "missing-grok",
            "",
            Duration::from_millis(50),
        )
        .await;
        assert!(checks.iter().any(|check| {
            check.name == "Kimi auth"
                && !check.ok
                && check.detail.contains("expired")
                && check.detail.contains("no refresh token")
        }));
    }

    #[tokio::test]
    async fn mapped_kimi_doctor_checks_live_kimi_readiness() {
        use axum::{Json, Router, extract::Query, http::StatusCode, routing::get};
        use serde_json::json;
        use std::collections::HashMap;

        let app = Router::new()
            .route(
                "/healthz",
                get(|| async { Json(json!({"service":"grok-build-proxy"})) }),
            )
            .route(
                "/readyz",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    let status = if query.get("provider").map(String::as_str) == Some("kimi") {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    };
                    (status, Json(json!({"ok":status.is_success()})))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let directory = tempfile::tempdir().unwrap();
        let kimi_auth = directory.path().join("kimi-auth.json");
        let grok_config = directory.path().join("grok.toml");
        tokio::fs::write(
            &kimi_auth,
            br#"{"access":"token","refresh":"refresh","expires":4102444800000}"#,
        )
        .await
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&kimi_auth, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        tokio::fs::write(
            &grok_config,
            format!(
                "[model.kimi]\nmodel = \"mapped-kimi\"\nname = \"Mapped Kimi\"\nbase_url = \"http://{address}/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = 256000\n"
            ),
        )
        .await
        .unwrap();

        let checks = run_full(
            &directory.path().join("missing-codex.json"),
            Some(&kimi_auth),
            &grok_config,
            directory.path(),
            &address.to_string(),
            "",
            "missing-codex",
            "missing-grok",
            "mapped-kimi=kimi-for-coding",
            Duration::from_secs(1),
        )
        .await;
        assert!(checks.iter().any(|check| {
            check.name == "Proxy readiness" && !check.ok && check.detail.contains("kimi readiness")
        }));
    }
}
