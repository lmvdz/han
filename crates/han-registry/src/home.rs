//! How a command or a spec names a home.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The start of a home ticket, as `han seq` and `han ticket` print it.
pub const TICKET_PREFIX: &str = "han1home.";

/// A home, as a command names it. The node resolves it: a ticket is parsed and pinned (its node, addresses and
/// receipt keys are trusted as they stand), a ServerName is looked up in the server's `/.well-known/han`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HomeRef {
    /// A home ticket (`han1home.…`). It needs no HTTPS well-known.
    Ticket(String),
    /// A ServerName, such as `han.example`; the node finds the home's node and keys at its well-known.
    Server(String),
}

impl HomeRef {
    /// The ticket text, if this names the home by ticket.
    pub fn ticket(&self) -> Option<&str> {
        match self {
            HomeRef::Ticket(t) => Some(t),
            HomeRef::Server(_) => None,
        }
    }
}

impl FromStr for HomeRef {
    type Err = String;

    /// A `han1home.…` ticket, else a ServerName. Whether either is well formed is the node's to check.
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() || s.chars().any(char::is_whitespace) {
            return Err("a home is a ticket (han1home.…) or a ServerName".into());
        }
        Ok(if s.starts_with(TICKET_PREFIX) {
            HomeRef::Ticket(s.to_owned())
        } else {
            HomeRef::Server(s.to_owned())
        })
    }
}

impl std::fmt::Display for HomeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HomeRef::Ticket(t) => f.write_str(t),
            HomeRef::Server(s) => f.write_str(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_or_a_server_name() {
        assert_eq!(
            "han1home.abc".parse(),
            Ok(HomeRef::Ticket("han1home.abc".into()))
        );
        assert_eq!(
            " han.example ".parse(),
            Ok(HomeRef::Server("han.example".into()))
        );
        assert!("".parse::<HomeRef>().is_err());
        assert!("two words".parse::<HomeRef>().is_err());
        let r = HomeRef::Ticket("han1home.abc".into());
        assert_eq!(r.to_string().parse(), Ok(r.clone()));
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"ticket":"han1home.abc"}"#
        );
    }
}
