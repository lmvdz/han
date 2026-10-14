//! `han`: one binary for every role of a HAN installation. M3 adds `node` (the device daemon) and `board` (a
//! plain-text board over the node's pipe); M5 adds `tui` (the live terminal face); M2 has `seq` (run a home), `dial` (a small client) and `conform`
//! (the wire vectors over QUIC).

mod agent;
mod board;
mod conform;
mod device;
mod dial;
mod exec;
mod home;
mod host;
mod keys;
mod node;
mod rolenode;
mod seq;
mod space;
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
    Node(node::NodeArgs),
    /// Print a space's board as text, from the running node.
    Board(board::BoardArgs),
    /// The live board in the terminal: sessions, the session pane, approvals. Needs the running node.
    Tui(tui::TuiArgs),
    /// Create, follow and list spaces, set roles and policy, through the running node.
    Space(space::SpaceArgs),
    /// Pair a new device into this person's profile, or revoke one, through the running node.
    Device(device::DeviceArgs),
    /// Run a home: the sequencer of one server on a QUIC endpoint.
    Seq(seq::SeqArgs),
    /// Print the home ticket a running `han seq` wrote to its data directory, to hand to teammates.
    Ticket(seq::TicketArgs),
    /// Dial a home and make calls.
    Dial(dial::DialArgs),
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
        Cmd::Node(a) => node::run(a).await,
        Cmd::Board(a) => board::run(a).await,
        Cmd::Tui(a) => tui::run(a).await,
        Cmd::Space(a) => space::run(a).await,
        Cmd::Device(a) => device::run(a).await,
        Cmd::Seq(a) => seq::run(a).await,
        Cmd::Ticket(a) => seq::ticket(&a),
        Cmd::Dial(a) => dial::run(a).await,
        Cmd::Conform(a) => conform::run(a).await,
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
