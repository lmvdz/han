//! `han space`: follow a space and list the followed ones, through the running node (a face over its pipe).
//!
//! * `han space follow <space> --home <ticket>` makes the node follow a space this person already belongs to.
//! * `han space list` prints the spaces the node follows, with the state of each link.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{Command, Query, Row, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};

use crate::home::HomeOpts;

/// Options of `han space`.
#[derive(Args)]
pub struct SpaceArgs {
    #[command(subcommand)]
    cmd: SpaceCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum SpaceCmd {
    /// Follows a space this person already belongs to.
    Follow {
        /// The space id.
        space: String,
        #[command(flatten)]
        home: HomeOpts,
    },
    /// Lists the spaces the node follows.
    List,
}

pub async fn run(args: SpaceArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await?;
    match args.cmd {
        SpaceCmd::Follow { space, home } => {
            // The space id ends in its home's ServerName, which stands in for `--home-server`.
            let choice = home
                .resolve(Some(space.as_str()))?
                .ok_or("give --home <ticket>, or --home-node (and --home-server)")?;
            if let Some(t) = &choice.pin {
                client
                    .call(Command::HomePin {
                        ticket: t.to_text(),
                    })
                    .await?;
            }
            client
                .call(Command::SpaceFollow {
                    home: choice.home,
                    space: space.clone(),
                })
                .await?;
            println!("following {space}");
        }
        SpaceCmd::List => {
            let mut view = client.view(Query::Spaces {
                window: Window::default(),
            });
            match view.next().await {
                Some(ViewEvent::Snapshot { rows, .. }) => {
                    for row in rows {
                        if let Row::SpaceStatus(s) = row {
                            println!(
                                "{} {:?} head {} sessions {} home {}",
                                s.space, s.link, s.head, s.sessions, s.home
                            );
                        }
                    }
                }
                Some(ViewEvent::Failed { error, .. }) => return Err(error.into()),
                _ => return Err("the node sent no list of spaces".into()),
            }
        }
    }
    Ok(())
}
