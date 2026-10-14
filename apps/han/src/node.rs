//! `han node`: the daemon of one device. It holds the device key, binds the device's one iroh endpoint, follows
//! the spaces the device belongs to, keeps `node.db`, and serves the node API to local faces over a pipe.
//!
//! `han node install-service` makes it start at logon (a per-user scheduled task on Windows, a systemd user
//! unit on Linux; no admin rights), and `han node status` says whether it is running.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
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
use crate::service;

/// Options of `han node`.
#[derive(Args)]
pub struct NodeArgs {
    #[command(subcommand)]
    cmd: Option<NodeCmd>,
    #[command(flatten)]
    opts: NodeOpts,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum NodeCmd {
    /// Installs the node as a background service that starts at logon, for the current user and with no admin
    /// rights: a scheduled task on Windows, a systemd user unit on Linux. Takes the options of `han node`.
    InstallService {
        #[command(flatten)]
        opts: NodeOpts,
        /// Install it but do not start it now.
        #[arg(long)]
        no_start: bool,
        /// Print what would be installed and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Removes the background service.
    UninstallService,
    /// Says whether the node is running, who it acts as, and how its links stand.
    Status {
        /// The data directory of the node (names its pipe).
        #[arg(long, env = "HAN_DATA", default_value = "han-data")]
        data: PathBuf,
        /// The node's endpoint, if it is not the default of `--data`.
        #[arg(long)]
        endpoint: Option<String>,
    },
}

/// The options that start a node.
#[derive(Args)]
pub struct NodeOpts {
    /// The actor this device acts as, for example `@lars:han.example`.
    #[arg(long)]
    actor: Option<String>,
    /// The id of this device's key in the actor's profile.
    #[arg(long, default_value = "d1")]
    key_id: String,
    /// The data directory: `node.db`, and the key files with `--file-keys`.
    #[arg(long, env = "HAN_DATA", default_value = "han-data")]
    data: PathBuf,
    /// Keep the keys in files under the data directory instead of the OS keychain (for runners with no
    /// keychain).
    #[arg(long)]
    file_keys: bool,
    /// UDP address of the QUIC endpoint.
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Relays: `n0` (n0's public relays and DNS discovery), `none`, or relay URLs separated by commas.
    #[arg(long, env = "HAN_RELAY", default_value = "n0")]
    relay: String,
    /// The pipe or socket to serve the node API on, if it is not the default of `--data`.
    #[arg(long)]
    endpoint: Option<String>,
    #[command(flatten)]
    home: HomeOpts,
}

/// The arguments of `han node` that reproduce `o` (for a service).
fn run_args(o: &NodeOpts) -> Result<Vec<String>, String> {
    let mut a = vec![
        "--actor".to_owned(),
        o.actor()?.to_owned(),
        "--key-id".into(),
        o.key_id.clone(),
        "--data".into(),
        service::absolute(&o.data).to_string_lossy().into_owned(),
        "--relay".into(),
        o.relay.clone(),
    ];
    if o.file_keys {
        a.push("--file-keys".into());
    }
    if let Some(b) = o.bind {
        a.extend(["--bind".into(), b.to_string()]);
    }
    if let Some(e) = &o.endpoint {
        a.extend(["--endpoint".into(), e.clone()]);
    }
    if let Some(h) = &o.home.home {
        a.extend(["--home".into(), h.clone()]);
    }
    Ok(a)
}

impl NodeOpts {
    fn actor(&self) -> Result<&str, String> {
        self.actor
            .as_deref()
            .ok_or_else(|| "han node needs --actor".to_owned())
    }
}

/// `han node status`.
async fn status(
    data: &std::path::Path,
    endpoint: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    use han_node::api::{Query, Row, ViewEvent, Window};
    let endpoint = endpoint.unwrap_or_else(|| default_endpoint(data));
    println!("{}", service::service_state());
    let Ok(client) = han_node::rpc::RpcClient::connect(&endpoint).await else {
        return Err(format!("not running: nothing answers at {endpoint}").into());
    };
    let mut view = client.view(Query::NodeStatus {
        window: Window::default(),
    });
    match view.next().await {
        Some(ViewEvent::Snapshot { rows, .. }) => {
            for row in rows {
                if let Row::NodeStatus(n) = row {
                    println!(
                        "running: {} node {} version {}",
                        n.actor, n.node_id, n.version
                    );
                    println!(
                        "spaces {} ({} live), outbox {}, roles {}",
                        n.spaces,
                        n.spaces_live,
                        n.outbox_pending,
                        n.roles.len()
                    );
                    for p in &n.problems {
                        println!("problem: {p}");
                    }
                }
            }
            Ok(())
        }
        Some(ViewEvent::Failed { error, .. }) => Err(error.into()),
        _ => Err("the node sent no status".into()),
    }
}

pub async fn run(args: NodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.cmd {
        None => serve_node(args.opts).await,
        Some(NodeCmd::Status { data, endpoint }) => status(&data, endpoint).await,
        Some(NodeCmd::UninstallService) => {
            println!("{}", service::uninstall()?);
            Ok(())
        }
        Some(NodeCmd::InstallService {
            opts,
            no_start,
            dry_run,
        }) => {
            let spec = service::Spec {
                exe: service::current_exe()?,
                args: run_args(&opts)?,
            };
            println!(
                "{}",
                service::install(&spec, opts.actor()?, !no_start, dry_run)?
            );
            Ok(())
        }
    }
}

async fn serve_node(args: NodeOpts) -> Result<(), Box<dyn std::error::Error>> {
    let actor = args.actor()?.to_owned();
    let actor = actor.as_str();
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

    let mut config = NodeConfig::new(actor);
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
    if let Some(choice) = args.home.resolve(Some(actor))? {
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
