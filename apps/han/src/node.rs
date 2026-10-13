//! `han node`: the daemon of one device. It holds the device key, binds the device's one iroh endpoint, follows
//! the spaces the device belongs to, keeps `node.db`, and serves the node API to local faces over a pipe.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use han_net::wellknown::Fetcher;
use han_net::{IrohOptions, IrohTransport, Transport};
use han_node::api::{Command, NodeApi};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore, KeyringStore};
use han_node::rpc::{default_endpoint, serve};
use han_node::supervisor::ProcessSpawner;
use han_node::{Node, NodeConfig, NodeParts, WellKnownKeys};
use han_seq::SystemClock;

use crate::home::HomeOpts;
use crate::seq::relay_choice;

/// Options of `han node`.
#[derive(Args)]
pub struct NodeArgs {
    /// The actor this device acts as, for example `@lars:han.example`.
    #[arg(long)]
    actor: String,
    /// The id of this device's key in the actor's profile.
    #[arg(long, default_value = "d1")]
    key_id: String,
    /// The data directory: `node.db`, and the key files with `--file-keys`.
    #[arg(long, default_value = "han-data")]
    data: PathBuf,
    /// Keep the keys in files under the data directory instead of the OS keychain (for runners with no
    /// keychain).
    #[arg(long)]
    file_keys: bool,
    /// UDP address of the QUIC endpoint.
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Relays: `n0` (n0's public relays and DNS discovery), `none`, or relay URLs separated by commas.
    #[arg(long, default_value = "n0")]
    relay: String,
    /// The pipe or socket to serve the node API on, if it is not the default of `--data`.
    #[arg(long)]
    endpoint: Option<String>,
    #[command(flatten)]
    home: HomeOpts,
}

pub async fn run(args: NodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&args.data)?;
    let keys: Arc<dyn KeyStore> = if args.file_keys {
        Arc::new(FileKeyStore::new(args.data.join("keys")))
    } else {
        Arc::new(KeyringStore::new("han"))
    };
    let seed = keys.load_or_create(DEVICE_KEY)?;
    let mut opts = IrohOptions::new(seed);
    opts.bind = args.bind;
    opts.relay = relay_choice(&args.relay);
    let transport = IrohTransport::bind(opts).await?;
    transport.online(std::time::Duration::from_secs(5)).await;

    let mut config = NodeConfig::new(&args.actor);
    config.key_id = args.key_id;
    config.data_dir = Some(args.data.clone());
    let node = Node::start(NodeParts {
        config,
        transport: transport.clone(),
        clock: Arc::new(SystemClock::new()),
        keys,
        server_keys: Arc::new(WellKnownKeys::new(Fetcher::new())),
        supervisor: Arc::new(ProcessSpawner::new(std::env::current_exe()?)),
    })?;

    // A home named by ticket or node address is kept in the data directory, so a restart needs no flag.
    if let Some(choice) = args.home.resolve(Some(&args.actor))? {
        match choice.pin {
            Some(t) => {
                node.call(Command::HomePin {
                    ticket: t.to_text(),
                })
                .await?;
            }
            None => return Err("--home-node needs --home-key (or use --home with a ticket)".into()),
        }
    }

    let endpoint = args
        .endpoint
        .unwrap_or_else(|| default_endpoint(&args.data));
    let server = serve(Arc::new(node.clone()), &endpoint)?;
    println!("actor {}", node.actor());
    println!("node {}", transport.node_id());
    println!("endpoint {}", server.endpoint());
    tokio::signal::ctrl_c().await?;
    node.shutdown();
    drop(server);
    transport.shutdown().await;
    Ok(())
}
