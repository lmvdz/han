//! Windows: Microsoft MXC ProcessContainer (AppContainer / BaseContainer, no admin).
//!
//! Egress: MXC's proxy mode and direct host loopback do not work on current Windows builds (spike Q2), so the
//! container may reach exactly one container-local loopback port, where the shim's `listen` runs; the shim
//! forwards to `\\.\pipe\hanproxy-<session>-<random>`, which this host creates with a DACL for SYSTEM, the
//! owner and ALL APPLICATION PACKAGES, and bridges to the proxy's TCP port.
//!
//! That port must work. Windows 11 Insider 26220 and later refuse every loopback connect from inside a
//! container with 10013, so [`probe`] proves it first: it runs `han shim selftest` in one container under the
//! policy [`container_request`] builds for a launch, and the backend is unavailable when the port does not
//! work. Of the two policies ([`Policy`]) the strict one is tried first; the other, MXC's identity-less
//! loopback proxy, sets `hostLoopback` and ingress to Allow but gives egress no rule, so the container reaches
//! only the shim's port. `sandbox_probe` checks nothing can connect in. `hostLoopback = Allow` together with
//! an egress allow rule is never used.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use han_node::api::{Fidelity, SandboxBackend};
use mxc_sdk::v1::configs::ProcessContainerConfig;
use mxc_sdk::v1::policy::{
    NetworkAction, NetworkEgressPolicy, NetworkIngressPolicy, NetworkPeerPolicy, NetworkPolicy,
    NetworkPortPolicy, NetworkProtocol, NetworkRulePolicy, NetworkRuntimeConfig, UiPolicy,
};
use mxc_sdk::v1::{self, ContainerRequest, Containment, FilesystemPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use super::{
    BackendAvailability, ChildIn, ChildOut, SandboxError, SandboxInfo, SandboxSpec, Sandboxed,
    ShimCommand, build_env, name_part, random_hex,
};
use crate::shim::{SELFTEST_BLOCKED, SELFTEST_OK};

/// `HAN_MXC_FORCE_UNAVAILABLE=loopback` makes the probe report the loopback port as blocked without running it.
pub const FORCE_BLOCKED: &str = "HAN_MXC_FORCE_UNAVAILABLE";

/// How long the self-test container may run before it counts as failed.
const SELFTEST_MS: u32 = 15_000;

/// What the MXC probe found for each shim, once per process: the self-test starts a container.
static PROBED: Mutex<Vec<(ShimCommand, BackendAvailability, Policy)>> = Mutex::new(Vec::new());

/// How the container reaches the shim's loopback port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Policy {
    /// Egress denied except an allow rule for `127.0.0.1:port`. Works where the OS lets a container use
    /// loopback; it is tried first because it opens nothing else.
    Port,
    /// MXC's identity-less loopback proxy (a documented development/testing mode): egress denied with no
    /// rule, ingress and host loopback allowed, and the container's proxy set to the shim's port. The
    /// container reaches only that port; `sandbox_probe` shows nothing connects in. Used on Windows builds
    /// where `Port` fails with 10013.
    Proxy,
}

pub(super) fn probe(shim: &ShimCommand) -> BackendAvailability {
    probed(shim).0
}

/// The policy the self-test found working for `shim` (`Port` where nothing works).
fn policy_of(shim: &ShimCommand) -> Policy {
    probed(shim).1
}

fn probed(shim: &ShimCommand) -> (BackendAvailability, Policy) {
    // Held across the self-test, so concurrent callers share one container.
    let mut done = PROBED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, found, policy)) = done.iter().find(|(s, _, _)| s == shim) {
        return (found.clone(), *policy);
    }
    let (found, policy) = probe_now(shim);
    done.push((shim.clone(), found.clone(), policy));
    (found, policy)
}

fn probe_now(shim: &ShimCommand) -> (BackendAvailability, Policy) {
    let unavailable = |detail: String, loopback_blocked: bool| BackendAvailability {
        backend: SandboxBackend::Mxc,
        available: false,
        detail,
        loopback_blocked,
    };
    // A switch that can only refuse (an operator, or a test, standing in for a build with no loopback port).
    if std::env::var(FORCE_BLOCKED).is_ok_and(|v| v == "loopback") {
        let why = format!(
            "MXC on this Windows build blocks the in-sandbox loopback port (10013); see docs/runner.md ({FORCE_BLOCKED}=loopback is set)"
        );
        return (unavailable(why, true), Policy::Port);
    }
    let tier = match v1::probe(None) {
        Ok(p) if p.error.is_none() && p.tier.is_some() => p.tier.unwrap_or("?"),
        Ok(p) => {
            let why = p.error.unwrap_or_else(|| "no usable tier".into());
            return (unavailable(why, false), Policy::Port);
        }
        Err(e) => {
            let why = format!("{e:?}").chars().take(300).collect();
            return (unavailable(why, false), Policy::Port);
        }
    };
    let mut last = Ok(());
    for policy in [Policy::Port, Policy::Proxy] {
        last = self_test(shim, policy);
        if last.is_ok() {
            let found = BackendAvailability {
                backend: SandboxBackend::Mxc,
                available: true,
                detail: format!("tier {tier}; in-sandbox loopback port works ({policy:?} policy)"),
                loopback_blocked: false,
            };
            return (found, policy);
        }
    }
    let found = match last {
        Err(SelfTest::Blocked) => unavailable(
            "MXC on this Windows build blocks the in-sandbox loopback port (10013); see docs/runner.md"
                .into(),
            true,
        ),
        Err(SelfTest::Failed(why)) => {
            unavailable(format!("the in-sandbox loopback self-test failed: {why}"), false)
        }
        Ok(()) => unreachable!("a passing self-test returned above"),
    };
    (found, Policy::Port)
}

/// Why the self-test did not pass.
enum SelfTest {
    /// The OS refused the loopback port with 10013.
    Blocked,
    /// It failed another way.
    Failed(String),
}

/// Runs `shim selftest` in one container under a launch's policy: the container binds a loopback port and
/// connects to it.
fn self_test(shim: &ShimCommand, policy: Policy) -> Result<(), SelfTest> {
    let failed = |why: String| SelfTest::Failed(why.chars().take(300).collect());
    let dir = std::env::temp_dir().join(format!(
        "han-mxc-selftest-{}-{}",
        std::process::id(),
        random_hex(4)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| failed(format!("{}: {e}", dir.display())))?;
    let run = || -> Result<(), SelfTest> {
        let port = super::pick_inside_port().map_err(|e| failed(e.to_string()))?;
        let mut cmd = vec![quote(&shim.program.to_string_lossy())];
        cmd.extend(shim.prefix_args.iter().map(|a| quote(a)));
        cmd.extend(["selftest".into(), "--port".into(), port.to_string()]);
        let mut env = Vec::new();
        let root = os_env(&mut env, &dir);
        let mut ro = vec![root];
        if let Some(d) = shim.program.parent() {
            ro.push(d.to_string_lossy().into_owned());
        }
        let req = container_request(
            policy,
            None,
            cmd.join(" "),
            vec![dir.to_string_lossy().into_owned()],
            ro,
            Some(port),
            SELFTEST_MS,
            dir.to_string_lossy().into_owned(),
            env,
        );
        let mut proc = KillOnDrop(
            v1::spawn(req, Default::default()).map_err(|e| failed(format!("mxc: {e:?}")))?,
        );
        let (out, err) = (proc.0.take_stdout(), proc.0.take_stderr());
        let (tx, rx) = std::sync::mpsc::channel();
        for stream in [out, err].into_iter().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = { stream }.take(4096).read_to_string(&mut buf);
                let _ = tx.send(buf);
            });
        }
        drop(tx);
        // The container has its own time limit; this one is the host's, in case that limit never fires.
        let deadline = Instant::now() + Duration::from_millis(u64::from(SELFTEST_MS) + 5_000);
        let code = loop {
            match proc.0.try_wait() {
                Ok(Some(c)) => break c,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Ok(None) => return Err(failed("timed out".into())),
                Err(e) => return Err(failed(format!("wait: {e}"))),
            }
        };
        match code {
            c if c == SELFTEST_OK => Ok(()),
            c if c == SELFTEST_BLOCKED => Err(SelfTest::Blocked),
            c => {
                let said: String = rx
                    .try_iter()
                    .chain(rx.recv_timeout(Duration::from_millis(500)))
                    .map(|s| s.trim().to_owned())
                    .collect();
                Err(failed(format!("exit {c}: {said}")))
            }
        }
    };
    let r = run();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// Ends the container's whole process tree when dropped, so a self-test that errors, times out or panics
/// never leaves its `han shim selftest` behind. `kill` tears the launch down, not just the direct child.
struct KillOnDrop(v1::MxcProcess);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Adds what the OS needs to start a process in a container (MXC refuses a verbatim environment without it;
/// none of it holds a secret). Returns `SystemRoot`.
fn os_env(env: &mut Vec<(String, String)>, temp: &Path) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    for (k, v) in [
        ("SystemRoot", root.clone()),
        ("windir", root.clone()),
        ("SystemDrive", root.chars().take(2).collect()),
        ("LOCALAPPDATA", temp.to_string_lossy().into_owned()),
        ("APPDATA", temp.to_string_lossy().into_owned()),
        ("USERPROFILE", temp.to_string_lossy().into_owned()),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD".into()),
        ("ComSpec", format!("{root}\\System32\\cmd.exe")),
    ] {
        if !env.iter().any(|(x, _)| x.eq_ignore_ascii_case(k)) {
            env.push((k.to_owned(), v));
        }
    }
    root
}

/// The container a launch and the self-test both run: egress denied except `127.0.0.1:allow_port` (the
/// shim's), no other network policy (in particular no `hostLoopback`).
#[allow(clippy::too_many_arguments)]
fn container_request(
    policy: Policy,
    token: Option<&str>,
    command: String,
    rw: Vec<String>,
    ro: Vec<String>,
    allow_port: Option<u16>,
    timeout_ms: u32,
    cwd: String,
    env: Vec<(String, String)>,
) -> ContainerRequest {
    let allow = allow_port.map(|port| {
        vec![NetworkRulePolicy {
            to: Some(vec![NetworkPeerPolicy::new("127.0.0.1/32")]),
            ports: Some(vec![NetworkPortPolicy {
                protocol: Some(NetworkProtocol::Tcp),
                port: Some(port),
                end_port: None,
            }]),
        }]
    });
    let proxy_mode = allow_port.is_some() && policy == Policy::Proxy;
    let (allow, ingress, runtime_config) = if proxy_mode {
        (
            None,
            Some(NetworkIngressPolicy {
                default: Some(NetworkAction::Allow),
                host_loopback: Some(NetworkAction::Allow),
            }),
            // MXC sets the container's `HTTP(S)_PROXY` to this URL, over ours; the credentials go in it.
            allow_port.map(|port| NetworkRuntimeConfig {
                network_proxy: Some(match token {
                    Some(t) => format!("http://{}:{t}@127.0.0.1:{port}", crate::proxy::PROXY_USER),
                    None => format!("http://127.0.0.1:{port}"),
                }),
            }),
        )
    } else {
        (allow, None, None)
    };
    ContainerRequest {
        filesystem: Some(FilesystemPolicy {
            readwrite_paths: rw,
            readonly_paths: ro,
            denied_paths: vec![],
            clear_policy_on_exit: None,
        }),
        network: Some(NetworkPolicy {
            egress: Some(NetworkEgressPolicy {
                default: Some(NetworkAction::Deny),
                allow,
                ..Default::default()
            }),
            ingress,
            runtime_config,
        }),
        ui: Some(UiPolicy {
            disable: false,
            ..Default::default()
        }),
        timeout_ms: Some(timeout_ms),
        containment: Containment::ProcessContainer(ProcessContainerConfig::default()),
        working_directory: Some(cwd),
        environment: Some(env),
        inherit_default_environment: Some(false),
        ..ContainerRequest::new(command)
    }
}

/// Quotes one argument for a Windows command line (the CommandLineToArgvW rules).
pub(crate) fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

fn pipe_listener(name: &str) -> std::io::Result<interprocess::local_socket::tokio::Listener> {
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _};
    use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let sddl =
        widestring::U16CString::from_str("D:P(A;;GA;;;SY)(A;;GA;;;OW)(A;;GRGW;;;S-1-15-2-1)")
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let options = ListenerOptions::new()
        .name(name.to_ns_name::<GenericNamespaced>()?)
        .security_descriptor(SecurityDescriptor::deserialize(&sddl)?);
    options.create_tokio()
}

/// Turns a blocking reader into an async one through a thread.
fn reader(mut r: Box<dyn Read + Send>) -> ChildOut {
    let (mut tx, rx) = tokio::io::duplex(64 * 1024);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if futures::executor::block_on(tx.write_all(&buf[..n])).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = futures::executor::block_on(tx.shutdown());
    });
    Box::new(rx)
}

/// Turns a blocking writer into an async one through a thread.
fn writer(mut w: Box<dyn Write + Send>) -> ChildIn {
    let (tx, mut rx) = tokio::io::duplex(64 * 1024);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match futures::executor::block_on(rx.read(&mut buf)) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if w.write_all(&buf[..n]).and_then(|_| w.flush()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Box::new(tx)
}

pub(super) async fn launch(spec: &SandboxSpec) -> Result<Sandboxed, SandboxError> {
    let shim = spec.shim.clone();
    let avail = tokio::task::spawn_blocking(move || probe(&shim))
        .await
        .map_err(|e| SandboxError::Spawn(e.to_string()))?;
    if avail.loopback_blocked {
        return Err(SandboxError::LoopbackBlocked(avail.detail));
    }
    if !avail.available {
        return Err(SandboxError::Unavailable(format!("mxc: {}", avail.detail)));
    }
    let policy = policy_of(&spec.shim);
    let mut tasks = Vec::new();

    // The container-local port of the shim, and the session pipe behind it.
    // Containers bind in the host's loopback port space, so the port must be free on the host too: take
    // one the OS hands out and release it for the shim.
    let port: u16 = match spec.inside_port {
        Some(p) => p,
        None => super::pick_inside_port()?,
    };
    let pipe = format!("hanproxy-{}-{}", name_part(&spec.session), random_hex(8));
    if let Some(p) = &spec.proxy {
        let listener = pipe_listener(&pipe)?;
        let proxy = p.addr;
        tasks.push(tokio::spawn(async move {
            use interprocess::local_socket::traits::tokio::Listener as _;
            loop {
                match listener.accept().await {
                    Ok(conn) => {
                        tokio::spawn(super::bridge_one(conn, proxy));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }));
    }

    let mut env = build_env(spec, spec.proxy.as_ref().map(|_| port), &spec.temp);
    let root = os_env(&mut env, &spec.temp);

    let mut cmd = vec![quote(&spec.shim.program.to_string_lossy())];
    cmd.extend(spec.shim.prefix_args.iter().map(|a| quote(a)));
    cmd.push("listen".into());
    if spec.proxy.is_some() {
        cmd.extend([
            "--port".into(),
            port.to_string(),
            "--pipe".into(),
            pipe.clone(),
        ]);
    } else {
        // No proxy: the shim still starts the agent, forwarding to nothing reachable.
        cmd.extend([
            "--port".into(),
            port.to_string(),
            "--tcp".into(),
            "127.0.0.1:9".into(),
        ]);
    }
    cmd.push("--".into());
    cmd.push(quote(&spec.program.to_string_lossy()));
    cmd.extend(
        super::build_args(spec, spec.proxy.as_ref().map(|_| port))
            .iter()
            .map(|a| quote(a)),
    );

    let mut rw: Vec<String> = spec
        .rw
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    rw.push(spec.temp.to_string_lossy().into_owned());
    let mut ro: Vec<String> = spec
        .ro
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(d) = spec.shim.program.parent() {
        ro.push(d.to_string_lossy().into_owned());
    }
    ro.push(root);

    let timeout_ms = spec
        .limits
        .wall_time
        .map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
        .unwrap_or(u32::MAX);
    let req = container_request(
        policy,
        spec.proxy.as_ref().map(|p| p.token.as_str()),
        cmd.join(" "),
        rw,
        ro,
        spec.proxy.as_ref().map(|_| port),
        timeout_ms,
        spec.cwd.to_string_lossy().into_owned(),
        env,
    );
    let mut proc = tokio::task::spawn_blocking(move || v1::spawn(req, Default::default()))
        .await
        .map_err(|e| SandboxError::Spawn(e.to_string()))?
        .map_err(|e| SandboxError::Spawn(format!("mxc: {e:?}").chars().take(600).collect()))?;
    let warnings = proc.warnings();
    let stdin = proc.take_stdin().map(writer);
    let stdout = proc.take_stdout().map(reader);
    let stderr = proc.take_stderr().map(reader);

    let proc = Arc::new(Mutex::new(proc));
    let (tx, rx) = watch::channel(None);
    let p2 = Arc::clone(&proc);
    std::thread::spawn(move || {
        loop {
            let r = p2.lock().unwrap_or_else(|e| e.into_inner()).try_wait();
            match r {
                Ok(Some(code)) => {
                    let _ = tx.send(Some(Some(code)));
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => {
                    let _ = tx.send(Some(None));
                    return;
                }
            }
        }
    });
    let kill = Arc::new(move || {
        let _ = proc.lock().unwrap_or_else(|e| e.into_inner()).kill();
    });

    let mut gaps = vec![
        "dns: names still resolve through the host's DNS client inside the container (SR-27 not met)".into(),
        "the session pipe admits any AppContainer on this machine; the per-session proxy token separates sessions (SR-28)".into(),
        "no memory, process or disk caps: MXC has none; wall time only (SR-31)".into(),
    ];
    gaps.extend(warnings.into_iter().map(|w| format!("mxc: {w}")));
    Ok(Sandboxed {
        stdin,
        stdout,
        stderr,
        info: SandboxInfo {
            backend: SandboxBackend::Mxc,
            fidelity: Fidelity::Enforced,
            gaps,
            dns_blocked: false,
            proxy_inside: spec
                .proxy
                .as_ref()
                .map(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        },
        kill,
        exit: rx,
        tasks,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn quoting() {
        assert_eq!(super::quote("a"), "a");
        assert_eq!(super::quote("a b"), "\"a b\"");
        assert_eq!(super::quote(""), "\"\"");
        assert_eq!(super::quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(super::quote("C:\\x y\\"), "\"C:\\x y\\\\\"");
    }
}
