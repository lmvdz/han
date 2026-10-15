//! Linux: bubblewrap. A new root with the system directories read-only, the listed paths bound, a private
//! `/tmp`, and new user, pid, ipc, uts, cgroup and network namespaces. The network namespace has only `lo`;
//! the in-sandbox shim forwards a loopback port to a unix socket bound in from the host, which this host
//! bridges to the proxy. With no other interface, DNS fails. A seccomp filter keeps the tree to AF_UNIX,
//! AF_INET, AF_INET6 and AF_NETLINK sockets (the network namespace does not cover AF_VSOCK, which reaches the
//! hypervisor on a VM host), and `--disable-userns` stops nested user namespaces where bwrap supports it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use han_node::api::{Fidelity, SandboxBackend};

use super::seccomp;
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

/// `HAN_BWRAP_FORCE_UNAVAILABLE=1` makes the probe of this process report bwrap as unable to create
/// namespaces, as it does in a unit with `RestrictNamespaces=yes`; it can only refuse.
pub const FORCE_UNAVAILABLE: &str = "HAN_BWRAP_FORCE_UNAVAILABLE";

pub(super) fn probe() -> BackendAvailability {
    if std::env::var(FORCE_UNAVAILABLE).is_ok_and(|v| v == "1") {
        return BackendAvailability {
            backend: SandboxBackend::Bwrap,
            loopback_blocked: false,
            available: false,
            detail: format!(
                "bwrap cannot create namespaces: bwrap: Can't read /proc/sys/kernel/overflowuid: No such file or directory ({FORCE_UNAVAILABLE}=1 is set)"
            ),
        };
    }
    let Some(bwrap) = find("bwrap") else {
        return BackendAvailability {
            backend: SandboxBackend::Bwrap,
            loopback_blocked: false,
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
            loopback_blocked: false,
            available: true,
            detail: format!("{}", bwrap.display()),
        },
        Ok(o) => BackendAvailability {
            backend: SandboxBackend::Bwrap,
            loopback_blocked: false,
            available: false,
            detail: format!(
                "bwrap cannot create namespaces: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
        },
        Err(e) => BackendAvailability {
            backend: SandboxBackend::Bwrap,
            loopback_blocked: false,
            available: false,
            detail: format!("bwrap: {e}"),
        },
    }
}

/// Whether this bwrap accepts `--disable-userns` (0.8+) and can apply it here.
fn supports_disable_userns(bwrap: &Path) -> bool {
    std::process::Command::new(bwrap)
        .args([
            "--unshare-all",
            "--unshare-user",
            "--disable-userns",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "true",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
    if supports_disable_userns(&bwrap) {
        // `--disable-userns` insists on an explicit `--unshare-user` (`--unshare-all` only tries one).
        args.extend(["--unshare-user".into(), "--disable-userns".into()]);
    } else {
        gaps.push(
            "nested user namespaces not blocked: this bwrap lacks a working --disable-userns (needs 0.8+)"
                .into(),
        );
    }
    // bwrap reads the filter from fd 3, which the `sh` wrapper below opens (the host forbids `unsafe`, so no
    // `pre_exec` fd juggling). The file sits in the private socket directory.
    let filter = seccomp::filter_bytes();
    let filter_path = sock_dir.join("seccomp.bpf");
    match &filter {
        Some(bytes) => {
            std::fs::write(&filter_path, bytes)?;
            args.extend(["--seccomp".into(), "3".into()]);
        }
        None => gaps.push(
            "socket families not restricted: no seccomp filter for this CPU architecture".into(),
        ),
    }
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
    let (program, full) = if filter.is_some() {
        let mut wrapped = vec![
            "-c".into(),
            "exec 3<\"$1\" && shift && exec \"$@\"".into(),
            "han-bwrap".into(),
            filter_path.to_string_lossy().into_owned(),
            program.to_string_lossy().into_owned(),
        ];
        wrapped.extend(full);
        (
            find("sh").unwrap_or_else(|| PathBuf::from("/bin/sh")),
            wrapped,
        )
    } else {
        (program, full)
    };
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
