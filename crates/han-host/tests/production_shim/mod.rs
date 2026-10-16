//! The shim the production host hands a sandbox: `han shim`, with `han` the real binary. Tests that launch an
//! agent under a sandbox use it and no stand-in, so the argv convention they exercise is the shipped one.
//!
//! `han` is not a dependency of this crate (it depends on this one), so the helper builds it with the cargo
//! that runs the tests. The build asks for what `cargo test --workspace` builds: the `test` profile and every
//! test target and binary of the workspace, so cargo resolves the same features and finds every unit, `han`
//! included, up to date. (`cargo build -p han --bin han` is a different graph: another profile, and none of the
//! dev-dependency features, so it recompiled `han` and its whole dependency chain next to the test build.)
//!
//! The variables `cargo test` sets for the test (`CARGO_MANIFEST_DIR`, `CARGO_PKG_*`, `CARGO_BIN_EXE_*`, ...) are
//! taken out of the build's environment. Build scripts such as `ring`'s track `CARGO_MANIFEST_DIR`, so with the
//! test's value leaked in, cargo took `ring` for changed and recompiled everything above it, which is most of the
//! tree.
#![allow(dead_code)]

use han_proc::NoWindow as _;
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
        build.no_window();
        build
            .args(["build", "--workspace", "--tests", "--bins", "--quiet"])
            .current_dir(env!("CARGO_MANIFEST_DIR"));

        // Cargo gives a test its crate's variables (CARGO_PKG_NAME, CARGO_MANIFEST_DIR, ...). Dependencies' build
        // scripts (ring's) watch them, so building with them set makes the next ordinary build in this target dir
        // redo those scripts and everything above them.
        for (name, _) in std::env::vars() {
            if name.starts_with("CARGO_PKG_")
                || name.starts_with("CARGO_BIN_EXE_")
                || matches!(
                    name.as_str(),
                    "CARGO_MANIFEST_DIR"
                        | "CARGO_MANIFEST_PATH"
                        | "CARGO_CRATE_NAME"
                        | "CARGO_PRIMARY_PACKAGE"
                        | "CARGO_BIN_NAME"
                )
            {
                build.env_remove(name);
            }
        }
        if dir.file_name().is_some_and(|n| n == "release") {
            build.arg("--release");
        } else {
            build.args(["--profile", "test"]);
        }
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            let set_for_the_test = name == "CARGO_MANIFEST_DIR"
                || name == "CARGO_CRATE_NAME"
                || name == "CARGO_PRIMARY_PACKAGE"
                || name == "CARGO_TARGET_TMPDIR"
                || name.starts_with("CARGO_PKG_")
                || name.starts_with("CARGO_BIN_");
            if set_for_the_test {
                build.env_remove(&*name);
            }
        }
        let status = build.status().expect("cargo runs");
        assert!(status.success(), "cargo build of the workspace failed");
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
