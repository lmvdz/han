//! `han`: one binary for every role of a HAN installation. M3 adds `node` (the device daemon) and `board` (a
//! plain-text board over the node's pipe); M5 adds `tui` (the live terminal face); M2 has `seq` (run a home), `dial` (a small client) and `conform`
//! (the wire vectors over QUIC).

mod actor;
mod agent;
#[cfg(feature = "conformance")]
mod conform;
mod dev;
mod dial;
mod doctor;
mod enroll;
mod exec;
mod executor;
mod gate;
mod harness;
mod homecmd;
mod host;
mod keys;
mod node;
mod review;
mod rolenode;
#[cfg(feature = "conformance")]
mod roleprobe;
mod runner;
mod rusttool;
mod seq;
mod seqadmin;
mod service;
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
    Board(han_faces::board::BoardArgs),
    /// The live board in the terminal: sessions, the session pane, approvals. Needs the running node.
    Tui(tui::TuiArgs),
    /// Create, follow and list spaces, set roles and policy, through the running node.
    Space(han_faces::space::SpaceArgs),
    /// Create, start, list and cancel tasks through the running node.
    Task(han_faces::task::TaskArgs),
    /// Pair a new device into this person's profile, or revoke one, through the running node.
    Device(han_faces::device::DeviceArgs),
    /// List the entries signed here and their way to the home, or retry a failed one, through the running node.
    Outbox(han_faces::outbox::OutboxArgs),
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
    /// Say what is wrong on this machine, each problem with its cause and the exact command that fixes it.
    Doctor(doctor::DoctorArgs),
    /// Linux runners: the root-level setup (users, units, polkit rule).
    Runner(runner::RunnerArgs),
    /// A complete local HAN on loopback for trying things and for the end-to-end tests.
    Dev(dev::DevArgs),
    /// Run conformance vectors against a home.
    #[cfg(feature = "conformance")]
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
    /// The tier-1 machine reviewer: run it as a service (`run`) or set it up (`set`). Off unless configured.
    Review(review::ReviewArgs),
    /// Try the commands that approve and govern as a host or an executor and show that its class refuses them.
    #[cfg(feature = "conformance")]
    #[command(hide = true)]
    RoleProbe(roleprobe::ProbeArgs),
    /// Check a signed release manifest for a newer version. Never installs anything.
    Update(update::UpdateArgs),
    /// Print the public key (the node id) of a key file, creating the file if it does not exist.
    Key { file: std::path::PathBuf },
}

fn main() {
    let cli = Cli::parse();
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
        Cmd::Board(a) => han_faces::board::run(a).await,
        Cmd::Tui(a) => tui::run(a).await,
        Cmd::Space(a) => han_faces::space::run(a).await,
        Cmd::Task(a) => han_faces::task::run(a).await,
        Cmd::Device(a) => han_faces::device::run(a).await,
        Cmd::Outbox(a) => han_faces::outbox::run(a).await,
        Cmd::Seq(a) => seq::run(*a).await,
        Cmd::Ticket(a) => seq::ticket(&a),
        Cmd::Dial(a) => dial::run(a).await,
        #[cfg(feature = "conformance")]
        Cmd::Conform(a) => conform::run(a).await,
        Cmd::Home(a) => homecmd::run(a).await,
        Cmd::Actor(a) => actor::run(a).await,
        Cmd::Executor(a) => executor::run(a).await,
        Cmd::Doctor(a) => doctor::run(a).await,
        Cmd::Runner(a) => runner::run(a).await,
        Cmd::Dev(a) => dev::run(a).await,
        Cmd::Agent(a) => agent::run(a).await,
        Cmd::Exec(a) => exec::run(a).await,
        Cmd::Review(a) => review::run(a).await,
        Cmd::Host(a) => host::run(*a).await,
        #[cfg(feature = "conformance")]
        Cmd::RoleProbe(a) => roleprobe::run(a).await,
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
