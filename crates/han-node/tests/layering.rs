//! The dependency rules of PLAN section 4, enforced:
//!
//! * han-core depends on nothing async or I/O.
//! * han-net, han-seq, han-timeline and han-node depend inward only (none depends on han-node, and the engines
//!   do not depend on each other upward).
//! * Faces depend only on han-node's API types: with `default-features = false` the crate has no han-core,
//!   han-net, han-seq, storage or keys, and the face sources use nothing of han-node but `api` and `rpc`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The names of the normal (non-dev, non-build) dependencies of a package, as `cargo tree` prints them.
fn tree(args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .current_dir(root())
        .args(["tree", "--offline", "-e", "normal", "--prefix", "none"])
        .args(args)
        .output()
        .expect("cargo tree runs");
    assert!(
        out.status.success(),
        "cargo tree {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

fn assert_absent(pkg: &str, deps: &[String], banned: &[&str]) {
    for b in banned {
        assert!(
            !deps.iter().any(|d| d == b),
            "{pkg} must not depend on {b} (PLAN section 4)"
        );
    }
}

const ASYNC_AND_IO: &[&str] = &[
    "tokio",
    "iroh",
    "rusqlite",
    "reqwest",
    "hyper",
    "interprocess",
    "keyring",
    "mio",
    "socket2",
];

#[test]
fn han_core_is_pure() {
    let deps = tree(&["-p", "han-core"]);
    assert_absent("han-core", &deps, ASYNC_AND_IO);
    assert_absent(
        "han-core",
        &deps,
        &["han-net", "han-seq", "han-node", "han-timeline"],
    );
}

#[test]
fn engines_depend_inward_only() {
    let timeline = tree(&["-p", "han-timeline"]);
    assert_absent("han-timeline", &timeline, ASYNC_AND_IO);
    assert_absent(
        "han-timeline",
        &timeline,
        &["han-net", "han-seq", "han-node"],
    );
    let seq = tree(&["-p", "han-seq"]);
    assert_absent(
        "han-seq",
        &seq,
        &["iroh", "han-net", "han-node", "interprocess", "keyring"],
    );
    let net = tree(&["-p", "han-net"]);
    assert_absent("han-net", &net, &["han-node", "interprocess", "keyring"]);
}

#[test]
fn a_face_gets_only_the_api_types() {
    let api = tree(&["-p", "han-node", "--no-default-features"]);
    assert_absent(
        "han-node without the runtime",
        &api,
        &[
            "han-core",
            "han-net",
            "han-seq",
            "han-timeline",
            "iroh",
            "rusqlite",
            "keyring",
            "ed25519-dalek",
        ],
    );
    assert!(api.iter().any(|d| d == "han-node"));
}

#[test]
fn the_agent_tools_get_only_the_api_types() {
    // PLAN 4: han-mcp, like a face, depends only on han-node's API types (SR-29: no runtime, no keys).
    let mcp = tree(&["-p", "han-mcp"]);
    assert_absent(
        "han-mcp",
        &mcp,
        &["han-net", "han-seq", "iroh", "keyring", "rusqlite"],
    );
}

#[test]
fn the_terminal_face_gets_only_the_api_types() {
    // PLAN 3: the TUI attaches to the daemon over the local pipe; it never links the runtime.
    let tui = tree(&["-p", "han-tui"]);
    assert_absent(
        "han-tui",
        &tui,
        &[
            "han-core",
            "han-net",
            "han-seq",
            "han-timeline",
            "iroh",
            "rusqlite",
            "keyring",
            "ed25519-dalek",
        ],
    );
    assert!(tui.iter().any(|d| d == "han-node"));
}

#[test]
fn han_github_holds_no_write_key_material() {
    // PLAN 6.7, SR-49: only han-exec holds write-capable key material. han-github parses untrusted issue text and
    // has its own read-only App, so it must not link the executor (or an RSA implementation to sign with).
    let gh = tree(&["-p", "han-github"]);
    assert_absent("han-github", &gh, &["han-exec", "rsa"]);
    assert!(gh.iter().any(|d| d == "han-node"));
}

#[test]
fn the_command_line_faces_get_only_the_api_types() {
    // PLAN 3: `han board`, `space`, `task`, `device` and `outbox` are a crate of their own, so the rule is a
    // fact of its dependency tree and not of a search of its sources.
    let faces = tree(&["-p", "han-faces"]);
    assert_absent(
        "han-faces",
        &faces,
        &[
            "han-core",
            "han-net",
            "han-seq",
            "han-timeline",
            "han-host",
            "iroh",
            "rusqlite",
            "keyring",
            "ed25519-dalek",
        ],
    );
    assert!(faces.iter().any(|d| d == "han-node"));
}

#[test]
fn the_registry_types_carry_no_runtime() {
    // han-registry is what a node and the processes it launches agree on; a face may depend on it.
    let reg = tree(&["-p", "han-registry"]);
    assert_absent("han-registry", &reg, ASYNC_AND_IO);
    assert_absent(
        "han-registry",
        &reg,
        &["han-core", "han-net", "han-seq", "han-node", "han-host"],
    );
}
