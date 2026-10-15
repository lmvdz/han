//! Git inside the sandbox on a workspace that borrows its objects (alternates) from the device's shared store.
//!
//! Bug: the store the alternates file names was not bound into the sandbox when the host picked up an existing
//! worktree (a restarted host has no `Workspace::shared_store`), so every git command in the session failed with
//! `fatal: bad object HEAD`. The store is bound read-only at its own path: `git status`, `git diff` and
//! `git commit` work (the commit's objects land in the worktree's own `.git/objects`), the agent cannot write
//! into the shared store, and the host's bundle for the executor (R15) still carries the commit. A backend that is
//! missing skips with a message.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use han_host::glue::store_binds;
use han_host::sandbox::{self, Limits, Preference, SandboxSpec, ShimCommand};
use han_node::api::SandboxBackend;
use han_vcs::{GitVcs, Vcs, Workspace, WorkspaceSpec};
use tokio::io::AsyncReadExt;

mod production_shim;

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

/// The test's own git, with no user or system configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.autocrlf=false",
            "-c",
            "safe.directory=*",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    vcs: GitVcs,
    ws: Workspace,
    /// The session's temp directory, as the host lays it out.
    temp: PathBuf,
}

/// A workspace made by the backend (alternates into its shared store), as a host that resumes it sees it: with
/// no `shared_store`.
fn fixture() -> Fixture {
    let tmp = tempfile::Builder::new()
        .prefix("han-gitsb-")
        .tempdir()
        .unwrap();
    let root = tmp.path().to_path_buf();
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q"]);
    std::fs::write(seed.join("README.md"), "hello\nworld\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "c1"]);
    git(&root, &["clone", "-q", "--bare", "seed", "remote.git"]);
    let vcs = GitVcs::new(root.join("objects")).with_protocols(["https", "file"]);
    let session = root.join("session");
    let made = vcs
        .create_workspace(&WorkspaceSpec {
            source: root.join("remote.git").to_string_lossy().into_owned(),
            base: "main".into(),
            branch: "han/1-a".into(),
            dir: session.join("wt"),
            sparse: vec![],
        })
        .unwrap();
    let temp = session.join("tmp");
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::create_dir_all(session.join("agent")).unwrap();
    let ws = Workspace {
        shared_store: None,
        ..made
    };
    Fixture {
        _tmp: tmp,
        vcs,
        ws,
        temp,
    }
}

/// The path as git writes it in the alternates file.
fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Runs `script` in the shell of the OS, inside `backend`, in the worktree, with `ro` readable; returns the
/// exit code and the output.
async fn run(
    backend: Preference,
    f: &Fixture,
    ro: Vec<PathBuf>,
    script: &str,
) -> (Option<i32>, String) {
    let (program, args, path) = if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        (
            PathBuf::from(format!("{root}\\System32\\cmd.exe")),
            vec!["/c".to_owned(), script.to_owned()],
            std::env::var("PATH").unwrap_or_default(),
        )
    } else {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_owned(), script.to_owned()],
            "/usr/local/bin:/usr/bin:/bin".to_owned(),
        )
    };
    #[cfg(windows)]
    let ro = {
        let mut ro = ro;
        ro.extend(which_git());
        ro
    };
    let spec = SandboxSpec {
        session: "gitsb".into(),
        backend,
        program,
        args,
        cwd: f.ws.dir.clone(),
        env: vec![
            ("PATH".into(), path),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ],
        rw: vec![f.ws.dir.clone()],
        ro,
        temp: f.temp.clone(),
        proxy: None,
        allow_advisory: false,
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
    let mut out = sb.stdout.take().unwrap();
    let mut err = sb.stderr.take().unwrap();
    drop(sb.stdin.take());
    let (mut text, mut etext) = (String::new(), String::new());
    let read = async {
        let _ = out.read_to_string(&mut text).await;
        let _ = err.read_to_string(&mut etext).await;
    };
    tokio::time::timeout(Duration::from_secs(120), read)
        .await
        .expect("the script finished");
    let code = sb.wait().await;
    text.push_str(&etext);
    eprintln!("{backend:?} exit {code:?}\n{text}");
    (code, text)
}

/// The directory of git on this machine (Windows: where MXC must be allowed to read it).
#[cfg(windows)]
fn which_git() -> Result<PathBuf, ()> {
    let out = Command::new("where").arg("git").output().map_err(|_| ())?;
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(PathBuf::from)
        .ok_or(())?;
    // `<git>/cmd/git.exe` or `<git>/bin/git.exe`: the install is two levels up
    first
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or(())
}

const GIT: &str = "git -c user.name=Agent -c user.email=a@example.invalid -c safe.directory=*";

async fn confines_git(backend: Preference) {
    let f = fixture();
    let ro = store_binds(&f.ws);
    let store = std::fs::read_to_string(f.ws.dir.join(".git/objects/info/alternates")).unwrap();
    let store = PathBuf::from(store.trim());
    assert!(
        ro.iter().any(|p| p == &store || store.starts_with(p)),
        "the store the alternates file names ({}) is bound: {ro:?}",
        store.display()
    );

    // The old behaviour, as a control: with the store not bound, git cannot read HEAD.
    let (code, out) = run(backend, &f, vec![], &format!("{GIT} status")).await;
    assert_ne!(code, Some(0), "git ran without the store: {out}");
    assert!(
        out.contains("bad object") || out.contains("fatal"),
        "git failed for another reason: {out}"
    );

    // With the store bound read-only: status, diff and commit work.
    let sh_and = "&&";
    let script = format!(
        "{GIT} status --porcelain {sh_and} echo changed>> README.md {sh_and} {GIT} diff --stat {sh_and} \
         {GIT} commit -q -am from-the-sandbox {sh_and} {GIT} log --oneline"
    );
    let (code, out) = run(backend, &f, ro.clone(), &script).await;
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("README.md"), "the diff: {out}");
    assert!(out.contains("from-the-sandbox"), "the log: {out}");

    // The agent cannot write into the shared store.
    let probe = store.join("pwned");
    let write = if cfg!(windows) {
        format!("echo x> \"{}\"", s(&probe))
    } else {
        format!("echo x > '{}'", s(&probe))
    };
    let (code, out) = run(backend, &f, ro, &write).await;
    assert_ne!(code, Some(0), "wrote into the shared store: {out}");
    assert!(!probe.exists(), "the shared store was written");

    // The commit's objects are in the worktree's own store, not the shared one, and the host bundles it for
    // the executor (R15) as before.
    let head = git(&f.ws.dir, &["rev-parse", "HEAD"]);
    assert_ne!(head, f.ws.base);
    let shared = store.parent().unwrap().to_path_buf();
    let in_shared = Command::new("git")
        .current_dir(&shared)
        .args(["cat-file", "-e", &head])
        .status()
        .unwrap();
    assert!(
        !in_shared.success(),
        "the commit landed in the shared store"
    );
    let bundle = f.vcs.bundle(&f.ws.dir, &f.ws.base, &head).unwrap();
    assert!(!bundle.is_empty());
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn git_works_in_bwrap_on_a_workspace_with_alternates() {
    if !available(SandboxBackend::Bwrap) {
        return;
    }
    confines_git(Preference::Bwrap).await;
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn git_works_in_mxc_on_a_workspace_with_alternates() {
    if !available(SandboxBackend::Mxc) {
        return;
    }
    confines_git(Preference::Mxc).await;
}

/// The binds come from the alternates file, whatever made the worktree: relative entries resolve against the
/// worktree's `.git/objects`, a chain of alternates is followed and a store inside the worktree is not bound
/// (it is writable already).
#[test]
fn the_binds_follow_the_alternates_file() {
    let f = fixture();
    let ro = store_binds(&f.ws);
    assert_eq!(ro.len(), 1, "{ro:?}");
    assert!(ro[0].ends_with("objects"), "{ro:?}");

    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a/objects"), d.path().join("b/objects"));
    let wt = d.path().join("wt");
    for p in [&a, &b, &wt.join(".git/objects/info"), &a.join("info")] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(
        wt.join(".git/objects/info/alternates"),
        format!("# a comment\n../../../a/objects\n\n{}\n", wt.display()),
    )
    .unwrap();
    std::fs::write(a.join("info/alternates"), format!("{}\n", b.display())).unwrap();
    let ws = Workspace {
        dir: wt,
        base: String::new(),
        branch: String::new(),
        shared_store: None,
    };
    let ro = store_binds(&ws);
    assert_eq!(ro.len(), 2, "{ro:?}");
    assert!(ro.iter().any(|p| p.ends_with("a/objects")), "{ro:?}");
    assert!(ro.contains(&b), "{ro:?}");
}
