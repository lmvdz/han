//! `han update --check`: reads a release manifest, verifies its Ed25519 signature against the public key
//! embedded at build time, and says whether a newer version exists. It never downloads or installs anything:
//! the binary does not update without explicit consent, and applying an update is not implemented yet
//! (docs/release.md, "Self-update"). Until then a user upgrades through the installer they used.

use std::path::PathBuf;

use clap::Args;
use han_core::PublicKey;

/// The release signing key, base64url of 32 bytes. This is a placeholder: release builds set
/// `HAN_UPDATE_PUBKEY` (see docs/release.md for generating the pair) and the check refuses to run with the
/// placeholder.
const UPDATE_PUBLIC_KEY: &str = match option_env!("HAN_UPDATE_PUBKEY") {
    Some(key) => key,
    None => PLACEHOLDER_KEY,
};
const PLACEHOLDER_KEY: &str = "TODO-placeholder-release-public-key";

/// The signature covers this prefix followed by the manifest's exact bytes, so a manifest signature cannot be
/// replayed as any other signature made with the same key.
const MANIFEST_DOMAIN: &[u8] = b"han-release-manifest/1\n";

/// Options of `han update`.
#[derive(Args)]
pub struct UpdateArgs {
    /// Check whether a newer release exists; the only mode there is (nothing is ever installed).
    #[arg(long)]
    check: bool,
    /// The release manifest (`latest.json`) as a file; download it from the release first.
    #[arg(long)]
    manifest: PathBuf,
    /// The detached signature (base64url); defaults to the manifest path plus `.sig`.
    #[arg(long)]
    sig: Option<PathBuf>,
}

/// What a check found.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    UpToDate,
    Available {
        version: String,
        notes: Option<String>,
    },
}

pub async fn run(args: UpdateArgs) -> Result<(), Box<dyn std::error::Error>> {
    if !args.check {
        return Err(
            "han update only supports --check: it never installs without your explicit action"
                .into(),
        );
    }
    if UPDATE_PUBLIC_KEY == PLACEHOLDER_KEY {
        return Err("this build has no release public key embedded (HAN_UPDATE_PUBKEY), so it cannot verify a manifest".into());
    }
    let key = PublicKey::from_base64url(UPDATE_PUBLIC_KEY)
        .map_err(|e| format!("the embedded release public key is invalid: {e}"))?;
    let manifest =
        std::fs::read(&args.manifest).map_err(|e| format!("{}: {e}", args.manifest.display()))?;
    let sig_path = args.sig.unwrap_or_else(|| {
        let mut p = args.manifest.clone().into_os_string();
        p.push(".sig");
        p.into()
    });
    let sig =
        std::fs::read_to_string(&sig_path).map_err(|e| format!("{}: {e}", sig_path.display()))?;
    match check(&key, &manifest, sig.trim(), env!("CARGO_PKG_VERSION"))? {
        Outcome::UpToDate => println!("han {} is up to date", env!("CARGO_PKG_VERSION")),
        Outcome::Available { version, notes } => {
            println!(
                "han {version} is available (this is {})",
                env!("CARGO_PKG_VERSION")
            );
            if let Some(url) = notes {
                println!("release notes: {url}");
            }
            println!("nothing was installed; upgrade with the installer you used");
        }
    }
    Ok(())
}

/// Verifies `sig` over `manifest` with `key`, then compares versions. Verification comes before parsing.
fn check(key: &PublicKey, manifest: &[u8], sig: &str, current: &str) -> Result<Outcome, String> {
    let message = [MANIFEST_DOMAIN, manifest].concat();
    key.verify(&message, sig)
        .map_err(|_| "the manifest signature does not verify; refusing to trust it".to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_slice(manifest).map_err(|e| format!("manifest: {e}"))?;
    let version = parsed["version"]
        .as_str()
        .ok_or("manifest has no string \"version\"")?
        .to_string();
    let notes = parsed["notes_url"].as_str().map(str::to_string);
    let latest = parse_version(&version)?;
    let now = parse_version(current)?;
    Ok(if latest > now {
        Outcome::Available { version, notes }
    } else {
        Outcome::UpToDate
    })
}

/// `MAJOR.MINOR.PATCH`; pre-release and build suffixes are refused (releases are plain 0.x.y).
fn parse_version(s: &str) -> Result<(u64, u64, u64), String> {
    let bad = || format!("{s:?} is not a MAJOR.MINOR.PATCH version");
    let mut parts = s.split('.').map(|p| p.parse::<u64>().map_err(|_| bad()));
    let v = (
        parts.next().ok_or_else(bad)??,
        parts.next().ok_or_else(bad)??,
        parts.next().ok_or_else(bad)??,
    );
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use han_core::SigningKey;

    fn signed(key: &SigningKey, manifest: &str) -> String {
        key.sign(&[MANIFEST_DOMAIN, manifest.as_bytes()].concat())
    }

    #[test]
    fn verifies_and_compares() {
        let key = SigningKey::from_seed([7; 32]);
        let m = r#"{"version":"0.2.0","notes_url":"https://example.invalid/n"}"#;
        let out = check(&key.public_key(), m.as_bytes(), &signed(&key, m), "0.1.0").unwrap();
        assert!(matches!(out, Outcome::Available { .. }));
        let out = check(&key.public_key(), m.as_bytes(), &signed(&key, m), "0.10.0").unwrap();
        assert_eq!(out, Outcome::UpToDate);
    }

    #[test]
    fn refuses_tampered_or_foreign_signatures() {
        let key = SigningKey::from_seed([7; 32]);
        let other = SigningKey::from_seed([8; 32]);
        let m = r#"{"version":"9.0.0"}"#;
        let sig = signed(&key, m);
        let tampered = r#"{"version":"9.0.1"}"#;
        assert!(check(&key.public_key(), tampered.as_bytes(), &sig, "0.1.0").is_err());
        assert!(check(&other.public_key(), m.as_bytes(), &sig, "0.1.0").is_err());
        // A signature over the bare manifest (no domain prefix) is not a manifest signature.
        let bare = key.sign(m.as_bytes());
        assert!(check(&key.public_key(), m.as_bytes(), &bare, "0.1.0").is_err());
    }

    #[test]
    fn the_placeholder_is_not_a_key() {
        assert!(PublicKey::from_base64url(PLACEHOLDER_KEY).is_err());
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.1.2"), Ok((0, 1, 2)));
        assert!(parse_version("0.1").is_err());
        assert!(parse_version("0.1.2-rc1").is_err());
    }
}
