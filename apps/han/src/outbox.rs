//! `han outbox`: the entries this device signed and their way to the home, through the running node (a face
//! over its pipe).
//!
//! * `han outbox list [space]` prints the outbox rows, newest last: `pending` ones are being tried, `failed`
//!   ones gave up after too many tries.
//! * `han outbox retry <id>` queues a `failed` entry again and tries to send it at once.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command, Query, Row, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};

/// Options of `han outbox`.
#[derive(Args)]
pub struct OutboxArgs {
    #[command(subcommand)]
    cmd: OutboxCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum OutboxCmd {
    /// Lists the outbox.
    List {
        /// The space; every followed space when absent.
        space: Option<String>,
    },
    /// Queues a failed entry again.
    Retry {
        /// The outbox id (as `han outbox list` prints it).
        id: u64,
    },
}

pub async fn run(args: OutboxArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await?;
    match args.cmd {
        OutboxCmd::List { space } => {
            let query = Query::Outbox {
                space,
                window: Window::default(),
            };
            match client.view(query).next().await {
                Some(ViewEvent::Snapshot { rows, .. }) => {
                    for row in rows {
                        if let Row::Outbox(o) = row {
                            let seq = o.seq.map(|s| format!(" seq {s}")).unwrap_or_default();
                            let error = o.error.map(|e| format!(": {e}")).unwrap_or_default();
                            let blocking = if o.blocking {
                                " [blocks the space]"
                            } else {
                                ""
                            };
                            println!(
                                "{} {} {} {} (tries {}){seq}{blocking}{error}",
                                o.outbox_id, o.space, o.kind, o.state, o.attempts
                            );
                        }
                    }
                }
                Some(ViewEvent::Failed { error, .. }) => return Err(error.into()),
                _ => return Err("the node sent no rows".into()),
            }
        }
        OutboxCmd::Retry { id } => match client.call(Command::OutboxRetry { outbox_id: id }).await?
        {
            CallResult::Receipt { seq, .. } => println!("sent: seq {seq}"),
            CallResult::Queued { .. } => println!("queued again; `han outbox list` follows it"),
            other => return Err(format!("unexpected answer: {other:?}").into()),
        },
    }
    Ok(())
}
