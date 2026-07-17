use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{fs, io::AsyncWriteExt, process::Command};

pub fn find_codex() -> Option<PathBuf> {
    find_binary("codex")
}
pub fn find_binary(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_owned());
    }
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(PathBuf::from)
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}

pub async fn ensure_auth_config(home: &Path) -> Result<()> {
    fs::create_dir_all(home).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let path = home.join("config.toml");
    let original = fs::read_to_string(&path).await.unwrap_or_default();
    let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
    let first_table = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    for (key, value) in [
        ("cli_auth_credentials_store", "\"file\""),
        ("forced_login_method", "\"chatgpt\""),
    ] {
        let mut found = false;
        for line in &mut lines[..first_table] {
            let trimmed = line.trim_start();
            if trimmed
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
            {
                let indent = &line[..line.len() - trimmed.len()];
                *line = format!("{indent}{key} = {value}");
                found = true;
                break;
            }
        }
        if !found {
            lines.insert(first_table, key.to_string() + " = " + value);
        }
    }
    let mut content = lines.join("\n");
    content.push('\n');
    let tmp = home.join(format!(".config.toml.{}", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .await?;
    file.write_all(content.as_bytes()).await?;
    file.sync_all().await?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await?;
    }
    fs::rename(tmp, &path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

pub async fn run(
    binary: &str,
    args: &[&str],
    codex_home: &Path,
    inherit_io: bool,
) -> Result<std::process::ExitStatus> {
    ensure_auth_config(codex_home).await?;
    let bin = find_binary(binary)
        .with_context(|| format!("official `{binary}` CLI not found in PATH"))?;
    let mut cmd = Command::new(bin);
    cmd.args(args).env("CODEX_HOME", codex_home);
    if inherit_io {
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    let status = cmd.status().await.context("run Codex CLI")?;
    if !status.success() {
        bail!("codex {} failed", args.join(" "))
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn edits_root_without_losing_tables() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("config.toml"),
            "model = \"x\"\n[project]\nkeep = true\n",
        )
        .await
        .unwrap();
        ensure_auth_config(d.path()).await.unwrap();
        let s = fs::read_to_string(d.path().join("config.toml"))
            .await
            .unwrap();
        assert!(s.contains("model = \"x\""));
        assert!(s.contains("cli_auth_credentials_store = \"file\""));
        assert!(s.contains("[project]\nkeep = true"));
    }
}
