//! A home resolved to where to dial it (the facts a ticket or a `/.well-known/han` document carries). A
//! command names a home as a [`han_registry::HomeRef`]; the node resolves that to this.

use serde::{Deserialize, Serialize};

/// A home and the node to dial for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeAddr {
    /// The ServerName, as put in `dialed` (G-08).
    pub server: String,
    /// The node id of the home's sequencer.
    pub node: String,
    /// Direct addresses, `ip:port`.
    #[serde(default)]
    pub addrs: Vec<String>,
    /// Relay URLs.
    #[serde(default)]
    pub relays: Vec<String>,
}
