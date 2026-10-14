//! `han agent`: create, start and stop agents through the running node (a face over its pipe).
//!
//! * `han agent create --name coder --server han.example --operator @lars:han.example` makes the agent's key
//!   in the node's key store and prints what its home must register (the profile with its operator), then the
//!   operator confirms it with `--confirm` once the profile exists.
//! * `han agent start --space S --agent @coder:han.example --harness pi|claude --repo github:o/r --issue
//!   github:o/r#12` asks the node's supervisor to start a `han host` for the session.
//! * `han agent stop @coder:han.example` stops its sessions.
//! * `han agent setup-harness claude|pi` installs the pinned ACP adapter into the data directory, so `start`
//!   needs no flags; `han agent login @coder:han.example` logs the agent in to Claude Code once, in its own
//!   login directory (`--check` only says whether a login exists).

use std::path::PathBuf;

use crate::home::{HomeChoice, HomeOpts};
use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command};
use han_node::rpc::{RpcClient, default_endpoint};

/// Options of `han agent`.
#[derive(Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    cmd: AgentCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Makes an agent's key and prints what its home registers.
    Create {
        /// The agent's name, without `@` and server.
        #[arg(long)]
        name: String,
        /// Its server.
        #[arg(long)]
        server: String,
        /// Its operator (a person).
        #[arg(long)]
        operator: String,
    },
    /// The operator confirms an agent whose profile exists: `member.set {actor: <operator>, role: owner}` in
    /// the agent's profile space (§4.3).
    Confirm {
        /// The agent.
        agent: String,
        #[command(flatten)]
        home: HomeOpts,
        /// The operator (this node's actor).
        #[arg(long)]
        operator: String,
    },
    /// Starts a session of an agent on this node.
    Start {
        /// The space.
        #[arg(long)]
        space: String,
        /// The agent actor.
        #[arg(long)]
        agent: String,
        /// The harness: `pi`, `claude`, or a driver configured by name.
        #[arg(long)]
        harness: String,
        /// The repository, `github:owner/repo` (or a local path for tests).
        #[arg(long)]
        repo: Option<String>,
        /// The issue, `github:owner/repo#123`.
        #[arg(long)]
        issue: Option<String>,
        #[command(flatten)]
        home: HomeOpts,
    },
    /// Installs a harness's pinned ACP adapter (npm, with Node 22) into the data directory and records where.
    SetupHarness {
        /// `claude` or `pi`.
        harness: String,
        /// Install here instead of `<data>/harness/<harness>` (for example a directory an OS-user agent can
        /// read).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// A Node 22 to use, instead of the one on the PATH or the pinned build `han` downloads.
        #[arg(long)]
        node: Option<PathBuf>,
    },
    /// Logs an agent in to Claude Code, once, in the agent's own login directory (`CLAUDE_CONFIG_DIR`). Runs
    /// `claude` interactively outside the sandbox; the login is the only thing of yours the agent's sandbox
    /// will see. With `--check` it only says whether a login exists, and never shows its contents.
    Login {
        /// The agent actor.
        agent: String,
        /// Only report whether a login exists (exit code 1 if not).
        #[arg(long)]
        check: bool,
        /// The login directory, if it is not the agent's default.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// The `claude` program, if it is not on the PATH.
        #[arg(long)]
        claude: Option<PathBuf>,
    },
    /// Stops an agent's sessions, or one `host-<agent>#<issue>`.
    Stop {
        /// The agent or session name.
        target: String,
    },
}

pub async fn run(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    // These two work on the data directory and need no running node.
    match &args.cmd {
        AgentCmd::Login {
            agent,
            check,
            dir,
            claude,
        } => {
            return crate::harness::login(
                &args.data,
                agent,
                *check,
                dir.as_deref(),
                claude.as_deref(),
            );
        }
        AgentCmd::SetupHarness { harness, dir, node } => {
            let r = crate::harness::setup(&args.data, harness, dir.as_deref(), node.as_deref())?;
            println!(
                "{} {} installed: node {}, adapter {}",
                r.package,
                r.version,
                r.node.display(),
                r.adapter_dir.display()
            );
            return Ok(());
        }
        _ => {}
    }
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await?;
    match args.cmd {
        AgentCmd::Create {
            name,
            server,
            operator,
        } => {
            let r = client
                .call(Command::AgentCreate {
                    name,
                    server,
                    operator: operator.clone(),
                })
                .await?;
            if let CallResult::Agent {
                actor,
                key_id,
                public_key,
            } = r
            {
                println!("actor {actor}");
                println!("key {key_id} {public_key}");
                println!(
                    "register at its home (admin, no restart): han seq register --data <home data> --actor {actor} --kind agent --key-id {key_id} --public-key {public_key} --operator {operator}"
                );
                println!("then confirm: han agent confirm {actor} --operator {operator} …");
            }
        }
        AgentCmd::Confirm {
            agent,
            home,
            operator,
        } => {
            let profile = format!("profile:{agent}");
            // The agent's profile lives at the agent's own server.
            let choice = home
                .resolve(Some(agent.as_str()))?
                .ok_or("give --home <ticket>, or --home-node (and --home-server)")?;
            pin(&client, &choice).await?;
            let home = choice.home;
            let _ = client
                .call(Command::SpaceFollow {
                    home,
                    space: profile.clone(),
                })
                .await;
            // The operator holds no role in the profile space; the peer filter admits this member.set because
            // the profile names it as the operator (N-02, P-03). Retry while the node is still fetching the space.
            let mut tries = 0;
            let r = loop {
                match client
                    .call(Command::MemberSet {
                        space: profile.clone(),
                        actor: operator.clone(),
                        role: "owner".into(),
                    })
                    .await
                {
                    Err(han_node::api::NodeError::NotFound { .. }) if tries < 100 => {
                        tries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    other => break other?,
                }
            };
            println!("{r:?}");
        }
        AgentCmd::Start {
            space,
            agent,
            harness,
            repo,
            issue,
            home,
        } => {
            if let Some(choice) = home.resolve(Some(agent.as_str()))? {
                pin(&client, &choice).await?;
                // The node names the agent's operator from its profile, so it follows the profile first.
                let _ = client
                    .call(Command::SpaceFollow {
                        home: choice.home,
                        space: format!("profile:{agent}"),
                    })
                    .await;
            }
            let mut tries = 0;
            loop {
                let r = client
                    .call(Command::AgentStart {
                        space: space.clone(),
                        agent: agent.clone(),
                        harness: harness.clone(),
                        repo: repo.clone(),
                        issue: issue.clone(),
                    })
                    .await;
                if tries < 100 && profile_pending(&r) {
                    // The profile is still being fetched.
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
                r?;
                break;
            }
            println!("started a session of {agent}");
        }
        AgentCmd::Login { .. } | AgentCmd::SetupHarness { .. } => {}
        AgentCmd::Stop { target } => {
            client.call(Command::AgentStop { target }).await?;
            println!("stopped");
        }
    }
    Ok(())
}

/// Whether a failed start only waits for the agent's profile to arrive.
fn profile_pending(r: &Result<CallResult, han_node::api::NodeError>) -> bool {
    match r {
        Err(e) => {
            let m = e.to_string();
            m.contains("profile of")
                && (m.contains("has not arrived") || m.contains("is not followed"))
        }
        Ok(_) => false,
    }
}

/// Hands the node a home named by ticket or keys, so it dials and trusts it with no HTTPS well-known.
async fn pin(client: &RpcClient, choice: &HomeChoice) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(t) = &choice.pin {
        client
            .call(Command::HomePin {
                ticket: t.to_text(),
            })
            .await?;
    }
    Ok(())
}
