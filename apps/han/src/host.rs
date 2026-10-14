//! `han host`: one agent session (PLAN §3). The node's supervisor starts it for `AgentStart`; it runs as the
//! agent's own node (the agent's key is its node id), drives the harness over ACP inside the sandbox, and
//! publishes status and timeline items until the work stops.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use han_host::driver::{DriverConfig, builtin};
use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_vcs::GitVcs;

use crate::rolenode::{self, HomeArgs};

/// Options of `han host`.
#[derive(Args)]
pub struct HostArgs {
    /// The space.
    #[arg(long)]
    space: String,
    /// The agent actor.
    #[arg(long)]
    agent: String,
    /// The agent's key id in its profile.
    #[arg(long, default_value = "k1")]
    key_id: String,
    /// The harness: `pi`, `claude`, or a path to a driver file (TOML or JSON).
    #[arg(long)]
    harness: String,
    /// The repository: `github:owner/repo`, an `https://` URL, or a local path.
    #[arg(long)]
    repo: Option<String>,
    /// The base to start from (branch, `HEAD` or commit).
    #[arg(long, default_value = "HEAD")]
    base: String,
    /// The branch pull requests target.
    #[arg(long, default_value = "main")]
    default_branch: String,
    /// The issue, `github:owner/repo#123`.
    #[arg(long)]
    issue: Option<String>,
    /// A task to work instead of finding or claiming the issue's.
    #[arg(long)]
    task: Option<String>,
    /// The agent's operator (its guard when guarded).
    #[arg(long)]
    operator: Option<String>,
    /// The executor that runs approved effects (default `@exec:<home server>`).
    #[arg(long)]
    executor: Option<String>,
    #[command(flatten)]
    home: HomeArgs,
    /// The agent's data directory (its node, sessions, taint store).
    #[arg(long, default_value = "han-agent")]
    data: PathBuf,
    /// Where the key store is (`<keys>/keys` for file keys; else the keychain).
    #[arg(long, default_value = "han-data")]
    keys: PathBuf,
    /// Sandbox: `auto`, `mxc`, `bwrap` or `none`.
    #[arg(long, default_value = "auto")]
    sandbox: String,
    /// Allow running without an OS sandbox (advisory; it shows in status).
    #[arg(long)]
    allow_advisory: bool,
    /// Run the session guarded.
    #[arg(long)]
    guarded: bool,
    /// The node executable for node-based adapters.
    #[arg(long, default_value = "node")]
    node: PathBuf,
    /// The adapter's installed directory.
    #[arg(long)]
    adapter_dir: Option<PathBuf>,
    /// Relays: `n0`, `none`, or URLs.
    #[arg(long, default_value = "n0")]
    relay: String,
    /// The agent's own login directory (the harness's config directory), bound read-write into this agent's
    /// sandbox only. Default `<data>/claude-config`; `han agent login` makes the login in it.
    #[arg(long)]
    login_dir: Option<PathBuf>,
    /// API-key mode: a file (readable only by this user) holding the model key. It stays in the proxy.
    #[arg(long)]
    model_key_file: Option<PathBuf>,
}

fn driver(spec: &str) -> Result<DriverConfig, Box<dyn std::error::Error>> {
    if let Some(d) = builtin(spec) {
        return Ok(d);
    }
    let text = std::fs::read_to_string(spec)?;
    Ok(if spec.ends_with(".toml") {
        DriverConfig::from_toml(&text)?
    } else {
        DriverConfig::from_json(&text)?
    })
}

fn repo(spec: Option<&str>, base: &str, default_branch: &str) -> RepoConfig {
    let spec = spec.unwrap_or(".");
    let source = match spec.strip_prefix("github:") {
        Some(r) => format!("https://github.com/{r}.git"),
        None => spec.to_owned(),
    };
    RepoConfig {
        name: spec.to_owned(),
        source,
        base: base.to_owned(),
        default_branch: default_branch.to_owned(),
    }
}

pub async fn run(args: HostArgs) -> Result<(), Box<dyn std::error::Error>> {
    let driver = driver(&args.harness)?;
    // The agent's own login must exist before a session opens: it is the one thing of the user's that the
    // sandbox gets (`han agent login <agent>`).
    let login_dir = args
        .login_dir
        .clone()
        .unwrap_or_else(|| args.data.join(han_node::agents::LOGIN_DIR));
    han_host::driver::login_binding(&driver, Some(&login_dir))?;
    let a = args.agent.trim_start_matches('@');
    let (name, server) = a.split_once(':').unwrap_or((a, ""));
    let key_name = format!("agent-{name}-{server}");
    let role = rolenode::start(
        &args.agent,
        &args.key_id,
        &key_name,
        &args.keys,
        &args.data.join("node"),
        &args.relay,
        &args.home,
        &args.space,
    )
    .await?;
    // API-key mode: the real key stays in the proxy (SR-33, SR-39); the agent sees a dummy.
    let model_key = match (&driver.credentials, &args.model_key_file) {
        (han_host::driver::CredentialMode::ApiKey { .. }, Some(f)) => {
            Some(std::fs::read_to_string(f)?.trim().to_owned())
        }
        _ => None,
    };
    let seed = rolenode::key_store(&args.keys, &key_name)?
        .load(han_node::keys::DEVICE_KEY)?
        .ok_or("the agent's key is missing")?;
    let shim = std::env::current_exe()?;
    let session_dir = args.data.join("sessions").join(
        args.issue
            .as_deref()
            .unwrap_or("adhoc")
            .replace(['/', '#', ':'], "_"),
    );
    let cfg = HostConfig {
        space: args.space.clone(),
        agent: args.agent.clone(),
        operator: args.operator.clone(),
        driver,
        repo: repo(args.repo.as_deref(), &args.base, &args.default_branch),
        issue: args.issue.clone(),
        task: args.task.clone(),
        executor: args
            .executor
            .clone()
            .unwrap_or(format!("@exec:{}", args.home.server()?)),
        session_dir,
        taint_db: args.data.join("taint.db"),
        guarded: args.guarded,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: args.sandbox.clone(),
            allow_advisory: args.allow_advisory,
            ro: Vec::new(),
            login_dir: Some(login_dir),
            allow_hosts: Vec::new(),
            test_loopback_upstream: false,
            test_roots: Vec::new(),
        },
        shim,
        node: args.node.clone(),
        adapter_dir: args.adapter_dir.clone().unwrap_or_default(),
        model_key,
        turn_timeout: Duration::from_secs(30 * 60),
        exclusion: Vec::new(),
    };
    let deps = HostDeps {
        api: Arc::new(role.node.clone()),
        vcs: Arc::new(GitVcs::new(args.data.join("objects")).with_protocols(["https"])),
        signer: Some((args.agent.clone(), args.key_id.clone(), seed)),
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let runner = run_session(cfg, deps, rx);
    tokio::pin!(runner);
    let end = tokio::select! {
        r = &mut runner => r,
        _ = tokio::signal::ctrl_c() => {
            let _ = stop.send(true);
            runner.await
        }
    };
    role.node.shutdown();
    role.transport.shutdown().await;
    let end = end?;
    println!("session {} ended {:?}", end.session, end.state);
    Ok(())
}
