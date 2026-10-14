//! Naming a home on the command line without its HTTPS well-known: by a home ticket, or by node address.
//!
//! `han seq` writes a home ticket to its data directory (`han ticket --data` prints it). A node given one keeps the home's node and
//! receipt keys, as ticket-pinned trust, in its data directory (`Command::HomePin`).

use clap::Args;
use han_net::{HomeTicket, NodeAddr};
use han_node::api::HomeRef;
use han_seq::ServerKey;

/// The start of time, for a key given on the command line (it has no `from` of its own).
const EPOCH: &str = "1970-01-01T00:00:00Z";

/// How a command is told which home to use (`--home`, or `--home-node` with its address and keys).
#[derive(Args, Default)]
pub struct HomeOpts {
    /// A home ticket (`han1home.…`, from `han seq` or `han ticket`): the home's ServerName, node, addresses and
    /// receipt keys. Needs no HTTPS well-known.
    #[arg(long, conflicts_with_all = ["home_node", "home_server", "home_addr", "home_relay", "home_key"])]
    pub home: Option<String>,
    /// The home's ServerName (the `dialed` of hello), with `--home-node`.
    #[arg(long)]
    pub home_server: Option<String>,
    /// The home's node id.
    #[arg(long)]
    pub home_node: Option<String>,
    /// A direct address `ip:port` of the home's node.
    #[arg(long)]
    pub home_addr: Vec<String>,
    /// A relay URL of the home's node.
    #[arg(long)]
    pub home_relay: Vec<String>,
    /// A key of the home that signs receipts, `keyId:publicKey`. With it the node pins the home's node and keys
    /// (ticket-pinned); without it the keys come from `https://<server>/.well-known/han`.
    #[arg(long)]
    pub home_key: Vec<String>,
}

/// A home as the command line named it.
pub struct HomeChoice {
    /// Where to dial.
    pub home: HomeRef,
    /// The ticket to pin, if the command line carried the keys.
    pub pin: Option<HomeTicket>,
}

impl HomeOpts {
    /// The home named, if any. The server of `default_actor` stands in for `--home-server`.
    pub fn resolve(&self, default_actor: Option<&str>) -> Result<Option<HomeChoice>, String> {
        if let Some(text) = &self.home {
            let t = HomeTicket::parse(text).ok_or("--home: not a home ticket (han1home.…)")?;
            return Ok(Some(choice(&t, Some(t.clone()))));
        }
        let Some(node) = &self.home_node else {
            return Ok(None);
        };
        let server = self
            .home_server
            .as_deref()
            .or(default_actor.and_then(|a| a.rsplit_once(':').map(|(_, server)| server)))
            .ok_or("--home-node needs --home-server")?;
        let addr = NodeAddr {
            node: node.clone(),
            addrs: self.home_addr.clone(),
            relays: self.home_relay.clone(),
        };
        if self.home_key.is_empty() {
            let t = HomeTicket {
                server: server.to_owned(),
                node: addr,
                keys: vec![],
            };
            return Ok(Some(choice(&t, None)));
        }
        let keys = self
            .home_key
            .iter()
            .map(|k| {
                let (key_id, public_key) =
                    k.split_once(':').ok_or("--home-key: keyId:publicKey")?;
                Ok(ServerKey {
                    key_id: key_id.to_owned(),
                    public_key: public_key.to_owned(),
                    from: EPOCH.to_owned(),
                    until: None,
                    compromised: None,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let t = HomeTicket {
            server: server.to_owned(),
            node: addr,
            keys,
        };
        // the same checks a ticket gets
        let t = HomeTicket::parse(&t.to_text())
            .ok_or("the home's node, addresses or keys are malformed")?;
        Ok(Some(choice(&t, Some(t.clone()))))
    }
}

fn choice(t: &HomeTicket, pin: Option<HomeTicket>) -> HomeChoice {
    HomeChoice {
        home: HomeRef {
            server: t.server.clone(),
            node: t.node.node.clone(),
            addrs: t.node.addrs.clone(),
            relays: t.node.relays.clone(),
        },
        pin,
    }
}
