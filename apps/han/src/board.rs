//! `han board`: a plain-text dump of the Board view over the node's pipe. A stand-in until the TUI exists.
//!
//! This is a face: it uses the node API types and the JSON-RPC client and nothing else of han-node
//! (`crates/han-node/tests/layering.rs` checks it).

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use han_node::NodeApi;
use han_node::api::{BoardRow, Group, Query, Row, SessionState, ViewEvent, ViewMirror, Window};
use han_node::rpc::{RpcClient, default_endpoint};

/// Options of `han board`.
#[derive(Args)]
pub struct BoardArgs {
    /// The space to show.
    space: String,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data")]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long)]
    endpoint: Option<String>,
    /// How to group the rows.
    #[arg(long, value_enum, default_value_t = GroupArg::Issue)]
    group: GroupArg,
    /// Include closed sessions.
    #[arg(long)]
    all: bool,
    /// Keep running and print the board again whenever it changes.
    #[arg(long)]
    watch: bool,
    /// Show at most this many rows.
    #[arg(long, default_value_t = 200)]
    limit: u32,
}

#[derive(Clone, Copy, ValueEnum)]
enum GroupArg {
    Issue,
    Person,
    State,
}

fn state(s: SessionState) -> &'static str {
    match s {
        SessionState::Working => "working",
        SessionState::WaitingApproval => "waiting-approval",
        SessionState::Blocked => "blocked",
        SessionState::Idle => "idle",
        SessionState::Done => "done",
        SessionState::Stale => "stale",
        SessionState::Failed => "failed",
    }
}

fn cell(s: &str, width: usize) -> String {
    let s: String = s.chars().take(width).collect();
    format!("{s:<width$}")
}

/// The board as text: one line per session, grouped.
fn render(rows: &[Row], total: u32) -> String {
    let mut out = String::new();
    let mut group: Option<&str> = None;
    for row in rows {
        let Row::Board(b) = row else { continue };
        if group != Some(b.group.as_str()) {
            group = Some(b.group.as_str());
            let label = if b.group.is_empty() {
                "(no issue)"
            } else {
                &b.group
            };
            out.push_str(&format!("== {label}\n"));
        }
        out.push_str(&line(b));
        out.push('\n');
    }
    out.push_str(&format!("{} of {total} session(s)\n", rows.len()));
    out
}

fn line(b: &BoardRow) -> String {
    let files = match b.files.len() {
        0 => String::new(),
        1 => b.files[0].path.clone(),
        n => format!("{} +{}", b.files[0].path, n - 1),
    };
    let overlap = if b.overlaps.is_empty() {
        String::new()
    } else {
        format!(
            "overlap(tier {})",
            b.overlaps.iter().map(|k| k.tier()).min().unwrap_or(0)
        )
    };
    let tokens = b
        .tokens
        .map(|t| format!("{}k tok", (t.input + t.out) / 1000))
        .unwrap_or_default();
    format!(
        "  {} {} {} {} {} {} {}",
        cell(&b.person, 22),
        cell(&b.agent, 22),
        cell(b.harness.as_deref().unwrap_or("-"), 10),
        cell(state(b.state), 16),
        cell(&b.step, 28),
        cell(&files, 24),
        [overlap, tokens].join(" ").trim_end()
    )
}

pub async fn run(args: BoardArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await.map_err(|e| {
        format!("cannot reach the node at {endpoint}: {e} (is `han node` running?)")
    })?;
    let query = Query::Board {
        space: args.space,
        group: match args.group {
            GroupArg::Issue => Group::Issue,
            GroupArg::Person => Group::Person,
            GroupArg::State => Group::State,
        },
        collapsed: vec![],
        window: Window {
            filter: han_node::api::Filter {
                include_closed: args.all,
                ..Default::default()
            },
            limit: args.limit,
            cursor: None,
        },
    };
    let mut view = client.view(query);
    let mut mirror = ViewMirror::new();
    while let Some(event) = view.next().await {
        if let ViewEvent::Failed { error } = &event {
            return Err(error.to_string().into());
        }
        mirror.apply(event);
        print!("{}", render(&mirror.rows(), mirror.total()));
        if !args.watch {
            break;
        }
        println!();
    }
    Ok(())
}
