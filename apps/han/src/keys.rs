//! Key files: a 32-byte Ed25519 seed as 64 hex digits. The same key is a device's signing key and its node id
//! (N-01); a server has its own for receipts and another for its node.

use std::path::Path;

use han_core::SigningKey;

/// Reads the seed in `path`, or makes a new one there if the file does not exist.
pub fn load_or_create(path: &Path) -> Result<[u8; 32], String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_seed(text.trim())
            .ok_or_else(|| format!("{} is not 64 hex digits", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            write_private(path, &to_hex(&seed))?;
            Ok(seed)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The key of a seed.
pub fn signing_key(seed: [u8; 32]) -> SigningKey {
    SigningKey::from_seed(seed)
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

fn parse_seed(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
