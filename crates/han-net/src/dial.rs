//! Finding the node to dial, and which node ids to trust there.
//!
//! A ticket's node is only a hint that `/.well-known/han` must confirm (round 32, Z4). Everything that decides "which node ids may answer as this server" goes through
//! [`trusted_nodes_for_server`] and [`trusted_nodes_for_ticket`].

use han_core::json::{self, Value};

use crate::addr::{NodeAddr, NodeId};
use crate::error::{NetError, Result};
use crate::ticket::InviteTicket;
use crate::wellknown::{Fetcher, wellknown_verdict};

/// The node ids that may answer as `server`: its `sequencer` list in `/.well-known/han`, fetched over HTTPS
/// (the one mandatory discovery path, section 4.3). Also returns the addresses to dial.
pub async fn trusted_nodes_for_server(
    fetcher: &Fetcher,
    server: &str,
) -> Result<(Vec<NodeId>, Vec<NodeAddr>)> {
    let doc = fetcher.wellknown(server).await?;
    Ok((doc.nodes(), doc.sequencer))
}

/// Z4: a ticket's or a hint's `node` is only where to dial. It is confirmed when `wellknown` (the document
/// fetched over HTTPS from `server`) is well formed, names `server` and lists `node` in `sequencer`. `None`
/// when confirmed, otherwise the reason: the client does not say `hello`. (`ref/carrier.py` `dial_verdict`,
/// replayed against `control.json`'s `dials`.)
pub fn dial_verdict(server: &str, node: &str, wellknown: &Value) -> Option<String> {
    if let Some(why) = wellknown_verdict(wellknown) {
        return Some(format!("well-known: {why}"));
    }
    if wellknown["server"].as_str() != Some(server) {
        return Some("the well-known is another server's".into());
    }
    let listed = wellknown["sequencer"]
        .as_array()
        .is_some_and(|l| l.iter().any(|n| n["node"].as_str() == Some(node)));
    (!listed).then(|| "node not listed for the server".into())
}

/// The node ids that may answer for a ticket's `server` (N-03): the ticket's `node`, once `server`'s
/// `/.well-known/han` confirms it ([`dial_verdict`], round 32 Z4). A node the document does not list is
/// refused here, before any `hello`.
pub async fn trusted_nodes_for_ticket(
    fetcher: &Fetcher,
    ticket: &InviteTicket,
) -> Result<Vec<NodeId>> {
    if ticket.node.addrs.is_empty() && ticket.node.relays.is_empty() {
        return Err(NetError::Dial("the ticket's node has no address".into()));
    }
    let doc = fetcher.wellknown(&ticket.server).await?;
    let doc = json::to_value(&doc).map_err(|e| NetError::Protocol(e.to_string()))?;
    match dial_verdict(&ticket.server, &ticket.node.node, &doc) {
        None => Ok(vec![ticket.node.node.clone()]),
        Some(why) => Err(NetError::Dial(format!(
            "the ticket's node is refused: {why}"
        ))),
    }
}
