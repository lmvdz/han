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
    run_with_ready(args, None).await
}

pub(crate) async fn run_with_ready(
    args: ExecArgs,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config: ExecConfig = serde_json::from_slice(&std::fs::read(&args.config)?)?;
    let key_name = args
        .key_name
        .clone()
        .unwrap_or_else(|| rolenode::key_name(&args.actor));
    let node_data = args.data.join("node");
    let starting = rolenode::start(
        &args.actor,
        &args.key_id,
        &key_name,
        &args.keys,
        &node_data,
        &args.relay,
        &args.home,
        &args.space,
        ConnectionClass::Executor,
    );
    let role = tokio::select! {
        biased;
        _ = crate::shutdown::requested() => return Ok(()),
        result = starting => result?,
    };
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
    if let Some(ready) = ready {
        ready
            .send(())
            .map_err(|_| "executor readiness receiver closed")?;
    }
    let (stop, rx) = tokio::sync::watch::channel(false);
    let runner = async { exec.run(rx.clone()).await };
    let landing = async {
        match &lane {
            Some(l) => l.run(rx.clone()).await,
            None => {
                let mut stopped = rx.clone();
                while !*stopped.borrow_and_update() {
                    if stopped.changed().await.is_err() {
                        break;
                    }
                }
            }
        }
    };
    tokio::pin!(runner, landing);
    let mut runner_done = false;
    let mut landing_done = false;
    let result = tokio::select! {
        biased;
        _ = crate::shutdown::requested() => Ok(()),
        r = &mut runner => { runner_done = true; r.map_err(|e| e.to_string()) },
        () = &mut landing => { landing_done = true; Ok(()) }
    };
    let _ = stop.send(true);
    // Keep the transport alive while an in-flight pass records its outcome.
    let drained = tokio::time::timeout(Duration::from_secs(7), async {
        let r = async {
            if !runner_done {
                (&mut runner).await.map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        };
        let l = async {
            if !landing_done {
                (&mut landing).await;
            }
        };
        let (r, ()) = tokio::join!(r, l);
        r
    })
    .await;
    role.node.shutdown();
    role.transport.shutdown().await;
    result?;
    drained.map_err(
        |_| "executor stop exceeded 7 seconds; inspect retained ledger before restarting",
    )??;
    Ok(())
}
