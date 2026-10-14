//! Language toolchains inside the agent's sandbox, so an agent can build and test before it proposes (PLAN
//! §6.4 "Toolchains").
//!
//! A node declares which toolchains an agent gets (`agents.json`: `toolchains`, `toolchainPaths`). For each
//! one this module returns a [`Plan`]: the read-only roots to grant, the writable per-agent and per-session
//! directories (package caches, build output), the environment, the `PATH` directories and the hosts the
//! egress proxy must also allow. The sandbox backends only see paths and environment, so a new toolchain is
//! one more arm in [`plan_one`] and [`hosts_of`].
//!
//! Only **Rust** exists so far (rustup and cargo):
//!
//! * `RUSTUP_HOME` and the directory with rustup's `cargo`/`rustc` proxies are granted **read-only**. Nothing
//!   is ever installed inside the sandbox (`RUSTUP_AUTO_INSTALL=0`): a toolchain pinned by the repository's
//!   `rust-toolchain.toml` must already be installed, and the session fails to start with a message that
//!   says which one is missing.
//! * `CARGO_HOME` is a **writable per-agent** directory (registry and git caches). The proxies are found on
//!   `PATH`, not through `CARGO_HOME/bin`.
//! * `CARGO_TARGET_DIR` is a writable per-session directory next to the worktree (not in it, so build output
//!   is not mirrored or proposed).
//! * Hosts: `crates.io`, `static.crates.io`, `index.crates.io`, and only while the toolchain is enabled.
//! * Windows (MSVC): the linker and the Windows SDK libraries are discovered and granted read-only, and
//!   `PATH`, `LIB` and the linker are set so rustc needs neither the registry nor COM to find them. Without
//!   MSVC, the GNU toolchain is the fallback (it needs no grants beyond the toolchain's own directory).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The toolchains this host knows.
pub const KNOWN: &[&str] = &["rust"];

/// Which toolchains an agent gets and where they live: the node's typed value, as it arrives.
pub use han_registry::ToolchainSpec as ToolchainConfig;

/// What the sandbox can do, which decides what a toolchain must be given. One value per backend; every
/// backend-specific grant or step of a toolchain is a branch on it in that toolchain's planner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// MXC: no path canonicalization (cargo cannot unpack downloads, so the host prefetches), and an MSVC
    /// linker and Windows SDK to grant.
    Mxc,
    /// bubblewrap: the system directories are already bound; nothing more.
    Bwrap,
    /// No sandbox: advisory, nothing to grant.
    None,
}

impl Target {
    /// The target of a resolved sandbox preference.
    pub fn of(p: crate::sandbox::Preference) -> Target {
        use crate::sandbox::Preference;
        match p {
            Preference::Mxc => Target::Mxc,
            Preference::Bwrap => Target::Bwrap,
            _ => Target::None,
        }
    }
}

/// Checks the names; the error lists the known ones.
pub fn validate(names: &[String]) -> Result<(), String> {
    for n in names {
        if !KNOWN.contains(&n.as_str()) {
            return Err(format!(
                "unknown toolchain {n:?}; known: {}",
                KNOWN.join(", ")
            ));
        }
    }
    Ok(())
}

/// What the enabled toolchains add to a session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Read-only grants.
    pub ro: Vec<PathBuf>,
    /// Writable directories (created).
    pub rw: Vec<PathBuf>,
    /// Environment, over the driver's.
    pub env: Vec<(String, String)>,
    /// Directories put in front of `PATH`.
    pub path: Vec<PathBuf>,
    /// Hosts the egress proxy allows in addition.
    pub allow_hosts: Vec<String>,
    /// Limits worth showing in status.
    pub gaps: Vec<String>,
    /// Steps the host runs **before** the sandbox starts, through the session's proxy, for backends whose
    /// sandbox cannot do them itself (see [`prewarm`]).
    pub prewarm: Vec<Prewarm>,
}

/// A command the host runs outside the sandbox to fill a cache the sandbox then reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prewarm {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// Its environment (nothing else of the host's is passed, apart from what the OS needs to start it).
    pub env: Vec<(String, String)>,
    /// The directory it runs in: never the worktree, so the repository's own `.cargo/config.toml` is not read.
    pub cwd: PathBuf,
}

/// The hosts the proxy allows because of the enabled toolchains (and nothing else).
pub fn allow_hosts(cfg: &ToolchainConfig) -> Vec<String> {
    let mut hosts: Vec<String> = KNOWN
        .iter()
        .filter(|n| cfg.on(n))
        .flat_map(|n| hosts_of(n).iter().map(|h| (*h).to_owned()))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

fn hosts_of(name: &str) -> &'static [&'static str] {
    match name {
        "rust" => &["crates.io", "static.crates.io", "index.crates.io"],
        _ => &[],
    }
}

/// Plans every enabled toolchain for a session whose files live in `session_dir` and whose worktree is
/// `worktree`. Fails, with a message for the operator, when something the toolchain needs is missing.
pub fn plan(
    cfg: &ToolchainConfig,
    target: Target,
    session_dir: &Path,
    worktree: &Path,
) -> Result<Plan, String> {
    validate(&cfg.enabled)?;
    let mut out = Plan::default();
    for name in KNOWN.iter().filter(|n| cfg.on(n)) {
        plan_one(name, cfg, target, session_dir, worktree, &mut out)?;
    }
    out.allow_hosts = allow_hosts(cfg);
    for d in &out.rw {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    Ok(out)
}

fn plan_one(
    name: &str,
    cfg: &ToolchainConfig,
    target: Target,
    session_dir: &Path,
    worktree: &Path,
    out: &mut Plan,
) -> Result<(), String> {
    match name {
        "rust" => plan_rust(cfg, target, session_dir, worktree, out),
        other => Err(format!("unknown toolchain {other:?}")),
    }
}

/// Puts the plan into `env`: its variables, and its directories in front of `PATH` (the default system
/// `PATH` if the environment has none).
pub fn apply(plan: &Plan, env: &mut Vec<(String, String)>) {
    for (k, v) in &plan.env {
        env.retain(|(x, _)| !x.eq_ignore_ascii_case(k));
        env.push((k.clone(), v.clone()));
    }
    if plan.path.is_empty() {
        return;
    }
    let existing = env
        .iter()
        .position(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|i| env.remove(i).1)
        .unwrap_or_else(default_path);
    let mut parts: Vec<OsString> = plan
        .path
        .iter()
        .map(|p| p.clone().into_os_string())
        .collect();
    parts.extend(std::env::split_paths(&existing).map(PathBuf::into_os_string));
    let joined = std::env::join_paths(parts)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(existing);
    env.push(("PATH".into(), joined));
}

/// Runs the plan's [`Plan::prewarm`] steps on the host, with only the session's proxy as a way out (the same
/// allow-list as the sandbox), and returns one status gap for each that fails. Needed under MXC, where
/// `std::fs::canonicalize` is denied to an AppContainer and so cargo cannot unpack a crate it downloads; the
/// sandbox then finds the sources already in its `CARGO_HOME`. The steps only download (`cargo fetch` runs no
/// build script and no code of the repository).
pub async fn prewarm(plan: &Plan, proxy: std::net::SocketAddr, token: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    for step in &plan.prewarm {
        let url = format!("http://{}:{token}@{proxy}", crate::proxy::PROXY_USER);
        let mut cmd = tokio::process::Command::new(&step.program);
        cmd.args(&step.args)
            .current_dir(&step.cwd)
            .env_clear()
            .envs(step.env.iter().map(|(k, v)| (k, v)))
            .env("HTTPS_PROXY", &url)
            .env("https_proxy", &url)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if cfg!(windows) {
            for k in ["SystemRoot", "windir", "SystemDrive", "ComSpec", "PATHEXT"] {
                if let Ok(v) = std::env::var(k) {
                    cmd.env(k, v);
                }
            }
        }
        let name = step
            .program
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let run = tokio::time::timeout(std::time::Duration::from_secs(600), cmd.output()).await;
        match run {
            Ok(Ok(o)) if o.status.success() => {}
            Ok(Ok(o)) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let last = err
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                gaps.push(format!(
                    "{name} prefetch failed ({}): {}; the sandbox cannot unpack new packages",
                    o.status,
                    last.trim().chars().take(200).collect::<String>()
                ));
            }
            Ok(Err(e)) => gaps.push(format!("{name} prefetch could not start: {e}")),
            Err(_) => gaps.push(format!("{name} prefetch timed out")),
        }
    }
    gaps
}

fn default_path() -> String {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        format!("{root}\\System32;{root}")
    } else {
        "/usr/local/bin:/usr/bin:/bin".into()
    }
}

// ───────────────────────── Rust ─────────────────────────

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The first `PATH` directory the plan added (rustup's proxies).
fn cargo_bin_of(out: &Plan) -> PathBuf {
    out.path.first().cloned().unwrap_or_default()
}

fn plan_rust(
    cfg: &ToolchainConfig,
    target: Target,
    session_dir: &Path,
    worktree: &Path,
    out: &mut Plan,
) -> Result<(), String> {
    let rustup_home = cfg
        .path("rust.rustupHome")
        .or_else(|| std::env::var_os("RUSTUP_HOME").map(PathBuf::from))
        .or_else(|| home_dir().map(|h| h.join(".rustup")))
        .ok_or("rust toolchain: cannot tell where rustup is; set toolchainPaths.rust.rustupHome")?;
    let cargo_bin = cfg
        .path("rust.cargoBin")
        .or_else(|| std::env::var_os("CARGO_HOME").map(|c| PathBuf::from(c).join("bin")))
        .or_else(|| home_dir().map(|h| h.join(".cargo").join("bin")))
        .ok_or("rust toolchain: cannot tell where cargo is; set toolchainPaths.rust.cargoBin")?;
    let toolchains = rustup_home.join("toolchains");
    if !toolchains.is_dir() {
        return Err(format!(
            "rust toolchain is enabled but {} has no toolchains; install rustup there or set \
             toolchainPaths.rust.rustupHome",
            rustup_home.display()
        ));
    }
    let cargo = cargo_bin.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
    if !cargo.is_file() {
        return Err(format!(
            "rust toolchain is enabled but {} does not exist; set toolchainPaths.rust.cargoBin to the \
             directory with rustup's cargo",
            cargo.display()
        ));
    }
    // The repository's pin must be installed already: the sandbox is read-only here and never downloads.
    if let Some(channel) = pinned_channel(worktree) {
        let installed = installed_toolchains(&toolchains);
        if !channel_installed(&channel, &installed) {
            return Err(format!(
                "the repository pins Rust toolchain {channel:?} (rust-toolchain.toml) but it is not \
                 installed in {}; install it outside the sandbox (rustup toolchain install {channel}). \
                 Installed: {}",
                rustup_home.display(),
                if installed.is_empty() {
                    "none".to_owned()
                } else {
                    installed.join(", ")
                }
            ));
        }
    }

    let cargo_home = cfg
        .cargo_home
        .clone()
        .unwrap_or_else(|| session_dir.join("cargo-home"));
    let build_dir = session_dir.join("target");
    out.ro.push(rustup_home.clone());
    out.ro.push(cargo_bin.clone());
    out.rw.push(cargo_home.clone());
    out.rw.push(build_dir.clone());
    out.path.push(cargo_bin);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    out.env.extend([
        ("RUSTUP_HOME".into(), s(&rustup_home)),
        // Never download a toolchain: a missing one is an error naming it.
        ("RUSTUP_AUTO_INSTALL".into(), "0".into()),
        ("CARGO_HOME".into(), s(&cargo_home)),
        ("CARGO_TARGET_DIR".into(), s(&build_dir)),
        // Certificate revocation checks (schannel) would reach hosts the proxy denies.
        ("CARGO_HTTP_CHECK_REVOKE".into(), "false".into()),
    ]);
    if target == Target::Mxc {
        plan_msvc(cfg, session_dir, out);
    }
    // Dependencies of the repository, fetched by the host through the proxy (used under MXC).
    let manifest = worktree.join("Cargo.toml");
    if target == Target::Mxc && manifest.is_file() && worktree.join("Cargo.lock").is_file() {
        let scratch = session_dir.join("prewarm");
        let mut env = out.env.clone();
        env.retain(|(k, _)| k != "CARGO_TARGET_DIR");
        let sys = default_path();
        let path = std::env::join_paths(
            std::iter::once(cargo_bin_of(out)).chain(std::env::split_paths(&sys)),
        )
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(sys);
        env.push(("PATH".into(), path));
        out.rw.push(scratch.clone());
        out.prewarm.push(Prewarm {
            program: cargo,
            args: vec![
                "fetch".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest.to_string_lossy().into_owned(),
            ],
            env,
            cwd: scratch,
        });
    }
    Ok(())
}

/// The channel a repository pins, from `rust-toolchain.toml` or the older `rust-toolchain`.
pub fn pinned_channel(worktree: &Path) -> Option<String> {
    for name in ["rust-toolchain.toml", "rust-toolchain"] {
        let Ok(text) = std::fs::read_to_string(worktree.join(name)) else {
            continue;
        };
        if let Ok(v) = text.parse::<toml::Table>() {
            return v
                .get("toolchain")
                .and_then(|t| t.get("channel"))
                .and_then(|c| c.as_str())
                .map(str::to_owned);
        }
        // The legacy form is one bare channel name.
        let t = text.trim();
        if !t.is_empty() && !t.contains(char::is_whitespace) {
            return Some(t.to_owned());
        }
    }
    None
}

fn installed_toolchains(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// `1.97.1` matches `1.97.1-x86_64-pc-windows-msvc`; a full name matches itself. A channel naming a
/// directory (a custom toolchain path) is not checked.
fn channel_installed(channel: &str, installed: &[String]) -> bool {
    installed
        .iter()
        .any(|n| n == channel || n.strip_prefix(channel).is_some_and(|r| r.starts_with('-')))
}

// ───────────────────────── Windows: MSVC linker and SDK ─────────────────────────

/// The highest-versioned subdirectory of `dir` for which `ok` holds.
fn newest_child(dir: &Path, ok: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut kids: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && ok(p))
        .map(|p| {
            let v = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .split('.')
                .map(|x| x.parse().unwrap_or(0))
                .collect();
            (v, p)
        })
        .collect();
    kids.sort();
    kids.pop().map(|(_, p)| p)
}

/// The MSVC tools directory (`...\VC\Tools\MSVC\<version>`) with an x64 linker and libraries.
pub fn find_msvc(program_files: &[PathBuf]) -> Option<PathBuf> {
    let has_linker =
        |p: &Path| p.join("bin/Hostx64/x64/link.exe").is_file() && p.join("lib/x64").is_dir();
    for pf in program_files {
        let vs = pf.join("Microsoft Visual Studio");
        let mut best: Option<PathBuf> = None;
        // <year>\<edition>, plus the Build Tools layouts.
        for year in std::fs::read_dir(&vs).into_iter().flatten().flatten() {
            for edition in std::fs::read_dir(year.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Some(v) = newest_child(&edition.path().join("VC/Tools/MSVC"), has_linker) {
                    best = Some(v);
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// The Windows SDK library directory (`...\Windows Kits\10\Lib\<version>`) with `um` and `ucrt` for x64.
pub fn find_sdk_lib(program_files: &[PathBuf]) -> Option<PathBuf> {
    program_files.iter().find_map(|pf| {
        newest_child(&pf.join("Windows Kits/10/Lib"), |p| {
            p.join("um/x64").is_dir() && p.join("ucrt/x64").is_dir()
        })
    })
}

fn plan_msvc(cfg: &ToolchainConfig, session_dir: &Path, out: &mut Plan) {
    let pfs: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
        .collect();
    let msvc = cfg.path("rust.msvcDir").or_else(|| find_msvc(&pfs));
    let sdk = cfg.path("rust.sdkLibDir").or_else(|| find_sdk_lib(&pfs));
    let (Some(msvc), Some(sdk)) = (msvc, sdk) else {
        out.gaps.push(
            "rust: the MSVC linker or the Windows SDK was not found; builds that link fail unless the \
             GNU toolchain is the default (set toolchainPaths.rust.msvcDir and rust.sdkLibDir)"
                .into(),
        );
        return;
    };
    let bin = msvc.join("bin/Hostx64/x64");
    let lib = [
        msvc.join("lib/x64"),
        sdk.join("ucrt/x64"),
        sdk.join("um/x64"),
    ];
    // `link.exe` keeps its scratch file in `%TEMP%`, and MXC points that at a fresh per-launch directory
    // that does not exist yet. A one-line wrapper makes it first.
    let tools = session_dir.join("toolchain");
    let wrapper = tools.join("link.cmd");
    let text = format!(
        "@echo off
if not exist \"%TEMP%\\\" mkdir \"%TEMP%\" >nul 2>&1
\"{}\" %*
exit /b %ERRORLEVEL%
",
        bin.join("link.exe").display()
    );
    if let Err(e) = std::fs::create_dir_all(&tools).and_then(|()| std::fs::write(&wrapper, text)) {
        out.gaps
            .push(format!("rust: cannot write the linker wrapper: {e}"));
        return;
    }
    out.ro.push(msvc);
    out.ro.push(sdk);
    out.ro.push(tools);
    out.path.push(bin.clone());
    let lib_s = std::env::join_paths(&lib)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    out.env.extend([
        ("LIB".into(), lib_s),
        // Tells rustc a developer prompt set the linker environment: it runs plain `link.exe` from PATH
        // instead of asking the registry and COM, neither of which a container can use.
        ("VSCMD_ARG_TGT_ARCH".into(), "x64".into()),
        (
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER".into(),
            wrapper.to_string_lossy().into_owned(),
        ),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn rust_adds_exactly_the_crates_hosts() {
        let none = ToolchainConfig::default();
        assert!(allow_hosts(&none).is_empty());
        let rust = ToolchainConfig {
            enabled: vec!["rust".into()],
            ..Default::default()
        };
        assert_eq!(
            allow_hosts(&rust),
            ["crates.io", "index.crates.io", "static.crates.io"]
        );
    }

    #[test]
    fn unknown_names_are_refused() {
        assert!(validate(&["rust".into()]).is_ok());
        assert!(
            validate(&["cobol".into()])
                .unwrap_err()
                .contains("known: rust")
        );
    }

    #[test]
    fn the_pin_is_read_and_matched_by_prefix() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(pinned_channel(d.path()), None);
        std::fs::write(
            d.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.97.1\"\ncomponents = [\"clippy\"]\n",
        )
        .unwrap();
        assert_eq!(pinned_channel(d.path()).as_deref(), Some("1.97.1"));
        let inst = vec![
            "1.97.1-x86_64-pc-windows-msvc".to_owned(),
            "stable-x86_64-pc-windows-msvc".to_owned(),
        ];
        assert!(channel_installed("1.97.1", &inst));
        assert!(channel_installed("stable", &inst));
        assert!(!channel_installed("1.97", &inst));
        assert!(!channel_installed("1.9", &inst));
        assert!(!channel_installed("nightly", &inst));
    }

    #[test]
    fn a_missing_pin_fails_with_the_name_and_what_is_installed() {
        let d = tempfile::tempdir().unwrap();
        let rustup = d.path().join("rustup");
        std::fs::create_dir_all(rustup.join("toolchains/1.90.0-x86_64-unknown-linux-gnu")).unwrap();
        let bin = d.path().join("bin");
        touch(&bin.join(if cfg!(windows) { "cargo.exe" } else { "cargo" }));
        let wt = d.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.97.1\"\n",
        )
        .unwrap();
        let cfg = ToolchainConfig {
            enabled: vec!["rust".into()],
            paths: [
                ("rust.rustupHome".to_owned(), rustup),
                ("rust.cargoBin".to_owned(), bin),
            ]
            .into(),
            cargo_home: None,
        };
        let err = plan(&cfg, Target::Bwrap, &d.path().join("session"), &wt).unwrap_err();
        assert!(
            err.contains("1.97.1") && err.contains("not installed"),
            "{err}"
        );
        assert!(err.contains("1.90.0-x86_64-unknown-linux-gnu"), "{err}");
        // With the pin installed the plan has the read-only roots and writable cache and target.
        std::fs::create_dir_all(
            cfg.paths["rust.rustupHome"].join("toolchains/1.97.1-x86_64-unknown-linux-gnu"),
        )
        .unwrap();
        let p = plan(&cfg, Target::Bwrap, &d.path().join("session"), &wt).unwrap();
        assert!(p.ro.contains(&cfg.paths["rust.rustupHome"]));
        assert!(p.rw.contains(&d.path().join("session/cargo-home")));
        assert!(p.env.contains(&("RUSTUP_AUTO_INSTALL".into(), "0".into())));
        assert_eq!(p.allow_hosts.len(), 3);
        // CARGO_HOME is the per-agent directory, never the read-only one.
        let ch = p.env.iter().find(|(k, _)| k == "CARGO_HOME").unwrap();
        assert!(ch.1.ends_with("cargo-home"));
    }

    #[test]
    fn apply_puts_directories_in_front_of_path() {
        let plan = Plan {
            path: vec![PathBuf::from("/opt/rust/bin")],
            env: vec![("CARGO_HOME".into(), "/c".into())],
            ..Default::default()
        };
        let mut env = vec![("Path".to_owned(), "/usr/bin".to_owned())];
        apply(&plan, &mut env);
        let path = &env.iter().find(|(k, _)| k == "PATH").unwrap().1;
        assert!(path.starts_with("/opt/rust/bin"), "{path}");
        assert!(path.contains("/usr/bin"));
        assert_eq!(
            env.iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("path"))
                .count(),
            1
        );
    }

    #[test]
    fn msvc_and_sdk_pick_the_newest_complete_version() {
        let d = tempfile::tempdir().unwrap();
        let pf = d.path().to_path_buf();
        for v in ["14.38.33130", "14.44.35207"] {
            let base = pf.join(format!(
                "Microsoft Visual Studio/2022/Pro/VC/Tools/MSVC/{v}"
            ));
            touch(&base.join("bin/Hostx64/x64/link.exe"));
            std::fs::create_dir_all(base.join("lib/x64")).unwrap();
        }
        // An incomplete newer one is skipped.
        std::fs::create_dir_all(
            pf.join("Microsoft Visual Studio/2022/Pro/VC/Tools/MSVC/14.50.00000"),
        )
        .unwrap();
        for v in ["10.0.22621.0", "10.0.26100.0"] {
            let base = pf.join(format!("Windows Kits/10/Lib/{v}"));
            std::fs::create_dir_all(base.join("um/x64")).unwrap();
            std::fs::create_dir_all(base.join("ucrt/x64")).unwrap();
        }
        let m = find_msvc(std::slice::from_ref(&pf)).unwrap();
        assert!(m.ends_with("14.44.35207"), "{m:?}");
        let s = find_sdk_lib(std::slice::from_ref(&pf)).unwrap();
        assert!(s.ends_with("10.0.26100.0"), "{s:?}");
    }
}
