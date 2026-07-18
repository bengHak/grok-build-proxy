use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use grok_build_proxy::{
    auth::{DEFAULT_REFRESH_URL, Store},
    catalog::Catalog,
    codexcli, doctor,
    grokconfig::{self, GrokConfig},
    modelmap::ModelMap,
    monitor,
    provider::kimi::auth::{
        DEFAULT_OAUTH_HOST as DEFAULT_KIMI_OAUTH_HOST, DEFAULT_UPSTREAM as DEFAULT_KIMI_UPSTREAM,
        Store as KimiStore,
    },
    proxy::{
        self, CompatMode, DEFAULT_CODEX_COMPATIBILITY_VERSION, DEFAULT_MAX_BODY_BYTES, ProxyConfig,
    },
};
use std::{
    env,
    future::IntoFuture,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

mod kimi_cli;

const DEFAULT_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex/responses";
const VERSION: &str = env!("GROK_BUILD_PROXY_BUILD_VERSION");

#[derive(Parser)]
#[command(name="grok-build-proxy",version=VERSION,about="Local Grok Build proxy for ChatGPT Codex and Kimi")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Auth(AuthArgs),
    Kimi(kimi_cli::KimiArgs),
    Doctor(DoctorArgs),
    Models(ModelsArgs),
    Version,
}
#[derive(Args, Clone)]
struct ServeArgs {
    #[arg(
        long,
        env = "GROK_BUILD_PROXY_LISTEN",
        default_value = "127.0.0.1:18765"
    )]
    listen: String,
    #[arg(long, env = "GROK_BUILD_PROXY_AUTH_FILE")]
    auth_file: Option<PathBuf>,
    #[arg(long,env="GROK_BUILD_PROXY_UPSTREAM",default_value=DEFAULT_UPSTREAM)]
    upstream: String,
    #[arg(long,env="GROK_BUILD_PROXY_REFRESH_URL",default_value=DEFAULT_REFRESH_URL)]
    refresh_url: String,
    #[arg(long, env = "GROK_BUILD_PROXY_KIMI_AUTH_FILE")]
    kimi_auth_file: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_KIMI_API_KEY", default_value = "")]
    kimi_api_key: String,
    #[arg(long,env="GROK_BUILD_PROXY_KIMI_UPSTREAM",default_value=DEFAULT_KIMI_UPSTREAM)]
    kimi_upstream: String,
    #[arg(long,env="GROK_BUILD_PROXY_KIMI_OAUTH_HOST",default_value=DEFAULT_KIMI_OAUTH_HOST)]
    kimi_oauth_host: String,
    #[arg(long, env = "GROK_BUILD_PROXY_MODELS", default_value = "")]
    models: String,
    #[arg(long, env = "GROK_BUILD_PROXY_MODEL_MAP", default_value = "")]
    model_map: String,
    #[arg(long,env="GROK_BUILD_PROXY_CODEX_COMPAT_VERSION",default_value=DEFAULT_CODEX_COMPATIBILITY_VERSION)]
    codex_compat_version: String,
    #[arg(long, env = "GROK_BUILD_PROXY_TOKEN", default_value = "")]
    client_token: String,
    #[arg(long, env = "GROK_BUILD_PROXY_LOG_FORMAT", default_value = "text")]
    log_format: String,
    #[arg(long, env = "GROK_BUILD_PROXY_LITE_TOOL_BATCHING")]
    lite_tool_batching: bool,
    #[arg(long)]
    no_monitor: bool,
    #[arg(long)]
    print_grok_config: bool,
    #[arg(long = "version")]
    show_version: bool,
}
#[derive(Args)]
struct AuthArgs {
    #[command(subcommand)]
    action: AuthAction,
}
#[derive(Subcommand)]
enum AuthAction {
    Login(AuthCommon),
    Device(AuthCommon),
    Status(AuthCommon),
    Logout(AuthCommon),
}
#[derive(Args)]
struct AuthCommon {
    #[arg(long, env = "GROK_BUILD_PROXY_CODEX_HOME")]
    codex_home: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_CODEX_BINARY", default_value = "codex")]
    codex_binary: String,
}
#[derive(Args)]
struct DoctorArgs {
    #[arg(long, env = "GROK_BUILD_PROXY_CODEX_HOME")]
    codex_home: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_AUTH_FILE")]
    auth_file: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_KIMI_AUTH_FILE")]
    kimi_auth_file: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_KIMI_API_KEY", default_value = "")]
    kimi_api_key: String,
    #[arg(long, env = "GROK_BUILD_PROXY_GROK_CONFIG")]
    grok_config: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_CODEX_BINARY", default_value = "codex")]
    codex_binary: String,
    #[arg(long, env = "GROK_BUILD_PROXY_GROK_BINARY", default_value = "grok")]
    grok_binary: String,
    #[arg(
        long,
        env = "GROK_BUILD_PROXY_LISTEN",
        default_value = "127.0.0.1:18765"
    )]
    listen: String,
    #[arg(long, env = "GROK_BUILD_PROXY_MODEL_MAP", default_value = "")]
    model_map: String,
    #[arg(long, env = "GROK_BUILD_PROXY_TOKEN", default_value = "")]
    client_token: String,
    #[arg(long, default_value_t = 5)]
    timeout: u64,
}

#[derive(Args)]
struct ModelsArgs {
    #[command(subcommand)]
    action: ModelsAction,
    #[arg(long, env = "GROK_BUILD_PROXY_GROK_CONFIG", global = true)]
    grok_config: Option<PathBuf>,
    #[arg(long, env = "GROK_BUILD_PROXY_LISTEN", global = true)]
    listen: Option<String>,
    #[arg(
        long,
        env = "GROK_BUILD_PROXY_TOKEN",
        default_value = "",
        global = true
    )]
    client_token: String,
    #[arg(
        long,
        env = "GROK_BUILD_PROXY_MODELS",
        default_value = "",
        global = true
    )]
    models: String,
}

#[derive(Subcommand)]
enum ModelsAction {
    Add(ModelAddArgs),
    Update(ModelUpdateArgs),
    Remove(ModelRemoveArgs),
    List(ModelListArgs),
    Status(ModelStatusArgs),
    Sync(ModelSyncArgs),
}

#[derive(Args)]
struct ModelAddArgs {
    alias: String,
    #[arg(long)]
    model: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    fast: bool,
    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModelUpdateArgs {
    alias: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, conflicts_with = "no_fast")]
    fast: bool,
    #[arg(long, conflicts_with = "fast")]
    no_fast: bool,
    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModelRemoveArgs {
    alias: String,
    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct ModelListArgs {
    #[arg(long)]
    available: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ModelStatusArgs {
    alias: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 5)]
    timeout: u64,
}

#[derive(Args)]
struct ModelSyncArgs {
    #[arg(long)]
    include_fast: bool,
    #[arg(long)]
    prune: bool,
    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args)]
struct WriteArgs {
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    dry_run: bool,
}

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("resolve home directory")
}
fn codex_home(explicit: Option<PathBuf>) -> Result<PathBuf> {
    Ok(explicit
        .or_else(|| env::var_os("GROK_BUILD_PROXY_CODEX_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("CODEX_HOME").map(PathBuf::from))
        .unwrap_or(home()?.join(".codex-grok-build-proxy")))
}
fn require_macos() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "grok-build-proxy supports macOS only (detected {})",
            env::consts::OS
        )
    }
    if !matches!(env::consts::ARCH, "aarch64" | "x86_64") {
        bail!(
            "grok-build-proxy does not support macOS/{}",
            env::consts::ARCH
        )
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    let mut args: Vec<_> = env::args_os().collect();
    if args.len() == 1 {
        args.push("serve".into())
    } else {
        let first = args[1].to_string_lossy();
        let known = matches!(
            first.as_ref(),
            "serve"
                | "auth"
                | "doctor"
                | "models"
                | "kimi"
                | "version"
                | "help"
                | "--help"
                | "-h"
                | "--version"
                | "-V"
        );
        if matches!(first.as_ref(), "--version" | "-V") {
            args[1] = "version".into()
        } else if first.starts_with('-')
            && !matches!(first.as_ref(), "--help" | "-h" | "--version" | "-V")
        {
            args.insert(1, "serve".into())
        } else if first == "help" {
            args[1] = "--help".into()
        } else if !known {
        }
    }
    let cli = Cli::parse_from(args);
    match cli.command {
        Command::Serve(a) => serve(a).await,
        Command::Auth(a) => auth_command(a).await,
        Command::Kimi(a) => {
            require_macos()?;
            kimi_cli::run(a).await
        }
        Command::Doctor(a) => doctor_command(a).await,
        Command::Models(a) => models_command(a).await,
        Command::Version => {
            println!("{VERSION}");
            Ok(())
        }
    }
}
async fn serve(a: ServeArgs) -> Result<()> {
    require_macos()?;
    if a.show_version {
        println!("{VERSION}");
        return Ok(());
    }
    let mappings = ModelMap::parse(&a.model_map).context("parse model substitutions")?;
    let catalog = Catalog::new(&a.models);
    if a.print_grok_config {
        print!("{}", render_config(&a.listen, &catalog, &mappings));
        return Ok(());
    }
    if !proxy::is_loopback_listen(&a.listen) && a.client_token.trim().is_empty() {
        bail!(
            "refusing to bind to a non-loopback address without --client-token or GROK_BUILD_PROXY_TOKEN"
        )
    }
    let monitor_enabled = !a.no_monitor && monitor::is_interactive();
    if a.log_format.eq_ignore_ascii_case("json") {
        if monitor_enabled {
            tracing_subscriber::fmt()
                .json()
                .with_writer(std::io::sink)
                .init()
        } else {
            tracing_subscriber::fmt()
                .json()
                .with_writer(std::io::stderr)
                .init()
        }
    } else if monitor_enabled {
        tracing_subscriber::fmt().with_writer(std::io::sink).init()
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .init()
    }
    let codex_home_path = codex_home(None)?;
    let auth_path = a
        .auth_file
        .unwrap_or_else(|| codex_home_path.join("auth.json"));
    let store = Arc::new(Store::new(auth_path, a.refresh_url)?);
    let kimi_auth_path = a
        .kimi_auth_file
        .unwrap_or(home()?.join(".grok-build-proxy/kimi/auth.json"));
    let kimi_store = Arc::new(KimiStore::new(kimi_auth_path, a.kimi_oauth_host)?);
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(20)
        .build()?;
    let dashboard = Arc::new(monitor::Dashboard::new());
    let observer: Option<Arc<dyn proxy::Observer>> = if monitor_enabled {
        Some(dashboard.clone())
    } else {
        None
    };
    let app = proxy::router(ProxyConfig {
        upstream_url: a.upstream,
        credentials: store,
        kimi: Some(proxy::KimiConfig {
            upstream_url: a.kimi_upstream,
            credentials: kimi_store,
            api_key: a.kimi_api_key.trim().to_owned(),
        }),
        catalog,
        model_map: mappings,
        client,
        client_token: a.client_token,
        version: VERSION.into(),
        compatibility_version: a.codex_compat_version,
        responses_compat: CompatMode::from_env()?,
        lite_tool_batching: a.lite_tool_batching,
        observer,
        max_body_bytes: DEFAULT_MAX_BODY_BYTES,
    })?;
    let listener = tokio::net::TcpListener::bind(&a.listen)
        .await
        .with_context(|| format!("listen on {}", a.listen))?;
    tracing::info!(
        address = a.listen,
        version = VERSION,
        monitor = monitor_enabled,
        lite_tool_batching = a.lite_tool_batching,
        "proxy listening"
    );
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .into_future();
    tokio::pin!(server);
    if monitor_enabled {
        tokio::select! { result=&mut server=>result?, result=monitor::run(dashboard,&a.listen,VERSION)=>result? }
    } else {
        server.await?;
    }
    Ok(())
}
async fn auth_command(a: AuthArgs) -> Result<()> {
    require_macos()?;
    let common = match &a.action {
        AuthAction::Login(c)
        | AuthAction::Device(c)
        | AuthAction::Status(c)
        | AuthAction::Logout(c) => c,
    };
    let home = codex_home(common.codex_home.clone())?;
    let args: &[&str] = match a.action {
        AuthAction::Login(_) => &["login"],
        AuthAction::Device(_) => &["login", "--device-auth"],
        AuthAction::Status(_) => &["login", "status"],
        AuthAction::Logout(_) => &["logout"],
    };
    codexcli::run(&common.codex_binary, args, &home, true).await?;
    if matches!(args, ["logout"]) {
        println!("Codex credentials cleared from {}", home.display());
        return Ok(());
    }
    let status = Store::new(home.join("auth.json"), DEFAULT_REFRESH_URL)?
        .inspect()
        .await?;
    println!(
        "\nProxy credential file: {}\nAuthentication mode: {}",
        status.path.display(),
        status.auth_mode
    );
    if !status.account_id.is_empty() {
        let n = status.account_id.len();
        println!(
            "ChatGPT account: {}",
            if n <= 8 {
                format!("…{}", status.account_id)
            } else {
                format!(
                    "{}…{}",
                    &status.account_id[..4],
                    &status.account_id[n - 4..]
                )
            }
        );
    }
    println!(
        "Refresh token: {}",
        if status.has_refresh_token {
            "present"
        } else {
            "missing"
        }
    );
    println!("Run `grok-build-proxy doctor` to validate the complete setup.");
    Ok(())
}
async fn doctor_command(a: DoctorArgs) -> Result<()> {
    let codex_home_path = codex_home(a.codex_home)?;
    let auth = a
        .auth_file
        .unwrap_or_else(|| codex_home_path.join("auth.json"));
    let kimi_auth = a
        .kimi_auth_file
        .unwrap_or(home()?.join(".grok-build-proxy/kimi/auth.json"));
    let config = a.grok_config.unwrap_or(home()?.join(".grok/config.toml"));
    let checks = doctor::run_full(
        &auth,
        Some(&kimi_auth),
        &a.kimi_api_key,
        &config,
        &codex_home_path,
        &a.listen,
        &a.client_token,
        &a.codex_binary,
        &a.grok_binary,
        &a.model_map,
        std::time::Duration::from_secs(a.timeout),
    )
    .await;
    for c in &checks {
        println!(
            "[{}] {}: {}",
            if !c.ok {
                "FAIL"
            } else if c.warning {
                "WARN"
            } else {
                "PASS"
            },
            c.name,
            c.detail
        )
    }
    let pass = checks.iter().filter(|c| c.ok).count();
    println!("\n{pass} passed, {} failed", checks.len() - pass);
    doctor::ensure_ok(&checks)
}
async fn models_command(args: ModelsArgs) -> Result<()> {
    let path = args
        .grok_config
        .unwrap_or(home()?.join(".grok/config.toml"));
    let listen_explicit = args.listen.is_some();
    let listen = args.listen.unwrap_or_else(|| "127.0.0.1:18765".into());
    let catalog = Catalog::new(&args.models);
    match args.action {
        ModelsAction::Add(action) => {
            let mut config = GrokConfig::load(&path)?;
            let spec = grokconfig::model_spec(
                &catalog,
                action.alias,
                &action.model,
                action.fast,
                &listen,
                &args.client_token,
                action.name.as_deref(),
            )?;
            let changes = config.add(&spec)?;
            apply_changes(config, changes, action.write)
        }
        ModelsAction::Update(action) => {
            let mut config = GrokConfig::load(&path)?;
            let current = config.record(&action.alias)?;
            let (_, existing_fast) = grok_build_proxy::catalog::normalize_id(&current.model);
            let target = action.model.as_deref().unwrap_or(&current.model);
            let (base, requested_fast) = grok_build_proxy::catalog::normalize_id(target);
            let fast = if action.fast {
                true
            } else if action.no_fast {
                false
            } else if requested_fast {
                true
            } else {
                existing_fast
            };
            let api_key = if args.client_token.is_empty() {
                config
                    .raw_api_key(&action.alias)
                    .unwrap_or_else(|_| "unused".into())
            } else {
                args.client_token.clone()
            };
            let update_listen = if listen_explicit {
                listen.clone()
            } else {
                listen_from_base_url(&current.base_url).unwrap_or_else(|| listen.clone())
            };
            let name = action.name.as_deref().unwrap_or(&current.name);
            let spec = grokconfig::model_spec(
                &catalog,
                action.alias,
                &base,
                fast,
                &update_listen,
                &api_key,
                Some(name),
            )?;
            let changes = config.update(&spec)?;
            apply_changes(config, changes, action.write)
        }
        ModelsAction::Remove(action) => {
            let mut config = GrokConfig::load(&path)?;
            let changes = config.remove(&action.alias)?;
            apply_changes(config, changes, action.write)
        }
        ModelsAction::List(action) => {
            let config = GrokConfig::load(&path)?;
            if action.available {
                let available = grokconfig::available_models(&catalog);
                if action.json {
                    println!("{}", serde_json::to_string_pretty(&available)?);
                } else {
                    println!("MODEL\tNAME\tFAST");
                    for model in available {
                        println!(
                            "{}\t{}\t{}",
                            model.model,
                            model.name,
                            if model.fast { "yes" } else { "no" }
                        );
                    }
                }
            } else {
                let records = config.records();
                if action.json {
                    println!("{}", serde_json::to_string_pretty(&records)?);
                } else if records.is_empty() {
                    println!("No proxy-backed models configured in {}", path.display());
                } else {
                    println!("ALIAS\tMODEL\tTIER\tMANAGED\tVALID\tAPI KEY");
                    for record in records {
                        println!(
                            "{}\t{}\t{}\t{}\t{}\t{}",
                            record.alias,
                            record.model,
                            record.service_tier,
                            yes_no(record.managed),
                            yes_no(record.valid),
                            record.api_key
                        );
                    }
                }
            }
            Ok(())
        }
        ModelsAction::Status(action) => {
            let config = GrokConfig::load(&path)?;
            let status_listen = if listen_explicit {
                listen.clone()
            } else {
                config
                    .records()
                    .first()
                    .and_then(|record| listen_from_base_url(&record.base_url))
                    .unwrap_or_else(|| listen.clone())
            };
            let statuses = grokconfig::status(
                &config,
                action.alias.as_deref(),
                &status_listen,
                &args.client_token,
                Duration::from_secs(action.timeout),
            )
            .await?;
            if action.json {
                println!("{}", serde_json::to_string_pretty(&statuses)?);
            } else if statuses.is_empty() {
                println!("No proxy-backed models configured in {}", path.display());
            } else {
                println!("ALIAS\tTIER\tCONFIG\tPROXY\tREADY\tADVERTISED\tMETADATA\tDETAIL");
                for status in &statuses {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        status.alias,
                        status.service_tier,
                        pass_fail(status.configured),
                        pass_fail(status.proxy),
                        pass_fail(status.ready),
                        pass_fail(status.advertised),
                        pass_fail(status.metadata),
                        status.detail
                    );
                }
            }
            if statuses.iter().any(|status| {
                !status.configured
                    || !status.proxy
                    || !status.ready
                    || !status.advertised
                    || !status.metadata
            }) {
                bail!("one or more model status checks failed")
            }
            Ok(())
        }
        ModelsAction::Sync(action) => {
            let mut config = GrokConfig::load(&path)?;
            let (mut specs, unsupported) =
                grokconfig::sync_specs(&catalog, &listen, &args.client_token, action.include_fast)?;
            if args.client_token.is_empty() {
                let inherited = config
                    .inherited_api_key(&format!("http://{}/v1", listen.trim_end_matches('/')))?;
                for spec in &mut specs {
                    if let Ok(api_key) = config.raw_api_key(&spec.alias) {
                        spec.api_key = api_key;
                    } else if let Some(api_key) = &inherited {
                        spec.api_key = api_key.clone();
                    }
                }
            }
            let changes = config.sync(&specs, action.prune)?;
            for model in unsupported {
                println!("fast unsupported: {model}");
            }
            apply_changes(config, changes, action.write)
        }
    }
}

fn apply_changes(
    config: GrokConfig,
    changes: grokconfig::ChangeSet,
    args: WriteArgs,
) -> Result<()> {
    if changes.is_empty() {
        println!("No changes needed.");
        return Ok(());
    }
    println!("Planned changes to {}:", config.path().display());
    for change in &changes.changes {
        println!("  - {change}");
    }
    if args.dry_run {
        println!("Dry run; no file was changed.");
        return Ok(());
    }
    if !args.yes {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!("refusing to modify config non-interactively without --yes (or use --dry-run)")
        }
        print!("Apply these changes? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Cancelled; no file was changed.");
            return Ok(());
        }
    }
    let path = config.path().to_owned();
    let backup = config.commit()?;
    println!("Updated {}", path.display());
    if let Some(backup) = backup {
        println!("Backup: {}", backup.display());
    }
    Ok(())
}

fn listen_from_base_url(base_url: &str) -> Option<String> {
    let url = url::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn pass_fail(value: bool) -> &'static str {
    if value { "PASS" } else { "FAIL" }
}

fn render_config(listen: &str, catalog: &Catalog, mappings: &ModelMap) -> String {
    let mut out="# Add selected blocks to ~/.grok/config.toml\n\n# Optional global default used by the Quick Start:\n# [models]\n# default_reasoning_effort = \"xhigh\"\n\n".to_owned();
    let mut mapped = std::collections::HashSet::new();
    for e in mappings.entries() {
        mapped.insert(e.source.clone());
        let r = mappings.resolve(&e.source);
        let (m, _) = catalog.lookup(&r.model);
        out.push_str(&config_block(
            &e.source,
            &e.source,
            &format!(
                "{} via {} {}",
                display_id(&e.source),
                m.provider.owned_by(),
                m.display_name
            ),
            listen,
            m.context_window,
            m.reasoning.as_ref(),
        ));
    }
    for m in catalog.models() {
        if !mapped.contains(&m.id) {
            out.push_str(&config_block(
                &format!(
                    "{}-{}",
                    m.provider.as_str(),
                    m.id.replace(['.', '_', '/'], "-")
                ),
                &m.id,
                &if m.provider == grok_build_proxy::provider::Provider::Kimi {
                    m.display_name.clone()
                } else {
                    format!("Codex {}", m.display_name)
                },
                listen,
                m.context_window,
                m.reasoning.as_ref(),
            ));
        }
    }
    out
}
fn config_block(
    key: &str,
    model: &str,
    name: &str,
    listen: &str,
    context: u64,
    reasoning: Option<&grok_build_proxy::catalog::ReasoningCapability>,
) -> String {
    let key = if key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.into()
    } else {
        format!("{key:?}")
    };
    let mut s = format!(
        "[model.{key}]\nmodel = {model:?}\nname = {name:?}\nbase_url = \"http://{listen}/v1\"\napi_backend = \"responses\"\napi_key = \"unused\"\ncontext_window = {context}\n"
    );
    if let Some(r) = reasoning {
        s.push_str("supports_reasoning_effort = true\nreasoning_efforts = [");
        s.push_str(
            &r.efforts
                .iter()
                .map(|e| format!("{:?}", e.value))
                .collect::<Vec<_>>()
                .join(", "),
        );
        s.push_str("]\n");
    }
    s.push('\n');
    s
}
fn display_id(id: &str) -> String {
    id.split(['-', '_', '/'])
        .map(|s| match s.to_ascii_lowercase().as_str() {
            "grok" => "Grok".into(),
            "gpt" => "GPT".into(),
            "codex" => "Codex".into(),
            _ => {
                let mut c = s.chars();
                c.next()
                    .map(|x| x.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
