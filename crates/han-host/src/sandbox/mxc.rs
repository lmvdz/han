//! Windows: Microsoft MXC ProcessContainer (AppContainer / BaseContainer, no admin).
//!
//! Egress: MXC's proxy mode and direct host loopback do not work on current Windows builds (spike Q2), so the
//! container may reach exactly one container-local loopback port, where the shim's `listen` runs; the shim
//! forwards to `\\.\pipe\hanproxy-<session>-<random>`, which this host creates with a DACL for SYSTEM, the
//! owner and ALL APPLICATION PACKAGES, and bridges to the proxy's TCP port.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_node::api::{Fidelity, SandboxBackend};
use mxc_sdk::v1::configs::ProcessContainerConfig;
use mxc_sdk::v1::policy::{
    NetworkAction, NetworkEgressPolicy, NetworkPeerPolicy, NetworkPolicy, NetworkPortPolicy,
    NetworkProtocol, NetworkRulePolicy, UiPolicy,
};
use mxc_sdk::v1::{self, ContainerRequest, Containment, FilesystemPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use super::{
    BackendAvailability, ChildIn, ChildOut, SandboxError, SandboxInfo, SandboxSpec, Sandboxed,
    build_env, name_part, random_hex,
};

pub(super) fn probe() -> BackendAvailability {
    match v1::probe(None) {
        Ok(p) if p.error.is_none() && p.tier.is_some() => BackendAvailability {
            backend: SandboxBackend::Mxc,
            available: true,
            detail: format!("tier {}", p.tier.unwrap_or("?")),
        },
        Ok(p) => BackendAvailability {
            backend: SandboxBackend::Mxc,
            available: false,
            detail: p.error.unwrap_or_else(|| "no usable tier".into()),
        },
        Err(e) => BackendAvailability {
            backend: SandboxBackend::Mxc,
            available: false,
            detail: format!("{e:?}").chars().take(300).collect(),
        },
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
    let avail = probe();
    if !avail.available {
        return Err(SandboxError::Unavailable(format!("mxc: {}", avail.detail)));
    }
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
    // MXC refuses a verbatim environment without these; neither holds a secret.
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    for (k, v) in [
        ("SystemRoot", root.clone()),
        ("windir", root.clone()),
        ("SystemDrive", root.chars().take(2).collect()),
        ("LOCALAPPDATA", spec.temp.to_string_lossy().into_owned()),
        ("APPDATA", spec.temp.to_string_lossy().into_owned()),
        ("USERPROFILE", spec.temp.to_string_lossy().into_owned()),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD".into()),
        ("ComSpec", format!("{root}\\System32\\cmd.exe")),
    ] {
        if !env.iter().any(|(x, _)| x.eq_ignore_ascii_case(k)) {
            env.push((k.to_owned(), v));
        }
    }

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
    ro.push(root.clone());

    let allow = if spec.proxy.is_some() {
        Some(vec![NetworkRulePolicy {
            to: Some(vec![NetworkPeerPolicy::new("127.0.0.1/32")]),
            ports: Some(vec![NetworkPortPolicy {
                protocol: Some(NetworkProtocol::Tcp),
                port: Some(port),
                end_port: None,
            }]),
        }])
    } else {
        None
    };
    let timeout_ms = spec
        .limits
        .wall_time
        .map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
        .unwrap_or(u32::MAX);
    let req = ContainerRequest {
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
            ..Default::default()
        }),
        ui: Some(UiPolicy {
            disable: false,
            ..Default::default()
        }),
        timeout_ms: Some(timeout_ms),
        containment: Containment::ProcessContainer(ProcessContainerConfig::default()),
        working_directory: Some(spec.cwd.to_string_lossy().into_owned()),
        environment: Some(env),
        inherit_default_environment: Some(false),
        ..ContainerRequest::new(cmd.join(" "))
    };
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
