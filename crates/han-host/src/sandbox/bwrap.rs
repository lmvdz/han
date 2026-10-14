//! Linux: bubblewrap. A new root with the system directories read-only, the listed paths bound, a private
//! `/tmp`, and new user, pid, ipc, uts, cgroup and network namespaces. The network namespace has only `lo`;
//! the in-sandbox shim forwards a loopback port to a unix socket bound in from the host, which this host
//! bridges to the proxy. With no other interface, DNS fails.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use han_node::api::{Fidelity, SandboxBackend};

use super::{
    BackendAvailability, SandboxError, SandboxInfo, SandboxSpec, Sandboxed, build_env,
    from_tokio_child, random_hex,
};

const SOCK_DIR_INSIDE: &str = "/run/han";
const SHIM_PORT: u16 = 18_080;

fn find(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

pub(super) fn probe() -> BackendAvailability {
    let Some(bwrap) = find("bwrap") else {
        return BackendAvailability {
            backend: SandboxBackend::Bwrap,
            available: false,
            detail: "bwrap is not installed".into(),
        };
    };
    // Unprivileged user namespaces can be disabled (sysctl, AppArmor): try one.
    let out = std::process::Command::new(&bwrap)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "true",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => BackendAvailability {
            backend: SandboxBackend::Bwrap,
            available: true,
            detail: format!("{}", bwrap.display()),
        },
        Ok(o) => BackendAvailability {
            backend: SandboxBackend::Bwrap,
            available: false,
            detail: format!(
                "bwrap cannot create namespaces: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
        },
        Err(e) => BackendAvailability {
            backend: SandboxBackend::Bwrap,
            available: false,
            detail: format!("bwrap: {e}"),
        },
    }
}

fn add_system(args: &mut Vec<String>) {
    for d in [
        "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt",
    ] {
        let p = Path::new(d);
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                if let Ok(t) = std::fs::read_link(p) {
                    args.extend([
                        "--symlink".into(),
                        t.to_string_lossy().into_owned(),
                        d.into(),
                    ]);
                }
            }
            Ok(_) => args.extend(["--ro-bind".into(), d.into(), d.into()]),
            Err(_) => {}
        }
    }
}

pub(super) async fn launch(spec: &SandboxSpec) -> Result<Sandboxed, SandboxError> {
    let avail = probe();
    if !avail.available {
        return Err(SandboxError::Unavailable(avail.detail));
    }
    let bwrap = find("bwrap").ok_or_else(|| SandboxError::Unavailable("bwrap".into()))?;
    let mut tasks = Vec::new();
    let mut gaps = Vec::new();

    // The host side of the session channel: a socket in a directory only this user can enter.
    let sock_dir = std::env::temp_dir().join(format!("han-sock-{}", random_hex(8)));
    std::fs::create_dir_all(&sock_dir)?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if let Some(p) = &spec.proxy {
        let listener = tokio::net::UnixListener::bind(sock_dir.join("proxy.sock"))?;
        let proxy = p.addr;
        tasks.push(tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((conn, _)) => {
                        tokio::spawn(super::bridge_one(conn, proxy));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }));
    }

    let port = spec.inside_port.unwrap_or(SHIM_PORT);
    let env = build_env(spec, spec.proxy.as_ref().map(|_| port), &spec.temp);
    let mut args: Vec<String> = vec![
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--clearenv".into(),
        "--cap-drop".into(),
        "ALL".into(),
    ];
    add_system(&mut args);
    args.extend([
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ]);
    for p in &spec.ro {
        let s = p.to_string_lossy().into_owned();
        args.extend(["--ro-bind".into(), s.clone(), s]);
    }
    let shim_s = spec.shim.program.to_string_lossy().into_owned();
    args.extend(["--ro-bind".into(), shim_s.clone(), shim_s.clone()]);
    for p in spec.rw.iter().chain(std::iter::once(&spec.temp)) {
        let s = p.to_string_lossy().into_owned();
        args.extend(["--bind".into(), s.clone(), s]);
    }
    args.extend([
        "--bind".into(),
        sock_dir.to_string_lossy().into_owned(),
        SOCK_DIR_INSIDE.into(),
    ]);
    for (k, v) in &env {
        args.extend(["--setenv".into(), k.clone(), v.clone()]);
    }
    args.extend(["--chdir".into(), spec.cwd.to_string_lossy().into_owned()]);
    args.push("--".into());
    args.push(shim_s);
    args.extend(spec.shim.prefix_args.iter().cloned());
    args.push("listen".into());
    args.extend(["--port".into(), port.to_string()]);
    if spec.proxy.is_some() {
        args.extend(["--unix".into(), format!("{SOCK_DIR_INSIDE}/proxy.sock")]);
    } else {
        args.extend(["--tcp".into(), "127.0.0.1:9".into()]);
    }
    args.push("--".into());
    args.push(spec.program.to_string_lossy().into_owned());
    args.extend(super::build_args(spec, spec.proxy.as_ref().map(|_| port)));

    let (program, mut full) = match (spec.limits.memory_bytes, find("prlimit")) {
        (Some(m), Some(prlimit)) => (
            prlimit,
            vec![
                format!("--as={m}"),
                "--".into(),
                bwrap.to_string_lossy().into_owned(),
            ],
        ),
        (Some(_), None) => {
            gaps.push("memory cap not enforced: prlimit is not installed (SR-31)".into());
            (bwrap, Vec::new())
        }
        (None, _) => (bwrap, Vec::new()),
    };
    full.extend(args);
    if spec.limits.processes.is_some() {
        gaps.push(
            "process-count cap not enforced: RLIMIT_NPROC counts every process of the user, so it is not set (SR-31)"
                .into(),
        );
    }
    gaps.push("no disk cap on the worktree and temp directory (SR-31)".into());
    let child = tokio::process::Command::new(program)
        .args(full)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SandboxError::Spawn(format!("bwrap: {e}")))?;
    let dir = sock_dir.clone();
    tasks.push(tokio::spawn(async move {
        // Removed when the session's tasks are aborted at drop; best effort.
        let _guard = RemoveOnDrop(dir);
        std::future::pending::<()>().await;
    }));
    Ok(from_tokio_child(
        child,
        SandboxInfo {
            backend: SandboxBackend::Bwrap,
            fidelity: Fidelity::Enforced,
            gaps,
            dns_blocked: true,
            proxy_inside: spec
                .proxy
                .as_ref()
                .map(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        },
        tasks,
    ))
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
