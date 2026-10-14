//! `han actor`: commands that work on any actor that is not a person, agent or executor alike.
//!
//! * `han actor confirm <actor> --operator O --home <ticket>`: the operator confirms the actor's profile. It
//!   is what `han agent confirm` always did; services (the executor) need it too.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::rpc::{RpcClient, default_endpoint};

use crate::home::HomeOpts;

/// Options of `han actor`.
#[derive(Args)]
pub struct ActorArgs {
    #[command(subcommand)]
    cmd: ActorCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum ActorCmd {
    /// The operator confirms an actor (agent or service) whose profile exists: `member.set {operator: owner}`
    /// in its profile space (§4.3). Run it on the operator's device. Confirming again changes nothing.
    Confirm {
        /// The actor, `@name:server`.
        actor: String,
        #[command(flatten)]
        home: HomeOpts,
        /// The operator (this node's actor).
        #[arg(long)]
        operator: String,
    },
}

pub async fn run(args: ActorArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await.map_err(|e| {
        format!(
            "cannot reach the node at {endpoint}: {e} (is `han node` running with --data {}?)",
            args.data.display()
        )
    })?;
    match args.cmd {
        ActorCmd::Confirm {
            actor,
            home,
            operator,
        } => crate::agent::confirm_cmd(&client, &actor, &home, &operator).await,
    }
}
