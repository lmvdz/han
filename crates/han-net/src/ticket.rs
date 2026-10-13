//! Invite tickets (N-03): `han1` followed by the base64url of the JCS of `{server, node, space, secret}`.

use han_core::json::{self, Value};
use han_core::{base64url, grammar};
use serde::{Deserialize, Serialize};

use crate::addr::{NodeAddr, node_addr_verdict};

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
