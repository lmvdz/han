//! Finding the node to dial, and which node ids to trust there.
//!
//! These are the seams round 32 changes: a ticket's node becomes a hint that `/.well-known/han` must
//! confirm. Everything that decides "which node ids may answer as this server" goes through
//! [`trusted_nodes_for_server`] and [`trusted_nodes_for_ticket`].

use crate::addr::{NodeAddr, NodeId};
use crate::error::{NetError, Result};
use crate::ticket::InviteTicket;
use crate::wellknown::Fetcher;

/// The node ids that may answer as `server`: its `sequencer` list in `/.well-known/han`, fetched over HTTPS
/// (the one mandatory discovery path, section 4.3). Also returns the addresses to dial.
pub async fn trusted_nodes_for_server(fetcher: &Fetcher, server: &str) -> Result<(Vec<NodeId>, Vec<NodeAddr>)> {
    let doc = fetcher.wellknown(server).await?;
    Ok((doc.nodes(), doc.sequencer))
}

/// The node ids that may answer for a ticket's `server` (N-03).
///
/// Round 31: the ticket's own `node` is trusted as given, so a ticket works with no web server. Round 32
/// makes it a hint: the node must also be listed in `server`'s well-known document. Only this function
/// changes then.
pub async fn trusted_nodes_for_ticket(_fetcher: &Fetcher, ticket: &InviteTicket) -> Result<Vec<NodeId>> {
    if ticket.node.addrs.is_empty() && ticket.node.relays.is_empty() {
        return Err(NetError::Dial("the ticket's node has no address".into()));
    }
    Ok(vec![ticket.node.node.clone()])
}
