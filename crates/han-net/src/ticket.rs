//! Invite tickets (N-03): `han1` followed by the base64url of the JCS of `{server, node, space, secret}`.

use han_core::json::{self, Value};
use han_core::{base64url, grammar};
use serde::{Deserialize, Serialize};

use han_seq::ServerKey;

use crate::addr::{NodeAddr, node_addr_verdict};
use crate::wellknown::wellknown_verdict;

const PREFIX: &str = "han1";

/// What an inviter hands a joining person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteTicket {
    /// The ServerName to put in `dialed` (G-08): the space's home when the ticket was made.
    pub server: String,
    /// A node of that home with its addresses and relays. A hint: see [`crate::dial::trusted_nodes`].
    pub node: NodeAddr,
    /// The space.
    pub space: String,
    /// 32 random bytes, base64url. The home keeps only its hash.
    pub secret: String,
}

impl InviteTicket {
    /// The text people pass on.
    pub fn to_text(&self) -> String {
        let value = json::to_value(self).unwrap_or(Value::Null);
        let jcs = json::canonicalize(&value).unwrap_or_default();
        format!("{PREFIX}{}", base64url::encode(&jcs))
    }

    /// Parses ticket text, which must be exactly canonical: base64url of the ticket's JCS.
    pub fn parse(text: &str) -> Option<InviteTicket> {
        let raw = base64url::decode(text.strip_prefix(PREFIX)?).ok()?;
        let v = json::parse(&raw).ok()?;
        if json::canonicalize(&v).ok()? != raw {
            return None; // not JCS
        }
        let Value::Object(m) = &v else {
            return None;
        };
        if m.len() != 4 {
            return None;
        }
        let ok = v["server"].as_str().is_some_and(grammar::is_server_name)
            && node_addr_verdict(&v["node"]).is_none()
            && v["space"]
                .as_str()
                .is_some_and(|s| grammar::is_space_id(s) && !s.starts_with("profile:"))
            && v["secret"]
                .as_str()
                .and_then(|s| base64url::decode(s).ok())
                .is_some_and(|b| b.len() == 32);
        if !ok {
            return None;
        }
        json::from_value(&v).ok()
    }

    /// The secret's bytes.
    pub fn secret_bytes(&self) -> Option<Vec<u8>> {
        base64url::decode(&self.secret).ok()
    }
}

const HOME_PREFIX: &str = "han1home.";

/// What a home hands people so their devices can join it with no HTTPS `/.well-known/han`: the home's
/// ServerName, one node with its addresses and relays, and the server keys that sign its receipts.
///
/// The architecture has no han/1 call that returns a server's keys, and G-08 requires the well-known over HTTPS
/// before `hello`; a home with a placeholder ServerName (no DNS, no HTTPS) cannot meet that. A home ticket
/// stands in for the document: whoever holds it trusts the node and keys in it, handed over out of band. The
/// trust is **ticket-pinned**, weaker than a confirmed well-known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeTicket {
    /// The ServerName to put in `dialed` (G-08).
    pub server: String,
    /// A node of the home with its addresses and relays.
    pub node: NodeAddr,
    /// The keys that sign the home's receipts (the `keys` of its well-known document).
    pub keys: Vec<ServerKey>,
}

impl HomeTicket {
    /// The text people pass on: `han1home.` and the base64url of the JCS.
    pub fn to_text(&self) -> String {
        let value = json::to_value(self).unwrap_or(Value::Null);
        let jcs = json::canonicalize(&value).unwrap_or_default();
        format!("{HOME_PREFIX}{}", base64url::encode(&jcs))
    }

    /// Parses ticket text (exactly canonical), checking the shape the way a well-known document is checked.
    pub fn parse(text: &str) -> Option<HomeTicket> {
        let raw = base64url::decode(text.strip_prefix(HOME_PREFIX)?).ok()?;
        let v = json::parse(&raw).ok()?;
        if json::canonicalize(&v).ok()? != raw {
            return None; // not JCS
        }
        let Value::Object(m) = &v else {
            return None;
        };
        if m.len() != 3 || !v["server"].as_str().is_some_and(grammar::is_server_name) {
            return None;
        }
        let doc = Value::Object(
            [
                ("server".to_owned(), v["server"].clone()),
                (
                    "sequencer".to_owned(),
                    Value::Array(vec![v["node"].clone()]),
                ),
                ("keys".to_owned(), v["keys"].clone()),
            ]
            .into(),
        );
        if wellknown_verdict(&doc).is_some() {
            return None;
        }
        json::from_value(&v).ok()
    }
}
