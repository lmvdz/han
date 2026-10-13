//! `han agent`: create, start and stop agents through the running node (a face over its pipe).
//!
//! * `han agent create --name coder --server han.example --operator @lars:han.example` makes the agent's key
//!   in the node's key store and prints what its home must register (the profile with its operator), then the
//!   operator confirms it with `--confirm` once the profile exists.
//! * `han agent start --space S --agent @coder:han.example --harness pi|claude --repo github:o/r --issue
//!   github:o/r#12` asks the node's supervisor to start a `han host` for the session.
//! * `han agent stop @coder:han.example` stops its sessions.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command, HomeRef};
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
        /// The home of the agent's profile: ServerName.
        #[arg(long)]
        home_server: String,
        /// The home's node id.
        #[arg(long)]
        home_node: String,
        /// The home's relays.
        #[arg(long)]
        home_relay: Vec<String>,
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
            home_server,
            home_node,
            home_relay,
            operator,
        } => {
            let profile = format!("profile:{agent}");
            let home = HomeRef {
                server: home_server,
                node: home_node,
                addrs: vec![],
                relays: home_relay,
            };
            let _ = client
                .call(Command::SpaceFollow {
                    home,
                    space: profile.clone(),
                })
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let r = client
                .call(Command::MemberSet {
                    space: profile,
                    actor: operator,
                    role: "owner".into(),
                })
                .await?;
            println!("{r:?}");
        }
        AgentCmd::Start {
            space,
            agent,
            harness,
            repo,
            issue,
        } => {
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
