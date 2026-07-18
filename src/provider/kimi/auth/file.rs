use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{fs, io::AsyncWriteExt};

use super::StoredAuth;

pub(super) async fn load_auth(path: &Path) -> Result<StoredAuth> {
    load_auth_with_policy(path, true).await
}

pub(super) async fn load_auth_for_inspection(path: &Path) -> Result<StoredAuth> {
    load_auth_with_policy(path, false).await
}

async fn load_auth_with_policy(path: &Path, require_private: bool) -> Result<StoredAuth> {
    validate_private_file(path, "Kimi credentials", require_private).await?;
    let data = fs::read(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "Kimi credentials not found at {}; run `grok-build-proxy kimi auth login`",
                path.display()
            )
        } else {
            anyhow!(error).context("read Kimi auth file")
        }
    })?;
    let auth: StoredAuth = serde_json::from_slice(&data).context("parse Kimi auth file")?;
    if auth.access.trim().is_empty() {
        bail!("Kimi auth file is missing an access token; run `grok-build-proxy kimi auth login`")
    }
    Ok(auth)
}

async fn validate_private_file(
    path: &Path,
    description: &str,
    require_private: bool,
) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to read {description} through a symbolic link")
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("{description} path is not a regular file")
        }
        #[cfg(unix)]
        Ok(metadata) if require_private => {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!(
                    "refusing to read group/world-accessible {description}; run `chmod 600 {}`",
                    path.display()
                )
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context(format!("inspect {description}")),
    }
    Ok(())
}

pub(super) async fn save_auth(path: &Path, auth: &StoredAuth) -> Result<()> {
    let mut data = serde_json::to_vec_pretty(auth)?;
    data.push(b'\n');
    save_private(path, &data).await
}

pub(super) async fn remove_auth(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove Kimi auth file"),
    }
}

pub(super) async fn device_id(auth_path: &Path) -> Result<String> {
    let path = device_id_path(auth_path);
    validate_private_file(&path, "Kimi device ID", true).await?;
    match fs::read_to_string(&path).await {
        Ok(value) if !value.trim().is_empty() => return Ok(value.trim().to_owned()),
        Ok(_) => bail!("Kimi device ID file is empty at {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("read Kimi device ID"),
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    save_private(&path, format!("{id}\n").as_bytes()).await?;
    Ok(id)
}

fn device_id_path(auth_path: &Path) -> PathBuf {
    auth_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("device_id")
}

async fn save_private(path: &Path, data: &[u8]) -> Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let temporary = directory.join(format!(".kimi-auth-{}", uuid::Uuid::new_v4()));
    let result = async {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(data).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, path).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        fs::File::open(directory).await?.sync_all().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}
