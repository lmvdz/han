//! Device keys (PLAN 6.2): an Ed25519 seed per name, behind a [`KeyStore`]. The OS keychain implements it for
//! people's machines; a file implements it for tests and headless runners. Keys never leave han-node: nothing
//! in the node API returns a seed, and the seed of the device key is also its iroh secret (N-01).

use std::path::PathBuf;

use han_core::SigningKey;

/// The name of the device key.
pub const DEVICE_KEY: &str = "device";

/// Why a key could not be read or stored.
#[derive(Debug, thiserror::Error)]
#[error("key store: {0}")]
pub struct KeyError(pub String);

/// Where key seeds live.
pub trait KeyStore: Send + Sync + 'static {
    /// The seed stored under `name`, if there is one.
    fn load(&self, name: &str) -> Result<Option<[u8; 32]>, KeyError>;

    /// Stores `seed` under `name`, replacing any.
    fn store(&self, name: &str, seed: [u8; 32]) -> Result<(), KeyError>;

    /// The seed under `name`; a new random one, stored, if there was none.
    fn load_or_create(&self, name: &str) -> Result<[u8; 32], KeyError> {
        if let Some(seed) = self.load(name)? {
            return Ok(seed);
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| KeyError(e.to_string()))?;
        self.store(name, seed)?;
        Ok(seed)
    }
}

/// The signing key of a seed.
pub fn signing_key(seed: [u8; 32]) -> SigningKey {
    SigningKey::from_seed(seed)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_seed(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Seeds in files, `<dir>/<name>.key`, as 64 hex digits (mode 0600 on Unix). For tests and for runners with
/// no keychain; people's devices use [`KeyringStore`].
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    /// A store in `dir`, which is created when a key is first stored.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileKeyStore { dir: dir.into() }
    }

    fn path(&self, name: &str) -> Result<PathBuf, KeyError> {
        // The name becomes a file name: keep it to a plain, short component.
        let ok = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !name.starts_with('.');
        if !ok {
            return Err(KeyError(format!("bad key name {name:?}")));
        }
        Ok(self.dir.join(format!("{name}.key")))
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self, name: &str) -> Result<Option<[u8; 32]>, KeyError> {
        let path = self.path(name)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => parse_seed(&text)
                .map(Some)
                .ok_or_else(|| KeyError(format!("{} is not 64 hex digits", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(KeyError(format!("{}: {e}", path.display()))),
        }
    }

    fn store(&self, name: &str, seed: [u8; 32]) -> Result<(), KeyError> {
        use std::io::Write as _;
        let path = self.path(name)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| KeyError(e.to_string()))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|e| KeyError(format!("{}: {e}", path.display())))?;
        file.write_all(to_hex(&seed).as_bytes())
            .map_err(|e| KeyError(e.to_string()))
    }
}

/// Seeds in the OS keychain (Windows Credential Manager, macOS Keychain, the Secret Service on Linux), one
/// entry per name under a service name.
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// A store whose entries are `service` / name.
    pub fn new(service: impl Into<String>) -> Self {
        KeyringStore {
            service: service.into(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, KeyError> {
        keyring::Entry::new(&self.service, name).map_err(|e| KeyError(e.to_string()))
    }
}

impl KeyStore for KeyringStore {
    fn load(&self, name: &str) -> Result<Option<[u8; 32]>, KeyError> {
        match self.entry(name)?.get_secret() {
            Ok(bytes) => <[u8; 32]>::try_from(bytes.as_slice())
                .map(Some)
                .map_err(|_| KeyError(format!("keychain entry {name} is not a 32-byte seed"))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeyError(e.to_string())),
        }
    }

    fn store(&self, name: &str, seed: [u8; 32]) -> Result<(), KeyError> {
        self.entry(name)?
            .set_secret(&seed)
            .map_err(|e| KeyError(e.to_string()))
    }
}
