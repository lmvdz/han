//! `han space`: follow a space and list the followed ones, through the running node (a face over its pipe).
//!
//! * `han space follow <space> [--home <ticket>]` makes the node follow a space this person already belongs to.
//! * `han space list` prints the spaces the node follows, with the state of each link.
//! * `han space create --name N [--home <ticket>]` creates a space at that home, owned by this person.
//! * `han space member <space> <actor> <role>` gives an actor a role (an owner's command).
//! * `han space policy <space> <file>` replaces the space's policy with the JSON in the file.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command, NodeError, Query, Row, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};
use han_registry::HomeRef;

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
    /// Preview a Trusted team space; --apply creates it with the displayed authority.
    CreateTrusted {
        /// Space name.
        #[arg(long)]
        name: String,
        /// Exact repository identifier, repeated for each repository.
        #[arg(long = "repo", required = true)]
        repos: Vec<String>,
        /// Home ticket or server name (default: this actor's home).
        #[arg(long)]
        home: Option<HomeRef>,
        /// Create the space after reviewing the preview.
        #[arg(long)]
        apply: bool,
    },
    /// Follows a space this person already belongs to.
    Follow {
        /// The space id.
        space: String,
        /// The space's home: a home ticket (`han1home.…`) or a ServerName. Default: the server the space id
        /// names.
        #[arg(long)]
        home: Option<HomeRef>,
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
        /// The home that sequences it: a home ticket (`han1home.…`) or a ServerName. Default: the home of this
        /// device's actor.
        #[arg(long)]
        home: Option<HomeRef>,
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

/// Runs `han space` against the running node.
pub async fn run(args: SpaceArgs) -> Result<(), Box<dyn std::error::Error>> {
    if let SpaceCmd::CreateTrusted {
        name, repos, apply, ..
    } = &args.cmd
    {
        let policy = han_node::api::trusted_team_policy(repos)?;
        println!("Trusted team: {name}");
        println!("Ordinary changes (at most 500 changed lines) may land automatically in:");
        for repo in repos {
            println!("  {repo}");
        }
        println!("Requires confirmed operators, a configured runner and passing required checks.");
        println!(
            "Sensitive paths, larger changes, other repositories and other effects require owner approval."
        );
        println!(
            "A session guard still requires its person's approval. Membership is invite-only."
        );
        println!("{}", serde_json::to_string_pretty(&policy)?);
        if !apply {
            println!("Preview only. Add --apply to create this space.");
            return Ok(());
        }
    }
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await?;
    match args.cmd {
        SpaceCmd::CreateTrusted {
            name, repos, home, ..
        } => {
            let result = client
                .call(Command::SpaceCreateTrusted { name, repos, home })
                .await;
            let result = match result {
                Err(error @ NodeError::Unavailable { .. }) => {
                    return Err(format!("Creation result is unknown: {error}. Inspect `han space list` and `han outbox list` on this same node before retrying; another --apply creates a new space.").into());
                }
                other => other?,
            };
            match result {
                CallResult::Space { space } => println!("{space}"),
                CallResult::Queued {
                    space, outbox_id, ..
                } => {
                    println!(
                        "Creation queued for {space} (outbox {outbox_id}); do not create it again."
                    );
                }
                other => return Err(format!("unexpected answer: {other:?}").into()),
            }
        }
        SpaceCmd::Follow { space, home } => {
            client
                .call(Command::SpaceFollow {
                    home,
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
        SpaceCmd::Create {
            name,
            visibility,
            join,
            home,
        } => {
            match client
                .call(Command::SpaceCreate {
                    home,
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
