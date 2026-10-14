//! `han space`: follow a space and list the followed ones, through the running node (a face over its pipe).
//!
//! * `han space follow <space> --home <ticket>` makes the node follow a space this person already belongs to.
//! * `han space list` prints the spaces the node follows, with the state of each link.
//! * `han space create --name N --home <ticket>` creates a space at that home, owned by this person.
//! * `han space member <space> <actor> <role>` gives an actor a role (an owner's command).
//! * `han space policy <space> <file>` replaces the space's policy with the JSON in the file.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command, Query, Row, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};

use crate::home::HomeOpts;

/// Options of `han space`.
#[derive(Args)]
pub struct SpaceArgs {
    #[command(subcommand)]
    cmd: SpaceCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
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
    /// Creates a space at a home, with this device's person as its owner, and follows it. Prints the space id.
    Create {
        /// The space's name.
        #[arg(long)]
        name: String,
        /// `members` or `public`.
        #[arg(long, default_value = "members")]
        visibility: String,
        /// `invite`, `open` or `closed`.
        #[arg(long, default_value = "invite")]
        join: String,
        #[command(flatten)]
        home: HomeOpts,
    },
    /// Gives an actor a role in a space: `owner`, `member`, `guest`, `banned` or `none`.
    Member {
        /// The space.
        space: String,
        /// The actor.
        actor: String,
        /// Its new role.
        role: String,
    },
    /// Replaces a space's policy with the policy document (JSON) in a file; an owner's command.
    Policy {
        /// The space.
        space: String,
        /// The file holding the whole policy as JSON.
        file: PathBuf,
    },
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
                Some(ViewEvent::Failed { error }) => return Err(error.into()),
                _ => return Err("the node sent no list of spaces".into()),
            }
        }
        SpaceCmd::Create {
            name,
            visibility,
            join,
            home,
        } => {
            let choice = home
                .resolve(None)?
                .ok_or("give --home <ticket>, or --home-node with --home-server")?;
            if let Some(t) = &choice.pin {
                client
                    .call(Command::HomePin {
                        ticket: t.to_text(),
                    })
                    .await?;
            }
            match client
                .call(Command::SpaceCreate {
                    home: choice.home,
                    name,
                    visibility,
                    join,
                })
                .await?
            {
                CallResult::Space { space } => println!("{space}"),
                other => return Err(format!("unexpected answer: {other:?}").into()),
            }
        }
        SpaceCmd::Member { space, actor, role } => {
            client
                .call(Command::MemberSet {
                    space,
                    actor: actor.clone(),
                    role: role.clone(),
                })
                .await?;
            println!("{actor} is {role}");
        }
        SpaceCmd::Policy { space, file } => {
            let text = std::fs::read_to_string(&file)?;
            // Checked here so a typo is reported before it reaches the node.
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|e| format!("{}: not JSON: {e}", file.display()))?;
            client
                .call(Command::PolicySet {
                    space: space.clone(),
                    policy: text,
                })
                .await?;
            println!("policy set on {space}");
        }
    }
    Ok(())
}
