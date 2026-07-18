use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use grok_build_proxy::provider::kimi::auth::{DEFAULT_OAUTH_HOST, Store};

#[derive(Args)]
pub struct KimiArgs {
    #[command(subcommand)]
    command: KimiCommand,
}

#[derive(Subcommand)]
enum KimiCommand {
    Auth(KimiAuthArgs),
}

#[derive(Args)]
struct KimiAuthArgs {
    #[command(subcommand)]
    action: KimiAuthAction,
}

#[derive(Subcommand)]
enum KimiAuthAction {
    Login(KimiAuthCommon),
    Status(KimiAuthCommon),
    Logout(KimiAuthCommon),
}

#[derive(Args)]
struct KimiAuthCommon {
    #[arg(long, env = "GROK_BUILD_PROXY_KIMI_AUTH_FILE")]
    auth_file: Option<PathBuf>,
    #[arg(
        long,
        env = "GROK_BUILD_PROXY_KIMI_OAUTH_HOST",
        default_value = DEFAULT_OAUTH_HOST
    )]
    oauth_host: String,
}

pub async fn run(args: KimiArgs) -> Result<()> {
    match args.command {
        KimiCommand::Auth(args) => auth(args).await,
    }
}

async fn auth(args: KimiAuthArgs) -> Result<()> {
    let common = match &args.action {
        KimiAuthAction::Login(common)
        | KimiAuthAction::Status(common)
        | KimiAuthAction::Logout(common) => common,
    };
    let path = match common.auth_file.clone() {
        Some(path) => path,
        None => default_auth_path()?,
    };
    let store = Store::new(&path, &common.oauth_host)?;
    match args.action {
        KimiAuthAction::Login(_) => login(&store).await,
        KimiAuthAction::Status(_) => status(&store).await,
        KimiAuthAction::Logout(_) => {
            store.logout().await?;
            println!("Kimi credentials cleared from {}", path.display());
            Ok(())
        }
    }
}

async fn login(store: &Store) -> Result<()> {
    let authorization = store.begin_device_login().await?;
    println!("Visit: {}", authorization.verification_uri_complete);
    println!("Code:  {}", authorization.user_code);
    println!("Waiting for authorization…");
    store.finish_device_login(&authorization).await?;
    println!("Kimi credentials saved to {}", store.path().display());
    Ok(())
}

async fn status(store: &Store) -> Result<()> {
    let status = store.inspect().await?;
    println!("Kimi credential file: {}", status.path.display());
    println!("Expires at: {}", status.expires_at.to_rfc3339());
    println!(
        "Refresh token: {}",
        if status.has_refresh_token {
            "present"
        } else {
            "missing"
        }
    );
    if let Some(user_id) = status.user_id {
        println!("User: {}", masked(&user_id));
    }
    Ok(())
}

fn default_auth_path() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("resolve home directory")
        .map(|home| home.join(".grok-build-proxy/kimi/auth.json"))
}

fn masked(value: &str) -> String {
    let characters: Vec<_> = value.chars().collect();
    if characters.len() <= 8 {
        format!("…{value}")
    } else {
        format!(
            "{}…{}",
            characters[..4].iter().collect::<String>(),
            characters[characters.len() - 4..]
                .iter()
                .collect::<String>()
        )
    }
}
