//! `han host` hands its own executable to the sandbox as the in-sandbox shim (`ShimCommand`: `han`, with
//! `shim` in front), and the sandbox runs it: `han shim listen … -- <agent>` and `han shim mcp-stub …`. The
//! first real Claude session died in `initialize` because the verbs were wrong and the agent never started.
//! These run the real binary the way the sandboxes do.

use han_proc::NoWindow as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use han_host::sandbox::{self, Limits, Preference, SandboxSpec, ShimCommand};
use han_node::api::SandboxBackend;
use tokio::io::AsyncReadExt;

const HAN: &str = env!("CARGO_BIN_EXE_han");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn han_shim_takes_the_listen_verb_and_runs_the_agent_with_its_stdio() {
    let port = free_port();
    let out = Command::new(HAN)
        .no_window()
        .args([
            "shim",
            "listen",
            "--port",
            &port.to_string(),
            "--tcp",
            "127.0.0.1:9",
            "--",
            HAN,
            "--version",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.status.success(),
        "exit {:?}: {stderr}",
        out.status.code()
    );
    assert!(!stderr.contains("unrecognized subcommand"), "{stderr}");
    assert!(
        stdout.starts_with("han "),
        "the agent's stdout comes through: {stdout}"
    );
}

#[test]
fn han_shim_takes_the_mcp_stub_verb() {
    // No proxy given: the shim's own usage, not clap's.
    let out = Command::new(HAN)
        .no_window()
        .args(["shim", "mcp-stub"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("unrecognized subcommand"), "{stderr}");
    assert!(stderr.contains("han shim mcp-stub"), "{stderr}");
}

/// The real launch: the OS sandbox runs `han` as its shim, which starts the agent inside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn han_as_the_shim_starts_an_agent_inside_the_os_sandbox() {
    let native = if cfg!(windows) {
        SandboxBackend::Mxc
    } else {
        SandboxBackend::Bwrap
    };
    match sandbox::probe(&sandbox::ShimCommand::new(HAN, ["shim"]))
        .into_iter()
        .find(|b| b.backend == native)
    {
        Some(b) if b.available => {}
        Some(b) => return eprintln!("SKIP: {native:?} unavailable here: {}", b.detail),
        None => return eprintln!("SKIP: {native:?} does not exist on this OS"),
    }
    let root = tempfile::Builder::new()
        .prefix("han-shim-")
        .tempdir()
        .unwrap();
    let (wt, temp) = (root.path().join("wt"), root.path().join("temp"));
    std::fs::create_dir_all(&wt).unwrap();
    let han = PathBuf::from(HAN);
    let ro = if cfg!(windows) {
        vec![han.parent().unwrap().to_path_buf()]
    } else {
        vec![han.clone()]
    };
    let spec = SandboxSpec {
        session: "shim-verbs".into(),
        backend: if cfg!(windows) {
            Preference::Mxc
        } else {
            Preference::Bwrap
        },
        program: han.clone(),
        args: vec!["--version".into()],
        cwd: wt.clone(),
        env: vec![],
        rw: vec![wt],
        ro,
        temp,
        proxy: None,
        volatile_dirs: Vec::new(),
        allow_advisory: false,
        limits: Limits {
            wall_time: Some(Duration::from_secs(120)),
            ..Limits::default()
        },
        shim: ShimCommand::new(han, ["shim"]),
        inside_port: None,
    };
    let mut sb = sandbox::launch(spec)
        .await
        .unwrap_or_else(|e| panic!("launch: {e}"));
    drop(sb.stdin.take());
    let (mut out, mut err) = (sb.stdout.take().unwrap(), sb.stderr.take().unwrap());
    let (mut o, mut e) = (String::new(), String::new());
    let read = async {
        let _ = tokio::join!(out.read_to_string(&mut o), err.read_to_string(&mut e));
    };
    tokio::time::timeout(Duration::from_secs(150), read)
        .await
        .expect("the sandboxed agent finished");
    let code = sb.wait().await;
    assert!(!e.contains("unrecognized subcommand"), "{e}");
    assert_eq!(code, Some(0), "stderr: {e}");
    assert!(o.starts_with("han "), "stdout: {o} stderr: {e}");
}
