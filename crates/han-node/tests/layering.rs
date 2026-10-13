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
fn face_sources_use_only_the_api_and_the_rpc_client() {
    let faces = [
        root().join("apps/han/src/board.rs"),
        root().join("apps/han/src/agent.rs"),
    ];
    for file in faces {
        let text =
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for (i, line) in text.lines().enumerate() {
            let mut rest = line;
            while let Some(at) = rest.find("han_node::") {
                let after = &rest[at + "han_node::".len()..];
                let ok = ["api", "rpc::", "NodeApi", "NodeError", "ViewStream"]
                    .iter()
                    .any(|p| after.starts_with(p));
                assert!(
                    ok,
                    "{}:{}: a face may use only han_node::api and the rpc client: {line}",
                    file.display(),
                    i + 1
                );
                rest = after;
            }
            for banned in ["han_core::", "han_net::", "han_seq::"] {
                assert!(
                    !line.contains(banned),
                    "{}:{}: a face must not use {banned}",
                    file.display(),
                    i + 1
                );
            }
        }
    }
}
