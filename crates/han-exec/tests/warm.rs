//! The warm-runner checks backend on a small temporary cargo workspace: `a`, `b` (depends on `a`) and `c`.

use han_proc::NoWindow as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use std::sync::Arc;

use han_exec::checks::warm::{
    Profile, Scope, StepSpec, WarmConfig, WarmError, WarmEvidence, WarmRequest, WarmRunner,
    scrubbed_env, select_scope,
};
use han_exec::checks::{
    CheckRequest, CheckRun, Checks, ChecksEvidence, RoutedChecks, Stage, Verdict, WarmChecks,
    verdict_of,
};
use han_exec::{ChecksBackend, RepoTarget};
use serde_json::json;

struct Fixture {
    dir: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Fixture {
    fn repo(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .no_window()
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A crate's manifest and source. `extra` is a statement added to its test.
fn lib(name: &str, dep: Option<&str>, extra: &str) -> (String, String) {
    let deps = dep
        .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
        .unwrap_or_default();
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
    );
    let (call, want) = match dep {
        Some(d) => (format!("{d}::id()"), "a".to_string()),
        None => (format!("\"{name}\""), name.to_string()),
    };
    let body = format!(
        "pub fn id() -> &'static str {{\n    {call}\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn ok() {{\n        assert_eq!(super::id(), \"{want}\");\n{extra}    }}\n}}\n"
    );
    (manifest, body)
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(
        r,
        "Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"crates/a\", \"crates/b\", \"crates/c\"]\n",
    );
    let toolchain = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml");
    std::fs::copy(toolchain, r.join("rust-toolchain.toml")).unwrap();
    for (name, dep) in [("a", None), ("b", Some("a")), ("c", None)] {
        let (m, l) = lib(name, dep, "");
        write(r, &format!("crates/{name}/Cargo.toml"), &m);
        write(r, &format!("crates/{name}/src/lib.rs"), &l);
    }
    write(r, ".gitignore", "target/\n");
    write(r, "README.md", "fixture\n");
    git(r, &["init", "-q"]);
    let out = Command::new("cargo")
        .no_window()
        .args(["generate-lockfile"])
        .current_dir(r)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(r, &["add", "-A"]);
    git(r, &["commit", "-q", "-m", "base"]);
    Fixture {
        dir,
        state: tempfile::tempdir().unwrap(),
    }
}

fn head(f: &Fixture) -> String {
    git(&f.repo(), &["rev-parse", "HEAD"])
}

fn commit_file(f: &Fixture, rel: &str, text: &str) -> String {
    write(&f.repo(), rel, text);
    git(&f.repo(), &["add", "-A"]);
    git(&f.repo(), &["commit", "-q", "-m", "change"]);
    head(f)
}

fn runner(f: &Fixture, edit: impl FnOnce(&mut WarmConfig)) -> WarmRunner {
    let mut cfg = WarmConfig::new(f.state.path());
    cfg.step_timeout = Duration::from_secs(600);
    cfg.run_timeout = Duration::from_secs(900);
    edit(&mut cfg);
    WarmRunner::new(cfg).unwrap()
}

fn request(f: &Fixture, commit: &str, scope: Scope, stage: Stage) -> WarmRequest {
    WarmRequest {
        repo: f.repo(),
        lane: "main".into(),
        commit: commit.into(),
        scope,
        stage,
    }
}

fn logs(ev: &WarmEvidence) -> String {
    ev.steps
        .iter()
        .filter_map(|s| s.log.as_ref())
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn cargo_step(name: &str, timeout_secs: Option<u64>) -> StepSpec {
    StepSpec {
        name: name.into(),
        command: ["cargo", "test", "{scope}", "--locked"]
            .map(String::from)
            .to_vec(),
        when_in_scope: None,
        probe: None,
        fallback: None,
        timeout_secs,
    }
}

#[tokio::test]
async fn second_run_rebuilds_only_the_changed_crate() {
    let f = fixture();
    let r = runner(&f, |_| {});
    let base = head(&f);
    let first = r
        .run(request(&f, &base, Scope::Full, Stage::Fast))
        .await
        .unwrap();
    assert!(first.ok, "{:#?}\n{}", first.steps, logs(&first));
    assert_eq!(first.backend, "warm");
    assert_eq!(first.commit, base);
    assert_eq!(
        first.tree,
        git(&f.repo(), &["rev-parse", &format!("{base}^{{tree}}")])
    );
    let first_logs = logs(&first);
    for c in ["a", "b", "c"] {
        assert!(
            first_logs.contains(&format!("Compiling {c} v0.1.0")),
            "{c} built cold:\n{first_logs}"
        );
    }

    // b is a leaf: nothing depends on it
    let (_, body) = lib("b", Some("a"), "        let _ = 1;\n");
    let next = commit_file(&f, "crates/b/src/lib.rs", &body);
    let scope = r.scope_for(&f.repo(), "main", &base, &next).await.unwrap();
    assert_eq!(scope, Scope::Crates(vec!["b".into()]));
    let second = r.run(request(&f, &next, scope, Stage::Fast)).await.unwrap();
    assert!(second.ok, "{:#?}\n{}", second.steps, logs(&second));
    let second_logs = logs(&second);
    assert!(second_logs.contains("Compiling b v0.1.0"), "{second_logs}");
    assert!(
        !second_logs.contains("Compiling a v0.1.0"),
        "a was rebuilt:\n{second_logs}"
    );
    assert!(
        !second_logs.contains("Compiling c v0.1.0"),
        "c was rebuilt:\n{second_logs}"
    );
}

#[tokio::test]
async fn a_failing_test_is_evidence_with_that_step() {
    let f = fixture();
    let r = runner(&f, |_| {});
    let (_, body) = lib("c", None, "        assert_eq!(1, 2);\n");
    let bad = commit_file(&f, "crates/c/src/lib.rs", &body);
    let ev = r
        .run(request(
            &f,
            &bad,
            Scope::Crates(vec!["c".into()]),
            Stage::Fast,
        ))
        .await
        .unwrap();
    assert!(!ev.ok);
    let failed = ev.failed_step().expect("a failing step");
    assert_eq!(failed.name, "test");
    assert_ne!(failed.exit, Some(0));
    assert!(!failed.timed_out);
    // the run stops there: nothing ran after the test
    assert_eq!(ev.steps.last().unwrap().name, "test");
    assert!(failed.log.as_ref().unwrap().exists());
}

#[tokio::test]
async fn a_step_that_overruns_is_killed_and_reported() {
    let f = fixture();
    // warm the build first so the timeout measures the test, not the compile
    let base = head(&f);
    let warm = runner(&f, |c| {
        c.profile = Profile {
            fast: vec![cargo_step("test", None)],
            full: vec![],
        }
    });
    assert!(
        warm.run(request(&f, &base, Scope::Full, Stage::Fast))
            .await
            .unwrap()
            .ok
    );

    let (_, body) = lib(
        "c",
        None,
        "        std::thread::sleep(std::time::Duration::from_secs(300));\n",
    );
    let slow = commit_file(&f, "crates/c/src/lib.rs", &body);
    let r = runner(&f, |c| {
        c.profile = Profile {
            fast: vec![cargo_step("test", Some(25))],
            full: vec![],
        }
    });
    let t = Instant::now();
    let ev = r
        .run(request(
            &f,
            &slow,
            Scope::Crates(vec!["c".into()]),
            Stage::Fast,
        ))
        .await
        .unwrap();
    assert!(!ev.ok);
    let step = ev.failed_step().unwrap();
    assert!(step.timed_out, "{step:?}");
    assert_eq!(step.exit, None);
    assert!(
        t.elapsed() < Duration::from_secs(120),
        "took {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn the_run_timeout_bounds_the_whole_run() {
    let f = fixture();
    // a test that sleeps, so the run outlasts its timeout however fast the machine builds
    let (_, body) = lib(
        "c",
        None,
        "        std::thread::sleep(std::time::Duration::from_secs(300));
",
    );
    let base = commit_file(&f, "crates/c/src/lib.rs", &body);
    let r = runner(&f, |c| {
        c.profile = Profile {
            fast: vec![cargo_step("one", None), cargo_step("two", None)],
            full: vec![],
        };
        c.run_timeout = Duration::from_millis(1500);
    });
    // preparing the tree is inside the run timeout too: either it ran out there or a step was marked
    match r.run(request(&f, &base, Scope::Full, Stage::Fast)).await {
        Ok(ev) => {
            assert!(!ev.ok);
            assert!(ev.failed_step().unwrap().timed_out);
        }
        Err(e) => assert!(matches!(e, WarmError::PrepareTimeout), "{e}"),
    }
}

#[tokio::test]
async fn steps_see_a_scrubbed_environment() {
    let f = fixture();
    let mut host: Vec<(String, String)> = std::env::vars().collect();
    for (k, v) in [
        ("BOAT_API_KEY", "sekret-boat"),
        ("GITHUB_TOKEN", "sekret-gh"),
        ("GH_TOKEN", "sekret-gh2"),
        ("HAN_SIGNING_SEED", "sekret-han"),
        ("AWS_SECRET_ACCESS_KEY", "sekret-aws"),
        ("SOMETHING_ELSE", "sekret-other"),
    ] {
        host.push((k.into(), v.into()));
    }
    let command: Vec<String> = if cfg!(windows) {
        vec!["cmd".into(), "/C".into(), "set".into()]
    } else {
        vec!["env".into()]
    };
    let step = StepSpec {
        name: "env".into(),
        command,
        when_in_scope: None,
        probe: None,
        fallback: None,
        timeout_secs: None,
    };
    let r = runner(&f, |c| {
        c.profile = Profile {
            fast: vec![step],
            full: vec![],
        };
        c.host_env = Some(host);
    });
    let ev = r
        .run(request(&f, &head(&f), Scope::Full, Stage::Fast))
        .await
        .unwrap();
    assert!(ev.ok, "{:#?}", ev.steps);
    let out = logs(&ev).to_ascii_uppercase();
    assert!(!out.contains("SEKRET"), "a secret reached the step:\n{out}");
    for name in [
        "BOAT_API_KEY",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "HAN_SIGNING_SEED",
        "AWS_SECRET_ACCESS_KEY",
        "SOMETHING_ELSE",
    ] {
        assert!(!out.contains(name), "{name} is set:\n{out}");
    }
    assert!(out.contains("CARGO_TARGET_DIR"), "{out}");
    assert!(out.contains("PATH"), "{out}");
}

#[test]
fn the_scrub_is_an_allow_list_and_refuses_secret_names_in_config() {
    let host: Vec<(String, String)> = [
        ("PATH", "/bin"),
        ("GITHUB_TOKEN", "x"),
        ("HOME", "/h"),
        ("HAN_KEY", "y"),
        ("CARGO_TARGET_DIR", "/elsewhere"),
    ]
    .map(|(k, v)| (k.into(), v.into()))
    .to_vec();
    let env = scrubbed_env(&host, &[], Path::new("/t"), Path::new("/tmp/x")).unwrap();
    let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        names.contains(&"PATH") && names.contains(&"CARGO_HOME") && names.contains(&"RUSTUP_HOME")
    );
    assert!(
        !names.contains(&"GITHUB_TOKEN") && !names.contains(&"HOME") && !names.contains(&"HAN_KEY")
    );
    assert_eq!(
        env.iter().filter(|(k, _)| k == "CARGO_TARGET_DIR").count(),
        1
    );
    assert!(env.contains(&("CARGO_TARGET_DIR".into(), "/t".into())));
    assert!(
        scrubbed_env(
            &host,
            &[("MY_TOKEN".into(), "z".into())],
            Path::new("/t"),
            Path::new("/x")
        )
        .is_err()
    );
}

#[test]
fn scope_selection_leaf_versus_root_manifest() {
    let meta = json!({
        "workspace_root": "/w",
        "packages": [
            {"name": "core", "manifest_path": "/w/crates/core/Cargo.toml", "dependencies": []},
            {"name": "mid", "manifest_path": "/w/crates/mid/Cargo.toml", "dependencies": [{"name": "core"}]},
            {"name": "app", "manifest_path": "/w/apps/app/Cargo.toml", "dependencies": [{"name": "mid"}, {"name": "serde"}]},
            {"name": "leaf", "manifest_path": "/w/crates/leaf/Cargo.toml", "dependencies": []}
        ]
    });
    let s = |paths: &[&str]| {
        select_scope(
            &meta,
            &paths.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            &["docs/".into()],
        )
        .unwrap()
    };
    let v = |names: &[&str]| Scope::Crates(names.iter().map(|n| n.to_string()).collect());
    assert_eq!(s(&["crates/leaf/src/lib.rs"]), v(&["leaf"]));
    assert_eq!(s(&["crates/core/src/x.rs"]), v(&["app", "core", "mid"]));
    assert_eq!(
        s(&["crates/mid/Cargo.toml", "crates/leaf/src/a.rs"]),
        v(&["app", "leaf", "mid"])
    );
    assert_eq!(s(&["Cargo.toml"]), Scope::Full);
    assert_eq!(s(&["Cargo.lock"]), Scope::Full);
    assert_eq!(
        s(&["crates/leaf/src/lib.rs", "rust-toolchain.toml"]),
        Scope::Full
    );
    assert_eq!(s(&[".github/workflows/ci.yml"]), Scope::Full);
    assert_eq!(s(&["docs/a.md", "README.md"]), v(&[]));
}

#[tokio::test]
async fn a_forced_checkout_discards_local_changes_and_keeps_the_target_dir() {
    let f = fixture();
    let r = runner(&f, |c| {
        c.profile = Profile {
            fast: vec![],
            full: vec![],
        }
    });
    let base = head(&f);
    let ev = r
        .run(request(&f, &base, Scope::Full, Stage::Fast))
        .await
        .unwrap();
    assert!(ev.ok && ev.steps.is_empty());

    let state = f.state.path();
    let only = |sub: &str| -> PathBuf {
        let mut it = std::fs::read_dir(state.join(sub))
            .unwrap()
            .map(|e| e.unwrap().path());
        let p = it.next().unwrap();
        assert!(it.next().is_none());
        p
    };
    let tree = only("trees");
    let target = only("targets");
    // the tree is a worktree of the executor's repo: it has no object store of its own
    assert!(tree.join(".git").is_file());
    write(&tree, "crates/a/src/lib.rs", "// vandalised\n");
    write(&tree, "stray.txt", "left by a step\n");
    write(&target, "debug/warm-marker", "kept\n");

    let ev = r
        .run(request(&f, &base, Scope::Full, Stage::Fast))
        .await
        .unwrap();
    assert!(ev.ok);
    // (line endings may differ with core.autocrlf)
    let restored = std::fs::read_to_string(tree.join("crates/a/src/lib.rs")).unwrap();
    assert!(
        restored.contains("pub fn id()") && !restored.contains("vandalised"),
        "{restored}"
    );
    assert!(!tree.join("stray.txt").exists());
    assert_eq!(
        std::fs::read_to_string(target.join("debug/warm-marker")).unwrap(),
        "kept\n"
    );
    assert_eq!(git(&tree, &["rev-parse", "HEAD"]), base);
}

#[tokio::test]
async fn only_a_full_commit_id_from_the_repository_is_run() {
    let f = fixture();
    let r = runner(&f, |_| {});
    let short = head(&f)[..12].to_string();
    for bad in [
        "HEAD",
        "main",
        "--upload-pack=x",
        short.as_str(),
        "../../etc",
        "",
    ] {
        let e = r
            .run(request(&f, bad, Scope::Full, Stage::Fast))
            .await
            .unwrap_err();
        assert!(matches!(e, WarmError::Refused(_)), "{bad}: {e}");
    }
    // a well-formed id the repository does not have
    let missing = "0123456789abcdef0123456789abcdef01234567";
    let e = r
        .run(request(&f, missing, Scope::Full, Stage::Fast))
        .await
        .unwrap_err();
    assert!(matches!(e, WarmError::Refused(_)), "{e}");
    let mut req = request(&f, &head(&f), Scope::Full, Stage::Fast);
    req.lane = "../escape".into();
    assert!(matches!(
        r.run(req).await.unwrap_err(),
        WarmError::Refused(_)
    ));
}

#[test]
fn concurrency_must_be_positive_and_secret_env_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = WarmConfig::new(dir.path());
    cfg.max_concurrent = 0;
    assert!(WarmRunner::new(cfg).is_err());
    let mut cfg = WarmConfig::new(dir.path());
    cfg.extra_env = vec![("GITHUB_TOKEN".into(), "x".into())];
    assert!(WarmRunner::new(cfg).is_err());
    assert_eq!(WarmConfig::new(dir.path()).max_concurrent, 1);
}

fn warm_repo(f: &Fixture) -> RepoTarget {
    let mut repo = RepoTarget::new("local:fixture", f.repo().to_string_lossy().into_owned());
    repo.checks.backend = ChecksBackend::Warm;
    repo
}

async fn poll_done(checks: &dyn Checks, req: &CheckRequest) -> Vec<CheckRun> {
    for _ in 0..600 {
        let runs = checks.poll(req).await.unwrap();
        if runs.iter().all(|r| r.verdict() != Verdict::Pending) {
            return runs;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the warm run did not finish");
}

#[tokio::test]
async fn the_checks_adapter_reports_steps_and_the_required_name() {
    let f = fixture();
    let repo_dir = f.repo();
    let warm = WarmChecks::new(
        Arc::new(runner(&f, |_| {})),
        Arc::new(move |_| repo_dir.clone()),
    );
    let checks = RoutedChecks::new(None, Some(Arc::new(warm)));
    let repo = warm_repo(&f);
    assert_eq!(checks.name_for(&repo), "warm-runner");

    // a green change: the lane's commit is a child of the target tip
    let (_, body) = lib("c", None, "        let _ = 1;\n");
    let good = commit_file(&f, "crates/c/src/lib.rs", &body);
    let req = CheckRequest {
        repo: repo.clone(),
        commit: good.clone(),
        branch: None,
        stage: Stage::Fast,
        names: vec!["quick".into()],
    };
    checks.start(&req).await.unwrap();
    checks.start(&req).await.unwrap(); // harmless twice
    let runs = poll_done(&checks, &req).await;
    assert_eq!(verdict_of(&runs, "quick"), Verdict::Green, "{runs:#?}");
    let fmt = runs.iter().find(|r| r.name == "fmt").unwrap();
    assert_eq!(fmt.exit, Some(0));
    assert!(
        fmt.duration_ms.is_some()
            && fmt.command.as_deref() == Some("cargo fmt --all --check")
            && fmt.log.is_some()
    );
    let evidence = ChecksEvidence::new(&good, checks.name_for(&repo), &runs);
    assert_eq!(evidence.backend, "warm-runner");
    assert!(evidence.digest().is_ok());

    // a red change, polled without a start (a restarted executor)
    let (_, body) = lib("c", None, "        assert_eq!(1, 2);\n");
    let bad = commit_file(&f, "crates/c/src/lib.rs", &body);
    let req = CheckRequest { commit: bad, ..req };
    let runs = poll_done(&checks, &req).await;
    assert_eq!(verdict_of(&runs, "quick"), Verdict::Red, "{runs:#?}");
    assert_eq!(verdict_of(&runs, "test"), Verdict::Red);
    assert!(checks.rerun(&req, &["quick".into()]).await.unwrap());
}

#[tokio::test]
async fn the_router_refuses_a_backend_that_is_not_set_up() {
    let f = fixture();
    let checks = RoutedChecks::new(None, None);
    let mut repo = warm_repo(&f);
    let req = CheckRequest {
        repo: repo.clone(),
        commit: head(&f),
        branch: None,
        stage: Stage::Fast,
        names: vec!["quick".into()],
    };
    assert!(checks.poll(&req).await.is_err());
    repo.checks.backend = ChecksBackend::Github;
    assert!(checks.start(&CheckRequest { repo, ..req }).await.is_err());
}
