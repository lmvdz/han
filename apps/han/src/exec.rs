//! `han exec`: the executor role (PLAN §3: always its own process, because it holds the credentials). It runs
//! as its service actor's node, watches the effects it is the executor of, and runs approved ones.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use han_exec::checks::warm::{WarmConfig, WarmRunner};
use han_exec::checks::{Checks, GithubChecks, RoutedChecks, WarmChecks};
use han_exec::{
    ChecksBackend, ExecConfig, Executor, ExecutorConfig, Lane, NeedsApproval, SystemElapsed,
    SystemWall,
};
use han_node::api::ConnectionClass;
use han_registry::HomeRef;
use han_vcs::GitVcs;

use crate::rolenode;

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
    /// The space's home: a home ticket (`han1home.…`) or a ServerName.
    #[arg(long, env = "HAN_HOME")]
    home: HomeRef,
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
        ConnectionClass::Executor,
    )
    .await?;
    let github = match &config.github {
        Some(app) => Some(Arc::new(han_exec::github::GithubMinter::from_app(app)?)),
        None => None,
    };
    // The landing lane runs only for a configuration that names landing refs (default: none). A repository's
    // `checks.backend` picks where its checks run: GitHub Actions (read through the App) or the warm runner.
    let vcs = Arc::new(GitVcs::new(args.data.join("objects")));
    let lane_checks = if Lane::configured(&config) {
        let uses = |b: ChecksBackend| {
            config
                .repos
                .iter()
                .any(|r| !r.landing_refs.is_empty() && r.checks.backend == b)
        };
        let github: Option<Arc<dyn Checks>> = if uses(ChecksBackend::Github) {
            let minter = github.clone().ok_or(
                "a repository's checks are read from GitHub, which needs a GitHub App in the configuration",
            )?;
            Some(Arc::new(GithubChecks::new(minter)))
        } else {
            None
        };
        let warm: Option<Arc<dyn Checks>> = if uses(ChecksBackend::Warm) {
            let runner = Arc::new(WarmRunner::new(WarmConfig::new(args.data.join("warm")))?);
            let vcs = vcs.clone();
            // the lane's commit is in the repository its bundle was imported into
            Some(Arc::new(WarmChecks::new(
                runner,
                Arc::new(move |repo| vcs.landing_for(&repo.remote)),
            )))
        } else {
            None
        };
        Some(Arc::new(RoutedChecks::new(github, warm)))
    } else {
        None
    };
    let exec = Arc::new(Executor::open(
        ExecutorConfig {
            actor: args.actor.clone(),
            actions: han_exec::actions::builtin(),
            gate: Arc::new(NeedsApproval),
            config: Arc::new(config),
            vcs,
            github,
            state_dir: args.data.join("state"),
            stall_bound: Duration::from_secs(10),
        },
        role.api.clone(),
        Arc::new(SystemElapsed::new()),
    )?);
    let lane = lane_checks.map(|c| Lane::new(exec.clone(), c, Arc::new(SystemWall)));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let runner = async { exec.run(rx.clone()).await };
    let landing = async {
        match &lane {
            Some(l) => l.run(rx.clone()).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        r = runner => r?,
        () = landing => {}
        _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); }
    }
    role.node.shutdown();
    role.transport.shutdown().await;
    Ok(())
}
