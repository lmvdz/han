//! `han agent`: create, start and stop agents through the running node (a face over its pipe).
//!
//! * `han agent create --name coder --server han.example --operator @lars:han.example` makes the agent's key
//!   in the node's key store and prints what its home must register (the profile with its operator), then the
//!   operator confirms it with `--confirm` once the profile exists.
//! * `han agent start --space S --agent @coder:han.example --harness pi|claude --repo github:o/r --issue
//!   github:o/r#12` asks the node's supervisor to start a `han host` for the session.
//! * `han agent stop @coder:han.example` stops its sessions.

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
    #[arg(long, default_value = "han-data", global = true)]
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
    /// Stops an agent's sessions, or one `host-<agent>#<issue>`.
    Stop {
        /// The agent or session name.
        target: String,
    },
}

pub async fn run(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
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
                    "register at its home: profile {{actorKind: agent, operator: {operator}, key: {{keyId: {key_id}, publicKey: {public_key}}}}}"
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
            }
            client
                .call(Command::AgentStart {
                    space,
                    agent: agent.clone(),
                    harness,
                    repo,
                    issue,
                })
                .await?;
            println!("started a session of {agent}");
        }
        AgentCmd::Stop { target } => {
            client.call(Command::AgentStop { target }).await?;
            println!("stopped");
        }
    }
    Ok(())
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
