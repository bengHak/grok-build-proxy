use std::{fs, process::Command};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_grok-build-proxy"))
}

#[test]
fn version_contract_is_plain_and_stable() {
    for argument in ["--version", "version"] {
        let output = binary().arg(argument).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn config_output_uses_real_catalog_and_requested_address() {
    let output = binary()
        .args([
            "serve",
            "--listen",
            "127.0.0.1:28765",
            "--print-grok-config",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("model = \"gpt-5.6-sol\""));
    assert!(text.contains("model = \"kimi-for-coding\""));
    assert!(text.contains("name = \"Kimi K2.6\""));
    assert!(text.contains("base_url = \"http://127.0.0.1:28765/v1\""));
    assert!(text.contains("reasoning_efforts = [\"low\", \"medium\", \"high\", \"xhigh\"]"));
}

#[test]
fn kimi_auth_status_reads_the_selected_auth_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("kimi-auth.json");
    std::fs::write(
        &path,
        br#"{"access":"secret","refresh":"rotate","expires":1893456000000,"userId":"user-1"}"#,
    )
    .unwrap();
    let output = binary()
        .args(["kimi", "auth", "status", "--auth-file"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Kimi credential file:"));
    assert!(text.contains("Refresh token: present"));
    assert!(!text.contains("secret"));
}

#[test]
fn unsafe_bind_without_token_is_rejected() {
    let output = binary()
        .args(["serve", "--listen", "0.0.0.0:28765", "--no-monitor"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to bind"));
}

#[test]
fn model_crud_supports_fast_and_preserves_other_config() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    fs::write(&config, "[ui]\nsimple_mode = true\n").unwrap();
    let path = config.to_str().unwrap();

    let add = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "add",
            "codex-sol",
            "--model",
            "gpt-5.6-sol",
            "--name",
            "My Codex",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("[ui]\nsimple_mode = true"));
    assert!(text.contains("model = \"gpt-5.6-sol\""));

    let fast = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "update",
            "codex-sol",
            "--fast",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(
        fast.status.success(),
        "{}",
        String::from_utf8_lossy(&fast.stderr)
    );
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("model = \"gpt-5.6-sol-fast\""));
    assert!(text.contains("name = \"My Codex\""));

    let retarget = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "update",
            "codex-sol",
            "--model",
            "gpt-5.6-terra",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(retarget.status.success());
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("model = \"gpt-5.6-terra-fast\""));
    assert!(text.contains("name = \"My Codex\""));

    let list = binary()
        .args(["models", "--grok-config", path, "list", "--json"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(stdout.contains("\"service_tier\": \"priority\""));

    let remove = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "remove",
            "codex-sol",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(remove.status.success());
    assert!(
        !fs::read_to_string(&config)
            .unwrap()
            .contains("[model.codex-sol]")
    );
}

#[test]
fn model_sync_preview_and_fast_selection_are_safe() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    let path = config.to_str().unwrap();

    let refused = binary()
        .args(["models", "--grok-config", path, "sync"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(!config.exists());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("without --yes"));

    let preview = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "sync",
            "--include-fast",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(!config.exists());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("priority"));

    let sync = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "sync",
            "--include-fast",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(
        sync.status.success(),
        "{}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("[model.codex-sol-fast]"));
    assert!(text.contains("model = \"gpt-5.6-sol-fast\""));

    let available = binary()
        .args([
            "models",
            "--grok-config",
            path,
            "list",
            "--available",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(available.status.success());
    assert!(String::from_utf8_lossy(&available.stdout).contains("\"fast\": true"));
}

#[test]
fn model_preview_never_prints_client_token() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    let token = "super-secret-local-token";
    let output = binary()
        .env("GROK_BUILD_PROXY_TOKEN", token)
        .args([
            "models",
            "--grok-config",
            config.to_str().unwrap(),
            "add",
            "codex-sol",
            "--model",
            "gpt-5.6-sol",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
}
