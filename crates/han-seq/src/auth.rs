//! The `hello` proof (G-08), in one place.
//!
//! Round 32 changes what a proof signs (the server's node id joins the bytes). Everything that builds or
//! checks a proof goes through these two functions, so that change is made here and in `han-core`'s
//! `sign::hello_proof`, nowhere else.

use han_core::sign::{hello_proof, verify_hello_proof};
use han_core::{Error, PublicKey, SigningKey};

/// The proof a client sends: `challenge` as received, `dialed` the ServerName it intends.
pub fn make_proof(key: &SigningKey, challenge: &str, dialed: &str) -> String {
    hello_proof(key, challenge, dialed)
}

/// Checks a proof against the connection's challenge and this server's own name.
pub fn verify_hello(
    key: &PublicKey,
    proof: &str,
    challenge: &str,
    server: &str,
) -> Result<(), Error> {
    verify_hello_proof(key, proof, challenge, server)
}
