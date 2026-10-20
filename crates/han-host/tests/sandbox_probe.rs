//! The sandbox probe (SR-23…SR-33): a probe program launched in each available backend tries every forbidden
//! operation and the one allowed route (the shim to the session proxy). A backend's test passes only when
//! every forbidden operation fails and the proxy route works. Backends this machine lacks are skipped with a
//! message; a backend that is available is always tested.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_host::proxy::{Proxy, ProxyConfig, ProxyHandle};
use han_host::sandbox::{
    self, Limits, Preference, ProxyTarget, SandboxError, SandboxSpec, ShimCommand,
};
use han_node::api::{Fidelity, SandboxBackend};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod production_shim;

const TOKEN: &str = "tok-PROBE-AAAAAAAAAAAAAAAAAAAA";
const CANARY: &str = "han-canary-secret-not-real-41d7";

fn probe_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_han-sandbox-probe"))
}

fn shim_exe() -> ShimCommand {
    production_shim::shim()
}

fn available(b: SandboxBackend) -> bool {
    let found = sandbox::probe(&shim_exe())
        .into_iter()
        .find(|x| x.backend == b);
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

/// Why no OS sandbox is usable here, for a skip or note line.
fn why_none() -> String {
    sandbox::probe(&shim_exe())
        .into_iter()
        .filter(|b| b.backend != SandboxBackend::None)
        .map(|b| format!("{:?}: {}", b.backend, b.detail))
        .collect::<Vec<_>>()
        .join("; ")
}

async fn session_proxy(token: &str) -> ProxyHandle {
    let mut cfg = ProxyConfig::new("probe", token);
    cfg.mcp = Some(Arc::new(|mut s: tokio::net::TcpStream| {
        tokio::spawn(async move {
            let _ = s.write_all(b"pong").await;
        });
    }));
    Proxy::start(cfg).await.unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    wt: PathBuf,
    temp: PathBuf,
}

fn fixture() -> Fixture {
    let root_dir = tempfile::Builder::new()
        .prefix("han-probe-")
        .tempdir()
        .unwrap();
    let root = root_dir.path().to_path_buf();
    for d in ["wt", "sib", "data", "outside", "main/.git/hooks", "temp"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("wt/hello.txt"), "hi").unwrap();
    std::fs::write(root.join("sib/secret.txt"), CANARY).unwrap();
    std::fs::write(root.join("data/node.db"), CANARY).unwrap();
    std::fs::write(root.join("outside/.env"), format!("KEY={CANARY}")).unwrap();
    std::fs::write(root.join("main/.git/config"), "[core]\n").unwrap();
    Fixture {
        wt: root.join("wt"),
        temp: root.join("temp"),
        _root: root_dir,
        root,
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Paths a sandboxed session must not write: outside writes and their host locations.
fn forbidden_writes(f: &Fixture) -> Vec<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default();
    let tag = format!("han-probe-{}.txt", std::process::id());
    vec![
        home.join(&tag),
        f.root.join("sib").join("written.txt"),
        f.root.join("data").join("written.txt"),
        std::env::temp_dir().join(&tag),
        f.root.join("main/.git/config"),
        f.root.join("main/.git/hooks/pre-push"),
        f.root.join("outside").join("written.txt"),
    ]
}

/// Runs the probe in `backend` and returns its report and the sandbox's info.
async fn run_probe(
    backend: Preference,
    f: &Fixture,
    proxy: &ProxyHandle,
    extra: Value,
) -> (Vec<Value>, sandbox::SandboxInfo) {
    let mut spec_json = json!({
        "write": forbidden_writes(f).iter().map(|p| s(p)).chain([s(&f.wt.join("ok.txt")), s(&f.temp.join("ok.txt"))]).collect::<Vec<_>>(),
        "read": [s(&f.root.join("sib/secret.txt")), s(&f.root.join("data/node.db")), s(&f.root.join("outside/.env")), s(&f.wt.join("hello.txt"))],
        "canary": CANARY,
        "env": true,
        "proxy": true,
        "child": true,
    });
    if let (Value::Object(m), Value::Object(e)) = (&mut spec_json, extra) {
        m.extend(e);
    }
    let program = probe_exe();
    let mut ro = vec![];
    if cfg!(windows) {
        ro.push(program.parent().unwrap().to_path_buf());
    } else {
        ro.push(program.clone());
    }
    let spec = SandboxSpec {
        session: "probe".into(),
        backend,
        program,
        args: vec![],
        cwd: f.wt.clone(),
        env: vec![
            ("HAN_PROBE_SPEC".into(), spec_json.to_string()),
            ("ANTHROPIC_API_KEY".into(), "han-dummy-key".into()),
            ("ANTHROPIC_BASE_URL".into(), "{proxy_url}".into()),
        ],
        rw: vec![f.wt.clone()],
        ro,
        temp: f.temp.clone(),
        proxy: Some(ProxyTarget {
            addr: proxy.addr(),
            token: TOKEN.into(),
        }),
        volatile_dirs: Vec::new(),
        allow_advisory: true,
        limits: Limits {
            wall_time: Some(Duration::from_secs(120)),
            ..Limits::default()
        },
        shim: shim_exe(),
        inside_port: None,
    };
    let mut sb = sandbox::launch(spec)
        .await
        .unwrap_or_else(|e| panic!("launch: {e}"));
    let info = sb.info.clone();
    let mut out = sb.stdout.take().unwrap();
    let mut err = sb.stderr.take().unwrap();
    drop(sb.stdin.take());
    let mut text = String::new();
    let mut etext = String::new();
    let read = async {
        let _ = out.read_to_string(&mut text).await;
        let _ = err.read_to_string(&mut etext).await;
    };
    tokio::time::timeout(Duration::from_secs(150), read)
        .await
        .expect("the probe finished");
    let _ = sb.wait().await;
    let report: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        !report.is_empty(),
        "the probe reported nothing; stdout: {text}\nstderr: {etext}"
    );
    eprintln!("{backend:?} probe report:");
    for r in &report {
        eprintln!("  {r}");
    }
    (report, info)
}

fn ok(report: &[Value], op: &str) -> bool {
    report
        .iter()
        .find(|r| r["op"] == op)
        .unwrap_or_else(|| panic!("no {op} in the report"))["ok"]
        .as_bool()
        .unwrap()
}

/// Checks the report: every forbidden operation failed, the allowed ones worked, in root and child.
fn check(report: &[Value], f: &Fixture, info: &sandbox::SandboxInfo, net: &[String]) {
    let mut failures = Vec::new();
    for prefix in ["", "child:"] {
        for p in forbidden_writes(f) {
            let op = format!("{prefix}write:{}", s(&p));
            // On Linux /tmp inside is private, so the write may succeed there without reaching the host.
            let host_written = std::fs::read(&p).is_ok_and(|b| b == b"probe");
            if host_written {
                failures.push(format!("{op}: reached the host"));
            }
        }
        for p in ["sib/secret.txt", "data/node.db", "outside/.env"] {
            let op = format!("{prefix}read:{}", s(&f.root.join(p)));
            if ok(report, &op) {
                failures.push(op);
            }
        }
        for a in net {
            let op = format!("{prefix}{a}");
            if ok(report, &op) {
                failures.push(op);
            }
        }
        if ok(report, &format!("{prefix}env-canary")) {
            failures.push(format!("{prefix}env-canary"));
        }
        for must in [
            format!("{prefix}write:{}", s(&f.wt.join("ok.txt"))),
            format!("{prefix}write:{}", s(&f.temp.join("ok.txt"))),
            format!("{prefix}read:{}", s(&f.wt.join("hello.txt"))),
            format!("{prefix}proxy-route"),
        ] {
            if !ok(report, &must) {
                failures.push(format!("{must} must work"));
            }
        }
        // DNS: blocked where the backend says so; MXC's DNS gap is reported, not hidden.
        let dns = format!("{prefix}dns:example.com");
        if report.iter().any(|r| r["op"] == dns) {
            let resolved = ok(report, &dns);
            if info.dns_blocked && resolved {
                failures.push(format!("{dns} resolved although the backend blocks DNS"));
            }
            if !info.dns_blocked {
                assert!(
                    info.gaps.iter().any(|g| g.contains("dns")),
                    "an unblocked DNS must be listed as a gap"
                );
            }
        }
    }
    // SR-33: the environment holds only what the spec and the sandbox put there.
    let env = report.iter().find(|r| r["op"] == "env").unwrap()["detail"]
        .as_str()
        .unwrap()
        .split(',')
        .map(|x| x.to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    let allowed: BTreeSet<String> = [
        "HAN_PROBE_SPEC",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "TEMP",
        "TMP",
        "TMPDIR",
        "PATH",
        "SYSTEMROOT",
        "WINDIR",
        "SYSTEMDRIVE",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "PATHEXT",
        "COMSPEC",
        "HAN_PROBE_CHILD",
        "PWD",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let extra: Vec<_> = env.difference(&allowed).collect();
    // Windows itself adds a few process variables to every process (for example `=C:`); they hold no secrets.
    let extra: Vec<_> = extra
        .into_iter()
        .filter(|k| !k.starts_with('=') && !k.is_empty())
        .collect();
    if !extra.is_empty() {
        failures.push(format!("unexpected environment variables: {extra:?}"));
    }
    assert!(
        failures.is_empty(),
        "forbidden operations succeeded: {failures:#?}"
    );
}

async fn listener_v4() -> (tokio::net::TcpListener, SocketAddr) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

fn net_ops(
    other: SocketAddr,
    v6: Option<SocketAddr>,
    proxy: SocketAddr,
    other_proxy: SocketAddr,
) -> (Value, Vec<String>) {
    let mut tcp = vec![
        other.to_string(),
        proxy.to_string(),
        other_proxy.to_string(),
        "169.254.169.254:80".into(),
        "192.168.1.1:80".into(),
        "1.1.1.1:443".into(),
    ];
    if let Some(v) = v6 {
        tcp.push(v.to_string());
    }
    let mut ops: Vec<String> = tcp.iter().map(|t| format!("tcp:{t}")).collect();
    ops.push("udp:1.1.1.1:53".into());
    (
        json!({"tcp": tcp, "udp": ["1.1.1.1:53"], "dns": ["example.com"]}),
        ops,
    )
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mxc_confines_the_tree() {
    if !available(SandboxBackend::Mxc) {
        return;
    }
    let f = fixture();
    let proxy = session_proxy(TOKEN).await;
    let other_proxy = session_proxy("tok-OTHER-BBBBBBBBBBBBBBBBBBB").await;
    let (_l4, other) = listener_v4().await;
    let l6 = tokio::net::TcpListener::bind("[::1]:0").await.ok();
    let v6 = l6.as_ref().map(|l| l.local_addr().unwrap());
    // A pipe standing in for the node API (default security: no access for AppContainers).
    let node_pipe = format!("han-node-probe-{}", std::process::id());
    let _server = tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(true)
        .create(format!(r"\\.\pipe\{node_pipe}"))
        .unwrap();
    let (mut extra, mut net) = net_ops(other, v6, proxy.addr(), other_proxy.addr());
    extra["local"] = json!([node_pipe.clone()]);
    net.push(format!("local:{node_pipe}"));
    let (report, info) = run_probe(Preference::Mxc, &f, &proxy, extra).await;
    assert_eq!(info.backend, SandboxBackend::Mxc);
    assert_eq!(info.fidelity, Fidelity::Enforced);
    check(&report, &f, &info, &net);
    eprintln!("MXC gaps: {:#?}", info.gaps);
}

/// Inbound: the identity-less loopback proxy policy allows ingress into the container, so a listener inside
/// (on `0.0.0.0` or `127.0.0.1`) must still be unreachable from the host (loopback) and from the PC's LAN
/// address, or it would be a channel out. Every inbound connect must be refused.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_connects_into_an_mxc_container() {
    if !available(SandboxBackend::Mxc) {
        return;
    }
    let f = fixture();
    let proxy = session_proxy(TOKEN).await;
    let (p_any, p_lo) = (
        sandbox::pick_inside_port().unwrap(),
        sandbox::pick_inside_port().unwrap(),
    );
    // the PC's LAN address: the source address the OS would use to leave the machine (nothing is sent)
    let lan = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|u| u.connect("192.0.2.1:9").and_then(|_| u.local_addr()))
        .map(|a| a.ip())
        .ok();
    let spec_json = json!({
        "listen": [format!("0.0.0.0:{p_any}"), format!("127.0.0.1:{p_lo}")],
        "hold_ms": 20000,
    });
    let program = probe_exe();
    let spec = SandboxSpec {
        session: "inbound".into(),
        backend: Preference::Mxc,
        program: program.clone(),
        args: vec![],
        cwd: f.wt.clone(),
        env: vec![("HAN_PROBE_SPEC".into(), spec_json.to_string())],
        rw: vec![f.wt.clone()],
        ro: vec![program.parent().unwrap().to_path_buf()],
        temp: f.temp.clone(),
        proxy: Some(ProxyTarget {
            addr: proxy.addr(),
            token: TOKEN.into(),
        }),
        volatile_dirs: Vec::new(),
        allow_advisory: false,
        limits: Limits {
            wall_time: Some(Duration::from_secs(60)),
            ..Limits::default()
        },
        shim: shim_exe(),
        inside_port: None,
    };
    let mut sb = sandbox::launch(spec).await.unwrap();
    let mut out = tokio::io::BufReader::new(sb.stdout.take().unwrap());
    drop(sb.stdin.take());
    // wait until both listeners are up inside (otherwise a refusal would prove nothing)
    let mut up = Vec::new();
    let waited = tokio::time::timeout(Duration::from_secs(60), async {
        let mut line = String::new();
        while up.len() < 2 {
            line.clear();
            if tokio::io::AsyncBufReadExt::read_line(&mut out, &mut line)
                .await
                .unwrap_or(0)
                == 0
            {
                break;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                up.push(v);
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "the listeners did not report: {up:?}");
    eprintln!("inside: {up:?}");
    assert!(
        up.iter().all(|v| v["ok"] == true),
        "the container could not listen, so the test would prove nothing: {up:?}"
    );
    let mut targets: Vec<SocketAddr> = vec![];
    for port in [p_any, p_lo] {
        targets.push(([127, 0, 0, 1], port).into());
        if let Some(ip) = lan {
            targets.push(SocketAddr::new(ip, port));
        }
    }
    let mut connected = Vec::new();
    for t in &targets {
        let r =
            tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(t)).await;
        eprintln!(
            "host connect {t}: {:?}",
            r.as_ref().map(|c| c.as_ref().map(|_| ()))
        );
        if matches!(r, Ok(Ok(_))) {
            connected.push(*t);
        }
    }
    sb.kill();
    assert!(
        connected.is_empty(),
        "the host (or LAN address) connected into the container: {connected:?} (lan {lan:?})"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bwrap_confines_the_tree() {
    if !available(SandboxBackend::Bwrap) {
        return;
    }
    let f = fixture();
    let proxy = session_proxy(TOKEN).await;
    let other_proxy = session_proxy("tok-OTHER-BBBBBBBBBBBBBBBBBBB").await;
    let (_l4, other) = listener_v4().await;
    let l6 = tokio::net::TcpListener::bind("[::1]:0").await.ok();
    let v6 = l6.as_ref().map(|l| l.local_addr().unwrap());
    // A socket standing in for the node API.
    let node_sock = f.root.join("data").join("node.sock");
    let _node = std::os::unix::net::UnixListener::bind(&node_sock).unwrap();
    let (mut extra, mut net) = net_ops(other, v6, proxy.addr(), other_proxy.addr());
    extra["local"] = json!([s(&node_sock)]);
    net.push(format!("local:{}", s(&node_sock)));
    net.push("dns:example.com".into());
    // Socket families the network namespace does not cover, and nested user namespaces.
    extra["linux"] = json!(true);
    net.extend(["vsock-socket", "vsock-connect", "packet-socket"].map(String::from));
    let (report, info) = run_probe(Preference::Bwrap, &f, &proxy, extra).await;
    assert_eq!(info.backend, SandboxBackend::Bwrap);
    assert_eq!(info.fidelity, Fidelity::Enforced);
    assert!(info.dns_blocked);
    // `--disable-userns` needs bwrap 0.8+; without it the gap must be listed rather than the test skipped.
    if info.gaps.iter().any(|g| g.contains("--disable-userns")) {
        eprintln!("NOTE: bwrap lacks --disable-userns; nested userns not asserted");
    } else {
        net.push("userns-unshare".into());
    }
    check(&report, &f, &info, &net);
}

/// SR-23: no sandbox means no session, unless the space allows advisory mode; then status says advisory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advisory_mode_needs_permission_and_says_so() {
    let f = fixture();
    let proxy = session_proxy(TOKEN).await;
    let mut spec = SandboxSpec {
        session: "adv".into(),
        backend: Preference::None,
        program: probe_exe(),
        args: vec![],
        cwd: f.wt.clone(),
        env: vec![("HAN_PROBE_SPEC".into(), json!({"proxy": true}).to_string())],
        rw: vec![f.wt.clone()],
        ro: vec![],
        temp: f.temp.clone(),
        proxy: Some(ProxyTarget {
            addr: proxy.addr(),
            token: TOKEN.into(),
        }),
        volatile_dirs: Vec::new(),
        allow_advisory: false,
        limits: Limits::default(),
        shim: shim_exe(),
        inside_port: None,
    };
    match sandbox::launch(spec.clone()).await {
        Err(SandboxError::AdvisoryNotAllowed(_)) => {}
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("an unsandboxed session started without advisory mode"),
    }
    // A backend this OS lacks is an error, never a silent fallback.
    spec.backend = if cfg!(windows) {
        Preference::Bwrap
    } else {
        Preference::Mxc
    };
    spec.allow_advisory = true;
    assert!(matches!(
        sandbox::launch(spec.clone()).await,
        Err(SandboxError::Unavailable(_))
    ));
    spec.backend = Preference::None;
    let mut sb = sandbox::launch(spec).await.unwrap();
    assert_eq!(sb.info.backend, SandboxBackend::None);
    assert_eq!(sb.info.fidelity, Fidelity::Advisory);
    let mut text = String::new();
    sb.stdout
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .await
        .unwrap();
    assert!(
        text.contains("\"op\":\"proxy-route\",\"ok\":true"),
        "{text}"
    );
}

/// SR-31 (wall time): the tree is killed when its wall time is up, and `kill` ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wall_time_and_kill_end_the_tree() {
    let f = fixture();
    let backend = sandbox::probe(&shim_exe())
        .into_iter()
        .find(|b| b.available && b.backend != SandboxBackend::None)
        .map(|b| match b.backend {
            SandboxBackend::Mxc => Preference::Mxc,
            SandboxBackend::Bwrap => Preference::Bwrap,
            SandboxBackend::None => Preference::None,
        })
        .unwrap_or_else(|| {
            eprintln!(
                "NOTE: no usable OS sandbox here, so this runs advisory: {}",
                why_none()
            );
            Preference::None
        });
    let shim = shim_exe();
    let mk = |wall: Option<Duration>| SandboxSpec {
        session: "wall".into(),
        backend,
        // the shim with no command forwards until it is killed
        program: shim.program.clone(),
        args: shim.args([
            "listen".into(),
            "--port".into(),
            "18999".into(),
            "--tcp".into(),
            "127.0.0.1:9".into(),
        ]),
        cwd: f.wt.clone(),
        env: vec![],
        rw: vec![f.wt.clone()],
        ro: vec![shim.program.clone()],
        temp: f.temp.clone(),
        proxy: None,
        volatile_dirs: Vec::new(),
        allow_advisory: true,
        limits: Limits {
            wall_time: wall,
            ..Limits::default()
        },
        shim: shim.clone(),
        inside_port: None,
    };
    let mut sb = sandbox::launch(mk(Some(Duration::from_secs(2))))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), sb.wait())
        .await
        .expect("the wall time ended the tree");
    let mut sb = sandbox::launch(mk(None)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sb.try_wait().is_none(), "still running");
    sb.kill();
    tokio::time::timeout(Duration::from_secs(30), sb.wait())
        .await
        .expect("kill ended the tree");
}

/// SR-29: the agent's stdin and stdout work through the sandbox, and the MCP stub inside reaches han-mcp
/// outside only through the proxy's MCP tunnel (here an echo).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdio_and_the_mcp_stub_cross_the_sandbox() {
    let f = fixture();
    let mut cfg = ProxyConfig::new("stub", TOKEN);
    cfg.mcp = Some(Arc::new(|s: tokio::net::TcpStream| {
        tokio::spawn(async move {
            let (mut r, mut w) = s.into_split();
            let _ = tokio::io::copy(&mut r, &mut w).await;
        });
    }));
    let proxy = Proxy::start(cfg).await.unwrap();
    let backend = sandbox::probe(&shim_exe())
        .into_iter()
        .find(|b| b.available && b.backend != SandboxBackend::None)
        .map(|b| match b.backend {
            SandboxBackend::Mxc => Preference::Mxc,
            _ => Preference::Bwrap,
        })
        .unwrap_or_else(|| {
            eprintln!(
                "NOTE: no usable OS sandbox here, so this runs advisory: {}",
                why_none()
            );
            Preference::None
        });
    let shim = shim_exe();
    let spec = SandboxSpec {
        session: "stub".into(),
        backend,
        program: shim.program.clone(),
        args: shim.args([
            "mcp-stub".into(),
            "--proxy".into(),
            "127.0.0.1:{proxy_port}".into(),
            "--token".into(),
            "{proxy_token}".into(),
        ]),
        cwd: f.wt.clone(),
        env: vec![],
        rw: vec![f.wt.clone()],
        ro: vec![shim.program.clone()],
        temp: f.temp.clone(),
        proxy: Some(ProxyTarget {
            addr: proxy.addr(),
            token: TOKEN.into(),
        }),
        volatile_dirs: Vec::new(),
        allow_advisory: true,
        limits: Limits::default(),
        shim: shim.clone(),
        inside_port: None,
    };
    let mut sb = sandbox::launch(spec).await.unwrap();
    eprintln!("stub test backend: {:?}", sb.info.backend);
    let mut stdin = sb.stdin.take().unwrap();
    let mut stdout = sb.stdout.take().unwrap();
    let msg = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
    stdin.write_all(msg).await.unwrap();
    stdin.flush().await.unwrap();
    let mut got = vec![0u8; msg.len()];
    tokio::time::timeout(Duration::from_secs(30), stdout.read_exact(&mut got))
        .await
        .expect("echo arrived")
        .unwrap();
    assert_eq!(&got[..], &msg[..]);
    assert!(
        proxy
            .decisions()
            .iter()
            .any(|d| d.reason == "allowed" && d.host == "han-mcp.invalid")
    );
    sb.kill();
}
