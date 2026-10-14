//! The node a role process runs as its own actor: a host as its agent, an executor as its service actor. Each
//! has its own key (its node id) and its own iroh endpoint, follows the space it works in, and talks to it
//! only through the node API, like any face.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use han_net::wellknown::Fetcher;
use han_net::{HomeTicket, IrohOptions, IrohTransport};
use han_node::api::{Command, HomeRef, NodeError};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyError, KeyStore, KeyringStore};
use han_node::supervisor::NoSupervisor;
use han_node::{Node, NodeApi, NodeConfig, NodeParts, WellKnownKeys};
use han_seq::SystemClock;

use crate::seq::relay_choice;

/// Where the space's home is, as the supervisor passes it.
#[derive(Args, Clone, Debug)]
pub struct HomeArgs {
    /// The home's ServerName (not needed when `--home-ticket` is given).
    #[arg(long)]
    pub home_server: Option<String>,
    /// The home's node id (not needed when `--home-ticket` is given).
    #[arg(long)]
    pub home_node: Option<String>,
    /// The home's direct addresses.
    #[arg(long)]
    pub home_addr: Vec<String>,
    /// The home's relays.
    #[arg(long)]
    pub home_relay: Vec<String>,
    /// A home ticket (`han1home.…`) to pin, for a home with no HTTPS well-known: the role's node then trusts
    /// the home's node and receipt keys as the person's node does. The supervisor passes the one its node
    /// pinned in the environment.
    #[arg(long, env = "HAN_HOME_TICKET", hide_env_values = true)]
    pub home_ticket: Option<String>,
}

impl HomeArgs {
    /// The home as the node API takes it: from the flags, else from the ticket.
    pub fn home_ref(&self) -> Result<HomeRef, String> {
        if let (Some(server), Some(node)) = (&self.home_server, &self.home_node) {
            return Ok(HomeRef {
                server: server.clone(),
                node: node.clone(),
                addrs: self.home_addr.clone(),
                relays: self.home_relay.clone(),
            });
        }
        let ticket = self
            .home_ticket
            .as_deref()
            .and_then(|t| HomeTicket::parse(t.trim()))
            .ok_or("give --home-ticket, or --home-server and --home-node")?;
        Ok(HomeRef {
            server: ticket.server,
            node: ticket.node.node,
            addrs: ticket.node.addrs,
            relays: ticket.node.relays,
        })
    }

    /// The home's ServerName.
    pub fn server(&self) -> Result<String, String> {
        self.home_ref().map(|h| h.server)
    }
}

/// A key store that serves one named key as the device key: the role's node signs with the agent's (or
/// executor's) key, which the person's node created under its own name.
struct AliasKeys {
    inner: Arc<dyn KeyStore>,
    name: String,
}

impl KeyStore for AliasKeys {
    fn load(&self, name: &str) -> Result<Option<[u8; 32]>, KeyError> {
        self.inner
            .load(if name == DEVICE_KEY { &self.name } else { name })
    }

    fn store(&self, name: &str, seed: [u8; 32]) -> Result<(), KeyError> {
        self.inner
            .store(if name == DEVICE_KEY { &self.name } else { name }, seed)
    }
}

/// The key store holding `name`: the file store under `<keys>/keys` when the file is there, else the OS
/// keychain. Never creates a key: a role without its key does not start.
pub fn key_store(keys: &Path, name: &str) -> Result<Arc<dyn KeyStore>, Box<dyn std::error::Error>> {
    let file: Arc<dyn KeyStore> = Arc::new(FileKeyStore::new(keys.join("keys")));
    let inner: Arc<dyn KeyStore> = if file.load(name)?.is_some() {
        file
    } else {
        let ring: Arc<dyn KeyStore> = Arc::new(KeyringStore::new("han"));
        if ring.load(name)?.is_none() {
            return Err(format!("no key {name} in {} or the keychain", keys.display()).into());
        }
        ring
    };
    Ok(Arc::new(AliasKeys {
        inner,
        name: name.to_owned(),
    }))
}

/// A running role node.
pub struct RoleNode {
    /// The node.
    pub node: Node<IrohTransport>,
    /// Its endpoint.
    pub transport: IrohTransport,
}

/// Starts a node for `actor` with key `key_name`, its data under `data`, and follows `space` at `home`.
#[allow(clippy::too_many_arguments)]
pub async fn start(
    actor: &str,
    key_id: &str,
    key_name: &str,
    keys: &Path,
    data: &Path,
    relay: &str,
    home: &HomeArgs,
    space: &str,
) -> Result<RoleNode, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data)?;
    let store = key_store(keys, key_name)?;
    let seed = store
        .load(DEVICE_KEY)?
        .ok_or_else(|| format!("no key {key_name}"))?;
    let mut opts = IrohOptions::new(seed);
    opts.relay = relay_choice(relay);
    let transport = IrohTransport::bind(opts).await?;
    transport.online(std::time::Duration::from_secs(5)).await;
    let mut config = NodeConfig::new(actor);
    config.key_id = key_id.to_owned();
    config.data_dir = Some(PathBuf::from(data));
    let node = Node::start(NodeParts {
        config,
        transport: transport.clone(),
        clock: Arc::new(SystemClock::new()),
        keys: store,
        server_keys: Arc::new(WellKnownKeys::new(Fetcher::new())),
        supervisor: Arc::new(NoSupervisor),
    })?;
    if let Some(ticket) = &home.home_ticket {
        node.call(Command::HomePin {
            ticket: ticket.trim().to_owned(),
        })
        .await?;
    }
    match node
        .call(Command::SpaceFollow {
            home: home.home_ref()?,
            space: space.to_owned(),
        })
        .await
    {
        Ok(_) | Err(NodeError::Conflict { .. }) => {}
        Err(e) => return Err(e.into()),
    }
    wait_live(&node, space).await?;
    Ok(RoleNode { node, transport })
}

/// How long a role waits for its first Live link before it fails.
const LIVE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Waits until the link to `space`'s home is Live, so a role does not report that it started while it cannot
/// reach its home. Fails at once on a setup fault (the home does not know this actor, a key does not match
/// the pin) and after [`LIVE_WAIT`] otherwise, naming the reason the link gave.
async fn wait_live(
    node: &Node<IrohTransport>,
    space: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use han_node::api::{Query, Row, SpaceLink, ViewEvent, Window};
    let deadline = tokio::time::Instant::now() + LIVE_WAIT;
    let mut why = String::from("no link yet");
    loop {
        let mut view = node.view(Query::SpaceStatus {
            space: space.to_owned(),
            window: Window::default(),
        });
        if let Some(ViewEvent::Snapshot { rows, .. }) = view.next().await {
            for row in rows {
                if let Row::SpaceStatus(s) = row {
                    match s.link {
                        SpaceLink::Live => return Ok(()),
                        SpaceLink::Misconfigured | SpaceLink::Rejected => {
                            return Err(format!(
                                "the link to the home of {space} cannot come up: {}",
                                s.error.unwrap_or_else(|| "no reason given".into())
                            )
                            .into());
                        }
                        _ => why = s.error.unwrap_or_else(|| "connecting".into()),
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the link to the home of {space} did not come up in {}s: {why}",
                LIVE_WAIT.as_secs()
            )
            .into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}
