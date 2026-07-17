use std::process::Command;

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
    assert!(text.contains("base_url = \"http://127.0.0.1:28765/v1\""));
    assert!(text.contains("reasoning_efforts = [\"low\", \"medium\", \"high\", \"xhigh\"]"));
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
