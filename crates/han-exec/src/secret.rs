//! A secret held only in memory: no `Serialize`, and `Debug` never shows it (SR-50).

/// A credential (an installation token, a JWT). It never goes to disk, logs or the ledger.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret.
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    /// The secret, for the one request that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
