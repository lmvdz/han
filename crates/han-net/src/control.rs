//! The client's checks of the control stream before it says `hello` (G-07, G-08).

use han_core::grammar;
use han_core::json::{self, Value};

use crate::addr::NodeId;

/// The Challenge the server sends first on the control stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    /// 22 to 128 characters of `[A-Za-z0-9_-]` (the server sends 43: 32 random bytes).
    pub challenge: String,
    /// The ServerName the server believes it is.
    pub server: String,
    /// The versions it speaks.
    pub versions: Vec<u64>,
    /// Its clock.
    pub time: String,
}

fn challenge_chars(s: &str) -> bool {
    (22..=128).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// What a client checks before it says `hello`: `frames` are the texts of the control stream so far. There is
/// exactly one, the bare Challenge `{challenge, server, versions, time}`; its `server` equals `dialed`; and
/// the node reached is one of `nodes`: `dialed`'s `sequencer` nodes in its well-known document, or a ticket's
/// node. Any further control frame is a protocol error. Returns the Challenge, or why to abort.
pub fn challenge_verdict(
    frames: &[String],
    dialed: &str,
    node: &str,
    nodes: &[NodeId],
) -> Result<Challenge, &'static str> {
    let [first] = frames else {
        return Err(if frames.is_empty() {
            "no challenge"
        } else {
            "the control stream carries only the challenge"
        });
    };
    let c = json::parse(first.as_bytes()).map_err(|_| "not I-JSON")?;
    let Value::Object(m) = &c else {
        return Err("not a bare Challenge");
    };
    if m.len() != 4 || !["challenge", "server", "versions", "time"].iter().all(|k| m.contains_key(*k)) {
        return Err("not a bare Challenge");
    }
    let challenge = c["challenge"].as_str().filter(|s| challenge_chars(s)).ok_or("challenge grammar")?;
    let versions: Vec<u64> = c["versions"]
        .as_array()
        .and_then(|l| l.iter().map(Value::as_u64).collect())
        .filter(|v: &Vec<u64>| v.contains(&1))
        .ok_or("no version 1")?;
    let time = c["time"].as_str().filter(|t| grammar::is_time(t)).ok_or("time")?;
    let server = c["server"].as_str().ok_or("server")?;
    if server != dialed {
        return Err("server is not the dialed ServerName");
    }
    if !nodes.iter().any(|n| n == node) {
        return Err("node is not one of dialed's");
    }
    Ok(Challenge {
        challenge: challenge.to_owned(),
        server: server.to_owned(),
        versions,
        time: time.to_owned(),
    })
}
