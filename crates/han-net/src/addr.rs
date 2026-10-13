//! Node ids and how to dial them (N-01, section 4.3 `NodeAddr`).

use std::net::{IpAddr, SocketAddr};

use han_core::json::Value;
use han_core::sign::is_node_id;
use serde::{Deserialize, Serialize};

/// A node id: a device's Ed25519 public key in canonical base64url (N-01).
pub type NodeId = String;

/// At most this many direct addresses and relay URLs.
pub const MAX_ADDRS: usize = 16;

/// A node and the ways to reach it, as `/.well-known/han` and tickets list them (`NodeAddr` of `han.ts`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeAddr {
    /// The node id.
    pub node: NodeId,
    /// Direct addresses: `ip:port`, an IPv6 address in brackets.
    pub addrs: Vec<String>,
    /// Relay URLs, `https://`.
    pub relays: Vec<String>,
}

impl NodeAddr {
    /// A node reachable at direct socket addresses.
    pub fn direct(node: &str, addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        NodeAddr {
            node: node.to_owned(),
            addrs: addrs.into_iter().map(|a| a.to_string()).collect(),
            relays: Vec::new(),
        }
    }

    /// The direct addresses that parse.
    pub fn socket_addrs(&self) -> Vec<SocketAddr> {
        self.addrs.iter().filter_map(|a| a.parse().ok()).collect()
    }
}

/// `ip:port` in canonical text: an IPv4 address, or an IPv6 address in brackets, then a port 1 to 65535.
fn addr_ok(text: &str) -> bool {
    let Some((host, port)) = text.rsplit_once(':') else {
        return false;
    };
    let port_ok = !port.starts_with('0')
        && !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|p| p <= 65_535);
    if !port_ok {
        return false;
    }
    let (inner, bracketed) = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(inner) => (inner, true),
        None => (host, false),
    };
    match inner.parse::<IpAddr>() {
        Ok(ip) => ip.is_ipv6() == bracketed && ip.to_string() == inner,
        Err(_) => false,
    }
}

fn relay_ok(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        (1..=1016).contains(&rest.len()) && rest.bytes().all(|b| (b'!'..=b'~').contains(&b))
    })
}

/// Why a `NodeAddr` is not one (X10), or `None` if it is: a strict node id, at most 16 direct addresses
/// and 16 relay URLs without repeats, and at least one of the two.
pub fn node_addr_verdict(v: &Value) -> Option<&'static str> {
    let Value::Object(m) = v else {
        return Some("NodeAddr members");
    };
    if m.len() != 3
        || !["node", "addrs", "relays"]
            .iter()
            .all(|k| m.contains_key(*k))
    {
        return Some("NodeAddr members");
    }
    if !v["node"].as_str().is_some_and(is_node_id) {
        return Some("node id");
    }
    let mut any = false;
    for (name, ok) in [("addrs", addr_ok as fn(&str) -> bool), ("relays", relay_ok)] {
        let Some(list) = v[name].as_array() else {
            return Some(name_static(name));
        };
        let texts: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
        let distinct = texts.iter().collect::<std::collections::HashSet<_>>().len() == texts.len();
        if list.len() > MAX_ADDRS
            || texts.len() != list.len()
            || !texts.iter().all(|t| ok(t))
            || !distinct
        {
            return Some(name_static(name));
        }
        any |= !list.is_empty();
    }
    (!any).then_some("no address and no relay")
}

fn name_static(name: &str) -> &'static str {
    if name == "addrs" { "addrs" } else { "relays" }
}
