//! `han host` hands its own executable to the sandbox as the in-sandbox shim, and the sandbox runs it the way
//! it runs `han-agent-shim`: `<shim> listen … -- <agent>` and `<shim> mcp-stub …`. The first real Claude session
//! died in `initialize` because `han` answered those with clap's "unrecognized subcommand" (exit 2) and the
//! agent never started. These run the real binary the way the sandboxes do.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use han_host::sandbox::{self, Limits, Preference, SandboxSpec};
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
fn han_takes_the_shims_listen_verb_and_runs_the_agent_with_its_stdio() {
    let port = free_port();
    let out = Command::new(HAN)
        .args([
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
fn han_takes_the_shims_mcp_stub_verb() {
    // No proxy given: the shim's own usage, not clap's.
    let out = Command::new(HAN).arg("mcp-stub").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("unrecognized subcommand"), "{stderr}");
    assert!(stderr.contains("han-agent-shim mcp-stub"), "{stderr}");
    // `han shim …` still works.
    let out = Command::new(HAN)
        .args(["shim", "mcp-stub"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("han-agent-shim mcp-stub"));
}

/// The real launch: the OS sandbox runs `han` as its shim, which starts the agent inside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn han_as_the_shim_starts_an_agent_inside_the_os_sandbox() {
    let native = if cfg!(windows) {
        SandboxBackend::Mxc
    } else {
        SandboxBackend::Bwrap
    };
    match sandbox::probe().into_iter().find(|b| b.backend == native) {
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
        allow_advisory: false,
        limits: Limits {
            wall_time: Some(Duration::from_secs(120)),
            ..Limits::default()
        },
        shim: Some(han),
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
