//! The `hello` proof (G-08), in one place.
//!
//! Everything that builds or checks a proof goes through these two functions. On `han/1` the proof binds
//! the server's node id (round 32, Z4: `"han-hello-v2\n" ‖ challenge ‖ "\n" ‖ dialed ‖ "\n" ‖ node`). The
//! legacy WebSocket carrier has no node ids and keeps round 31's `"han-hello-v1\n"` bytes, so the wire
//! vectors are unchanged: pass `None` for `node` there.

use han_core::sign::{han1_hello_proof, hello_proof, verify_han1_hello_proof, verify_hello_proof};
use han_core::{Error, PublicKey, SigningKey};

/// The proof a client sends: `challenge` as received, `dialed` the ServerName it intends, `node` the server's
/// node id as it reached it (`han/1`), or `None` on the legacy carrier.
pub fn make_proof(key: &SigningKey, challenge: &str, dialed: &str, node: Option<&str>) -> String {
    match node {
        Some(node) => han1_hello_proof(key, challenge, dialed, node),
        None => hello_proof(key, challenge, dialed),
    }
}

/// Checks a proof against the connection's challenge, this server's own name and, on `han/1`, its own node id
/// on this connection.
pub fn verify_hello(
    key: &PublicKey,
    proof: &str,
    challenge: &str,
    server: &str,
    node: Option<&str>,
) -> Result<(), Error> {
    match node {
        Some(node) => verify_han1_hello_proof(key, proof, challenge, server, node),
        None => verify_hello_proof(key, proof, challenge, server),
    }
}

/// G-08, SR-13: a user actor's `hello` is bound to the connection when its key is `valid` and is the
/// connection's node id (the device that signs is the device that connected). `key` is the key's
/// `(publicKey, valid)`, if the pinned profile has it. `Err` carries the reason.
pub fn user_binding(key: Option<(&str, bool)>, node: &str) -> Result<(), &'static str> {
    match key {
        Some((public, true)) if public == node => Ok(()),
        Some((_, true)) => Err("the signing key is not the connection's node"),
        _ => Err("key not valid"),
    }
}

/// G-08, SR-13: a server actor's `hello` is bound to the connection when the connection's node is in its
/// server's `sequencer` and the key is among the server's keys.
pub fn server_binding(
    sequencer: &[String],
    key_listed: bool,
    node: &str,
) -> Result<(), &'static str> {
    if !sequencer.iter().any(|n| n == node) {
        return Err("node not in the server's sequencer");
    }
    if !key_listed {
        return Err("no such server key");
    }
    Ok(())
}
