//! The shim the production host hands a sandbox: `han shim`, with `han` the real binary. Tests that launch an
//! agent under a sandbox use it and no stand-in, so the argv convention they exercise is the shipped one.
//!
//! `han` is not a dependency of this crate (it depends on this one), so the helper builds it with the cargo
//! that runs the tests. That is a no-op when `cargo test --workspace` built it already.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use han_host::sandbox::ShimCommand;

/// The `han` binary of this build profile, built if it is not up to date.
pub fn han() -> PathBuf {
    static HAN: OnceLock<PathBuf> = OnceLock::new();
    HAN.get_or_init(|| {
        let exe = std::env::current_exe().expect("the test's own path");
        // target/<profile>/deps/<test> -> target/<profile>
        let dir = exe
            .parent()
            .and_then(|p| p.parent())
            .expect("the test sits in target/<profile>/deps")
            .to_path_buf();
        let mut build = std::process::Command::new(env!("CARGO"));
        build
            .args(["build", "-p", "han", "--bin", "han", "--quiet"])
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        if dir.file_name().is_some_and(|n| n == "release") {
            build.arg("--release");
        }
        let status = build.status().expect("cargo runs");
        assert!(status.success(), "cargo build -p han failed");
        let han = dir.join(format!("han{}", std::env::consts::EXE_SUFFIX));
        assert!(han.is_file(), "{} is not there", han.display());
        han
    })
    .clone()
}

/// `han shim`.
pub fn shim() -> ShimCommand {
    ShimCommand::new(han(), ["shim"])
}
