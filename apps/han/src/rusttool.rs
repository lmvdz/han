//! The Rust toolchain of a runner: where the shared copy lives and which channels a repository pins. Shared by
//! `han doctor` (is the pin installed?) and `han runner setup --toolchain rust` (install it).

use std::path::{Path, PathBuf};

/// The names of the toolchains installed under a `RUSTUP_HOME` (`1.97.1-x86_64-unknown-linux-gnu`).
pub fn installed(rustup_home: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(rustup_home.join("toolchains"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// `1.97.1` matches `1.97.1-x86_64-unknown-linux-gnu`; a full name matches itself.
pub fn channel_installed(channel: &str, installed: &[String]) -> bool {
    installed
        .iter()
        .any(|n| n == channel || n.strip_prefix(channel).is_some_and(|r| r.starts_with('-')))
}

/// The channel each repository pins (`rust-toolchain.toml`), as `(repository, channel)`; a repository with no
/// pin is left out.
pub fn pins(repos: &[PathBuf]) -> Vec<(PathBuf, String)> {
    repos
        .iter()
        .filter_map(|r| han_host::toolchain::pinned_channel(r).map(|c| (r.clone(), c)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_matches_its_installed_name() {
        let have = vec!["1.97.1-x86_64-unknown-linux-gnu".to_owned()];
        assert!(channel_installed("1.97.1", &have));
        assert!(channel_installed("1.97.1-x86_64-unknown-linux-gnu", &have));
        assert!(!channel_installed("1.97", &have));
        assert!(!channel_installed("1.98.0", &have));
    }

    #[test]
    fn pins_come_from_the_repositories_that_have_one() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(
            a.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.90.0\"\n",
        )
        .unwrap();
        assert_eq!(pins(&[a.clone(), b]), vec![(a, "1.90.0".to_owned())]);
    }
}
