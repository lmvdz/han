//! `han task`: create, list and cancel tasks through the running node (a face over its pipe).
//!
//! * `han task list [space]` prints the open tasks of a space, or of every followed space; `--all` adds the
//!   closed ones.
//! * `han task cancel <task>` calls a task off (`task.cancel`; only its author may). The space is found among
//!   the followed ones unless `--space` names it. The protocol's `task.cancel` carries no reason, so there is
//!   no `--why`.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command, Query, Row, TaskRow, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};

/// Options of `han task`.
#[derive(Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    cmd: TaskCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Creates a task, optionally starting its assigned agent immediately.
    Create {
        /// The space in which to create the task.
        space: String,
        /// What to do. Use --file for a longer or multiline request.
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        ask: Option<String>,
        /// Read the request from this UTF-8 file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Assignees, comma-separated or repeated. --start requires exactly one.
        #[arg(long, required = true, value_delimiter = ',')]
        to: Vec<String>,
        /// Optional issue reference, for example github:owner/repo#123.
        #[arg(long)]
        external: Option<String>,
        /// Start the assigned agent once the home accepts the task.
        #[arg(long, requires = "repo")]
        start: bool,
        /// Repository for --start, for example github:owner/repo.
        #[arg(long, requires = "start")]
        repo: Option<String>,
    },
    /// Lists tasks.
    List {
        /// The space; every followed space when absent.
        space: Option<String>,
        /// Only tasks assigned to this actor.
        #[arg(long)]
        assignee: Option<String>,
        /// Include tasks that are closed.
        #[arg(long)]
        all: bool,
    },
    /// Calls a task off. Only the task's author may.
    Cancel {
        /// The task's entry id (as `han task list` prints it).
        task: String,
        /// The task's space, if it is not enough to find it among the followed ones.
        #[arg(long)]
        space: Option<String>,
    },
}

async fn first_rows(
    client: &RpcClient,
    query: Query,
) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
    match client.view(query).next().await {
        Some(ViewEvent::Snapshot { rows, .. }) => Ok(rows),
        Some(ViewEvent::Failed { error, .. }) => Err(error.into()),
        _ => Err("the node sent no rows".into()),
    }
}

async fn spaces(client: &RpcClient) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let rows = first_rows(
        client,
        Query::Spaces {
            window: Window::default(),
        },
    )
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| match r {
            Row::SpaceStatus(s) => Some(s.space),
            _ => None,
        })
        .collect())
}

async fn tasks(
    client: &RpcClient,
    space: &str,
    assignee: Option<&str>,
) -> Result<Vec<TaskRow>, Box<dyn std::error::Error>> {
    let rows = first_rows(
        client,
        Query::Tasks {
            space: space.to_owned(),
            assignee: assignee.map(str::to_owned),
            window: Window {
                limit: 1000,
                ..Window::default()
            },
        },
    )
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| match r {
            Row::Task(t) => Some(t),
            _ => None,
        })
        .collect())
}

/// One task as a line: its id, state, author, assignees, external reference and ask.
pub fn line(t: &TaskRow) -> String {
    let state = if !t.open {
        "closed"
    } else if t.accepted.is_empty() {
        "open"
    } else {
        "accepted"
    };
    format!(
        "{} {state} by {} to {} {} {}",
        t.task,
        t.author,
        if t.to.is_empty() {
            "-".into()
        } else {
            t.to.join(",")
        },
        t.external.as_deref().unwrap_or("-"),
        t.ask.as_deref().unwrap_or("")
    )
}

/// Runs `han task` against the running node.
pub async fn run(args: TaskArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await?;
    match args.cmd {
        TaskCmd::Create {
            space,
            ask,
            file,
            to,
            external,
            start,
            repo,
        } => {
            let ask = match file {
                Some(path) => std::fs::read_to_string(path)?,
                None => ask.unwrap_or_default(),
            };
            if ask.trim().is_empty() {
                return Err("the task request must not be empty".into());
            }
            if start && to.len() != 1 {
                return Err("--start needs exactly one --to agent; no task was created".into());
            }
            let result = client
                .call(Command::TaskCreate {
                    space: space.clone(),
                    to: to.clone(),
                    ask,
                    external,
                })
                .await.map_err(|error| format!("task creation did not return a receipt: {error}. Inspect `han task list` and `han outbox` before creating it again"))?;
            let (id, queued) = match result {
                CallResult::Receipt { id, .. } => (id, false),
                CallResult::Queued { id, .. } => (id, true),
                _ => {
                    return Err(
                        "unexpected task response; inspect `han task list` before retrying".into(),
                    );
                }
            };
            println!(
                "{} {id} in {space}",
                if queued { "queued" } else { "created" }
            );
            if start {
                let agent = &to[0];
                let repo = repo.expect("clap requires repo with start");
                let connection = if args.endpoint.is_some() {
                    format!("--endpoint {endpoint}")
                } else {
                    format!("--data {}", args.data.display())
                };
                let retry = format!(
                    "han agent start --space {space} --agent {agent} --repo {repo} --task {id} {connection}"
                );
                if queued {
                    return Err(format!("task is queued, not started. Wait for the outbox to reach the home, then run: {retry}. Do not create it again").into());
                }
                if let Err(error) = client
                    .call(Command::AgentStart {
                        space,
                        agent: agent.clone(),
                        harness: None,
                        repo: Some(repo),
                        issue: None,
                        task: Some(id.clone()),
                        home: None,
                    })
                    .await
                {
                    return Err(format!("task {id} was created, but start did not return success: {error}. Inspect the board before retrying start: {retry}. Do not create it again").into());
                }
                println!("started {agent} on task {id}");
            }
        }
        TaskCmd::List {
            space,
            assignee,
            all,
        } => {
            let spaces = match space {
                Some(s) => vec![s],
                None => spaces(&client).await?,
            };
            let mut n = 0;
            for s in spaces {
                let rows = tasks(&client, &s, assignee.as_deref())
                    .await
                    .map_err(|error| format!("cannot list tasks in {s}: {error}"))?;
                for t in rows.iter().filter(|t| all || t.open) {
                    println!("{s} {}", line(t));
                    n += 1;
                }
            }
            println!("{n} task(s)");
        }
        TaskCmd::Cancel { task, space } => {
            let candidates = match space {
                Some(s) => vec![s],
                None => spaces(&client).await?,
            };
            let mut found = None;
            for s in candidates {
                if let Ok(rows) = tasks(&client, &s, None).await
                    && rows.iter().any(|t| t.task == task)
                {
                    found = Some(s);
                    break;
                }
            }
            let space = found.ok_or("no followed space has that task (give --space)")?;
            client
                .call(Command::TaskCancel {
                    space: space.clone(),
                    task: task.clone(),
                })
                .await?;
            println!("cancelled {task} in {space}");
        }
    }
    Ok(())
}
