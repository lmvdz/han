//! The Rust toolchain inside the sandbox (PLAN §6.4 "Toolchains"): `cargo --version`, `cargo build` and
//! `cargo test` of a tiny crate work under each available backend; with the toolchain off `cargo` is absent;
//! crates.io is reachable only when it is on, and any other host stays denied. A backend that is missing, or
//! a machine without a Rust toolchain, skips with a message; the network test also needs crates.io.

use std::path::PathBuf;
use std::time::Duration;

use han_host::proxy::{AllowEntry, Proxy, ProxyConfig, ProxyHandle};
use han_host::sandbox::{self, Limits, Preference, ProxyTarget, SandboxSpec};
use han_host::toolchain::{self, ToolchainConfig};
use han_node::api::SandboxBackend;
use tokio::io::AsyncReadExt;

const TOKEN: &str = "tok-TOOLS-AAAAAAAAAAAAAAAAAAA";

fn shim_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_han-agent-shim"))
}

fn available(b: SandboxBackend) -> bool {
    let found = sandbox::probe().into_iter().find(|x| x.backend == b);
    match found {
        Some(x) if x.available => true,
        Some(x) => {
            eprintln!("SKIP: {b:?} unavailable here: {}", x.detail);
            false
        }
        None => {
            eprintln!("SKIP: {b:?} does not exist on this OS");
            false
        }
    }
}

fn config(enabled: bool) -> ToolchainConfig {
    ToolchainConfig {
        enabled: if enabled { vec!["rust".into()] } else { vec![] },
        ..Default::default()
    }
}

/// Whether this machine has a Rust toolchain to share (else the tests skip).
fn has_rust() -> bool {
    let probe = tempfile::tempdir().unwrap();
    match toolchain::plan(&config(true), probe.path(), probe.path()) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIP: no Rust toolchain to share: {e}");
            false
        }
    }
}

/// The proxy records a tunnel when it closes, which can be a moment after the sandbox's process has exited.
async fn settled(p: &ProxyHandle, host: &str) -> Vec<han_host::proxy::Decision> {
    for _ in 0..50 {
        if p.decisions().iter().any(|x| x.host == host) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    p.decisions()
}

async fn proxy(cfg: &ToolchainConfig) -> ProxyHandle {
    let mut pc = ProxyConfig::new("tools", TOKEN);
    pc.allow = toolchain::allow_hosts(cfg)
        .into_iter()
        .map(AllowEntry::https)
        .collect();
    Proxy::start(pc).await.unwrap()
}

struct Run {
    code: Option<i32>,
    out: String,
    /// What the host's prefetch (MXC only) reported.
    gaps: Vec<String>,
}

/// Runs `script` in the shell of the OS, inside `backend`, in a fresh session directory holding a worktree
/// with `files`.
async fn run(
    backend: Preference,
    tools: &ToolchainConfig,
    proxy: &ProxyHandle,
    files: &[(&str, &str)],
    script: &str,
) -> Run {
    let root = tempfile::Builder::new()
        .prefix("han-tools-")
        .tempdir()
        .unwrap();
    // The session directory, as the host lays it out: `wt`, `agent`, `tmp`, and the toolchain's own.
    let session = root.path().join("session");
    let wt = session.join("wt");
    let temp = session.join("tmp");
    for d in [&wt, &temp, &session.join("agent")] {
        std::fs::create_dir_all(d).unwrap();
    }
    for (name, text) in files {
        let p = wt.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let plan = toolchain::plan(tools, &session, &wt).unwrap_or_else(|e| panic!("plan: {e}"));
    let mut env = vec![
        ("ANTHROPIC_API_KEY".to_owned(), "han-dummy-key".to_owned()),
        ("CARGO_NET_RETRY".to_owned(), "1".to_owned()),
    ];
    toolchain::apply(&plan, &mut env);
    // As the host does under MXC, which cannot unpack the crates a build downloads.
    let gaps = if backend == Preference::Mxc {
        toolchain::prewarm(&plan, proxy.addr(), TOKEN).await
    } else {
        Vec::new()
    };
    let (program, args) = if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        (
            PathBuf::from(format!("{root}\\System32\\cmd.exe")),
            vec!["/c".to_owned(), script.to_owned()],
        )
    } else {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_owned(), script.to_owned()],
        )
    };
    let mut rw = vec![wt.clone(), session.join("agent")];
    rw.extend(plan.rw.iter().cloned());
    let spec = SandboxSpec {
        session: "tools".into(),
        backend,
        program,
        args,
        cwd: wt.clone(),
        env,
        rw,
        ro: plan.ro.clone(),
        temp,
        proxy: Some(ProxyTarget {
            addr: proxy.addr(),
            token: TOKEN.into(),
        }),
        allow_advisory: false,
        limits: Limits {
            wall_time: Some(Duration::from_secs(280)),
            ..Limits::default()
        },
        shim: Some(shim_exe()),
        inside_port: None,
    };
    let mut sb = sandbox::launch(spec)
        .await
        .unwrap_or_else(|e| panic!("launch: {e}"));
    let mut out = sb.stdout.take().unwrap();
    let mut err = sb.stderr.take().unwrap();
    drop(sb.stdin.take());
    let (mut text, mut etext) = (String::new(), String::new());
    let read = async {
        let _ = out.read_to_string(&mut text).await;
        let _ = err.read_to_string(&mut etext).await;
    };
    tokio::time::timeout(Duration::from_secs(300), read)
        .await
        .expect("the script finished");
    let code = sb.wait().await;
    text.push_str(&etext);
    eprintln!("{backend:?} exit {code:?}\n{text}");
    Run {
        code,
        out: text,
        gaps,
    }
}

const LOCK: &str = "version = 4

[[package]]
name = \"itoa\"
version = \"1.0.18\"
source = \"registry+https://github.com/rust-lang/crates.io-index\"
checksum = \"8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682\"

[[package]]
name = \"tiny\"
version = \"0.1.0\"
dependencies = [
 \"itoa\",
]
";

const CRATE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    ),
    (
        "src/lib.rs",
        "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod t { #[test] fn adds() { assert_eq!(super::two() + 2, 4); } }\n",
    ),
];

async fn builds_and_tests(backend: Preference, b: SandboxBackend) {
    if !available(b) || !has_rust() {
        return;
    }
    let tools = config(true);
    let p = proxy(&tools).await;
    let script = "cargo --version && cargo build --offline && cargo test --offline";
    let r = run(backend, &tools, &p, CRATE, script).await;
    assert_eq!(r.code, Some(0), "{}", r.out);
    assert!(r.out.contains("cargo 1."), "{}", r.out);
    assert!(r.out.contains("test result: ok. 1 passed"), "{}", r.out);
}

async fn absent_when_off(backend: Preference, b: SandboxBackend) {
    if !available(b) || !has_rust() {
        return;
    }
    let off = config(false);
    let p = proxy(&off).await;
    let probe = tempfile::tempdir().unwrap();
    let on = toolchain::plan(&config(true), probe.path(), probe.path()).unwrap();
    let script = if cfg!(windows) {
        "where cargo"
    } else {
        "command -v cargo"
    };
    let r = run(backend, &off, &p, &[], script).await;
    let found_in_toolchain = on
        .path
        .iter()
        .any(|d| r.out.contains(&*d.to_string_lossy()));
    assert!(
        !found_in_toolchain,
        "cargo must not be on the sandbox's PATH: {}",
        r.out
    );
    // And the toolchain's roots are not readable either.
    let root = on.ro[0].to_string_lossy().into_owned();
    let list = if cfg!(windows) {
        format!("dir \"{root}\"")
    } else {
        format!("ls \"{root}\"")
    };
    let r = run(backend, &off, &p, &[], &list).await;
    assert_ne!(r.code, Some(0), "the toolchain root is denied: {}", r.out);
}

async fn crates_io_only_when_on(backend: Preference, b: SandboxBackend) {
    if !available(b) || !has_rust() {
        return;
    }
    let tools = config(true);
    let p = proxy(&tools).await;
    // A registry dependency resolves through crates.io (index, then the download host).
    let with_dep: Vec<(&str, &str)> = vec![
        (
            "Cargo.toml",
            "[package]
name = \"tiny\"
version = \"0.1.0\"
edition = \"2021\"

[workspace]

[dependencies]
itoa = \"1\"
",
        ),
        (
            "src/lib.rs",
            "pub fn f() -> u32 { 1 }
",
        ),
        ("Cargo.lock", LOCK),
    ];
    let r = run(backend, &tools, &p, &with_dep, "cargo build --locked").await;
    if r.code != Some(0) && std::net::TcpStream::connect(("index.crates.io", 443)).is_err() {
        eprintln!("SKIP: crates.io is not reachable from this machine");
        return;
    }
    assert!(r.gaps.is_empty(), "{:?}", r.gaps);
    assert_eq!(r.code, Some(0), "{}", r.out);
    let d = settled(&p, "static.crates.io").await;
    for h in ["index.crates.io", "static.crates.io"] {
        assert!(
            d.iter().any(|x| x.host == h && x.allowed),
            "{h} must be reached: {d:?}"
        );
    }
    // Anything else is denied, and the fetch says so.
    // A second registry on another host, named by the repository's own cargo config.
    let other: Vec<(&str, &str)> = vec![
        (
            "Cargo.toml",
            "[package]
name = \"tiny\"
version = \"0.1.0\"
edition = \"2021\"

[workspace]

[dependencies]
x = { version = \"1\", registry = \"other\" }
",
        ),
        (
            ".cargo/config.toml",
            "[registries.other]
index = \"sparse+https://example.com/index/\"
",
        ),
        (
            "src/lib.rs",
            "pub fn f() {}
",
        ),
    ];
    let r = run(backend, &tools, &p, &other, "cargo fetch").await;
    assert_ne!(r.code, Some(0), "{}", r.out);
    let d = settled(&p, "example.com").await;
    assert!(
        d.iter()
            .any(|x| x.host == "example.com" && !x.allowed && x.reason == "not-on-allow-list"),
        "{d:?}"
    );
    assert!(
        d.iter().all(|x| x.allowed
            == ["crates.io", "index.crates.io", "static.crates.io"].contains(&x.host.as_str())),
        "only the crates.io hosts are allowed: {d:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mxc_cargo_builds_and_tests() {
    builds_and_tests(Preference::Mxc, SandboxBackend::Mxc).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mxc_no_cargo_when_off() {
    absent_when_off(Preference::Mxc, SandboxBackend::Mxc).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mxc_crates_io_only_when_on() {
    crates_io_only_when_on(Preference::Mxc, SandboxBackend::Mxc).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bwrap_cargo_builds_and_tests() {
    builds_and_tests(Preference::Bwrap, SandboxBackend::Bwrap).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bwrap_no_cargo_when_off() {
    absent_when_off(Preference::Bwrap, SandboxBackend::Bwrap).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bwrap_crates_io_only_when_on() {
    crates_io_only_when_on(Preference::Bwrap, SandboxBackend::Bwrap).await;
}
