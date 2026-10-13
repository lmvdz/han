//! `han conform wire`: runs the wire conformance vectors (`conformance/wire-vectors`) against a home in test
//! mode (`han seq --test-mode`), over QUIC.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use han_net::wirevec::{TestConfig, WireRunner};
use han_net::{IrohOptions, IrohTransport, NodeAddr, RelayChoice};

/// Options of `han conform`.
#[derive(Args)]
pub struct ConformArgs {
    #[command(subcommand)]
    what: What,
}

#[derive(Subcommand)]
enum What {
    /// Run the frame vectors against a test-mode home.
    Wire {
        /// The home's node id (printed by `han seq`).
        #[arg(long)]
        node: String,
        /// A direct address of the home.
        #[arg(long)]
        addr: Vec<SocketAddr>,
        /// The home's HTTP address, for the well-known vector.
        #[arg(long)]
        http: Option<SocketAddr>,
        /// The vectors.
        #[arg(long, default_value = "conformance/wire-vectors")]
        dir: PathBuf,
        /// Only vectors whose id or file name contains this.
        #[arg(long)]
        only: Option<String>,
    },
}

pub async fn run(args: ConformArgs) -> Result<(), Box<dyn std::error::Error>> {
    let What::Wire {
        node,
        addr,
        http,
        dir,
        only,
    } = args.what;
    let config = TestConfig::load(&dir.join("config.json"))?;
    let mut opts = IrohOptions::new([0x5a; 32]);
    opts.relay = RelayChoice::Disabled;
    let transport = IrohTransport::bind(opts).await?;
    let server = NodeAddr::direct(&node, addr);
    let report = WireRunner::new(transport.clone(), server, http, config)
        .run(&dir, only.as_deref())
        .await?;
    transport.shutdown().await;
    for (id, why) in &report.failed {
        println!("FAIL {id}: {why}");
    }
    println!("wire vectors over QUIC: {}", report.summary());
    if report.failed.is_empty() {
        Ok(())
    } else {
        Err("vectors failed".into())
    }
}
