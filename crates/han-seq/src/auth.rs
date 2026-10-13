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
