//! `han exec`: the executor role (PLAN §3: always its own process, because it holds the credentials). It runs
//! as its service actor's node, watches the effects it is the executor of, and runs approved ones.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use han_exec::{ExecConfig, Executor, ExecutorConfig, NeedsApproval, SystemElapsed};
use han_vcs::GitVcs;

use crate::rolenode::{self, HomeArgs};

/// Options of `han exec`.
#[derive(Args)]
pub struct ExecArgs {
    /// The executor actor, for example `@exec:han.example`.
    #[arg(long)]
    actor: String,
    /// The id of the executor instance's key in the actor's profile (X-01: `effect.claim.instance`).
    #[arg(long, default_value = "k1")]
    key_id: String,
    /// The name of the instance's key in the key store.
    #[arg(long)]
    key_name: Option<String>,
    /// The space to serve.
    #[arg(long)]
    space: String,
    #[command(flatten)]
    home: HomeArgs,
    /// The executor's state directory: ledger, lock, scratch, its node's data.
    #[arg(long, default_value = "han-exec")]
    data: PathBuf,
    /// Where the key store is (`<keys>/keys` for file keys; else the keychain).
    #[arg(long, default_value = "han-data")]
    keys: PathBuf,
    /// The executor's configuration (JSON `ExecConfig`): repositories, HTTP hosts, branch prefixes, GitHub
    /// App.
    #[arg(long)]
    config: PathBuf,
    /// Relays: `n0`, `none`, or URLs.
    #[arg(long, env = "HAN_RELAY", default_value = "n0")]
    relay: String,
}

pub async fn run(args: ExecArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config: ExecConfig = serde_json::from_slice(&std::fs::read(&args.config)?)?;
    let key_name = args
        .key_name
        .clone()
        .unwrap_or_else(|| rolenode::key_name(&args.actor));
    let role = rolenode::start(
        &args.actor,
        &args.key_id,
        &key_name,
        &args.keys,
        &args.data.join("node"),
        &args.relay,
        &args.home,
        &args.space,
    )
    .await?;
    let github = match &config.github {
        Some(app) => Some(Arc::new(han_exec::github::GithubMinter::from_app(app)?)),
        None => None,
    };
    let exec = Executor::open(
        ExecutorConfig {
            actor: args.actor.clone(),
            actions: han_exec::actions::builtin(),
            gate: Arc::new(NeedsApproval),
            config: Arc::new(config),
            vcs: Arc::new(GitVcs::new(args.data.join("objects"))),
            github,
            state_dir: args.data.join("state"),
            stall_bound: Duration::from_secs(10),
        },
        Arc::new(role.node.clone()),
        Arc::new(SystemElapsed::new()),
    )?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let runner = async { exec.run(rx).await };
    tokio::select! {
        r = runner => r?,
        _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); }
    }
    role.node.shutdown();
    role.transport.shutdown().await;
    Ok(())
}
