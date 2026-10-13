//! Launching an agent's whole process tree inside a sandbox (PLAN §0.4, §6.4; SR-23…SR-34).
//!
//! | Backend | OS | Filesystem | Network | DNS | Limits |
//! |---|---|---|---|---|---|
//! | MXC ProcessContainer | Windows 11 (no admin) | rw = worktree and temp, ro = listed paths, everything else denied | egress denied except one container-local loopback port: the in-sandbox shim, which forwards to a per-session named pipe bridged to the proxy | **resolves** (the host DNS client answers; SR-27 gap) | wall time only |
//! | bubblewrap | Linux | a new root: system dirs read-only, listed paths ro/rw, private `/tmp`, no home | a new network namespace with only `lo`: the shim forwards to a bind-mounted unix socket bridged to the proxy | blocked (no resolver reachable) | address space through `prlimit` if present; wall time |
//! | none | any | advisory | advisory (proxy settings only) | advisory | wall time |
//!
//! The backend, its fidelity and every known gap are returned in [`SandboxInfo`] for the host to publish in
//! status (SR-34, SR-99). Without a sandbox a session starts only when the space allows advisory mode (SR-23).
//!
//! The environment is explicit: nothing of the host's environment is passed except what is listed in the
//! spec, plus the proxy settings, a per-session temp directory and the few variables the OS needs to start a
//! process (SR-33). `LD_PRELOAD`, `LD_LIBRARY_PATH` and `DYLD_*` are always removed (SR-32).
//!
//! Driver notes (not enforced here): a Node-based agent under MXC needs `--preserve-symlinks
//! --preserve-symlinks-main` (an AppContainer cannot stat the worktree's ancestors), must be launched through
//! an `.exe`, never a `.cmd` shim, and finds no working Git Bash or PowerShell inside the container.

#[cfg(target_os = "linux")]
mod bwrap;
#[cfg(windows)]
mod mxc;

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_node::api::{Fidelity, SandboxBackend};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Which backend to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preference {
    /// The best one this OS has: MXC on Windows, bubblewrap on Linux; otherwise none (advisory).
    #[default]
    Auto,
    /// MXC, or fail.
    Mxc,
    /// bubblewrap, or fail.
    Bwrap,
    /// No sandbox (advisory).
    None,
}

/// Resource caps (SR-31). What a backend cannot enforce is listed in [`SandboxInfo::gaps`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Address space of each process, in bytes.
    pub memory_bytes: Option<u64>,
    /// Processes in the tree.
    pub processes: Option<u32>,
    /// Wall time of the whole session process; the tree is killed after it.
    pub wall_time: Option<Duration>,
}

/// The session's proxy, as the host runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyTarget {
    /// The proxy's address on the host (`127.0.0.1:port`).
    pub addr: SocketAddr,
    /// The session token.
    pub token: String,
}

/// What to launch, and how confined.
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    /// The session (names the pipe or socket; letters, digits, `-` and `_` are kept).
    pub session: String,
    /// The backend.
    pub backend: Preference,
    /// The agent program.
    pub program: PathBuf,
    /// Its arguments; they may hold the same placeholders as `env`.
    pub args: Vec<String>,
    /// Its working directory (normally the worktree).
    pub cwd: PathBuf,
    /// Its environment, explicit. Values may hold `{proxy_url}` (`http://127.0.0.1:P` as seen inside, no
    /// credentials), `{proxy_port}` and `{proxy_token}`.
    pub env: Vec<(String, String)>,
    /// Paths it may write: the worktree. The temp directory is added.
    pub rw: Vec<PathBuf>,
    /// Paths it may read (runtimes, the harness, the shim is added).
    pub ro: Vec<PathBuf>,
    /// The per-session temp directory (created if missing; writable).
    pub temp: PathBuf,
    /// The session's proxy; `None` gives no egress at all.
    pub proxy: Option<ProxyTarget>,
    /// Whether the session may run without a sandbox (the space allows advisory mode, SR-23).
    pub allow_advisory: bool,
    /// Resource caps.
    pub limits: Limits,
    /// The `han-agent-shim` program; default: `HAN_AGENT_SHIM`, then next to the current executable.
    pub shim: Option<PathBuf>,
    /// The loopback port the proxy has inside the sandbox; `None` lets the backend choose. A host that writes
    /// config files naming the port before launch picks one with [`pick_inside_port`].
    pub inside_port: Option<u16>,
}

/// A loopback port that is free on this machine now (MXC containers bind in the host's port space).
pub fn pick_inside_port() -> io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// How a session is confined, for status (SR-34).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxInfo {
    /// The backend.
    pub backend: SandboxBackend,
    /// Whether its limits are enforced.
    pub fidelity: Fidelity,
    /// What this backend does not enforce here, in words.
    pub gaps: Vec<String>,
    /// Whether name resolution is blocked inside.
    pub dns_blocked: bool,
    /// The proxy's address as seen inside the sandbox (`127.0.0.1:P`), when it has one.
    pub proxy_inside: Option<SocketAddr>,
}

/// Why a sandbox could not be launched.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The backend is not available on this machine.
    #[error("sandbox backend unavailable: {0}")]
    Unavailable(String),
    /// No sandbox could be established and the space does not allow advisory mode (SR-23).
    #[error("no sandbox is available and advisory mode is not allowed: {0}")]
    AdvisoryNotAllowed(String),
    /// The spec is not usable.
    #[error("invalid sandbox spec: {0}")]
    Invalid(String),
    /// The process could not be started.
    #[error("sandbox launch failed: {0}")]
    Spawn(String),
    /// An I/O error.
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// Whether a backend can be used here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendAvailability {
    /// The backend.
    pub backend: SandboxBackend,
    /// Whether it can launch.
    pub available: bool,
    /// What the probe found.
    pub detail: String,
}

/// The backends this machine offers.
pub fn probe() -> Vec<BackendAvailability> {
    let native: Option<BackendAvailability> = {
        #[cfg(windows)]
        {
            Some(mxc::probe())
        }
        #[cfg(target_os = "linux")]
        {
            Some(bwrap::probe())
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            None
        }
    };
    native
        .into_iter()
        .chain(std::iter::once(BackendAvailability {
            backend: SandboxBackend::None,
            available: true,
            detail: "no confinement: advisory only".into(),
        }))
        .collect()
}

/// A child's stdin.
pub type ChildIn = Box<dyn AsyncWrite + Send + Unpin>;
/// A child's stdout or stderr.
pub type ChildOut = Box<dyn AsyncRead + Send + Unpin>;

/// A launched, confined process tree.
pub struct Sandboxed {
    /// The agent's stdin (take it).
    pub stdin: Option<ChildIn>,
    /// The agent's stdout.
    pub stdout: Option<ChildOut>,
    /// The agent's stderr.
    pub stderr: Option<ChildOut>,
    /// How it is confined.
    pub info: SandboxInfo,
    kill: Arc<dyn Fn() + Send + Sync>,
    exit: watch::Receiver<Option<Option<i32>>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Sandboxed {
    /// Kills the whole tree.
    pub fn kill(&self) {
        (self.kill)();
    }

    /// Waits for the tree to end; its exit code (`None`: killed, or no code).
    pub async fn wait(&mut self) -> Option<i32> {
        loop {
            if let Some(code) = *self.exit.borrow() {
                return code;
            }
            if self.exit.changed().await.is_err() {
                return None;
            }
        }
    }

    /// The exit code if it has ended.
    pub fn try_wait(&self) -> Option<Option<i32>> {
        *self.exit.borrow()
    }
}

impl Drop for Sandboxed {
    fn drop(&mut self) {
        (self.kill)();
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Launches `spec`.
pub async fn launch(spec: SandboxSpec) -> Result<Sandboxed, SandboxError> {
    std::fs::create_dir_all(&spec.temp)?;
    let backend = match spec.backend {
        Preference::Auto => auto_backend(),
        Preference::Mxc => SandboxBackend::Mxc,
        Preference::Bwrap => SandboxBackend::Bwrap,
        Preference::None => SandboxBackend::None,
    };
    let mut s = match backend {
        #[cfg(windows)]
        SandboxBackend::Mxc => mxc::launch(&spec).await?,
        #[cfg(target_os = "linux")]
        SandboxBackend::Bwrap => bwrap::launch(&spec).await?,
        SandboxBackend::None => {
            if !spec.allow_advisory {
                let why = if spec.backend == Preference::None {
                    "the session asked for no sandbox".to_owned()
                } else {
                    probe()
                        .into_iter()
                        .filter(|b| b.backend != SandboxBackend::None)
                        .map(|b| format!("{:?}: {}", b.backend, b.detail))
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                return Err(SandboxError::AdvisoryNotAllowed(why));
            }
            launch_none(&spec).await?
        }
        #[allow(unreachable_patterns)]
        other => {
            return Err(SandboxError::Unavailable(format!(
                "{other:?} is not available on this OS"
            )));
        }
    };
    if let Some(wall) = spec.limits.wall_time {
        let kill = Arc::clone(&s.kill);
        s.tasks.push(tokio::spawn(async move {
            tokio::time::sleep(wall).await;
            kill();
        }));
    }
    Ok(s)
}

fn auto_backend() -> SandboxBackend {
    probe()
        .into_iter()
        .find(|b| b.available)
        .map(|b| b.backend)
        .unwrap_or(SandboxBackend::None)
}

/// The shim program to put inside the sandbox.
pub(crate) fn shim_path(spec: &SandboxSpec) -> Result<PathBuf, SandboxError> {
    if let Some(p) = &spec.shim {
        return Ok(p.clone());
    }
    if let Some(p) = std::env::var_os("HAN_AGENT_SHIM") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| SandboxError::Invalid("no executable directory".into()))?;
    let name = if cfg!(windows) {
        "han-agent-shim.exe"
    } else {
        "han-agent-shim"
    };
    for d in [Some(dir), dir.parent()].into_iter().flatten() {
        let p = d.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(SandboxError::Invalid(format!(
        "cannot find {name}; set HAN_AGENT_SHIM"
    )))
}

/// A random hex string from the OS CSPRNG.
pub(crate) fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A safe fragment of the session id for names.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn name_part(session: &str) -> String {
    let s: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    if s.is_empty() { "s".into() } else { s }
}

/// The environment the agent gets: the spec's entries with placeholders filled, then the proxy settings and
/// temp directory, without loader-injection variables.
pub(crate) fn fill(spec: &SandboxSpec, inside_port: Option<u16>, v: &str) -> String {
    let url = inside_port.map(|p| format!("http://127.0.0.1:{p}"));
    let token = spec
        .proxy
        .as_ref()
        .map(|p| p.token.clone())
        .unwrap_or_default();
    v.replace("{proxy_url}", url.as_deref().unwrap_or(""))
        .replace(
            "{proxy_port}",
            &inside_port.map(|p| p.to_string()).unwrap_or_default(),
        )
        .replace("{proxy_token}", &token)
}

/// The arguments with placeholders filled.
pub(crate) fn build_args(spec: &SandboxSpec, inside_port: Option<u16>) -> Vec<String> {
    spec.args
        .iter()
        .map(|a| fill(spec, inside_port, a))
        .collect()
}

pub(crate) fn build_env(
    spec: &SandboxSpec,
    inside_port: Option<u16>,
    temp_inside: &Path,
) -> Vec<(String, String)> {
    let token = spec
        .proxy
        .as_ref()
        .map(|p| p.token.clone())
        .unwrap_or_default();
    let fill = |v: &str| fill(spec, inside_port, v);
    let mut env: Vec<(String, String)> = Vec::new();
    let set = |k: &str, v: String, env: &mut Vec<(String, String)>| {
        env.retain(|(x, _)| !x.eq_ignore_ascii_case(k));
        env.push((k.to_owned(), v));
    };
    for (k, v) in &spec.env {
        set(k, fill(v), &mut env);
    }
    let proxy_names = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    env.retain(|(k, _)| !proxy_names.contains(&k.as_str()));
    if let Some(p) = inside_port {
        let with_creds = format!("http://{}:{token}@127.0.0.1:{p}", crate::proxy::PROXY_USER);
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env.push((k.to_owned(), with_creds.clone()));
        }
    }
    for k in ["NO_PROXY", "no_proxy"] {
        env.push((k.to_owned(), "127.0.0.1,localhost".into()));
    }
    let t = temp_inside.to_string_lossy().into_owned();
    for k in ["TEMP", "TMP", "TMPDIR"] {
        set(k, t.clone(), &mut env);
    }
    env.retain(|(k, _)| {
        let u = k.to_ascii_uppercase();
        u != "LD_PRELOAD" && u != "LD_LIBRARY_PATH" && u != "LD_AUDIT" && !u.starts_with("DYLD_")
    });
    if !env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")) {
        let path = if cfg!(windows) {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            format!("{root}\\System32;{root}")
        } else {
            "/usr/local/bin:/usr/bin:/bin".into()
        };
        env.push(("PATH".into(), path));
    }
    env
}

/// Bridges each connection on the session channel to the proxy's TCP port.
pub(crate) async fn bridge_one<S>(conn: S, proxy: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    if let Ok(tcp) = tokio::net::TcpStream::connect(proxy).await {
        // The channel side is the client; when the proxy's side ends, the channel is closed (named pipes
        // have no half-close, so close-delimited bodies would otherwise hang).
        let _ = crate::shim::relay(conn, tcp, true).await;
    }
}

/// Starts a tokio child and the tasks that watch it.
pub(crate) fn from_tokio_child(
    mut child: tokio::process::Child,
    info: SandboxInfo,
    mut tasks: Vec<JoinHandle<()>>,
) -> Sandboxed {
    let stdin = child.stdin.take().map(|s| Box::new(s) as ChildIn);
    let stdout = child.stdout.take().map(|s| Box::new(s) as ChildOut);
    let stderr = child.stderr.take().map(|s| Box::new(s) as ChildOut);
    let (tx, rx) = watch::channel(None);
    let stop = Arc::new(tokio::sync::Notify::new());
    let stop2 = Arc::clone(&stop);
    tasks.push(tokio::spawn(async move {
        let code = tokio::select! {
            r = child.wait() => r.ok().and_then(|s| s.code()),
            _ = stop2.notified() => {
                let _ = child.kill().await;
                None
            }
        };
        let _ = tx.send(Some(code));
    }));
    Sandboxed {
        stdin,
        stdout,
        stderr,
        info,
        kill: Arc::new(move || stop.notify_one()),
        exit: rx,
        tasks,
    }
}

async fn launch_none(spec: &SandboxSpec) -> Result<Sandboxed, SandboxError> {
    let port = spec.proxy.as_ref().map(|p| p.addr.port());
    let env = build_env(spec, port, &spec.temp);
    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(build_args(spec, port))
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // Windows needs these to start most programs; they hold no secrets.
        for k in ["SystemRoot", "windir", "SystemDrive", "ComSpec", "PATHEXT"] {
            if !spec.env.iter().any(|(x, _)| x.eq_ignore_ascii_case(k))
                && let Ok(v) = std::env::var(k)
            {
                cmd.env(k, v);
            }
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| SandboxError::Spawn(format!("{}: {e}", spec.program.display())))?;
    let info = SandboxInfo {
        backend: SandboxBackend::None,
        fidelity: Fidelity::Advisory,
        gaps: vec![
            "no sandbox: filesystem, network, process and resource limits are advisory".into(),
            "egress goes to the proxy only if the agent honours the proxy settings".into(),
            "killing reaches the agent process, not necessarily its children".into(),
        ],
        dns_blocked: false,
        proxy_inside: spec.proxy.as_ref().map(|p| p.addr),
    };
    Ok(from_tokio_child(child, info, Vec::new()))
}
