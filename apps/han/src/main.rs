//! `han`: one binary for every role of a HAN installation. M2 has `seq` (run a home), `dial` (a small
//! client) and `conform` (the wire vectors over QUIC).

mod conform;
mod dial;
mod keys;
mod seq;

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
    /// Run a home: the sequencer of one server on a QUIC endpoint.
    Seq(seq::SeqArgs),
    /// Dial a home and make calls.
    Dial(dial::DialArgs),
    /// Run conformance vectors against a home.
    Conform(conform::ConformArgs),
    /// Print the public key (the node id) of a key file, creating the file if it does not exist.
    Key { file: std::path::PathBuf },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let result = match Cli::parse().command {
        Cmd::Seq(a) => seq::run(a).await,
        Cmd::Dial(a) => dial::run(a).await,
        Cmd::Conform(a) => conform::run(a).await,
        Cmd::Key { file } => keys::load_or_create(&file)
            .map(|seed| println!("{}", keys::signing_key(seed).public_key().to_base64url()))
            .map_err(Into::into),
    };
    if let Err(e) = result {
        eprintln!("han: {e}");
        std::process::exit(1);
    }
}
