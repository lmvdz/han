//! `han-github run --config <file>`: poll the configured repositories until stopped.
//! `han-github request --config <file> github:owner/repo#N`: ask for a task on one issue now.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use han_github::{Config, Reader, Syncer};
use han_node::rpc::RpcClient;

#[derive(Parser)]
#[command(name = "han-github", version, about = "HAN's GitHub integration")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Poll the repositories of the config every `poll_secs` and surface their issues in the space.
    Run {
        /// The config file (TOML).
        #[arg(long)]
        config: PathBuf,
    },
    /// A human's request: create the task for one issue now, unless it has one.
    Request {
        /// The config file (TOML).
        #[arg(long)]
        config: PathBuf,
        /// The issue, `github:owner/repo#N`.
        reference: String,
        /// The issue's title, for the task's ask.
        #[arg(long, default_value = "")]
        title: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "han_github=info".into()),
        )
        .init();
    if let Err(e) = real_main(Cli::parse()).await {
        eprintln!("han-github: {e}");
        std::process::exit(1);
    }
}

async fn real_main(cli: Cli) -> Result<(), String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    match cli.command {
        Cmd::Run { config } => {
            let cfg = Config::load(&config).map_err(|e| e.to_string())?;
            let mut syncer = connect(&cfg).await?;
            let mut tick = tokio::time::interval(Duration::from_secs(cfg.poll_secs));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        for repo in syncer.repos() {
                            match syncer.poll(&repo).await {
                                Ok(t) if t.not_modified => tracing::debug!(%repo, "not modified"),
                                Ok(t) => tracing::info!(%repo, published = t.published, tasks = t.tasks.len(), "polled"),
                                Err(e) => tracing::warn!(%repo, error = %e, "poll failed"),
                            }
                        }
                    }
                    _ = tokio::signal::ctrl_c() => break,
                }
            }
            syncer.close().await;
            Ok(())
        }
        Cmd::Request {
            config,
            reference,
            title,
        } => {
            let cfg = Config::load(&config).map_err(|e| e.to_string())?;
            let (repo, number) = parse_ref(&reference)?;
            if !cfg.repos.contains(&repo) {
                return Err(format!("{repo} is not in the config"));
            }
            if cfg.tasks.to.is_empty() {
                return Err("tasks.to is empty in the config".into());
            }
            let mut syncer = connect(&cfg).await?;
            let r = syncer
                .request(&format!("github:{repo}#{number}"), &title)
                .await
                .map_err(|e| e.to_string())?;
            match r {
                Some(id) => println!("task {id}"),
                None => println!("{reference} already has a task"),
            }
            Ok(())
        }
    }
}

async fn connect(cfg: &Config) -> Result<Syncer<Reader>, String> {
    let reader = Reader::from_config(&cfg.app)?;
    let api = RpcClient::connect(&cfg.node_endpoint())
        .await
        .map_err(|e| format!("connecting to the node: {}", e.kind()))?;
    Ok(Syncer::new(cfg, Arc::new(api), reader))
}

fn parse_ref(r: &str) -> Result<(String, u64), String> {
    let bad = || format!("not github:owner/repo#N: {r}");
    let rest = r.strip_prefix("github:").ok_or_else(bad)?;
    let (repo, n) = rest.split_once('#').ok_or_else(bad)?;
    if !han_github::config::valid_repo(repo) {
        return Err(bad());
    }
    Ok((repo.to_owned(), n.parse().map_err(|_| bad())?))
}
