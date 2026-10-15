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

pub(super) fn probe(shim: &super::ShimCommand) -> BackendAvailability {
    let unavailable = |detail: String| BackendAvailability {
        backend: SandboxBackend::Bwrap,
        loopback_blocked: false,
        available: false,
        detail,
    };
    if std::env::var(FORCE_UNAVAILABLE).is_ok_and(|v| v == "1") {
        return unavailable(format!(
            "bwrap cannot create namespaces: bwrap: Can't read /proc/sys/kernel/overflowuid: No such file or directory ({FORCE_UNAVAILABLE}=1 is set)"
        ));
    }
    let Some(bwrap) = find("bwrap") else {
        return unavailable("bwrap is not installed".into());
    };
    // Unprivileged user namespaces can be disabled (sysctl, AppArmor), and a unit can forbid a fresh /proc:
    // run the launch's own argv with `true` in place of the agent. Without `--disable-userns` (old bwrap) the
    // launch still works, with a gap, so that is tried second.
    match trial(&bwrap, shim, true).or_else(|first| trial(&bwrap, shim, false).map_err(|_| first)) {
        Ok(()) => BackendAvailability {
            backend: SandboxBackend::Bwrap,
            loopback_blocked: false,
            available: true,
            detail: format!("{}", bwrap.display()),
        },
        Err(e) => unavailable(format!("bwrap cannot create namespaces: {e}")),
    }
}

/// What differs between sessions in a bwrap command line: everything but the command.
struct Layout<'a> {
    /// Whether `--disable-userns` is applied (bwrap 0.8+ that can).
    disable_userns: bool,
    /// Whether the seccomp filter is read from fd 3.
    seccomp: bool,
    ro: &'a [PathBuf],
    rw: &'a [PathBuf],
    temp: &'a Path,
    shim: &'a Path,
    sock_dir: &'a Path,
    env: &'a [(String, String)],
    cwd: &'a Path,
}

/// The bwrap arguments, ending with `-- command`. The launch and the admission probe both build theirs here,
/// so the probe fails exactly when the launch would (a unit that covers parts of /proc breaks `--proc`, and
/// only a probe that mounts it finds out).
fn bwrap_args(l: &Layout<'_>, command: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--clearenv".into(),
        "--cap-drop".into(),
        "ALL".into(),
    ];
    if l.disable_userns {
        // `--disable-userns` insists on an explicit `--unshare-user` (`--unshare-all` only tries one).
        args.extend(["--unshare-user".into(), "--disable-userns".into()]);
    }
    if l.seccomp {
        args.extend(["--seccomp".into(), "3".into()]);
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
    for p in l.ro {
        let s = p.to_string_lossy().into_owned();
        args.extend(["--ro-bind".into(), s.clone(), s]);
    }
    let shim_s = l.shim.to_string_lossy().into_owned();
    args.extend(["--ro-bind".into(), shim_s.clone(), shim_s]);
    for p in l.rw.iter().map(PathBuf::as_path).chain([l.temp]) {
        let s = p.to_string_lossy().into_owned();
        args.extend(["--bind".into(), s.clone(), s]);
    }
    args.extend([
        "--bind".into(),
        l.sock_dir.to_string_lossy().into_owned(),
        SOCK_DIR_INSIDE.into(),
    ]);
    for (k, v) in l.env {
        args.extend(["--setenv".into(), k.clone(), v.clone()]);
    }
    args.extend(["--chdir".into(), l.cwd.to_string_lossy().into_owned()]);
    args.push("--".into());
    args.extend(command.iter().cloned());
    args
}

/// `program args`, run by an `sh` that opens the seccomp filter as fd 3 first (the host forbids `unsafe`, so
/// no `pre_exec` fd juggling); unchanged when there is no filter.
fn with_filter_fd(
    program: PathBuf,
    args: Vec<String>,
    filter_path: Option<&Path>,
) -> (PathBuf, Vec<String>) {
    let Some(filter_path) = filter_path else {
        return (program, args);
    };
    let mut wrapped = vec![
        "-c".into(),
        "exec 3<\"$1\" && shift && exec \"$@\"".into(),
        "han-bwrap".into(),
        filter_path.to_string_lossy().into_owned(),
        program.to_string_lossy().into_owned(),
    ];
    wrapped.extend(args);
    (
        find("sh").unwrap_or_else(|| PathBuf::from("/bin/sh")),
        wrapped,
    )
}

/// Runs the launch's argv with `true` as the command. `Err` is bwrap's stderr (or why it did not run).
fn trial(bwrap: &Path, shim: &super::ShimCommand, disable_userns: bool) -> Result<(), String> {
    if !shim.program.is_file() {
        return Err(format!("the shim {} is not a file", shim.program.display()));
    }
    let dir = std::env::temp_dir().join(format!("han-probe-{}", random_hex(8)));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _guard = RemoveOnDrop(dir.clone());
    let filter = seccomp::filter_bytes();
    let filter_path = dir.join("seccomp.bpf");
    if let Some(bytes) = &filter {
        std::fs::write(&filter_path, bytes).map_err(|e| e.to_string())?;
    }
    let sock_dir = dir.join("sock");
    let temp = dir.join("tmp");
    for d in [&sock_dir, &temp] {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let layout = Layout {
        disable_userns,
        seccomp: filter.is_some(),
        ro: &[],
        rw: &[],
        temp: &temp,
        shim: &shim.program,
        sock_dir: &sock_dir,
        env: &[],
        cwd: Path::new("/"),
    };
    let args = bwrap_args(&layout, &["true".to_owned()]);
    let (program, args) = with_filter_fd(
        bwrap.to_path_buf(),
        args,
        filter.is_some().then_some(filter_path.as_path()),
    );
    let out = std::process::Command::new(program)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("bwrap: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
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
    let avail = probe(&spec.shim);
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
    let disable_userns = trial(&bwrap, &spec.shim, true).is_ok();
    if !disable_userns {
        gaps.push(
            "nested user namespaces not blocked: this bwrap lacks a working --disable-userns (needs 0.8+)"
                .into(),
        );
    }
    // The filter file sits in the private socket directory.
    let filter = seccomp::filter_bytes();
    let filter_path = sock_dir.join("seccomp.bpf");
    match &filter {
        Some(bytes) => std::fs::write(&filter_path, bytes)?,
        None => gaps.push(
            "socket families not restricted: no seccomp filter for this CPU architecture".into(),
        ),
    }
    let layout = Layout {
        disable_userns,
        seccomp: filter.is_some(),
        ro: &spec.ro,
        rw: &spec.rw,
        temp: &spec.temp,
        shim: &spec.shim.program,
        sock_dir: &sock_dir,
        env: &env,
        cwd: &spec.cwd,
    };
    let mut command = vec![spec.shim.program.to_string_lossy().into_owned()];
    command.extend(spec.shim.prefix_args.iter().cloned());
    command.push("listen".into());
    command.extend(["--port".into(), port.to_string()]);
    if spec.proxy.is_some() {
        command.extend(["--unix".into(), format!("{SOCK_DIR_INSIDE}/proxy.sock")]);
    } else {
        command.extend(["--tcp".into(), "127.0.0.1:9".into()]);
    }
    command.push("--".into());
    command.push(spec.program.to_string_lossy().into_owned());
    command.extend(super::build_args(spec, spec.proxy.as_ref().map(|_| port)));
    let args = bwrap_args(&layout, &command);

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
    let (program, full) = with_filter_fd(
        program,
        full,
        filter.is_some().then_some(filter_path.as_path()),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn layout<'a>(ro: &'a [PathBuf], rw: &'a [PathBuf], env: &'a [(String, String)]) -> Layout<'a> {
        Layout {
            disable_userns: true,
            seccomp: true,
            ro,
            rw,
            temp: Path::new("/t"),
            shim: Path::new("/bin/han"),
            sock_dir: Path::new("/s"),
            env,
            cwd: Path::new("/w"),
        }
    }

    #[test]
    fn the_probe_and_the_launch_differ_only_in_the_command() {
        // Old probe: a hand-written `--unshare-all --ro-bind / / true` with no `--proc`, seccomp or
        // `--disable-userns`, so a unit that breaks `--proc` passed admission and failed the launch.
        let ro = [PathBuf::from("/r")];
        let rw = [PathBuf::from("/w")];
        let l = layout(&ro, &rw, &[]);
        let probe = bwrap_args(&l, &["true".to_owned()]);
        let launch = bwrap_args(
            &l,
            &["/bin/han".to_owned(), "shim".to_owned(), "agent".to_owned()],
        );
        let cut = |a: &[String]| a.iter().position(|x| x == "--").expect("a separator");
        assert_eq!(probe[..cut(&probe)], launch[..cut(&launch)]);
        assert_eq!(probe[cut(&probe) + 1..], ["true".to_owned()]);
        let has = |w: &[&str]| {
            probe
                .windows(w.len())
                .any(|x| x.iter().map(String::as_str).eq(w.iter().copied()))
        };
        assert!(has(&["--proc", "/proc"]));
        assert!(has(&["--seccomp", "3"]));
        assert!(has(&["--unshare-user", "--disable-userns"]));
        assert!(has(&["--bind", "/s", SOCK_DIR_INSIDE]));
    }
}
