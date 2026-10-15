//! `han node`: the daemon of one device. It holds the device key, binds the device's one iroh endpoint, follows
//! the spaces the device belongs to, keeps `node.db`, and serves the node API to local faces over a pipe.
//!
//! `han node install-service` makes it start at logon (a per-user scheduled task on Windows, a systemd user
//! unit on Linux; no admin rights), and `han node status` says whether it is running.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
use han_host::sandbox::ShimCommand;
use han_net::wellknown::Fetcher;
use han_net::{IrohOptions, IrohTransport, Transport};
use han_node::api::{Command, ConnectionClass, NodeApi};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore, KeyringStore};
use han_node::rpc::{default_endpoint, role_endpoint, serve_roles};
use han_node::supervisor::ProcessSpawner;
use han_node::{Node, NodeConfig, NodeParts, WellKnownKeys};
use han_registry::{AgentsFile, HomeRef, RunAs};
use han_seq::SystemClock;

use crate::gate::GatedSpawner;
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
    /// Shows or sets this device's human-presence check for approvals and for unlocking governing and steering
    /// on a connection (stored in `node.db`, read at each use). While either requires a person, changing it
    /// needs one to confirm.
    Presence {
        /// The data directory of the node.
        #[arg(long, env = "HAN_DATA", default_value = "han-data")]
        data: PathBuf,
        /// `all`, or a comma-separated list of action kinds (`effect.approve`, `effect.cancel`, `face.unlock`);
        /// the empty list requires nothing.
        #[arg(long)]
        require: Option<String>,
        /// `auto`, `windows-hello`, `touch-id`, `polkit` or `none`.
        #[arg(long)]
        backend: Option<String>,
        /// How many seconds one unlock of a connection to govern and steer lasts (10 to 3600, default 600).
        #[arg(long)]
        unlock_secs: Option<u64>,
        /// The node keeps its keys in files (`han node --file-keys`), which is where the settings' seal key is.
        #[arg(long)]
        file_keys: bool,
    },
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
    /// A home ticket (`han1home.…`, from `han seq` or `han ticket`) to pin: the home's node and receipt keys are
    /// kept in the data directory as ticket-pinned trust, so a home with no HTTPS well-known needs no flag at
    /// the next start.
    #[arg(long)]
    home: Option<HomeRef>,
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
    if let Some(h) = &o.home {
        a.extend(["--home".into(), h.to_string()]);
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
/// `han node presence`: the settings, after a change if one is asked for.
async fn presence(
    data: &std::path::Path,
    file_keys: bool,
    require: Option<String>,
    backend: Option<String>,
    unlock_secs: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    use han_node::presence::{
        BackendChoice, HEADLESS_DIR, Require, change, headless_marker, load, seal_name,
    };
    let require = require.map(|r| r.parse::<Require>()).transpose()?;
    let backend = backend.map(|b| b.parse::<BackendChoice>()).transpose()?;
    std::fs::create_dir_all(data)?;
    let db = han_node::db::NodeDb::open(&data.join("node.db"))?;
    // the settings are sealed with a key from the store the node uses
    let keys: Arc<dyn KeyStore> = if file_keys {
        Arc::new(FileKeyStore::new(data.join("keys")))
    } else {
        Arc::new(KeyringStore::new("han"))
    };
    let name = seal_name(data);
    let seal = Some((&*keys, name.as_str()));
    let changing = require.is_some() || backend.is_some() || unlock_secs.is_some();
    // root made this node headless (`han runner setup`): its settings are not a person's to change here
    let marker = std::path::Path::new(HEADLESS_DIR).join(&name);
    if headless_marker(std::path::Path::new(HEADLESS_DIR), &name, 0) {
        if changing {
            return Err(format!(
                "this node is headless: nobody sits at it, so its presence settings are root's. They come from {}; as root, `han runner setup` writes it and removing it makes the node a person's again",
                marker.display()
            )
            .into());
        }
        println!(
            "headless: nobody sits at this node. It asks for no person to govern or steer, and it never approves. The operating system's boundary (who can run as this node's user) is the authority; root's marker is {}",
            marker.display()
        );
        return Ok(());
    }
    if changing {
        println!("confirm on this device if asked…");
        tokio::task::block_in_place(|| {
            change(
                &db,
                seal,
                require,
                backend,
                unlock_secs,
                None,
                han_node::presence::DEFAULT_TIMEOUT,
            )
        })?;
    }
    let now = load(&db, seal);
    println!("presence.require {}", now.require);
    println!("presence.backend {}", now.backend);
    println!("presence.unlock_secs {}", now.unlock.as_secs());
    if changing {
        println!("a running node keeps the settings it started with: restart it to apply this");
    }
    Ok(())
}

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
        None => {
            // A node started by the service has no console to show why it stopped: the log has it.
            let data = args.opts.data.clone();
            let result = serve_node(args.opts).await;
            if let Err(e) = &result {
                log_line(&data, &format!("the node stopped: {e}"));
            }
            result
        }
        Some(NodeCmd::Presence {
            data,
            require,
            backend,
            unlock_secs,
            file_keys,
        }) => presence(&data, file_keys, require, backend, unlock_secs).await,
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

/// The agents of `agents.json` that run as another OS user, with where their unit's spawn file is.
fn os_user_agents(data: &std::path::Path) -> Vec<(String, RunAs)> {
    let Ok(file) = AgentsFile::load(data) else {
        return Vec::new();
    };
    file.agents
        .iter()
        .filter_map(|(actor, a)| {
            Some((
                actor.clone(),
                RunAs {
                    os_user: a.os_user.clone()?,
                    state_dir: file.state_dir(data, actor),
                },
            ))
        })
        .collect()
}

/// Appends a line to `<data>/node.log`, the record of how a node start went. A failure to write it is ignored.
fn log_line(data: &std::path::Path, line: &str) {
    use std::io::Write as _;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("node.log"))
    {
        let _ = writeln!(f, "{at} {line}");
    }
}

async fn serve_node(args: NodeOpts) -> Result<(), Box<dyn std::error::Error>> {
    let actor = args.actor()?.to_owned();
    let actor = actor.as_str();
    std::fs::create_dir_all(&args.data)?;
    log_line(&args.data, "the node is starting");
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

    let endpoint = args
        .endpoint
        .unwrap_or_else(|| default_endpoint(&args.data));
    let mut config = NodeConfig::new(actor);
    config.key_id = args.key_id;
    config.data_dir = Some(args.data.clone());
    let spawner = ProcessSpawner::new(std::env::current_exe()?)
        .with_host_endpoint(role_endpoint(&endpoint, ConnectionClass::Host));
    // Hosts in `han-agent@` units outlive this node: take back the ones still running.
    for name in spawner.adopt(&os_user_agents(&args.data)) {
        println!("adopted {name} (its unit was already running)");
    }
    let node = Node::start(NodeParts {
        config,
        transport: transport.clone(),
        clock: Arc::new(SystemClock::new()),
        keys,
        server_keys: Arc::new(WellKnownKeys::new(Fetcher::new())),
        // Hosts are told the host endpoint and no other (SR-107).
        // and are started only where the agent's sandbox works (the gate says why not).
        supervisor: Arc::new(GatedSpawner::new(spawner, ShimCommand::of_current_exe()?)),
    })?;

    // A home named by ticket is kept in the data directory, so a restart needs no flag.
    match args.home {
        Some(HomeRef::Ticket(ticket)) => {
            node.call(Command::HomePin { ticket }).await?;
        }
        Some(HomeRef::Server(_)) => {
            return Err("--home is a home ticket (han1home.…): a ServerName needs no pin".into());
        }
        None => {}
    }

    let server = serve_roles(Arc::new(node.clone()), &endpoint)?;
    log_line(
        &args.data,
        &format!("the node serves {}", server.endpoint()),
    );
    println!("actor {}", node.actor());
    println!("node {}", transport.node_id());
    println!("endpoint {}", server.endpoint());
    tokio::signal::ctrl_c().await?;
    node.shutdown();
    drop(server);
    transport.shutdown().await;
    Ok(())
}
