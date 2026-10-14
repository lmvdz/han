//! `han`: one binary for every role of a HAN installation. M3 adds `node` (the device daemon) and `board` (a
//! plain-text board over the node's pipe); M5 adds `tui` (the live terminal face); M2 has `seq` (run a home), `dial` (a small client) and `conform`
//! (the wire vectors over QUIC).

mod actor;
mod agent;
mod board;
mod conform;
mod device;
mod dial;
mod enroll;
mod exec;
mod executor;
mod harness;
mod home;
mod homecmd;
mod host;
mod keys;
mod node;
mod outbox;
mod rolenode;
mod runner;
mod seq;
mod seqadmin;
mod service;
mod space;
mod task;
mod tui;
mod update;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "han",
    version,
    about = "HAN: sequencer, client and conformance tools"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon of this device: keys, endpoint, sync, views, and the node API on a local pipe.
    #[command(args_conflicts_with_subcommands = true)]
    Node(Box<node::NodeArgs>),
    /// Print a space's board as text, from the running node.
    Board(board::BoardArgs),
    /// The live board in the terminal: sessions, the session pane, approvals. Needs the running node.
    Tui(tui::TuiArgs),
    /// Create, follow and list spaces, set roles and policy, through the running node.
    Space(space::SpaceArgs),
    /// List and cancel tasks through the running node.
    Task(task::TaskArgs),
    /// Pair a new device into this person's profile, or revoke one, through the running node.
    Device(device::DeviceArgs),
    /// List the entries signed here and their way to the home, or retry a failed one, through the running node.
    Outbox(outbox::OutboxArgs),
    /// Run a home: the sequencer of one server on a QUIC endpoint.
    #[command(args_conflicts_with_subcommands = true)]
    Seq(Box<seq::SeqArgs>),
    /// Print the home ticket a running `han seq` wrote to its data directory, to hand to teammates.
    Ticket(seq::TicketArgs),
    /// Dial a home and make calls.
    Dial(dial::DialArgs),
    /// Make a home, and register profiles on a running one.
    Home(homecmd::HomeArgs),
    /// Confirm an agent or an executor as its operator.
    Actor(actor::ActorArgs),
    /// Set an executor up.
    Executor(executor::ExecutorArgs),
    /// Linux runners: the root-level setup (users, units, polkit rule).
    Runner(runner::RunnerArgs),
    /// Run conformance vectors against a home.
    Conform(conform::ConformArgs),
    /// Create, confirm, start and stop agents through the running node.
    Agent(agent::AgentArgs),
    /// Run one agent session (the host role): ACP client, sandbox, egress proxy, status and timeline.
    Host(Box<host::HostArgs>),
    /// The in-sandbox shim: `han shim listen …` or `han shim mcp-stub …` (runs inside the sandbox).
    #[command(disable_help_flag = true)]
    Shim {
        /// The shim's arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the executor role: claim approved effects and run them (holds the credentials).
    Exec(exec::ExecArgs),
    /// Check a signed release manifest for a newer version. Never installs anything.
    Update(update::UpdateArgs),
    /// Print the public key (the node id) of a key file, creating the file if it does not exist.
    Key { file: std::path::PathBuf },
}

/// `han host --args-file <file>`: the arguments come from a JSON file `{"args": [...]}` instead (the
/// `han-agent@.service` unit of a host that runs as another OS user reads what the node left for it).
fn argv() -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if argv.len() == 4 && argv[1] == "host" && argv[2] == "--args-file" {
        let args = std::fs::read_to_string(&argv[3])
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| e.to_string()))
            .ok()
            .and_then(|v| {
                v["args"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(std::ffi::OsString::from))
                        .collect::<Vec<_>>()
                })
            });
        match args {
            Some(a) => {
                argv.truncate(2);
                argv.extend(a);
            }
            None => {
                eprintln!(
                    "han: {}: not a JSON file with an args list",
                    argv[3].to_string_lossy()
                );
                std::process::exit(1);
            }
        }
    }
    argv
}

/// The shim's own verbs. `han host` hands its own executable to the sandbox as the shim, and the sandbox runs
/// it the way it runs `han-agent-shim`: `<shim> listen …` and `<shim> mcp-stub …`, with no `shim` in front.
const SHIM_VERBS: [&str; 2] = ["listen", "mcp-stub"];

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|v| v.to_str().is_some_and(|v| SHIM_VERBS.contains(&v)))
    {
        let args = std::env::args_os()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        std::process::exit(han_host::shim::main(args));
    }
    let cli = Cli::parse_from(argv());
    // The shim runs inside the sandbox with its own small runtime, before anything else starts.
    if let Cmd::Shim { args } = cli.command {
        std::process::exit(han_host::shim::main(args));
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("han: {e}");
            std::process::exit(1);
        }
    };
    rt.block_on(run(cli));
}

async fn run(cli: Cli) {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let result = match cli.command {
        Cmd::Node(a) => node::run(*a).await,
        Cmd::Board(a) => board::run(a).await,
        Cmd::Tui(a) => tui::run(a).await,
        Cmd::Space(a) => space::run(a).await,
        Cmd::Task(a) => task::run(a).await,
        Cmd::Device(a) => device::run(a).await,
        Cmd::Outbox(a) => outbox::run(a).await,
        Cmd::Seq(a) => seq::run(*a).await,
        Cmd::Ticket(a) => seq::ticket(&a),
        Cmd::Dial(a) => dial::run(a).await,
        Cmd::Conform(a) => conform::run(a).await,
        Cmd::Home(a) => homecmd::run(a).await,
        Cmd::Actor(a) => actor::run(a).await,
        Cmd::Executor(a) => executor::run(a).await,
        Cmd::Runner(a) => runner::run(a).await,
        Cmd::Agent(a) => agent::run(a).await,
        Cmd::Exec(a) => exec::run(a).await,
        Cmd::Host(a) => host::run(*a).await,
        Cmd::Update(a) => update::run(a).await,
        Cmd::Shim { .. } => Ok(()),
        Cmd::Key { file } => keys::load_or_create(&file)
            .map(|seed| println!("{}", keys::signing_key(seed).public_key().to_base64url()))
            .map_err(Into::into),
    };
    if let Err(e) = result {
        eprintln!("han: {e}");
        std::process::exit(1);
    }
}
