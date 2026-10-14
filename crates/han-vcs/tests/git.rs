//! The git backend against real repositories in temporary directories, with local bare repositories as
//! remotes.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

use han_vcs::{
    ChangeKind, GitVcs, LandOutcome, LandSpec, RemoteAuth, Vcs, VcsError, Workspace, WorkspaceSpec,
};

/// Runs the test's own git (not the backend) with no user or system configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let empty = std::env::temp_dir().join("han-vcs-test-empty.gitconfig");
    if !empty.exists() {
        std::fs::write(&empty, b"").unwrap();
    }
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty)
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

/// An `https://` URL on a local port whose listener accepts and drops every connection, so git fails fast.
fn dead_https(path: &str) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming() {
            drop(c);
        }
    });
    format!("https://127.0.0.1:{port}/{path}")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    /// A bare repository: the remote everything clones from and lands on.
    remote: PathBuf,
    vcs: GitVcs,
    /// The first commit on `main`.
    c1: String,
}

fn write(p: &Path, s: &str) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(p, s).unwrap();
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q"]);
    write(&seed.join("README.md"), "hello\nworld\n");
    write(
        &seed.join("src/lib.rs"),
        "fn a() {}\nfn b() {}\nfn c() {}\n",
    );
    write(&seed.join("docs/guide.md"), "guide\n");
    write(
        &seed.join("old.txt"),
        "moving content\nline two\nline three\n",
    );
    write(&seed.join(".gitignore"), "target/\n");
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "c1"]);
    let c1 = git(&seed, &["rev-parse", "HEAD"]);
    let remote = root.join("remote.git");
    git(&root, &["clone", "-q", "--bare", "seed", "remote.git"]);
    let vcs = GitVcs::new(root.join("store"));
    Fixture {
        _tmp: tmp,
        root,
        remote,
        vcs,
        c1,
    }
}

impl Fixture {
    fn source(&self) -> String {
        self.remote.to_string_lossy().into_owned()
    }

    fn workspace(&self, name: &str, branch: &str) -> Workspace {
        self.vcs
            .create_workspace(&WorkspaceSpec {
                source: self.source(),
                base: "main".into(),
                branch: branch.into(),
                dir: self.root.join(name),
                sparse: vec![],
            })
            .unwrap()
    }
}

#[test]
fn a_workspace_borrows_the_shared_store_and_has_its_own_refs_and_index() {
    let f = fixture();
    let a = f.workspace("wa", "han/1-a");
    let b = f.workspace("wb", "han/1-b");
    assert_eq!(a.base, f.c1);
    assert_eq!(b.base, f.c1);
    assert_eq!(a.shared_store, b.shared_store, "one store for one remote");
    let store = a.shared_store.clone().unwrap();
    assert!(store.join("HEAD").exists());
    for ws in [&a, &b] {
        let alt = std::fs::read_to_string(ws.dir.join(".git/objects/info/alternates")).unwrap();
        assert!(alt.contains("objects"), "{alt}");
        // its own git dir, not a `git worktree`
        assert!(ws.dir.join(".git").is_dir());
        assert!(ws.dir.join(".git/index").exists());
        assert_eq!(
            git(&ws.dir, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{}", ws.branch)
        );
        // no copied objects: the clone borrowed them
        let packs = std::fs::read_dir(ws.dir.join(".git/objects/pack"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(packs, 0);
        let origin = git(&ws.dir, &["remote", "get-url", "origin"]);
        assert!(
            !origin.contains("store"),
            "origin is the real source: {origin}"
        );
    }
    assert!(a.dir.join("README.md").exists());
    // a commit in one session is not visible in the other's refs
    write(&a.dir.join("new.txt"), "x\n");
    let ca = f
        .vcs
        .commit_all(&a.dir, "a work", "Agent A <a@example.invalid>")
        .unwrap();
    assert_ne!(ca, f.c1);
    assert_eq!(git(&b.dir, &["rev-parse", "HEAD"]), f.c1);
    assert!(
        Command::new("git")
            .current_dir(&b.dir)
            .args(["cat-file", "-e", &ca])
            .status()
            .map(|s| !s.success())
            .unwrap()
    );
    // a second workspace on an existing directory, or a bad branch, is refused
    let e = f
        .vcs
        .create_workspace(&WorkspaceSpec {
            source: f.source(),
            base: "main".into(),
            branch: "han/x".into(),
            dir: a.dir.clone(),
            sparse: vec![],
        })
        .unwrap_err();
    assert!(matches!(e, VcsError::Invalid(_)), "{e}");
    let e = f
        .vcs
        .create_workspace(&WorkspaceSpec {
            source: f.source(),
            base: "main".into(),
            branch: "bad..name".into(),
            dir: f.root.join("wc"),
            sparse: vec![],
        })
        .unwrap_err();
    assert!(matches!(e, VcsError::Invalid(_)), "{e}");
    f.vcs.remove_workspace(&a).unwrap();
    assert!(!a.dir.exists());
    assert!(store.join("HEAD").exists(), "the store stays");
}

#[test]
fn bases_resolve_by_branch_head_and_commit() {
    let f = fixture();
    for (i, base) in ["HEAD".to_owned(), f.c1.clone(), "main".to_owned()]
        .iter()
        .enumerate()
    {
        let ws = f
            .vcs
            .create_workspace(&WorkspaceSpec {
                source: f.source(),
                base: base.clone(),
                branch: format!("han/b{i}"),
                dir: f.root.join(format!("wb{i}")),
                sparse: vec![],
            })
            .unwrap();
        assert_eq!(ws.base, f.c1, "{base}");
    }
    let e = f
        .vcs
        .create_workspace(&WorkspaceSpec {
            source: f.source(),
            base: "no-such-branch".into(),
            branch: "han/z".into(),
            dir: f.root.join("wz"),
            sparse: vec![],
        })
        .unwrap_err();
    assert!(matches!(e, VcsError::NotFound(_)), "{e}");
    assert!(!f.root.join("wz").exists());
}

#[test]
fn sparse_checkout_materialises_only_the_cone() {
    let f = fixture();
    let ws = f
        .vcs
        .create_workspace(&WorkspaceSpec {
            source: f.source(),
            base: "main".into(),
            branch: "han/sparse".into(),
            dir: f.root.join("ws"),
            sparse: vec!["src".into()],
        })
        .unwrap();
    assert!(ws.dir.join("src/lib.rs").exists());
    assert!(
        ws.dir.join("README.md").exists(),
        "top-level files are in every cone"
    );
    assert!(!ws.dir.join("docs/guide.md").exists());
    // files outside the cone are not reported as deleted
    let cs = f.vcs.changeset(&ws.dir, &ws.base).unwrap();
    assert!(cs.files.is_empty(), "{:?}", cs.files);
    write(&ws.dir.join("src/lib.rs"), "fn a() {}\n");
    let cs = f.vcs.changeset(&ws.dir, &ws.base).unwrap();
    assert_eq!(cs.files.len(), 1);
    assert_eq!(cs.files[0].path, "src/lib.rs");
}

#[test]
fn a_changeset_has_every_kind_and_a_stable_digest() {
    let f = fixture();
    let ws = f.workspace("w", "han/kinds");
    let d = &ws.dir;
    let empty = f.vcs.changeset(d, &ws.base).unwrap();
    assert!(empty.files.is_empty());
    assert_eq!(empty.base, f.c1);
    assert_eq!(empty.head, None);

    write(
        &d.join("src/lib.rs"),
        "fn a() {}\nfn B() {}\nfn c() {}\nfn d() {}\n",
    ); // modified
    std::fs::remove_file(d.join("docs/guide.md")).unwrap(); // deleted
    std::fs::rename(d.join("old.txt"), d.join("new.txt")).unwrap(); // renamed
    write(&d.join("added.md"), "new file\n"); // added (untracked, not ignored)
    write(&d.join("target/build.out"), "ignored\n"); // ignored
    std::fs::write(d.join("blob.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap(); // binary
    let before_index = std::fs::read(d.join(".git/index")).unwrap();

    let cs = f.vcs.changeset(d, &ws.base).unwrap();
    let kinds: Vec<(&str, ChangeKind)> = cs
        .files
        .iter()
        .map(|c| (c.path.as_str(), c.kind.clone()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("added.md", ChangeKind::Added),
            ("blob.bin", ChangeKind::Added),
            ("docs/guide.md", ChangeKind::Deleted),
            ("new.txt", ChangeKind::Renamed),
            ("src/lib.rs", ChangeKind::Modified),
        ]
    );
    let by = |p: &str| cs.files.iter().find(|c| c.path == p).unwrap();
    assert_eq!(by("new.txt").from.as_deref(), Some("old.txt"));
    assert!(by("blob.bin").binary);
    assert!(by("blob.bin").hunks.is_empty());
    let lib = by("src/lib.rs");
    assert_eq!(lib.hunks, vec![(2, 1), (4, 1)]);
    assert_eq!((lib.added, lib.removed), (2, 1));
    assert!(lib.old_hash.is_some() && lib.new_hash.is_some() && lib.old_hash != lib.new_hash);
    assert_eq!(lib.mode.as_deref(), Some("100644"));
    assert!(by("docs/guide.md").new_hash.is_none() && by("docs/guide.md").mode.is_none());
    assert!(by("added.md").old_hash.is_none());
    assert_eq!(cs.digest.len(), 64);
    // the session's index is untouched
    assert_eq!(std::fs::read(d.join(".git/index")).unwrap(), before_index);
    // the same state gives the same digest; a change gives another
    let again = f.vcs.changeset(d, &ws.base).unwrap();
    assert_eq!(again, cs);
    write(&d.join("added.md"), "new file, changed\n");
    let other = f.vcs.changeset(d, &ws.base).unwrap();
    assert_ne!(other.digest, cs.digest);
    // the summary carries paths and kinds only
    let summary = cs.summary();
    assert_eq!(summary.digest, cs.digest);
    assert_eq!(summary.paths.len(), 5);

    // committed: the range between base and the commit is the same change set
    write(&d.join("added.md"), "new file\n");
    let c = f
        .vcs
        .commit_all(d, "work", "Agent <a@example.invalid>")
        .unwrap();
    let between = f.vcs.changeset_between(d, &ws.base, &c).unwrap();
    assert_eq!(between.digest, cs.digest);
    assert_eq!(between.head.as_deref(), Some(c.as_str()));
    // after the commit the working tree has nothing new against it
    assert!(f.vcs.changeset(d, &c).unwrap().files.is_empty());
    // nothing to commit returns the head
    assert_eq!(f.vcs.commit_all(d, "again", "Agent").unwrap(), c);
    assert_eq!(
        git(d, &["log", "-1", "--format=%an <%ae>|%s"]),
        "Agent <a@example.invalid>|work"
    );
}

#[cfg(unix)]
#[test]
fn mode_and_type_changes_are_kinds() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let ws = f.workspace("w", "han/modes");
    let d = &ws.dir;
    std::fs::set_permissions(d.join("src/lib.rs"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(d.join("README.md")).unwrap();
    std::os::unix::fs::symlink("src/lib.rs", d.join("README.md")).unwrap();
    let cs = f.vcs.changeset(d, &ws.base).unwrap();
    let by = |p: &str| cs.files.iter().find(|c| c.path == p).unwrap();
    assert_eq!(by("src/lib.rs").kind, ChangeKind::ModeChanged);
    assert_eq!(by("src/lib.rs").mode.as_deref(), Some("100755"));
    assert_eq!(by("README.md").kind, ChangeKind::TypeChanged);
    assert_eq!(by("README.md").mode.as_deref(), Some("120000"));
}

fn refname(b: &str) -> String {
    format!("refs/heads/{b}")
}

#[test]
fn land_is_compare_and_swap_and_a_retry_finds_it_done() {
    let f = fixture();
    let ws = f.workspace("w", "han/land");
    write(&ws.dir.join("feature.txt"), "feature\n");
    let c = f.vcs.commit_all(&ws.dir, "feature", "Agent").unwrap();
    let remote = f.source();
    let r = refname("han/land");
    assert_eq!(f.vcs.remote_ref(&ws.dir, &remote, &r, None).unwrap(), None);
    let spec = LandSpec {
        remote: remote.clone(),
        refname: r.clone(),
        expected: None,
        new: c.clone(),
    };
    assert_eq!(
        f.vcs.land(&ws.dir, &spec, None).unwrap(),
        LandOutcome::Landed
    );
    assert_eq!(
        f.vcs.remote_ref(&ws.dir, &remote, &r, None).unwrap(),
        Some(c.clone())
    );
    // a retry after a lost reply: the same spec finds the ref already at `new`
    assert_eq!(
        f.vcs.land(&ws.dir, &spec, None).unwrap(),
        LandOutcome::AlreadyThere
    );

    // a second move from c to c2
    write(&ws.dir.join("feature.txt"), "feature 2\n");
    let c2 = f.vcs.commit_all(&ws.dir, "feature 2", "Agent").unwrap();
    let spec2 = LandSpec {
        expected: Some(c.clone()),
        new: c2.clone(),
        ..spec.clone()
    };
    assert_eq!(
        f.vcs.land(&ws.dir, &spec2, None).unwrap(),
        LandOutcome::Landed
    );
    assert_eq!(
        f.vcs.land(&ws.dir, &spec2, None).unwrap(),
        LandOutcome::AlreadyThere
    );

    // someone else moves the ref: a land expecting c2 is a conflict and the remote is unchanged
    let other = f.workspace("o", "han/other");
    write(&other.dir.join("theirs.txt"), "theirs\n");
    let theirs = f.vcs.commit_all(&other.dir, "theirs", "Other").unwrap();
    let force = LandSpec {
        expected: Some(c2.clone()),
        new: theirs.clone(),
        ..spec.clone()
    };
    assert_eq!(
        f.vcs.land(&other.dir, &force, None).unwrap(),
        LandOutcome::Landed
    );
    write(&ws.dir.join("feature.txt"), "feature 3\n");
    let c3 = f.vcs.commit_all(&ws.dir, "feature 3", "Agent").unwrap();
    let stale = LandSpec {
        expected: Some(c2.clone()),
        new: c3,
        ..spec.clone()
    };
    assert_eq!(
        f.vcs.land(&ws.dir, &stale, None).unwrap(),
        LandOutcome::Conflict {
            actual: Some(theirs.clone())
        }
    );
    assert_eq!(
        f.vcs.remote_ref(&ws.dir, &remote, &r, None).unwrap(),
        Some(theirs)
    );
    // "must not exist" on an existing ref is a conflict too
    let create = LandSpec {
        expected: None,
        new: c2,
        ..spec
    };
    assert!(matches!(
        f.vcs.land(&ws.dir, &create, None).unwrap(),
        LandOutcome::Conflict { .. }
    ));
    // only full ids and full ref names
    let bad = LandSpec {
        remote,
        refname: "main".into(),
        expected: None,
        new: f.c1.clone(),
    };
    assert!(matches!(
        f.vcs.land(&ws.dir, &bad, None),
        Err(VcsError::Invalid(_))
    ));
}

#[test]
fn merge_simulation_reports_conflicts_without_touching_the_workspace() {
    let f = fixture();
    let a = f.workspace("a", "han/a");
    let b = f.workspace("b", "han/b");
    write(
        &a.dir.join("src/lib.rs"),
        "fn a() {}\nfn b_from_a() {}\nfn c() {}\n",
    );
    let ca = f.vcs.commit_all(&a.dir, "a", "A").unwrap();
    write(
        &b.dir.join("src/lib.rs"),
        "fn a() {}\nfn b_from_b() {}\nfn c() {}\n",
    );
    write(&b.dir.join("only_b.txt"), "b\n");
    let cb = f.vcs.commit_all(&b.dir, "b", "B").unwrap();
    // bring b's commit into a's repository the way overlap tier 3 would (a fetch of the other branch)
    let remote = f.source();
    f.vcs
        .land(
            &b.dir,
            &LandSpec {
                remote: remote.clone(),
                refname: refname("han/b"),
                expected: None,
                new: cb.clone(),
            },
            None,
        )
        .unwrap();
    git(
        &a.dir,
        &[
            "fetch",
            "-q",
            &remote,
            "refs/heads/han/b:refs/remotes/peer/b",
        ],
    );
    let head_before = git(&a.dir, &["rev-parse", "HEAD"]);
    let index_before = std::fs::read(a.dir.join(".git/index")).unwrap();
    write(&a.dir.join("dirty.txt"), "uncommitted\n");

    let sim = f.vcs.merge_simulate(&a.dir, &ca, &cb).unwrap();
    assert!(!sim.clean);
    assert_eq!(sim.conflicts, vec!["src/lib.rs".to_owned()]);
    // a clean pair: the base and b
    let clean = f.vcs.merge_simulate(&a.dir, &f.c1, &cb).unwrap();
    assert!(clean.clean, "{clean:?}");
    assert!(clean.conflicts.is_empty());
    assert_eq!(git(&a.dir, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        std::fs::read(a.dir.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        std::fs::read_to_string(a.dir.join("dirty.txt")).unwrap(),
        "uncommitted\n"
    );
    assert_eq!(
        std::fs::read_to_string(a.dir.join("src/lib.rs")).unwrap(),
        "fn a() {}\nfn b_from_a() {}\nfn c() {}\n"
    );
}

/// Plants every way a repository's own configuration could run code (SR-72) and checks none of it runs.
#[test]
fn no_code_from_the_worktree_runs() {
    let f = fixture();
    let ws = f.workspace("w", "han/hooks");
    let d = &ws.dir;
    let marker_dir = f.root.join("markers");
    std::fs::create_dir_all(&marker_dir).unwrap();
    let m = |name: &str| marker_dir.join(name).to_string_lossy().replace('\\', "/");
    let hooks = d.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in [
        "pre-push",
        "post-checkout",
        "pre-commit",
        "post-commit",
        "reference-transaction",
        "pre-auto-gc",
    ] {
        let p = hooks.join(hook);
        write(&p, &format!("#!/bin/sh\necho ran > '{}'\n", m(hook)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let script = |name: &str| format!("!sh -c 'echo ran > {}'", m(name));
    for (k, v) in [
        ("core.hooksPath", ".git/hooks".to_owned()),
        ("credential.helper", script("credential")),
        (
            "core.fsmonitor",
            format!("sh -c 'echo ran > {}'", m("fsmonitor")),
        ),
        (
            "filter.evil.clean",
            format!("sh -c 'echo ran > {}; cat'", m("clean")),
        ),
        (
            "filter.evil.smudge",
            format!("sh -c 'echo ran > {}; cat'", m("smudge")),
        ),
        (
            "diff.evil.textconv",
            format!("sh -c 'echo ran > {}; cat'", m("textconv")),
        ),
        (
            "diff.external",
            format!("sh -c 'echo ran > {}'", m("external")),
        ),
        (
            "merge.evil.driver",
            format!("sh -c 'echo ran > {}; false'", m("merge")),
        ),
        (
            "core.askPass",
            format!("sh -c 'echo ran > {}'", m("askpass")),
        ),
    ] {
        git(d, &["config", k, &v]);
    }
    write(
        &d.join(".gitattributes"),
        "* filter=evil diff=evil merge=evil\n",
    );
    write(&d.join("src/lib.rs"), "fn a() {}\nchanged\n");

    assert!(
        std::fs::read_dir(&marker_dir).unwrap().next().is_none(),
        "before changeset"
    );
    let cs = f.vcs.changeset(d, &ws.base).unwrap();
    assert!(cs.files.iter().any(|c| c.path == "src/lib.rs"));
    assert!(
        std::fs::read_dir(&marker_dir).unwrap().next().is_none(),
        "after changeset"
    );
    let c = f.vcs.commit_all(d, "evil", "Agent").unwrap();
    assert!(
        std::fs::read_dir(&marker_dir).unwrap().next().is_none(),
        "after commit"
    );
    let remote = f.source();
    let spec = LandSpec {
        remote: remote.clone(),
        refname: refname("han/hooks"),
        expected: None,
        new: c.clone(),
    };
    assert_eq!(f.vcs.land(d, &spec, None).unwrap(), LandOutcome::Landed);
    assert!(
        std::fs::read_dir(&marker_dir).unwrap().next().is_none(),
        "after land"
    );
    let _ = f.vcs.merge_simulate(d, &ws.base, &c).unwrap();
    assert!(
        std::fs::read_dir(&marker_dir).unwrap().next().is_none(),
        "after merge"
    );
    let _ = f
        .vcs
        .remote_ref(d, &dead_https("none.git"), "refs/heads/x", None);
    let ran: Vec<String> = std::fs::read_dir(&marker_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(ran.is_empty(), "these ran: {ran:?}");
    // and the test's own git does run them, so the fixture is real
    if cfg!(unix) {
        git(d, &["commit", "-q", "--allow-empty", "-m", "x"]);
        assert!(
            marker_dir.join("pre-commit").exists(),
            "the planted hook works when git is not isolated"
        );
    }
}

/// A token given as `RemoteAuth` never reaches error text or disk (SR-50).
#[test]
fn a_credential_never_reaches_errors_or_disk() {
    let f = fixture();
    let ws = f.workspace("w", "han/token");
    let canary = "ghs_CANARYtoken0123456789abcdef";
    let auth = RemoteAuth::new("x-access-token", canary);
    assert!(!format!("{auth:?}").contains(canary));
    let remote_url = dead_https("owner/repo.git");
    let remote = remote_url.as_str();
    let e = f
        .vcs
        .remote_ref(&ws.dir, remote, "refs/heads/x", Some(&auth))
        .unwrap_err();
    assert!(!e.to_string().contains(canary), "{e}");
    let c = f.vcs.commit_all(&ws.dir, "c", "A").unwrap();
    let e = f
        .vcs
        .land(
            &ws.dir,
            &LandSpec {
                remote: remote.into(),
                refname: refname("x"),
                expected: None,
                new: c,
            },
            Some(&auth),
        )
        .unwrap_err();
    let text = e.to_string();
    assert!(!text.contains(canary), "{text}");
    assert!(!text.contains("Q0FOQVJZ"), "no base64 form either: {text}");
    // a URL with userinfo is scrubbed as well
    let e = f
        .vcs
        .remote_ref(
            &ws.dir,
            &dead_https("r.git").replace("https://", &format!("https://u:{canary}@")),
            "refs/heads/x",
            None,
        )
        .unwrap_err();
    assert!(!e.to_string().contains(canary), "{e}");
    // nothing under the fixture holds it
    fn scan(dir: &Path, needle: &[u8], hits: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, needle, hits);
            } else if let Ok(bytes) = std::fs::read(&p)
                && bytes.windows(needle.len()).any(|w| w == needle)
            {
                hits.push(p);
            }
        }
    }
    let mut hits = Vec::new();
    scan(&f.root, canary.as_bytes(), &mut hits);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn only_allowed_protocols_are_used() {
    let tmp = tempfile::tempdir().unwrap();
    let vcs = GitVcs::new(tmp.path().join("store")).with_protocols(["https"]);
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q"]);
    write(&seed.join("a"), "a\n");
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "a"]);
    // a local path is the `file` protocol, which this backend does not allow
    let e = vcs
        .create_workspace(&WorkspaceSpec {
            source: seed.to_string_lossy().into_owned(),
            base: "main".into(),
            branch: "han/p".into(),
            dir: tmp.path().join("w"),
            sparse: vec![],
        })
        .unwrap_err();
    assert!(matches!(e, VcsError::Tool { .. }), "{e}");
}

/// R15: the host bundles `base..head`; an executor with its own store, which can't read the workspace,
/// imports it, finds the same changeset, and lands it from its own repository.
#[test]
fn a_bundle_lands_from_an_executors_own_repository() {
    let f = fixture();
    let ws = f.workspace("w", "han/bundle");
    write(&ws.dir.join("src/lib.rs"), "fn a() {}\nfn b2() {}\n");
    write(&ws.dir.join("new.txt"), "new\n");
    let head = f.vcs.commit_all(&ws.dir, "change", "Agent").unwrap();
    let cs = f.vcs.changeset_between(&ws.dir, &ws.base, &head).unwrap();
    let bundle = f.vcs.bundle(&ws.dir, &ws.base, &head).unwrap();
    // the temporary ref is gone from the workspace
    assert!(!git(&ws.dir, &["for-each-ref"]).contains(han_vcs::BUNDLE_REF));

    let exec = GitVcs::new(f.root.join("exec-store"));
    let remote = f.source();
    let repo = exec
        .import_bundle(&remote, None, &bundle, &ws.base, &head)
        .unwrap();
    assert_eq!(repo, exec.landing_for(&remote));
    let theirs = exec.changeset_between(&repo, &ws.base, &head).unwrap();
    assert_eq!(theirs.digest, cs.digest, "the same changeset");
    // importing again is harmless
    exec.import_bundle(&remote, None, &bundle, &ws.base, &head)
        .unwrap();
    let r = refname("han/bundle");
    let spec = LandSpec {
        remote: remote.clone(),
        refname: r.clone(),
        expected: None,
        new: head.clone(),
    };
    assert_eq!(exec.land(&repo, &spec, None).unwrap(), LandOutcome::Landed);
    assert_eq!(git(&f.remote, &["rev-parse", &r]), head);
}

#[test]
fn a_bundle_that_is_not_base_to_head_is_refused() {
    let f = fixture();
    let ws = f.workspace("w", "han/b");
    write(&ws.dir.join("a.txt"), "a\n");
    let c1 = f.vcs.commit_all(&ws.dir, "one", "Agent").unwrap();
    write(&ws.dir.join("b.txt"), "b\n");
    let c2 = f.vcs.commit_all(&ws.dir, "two", "Agent").unwrap();
    let bundle = f.vcs.bundle(&ws.dir, &ws.base, &c2).unwrap();
    let exec = GitVcs::new(f.root.join("exec-store"));
    let remote = f.source();
    let invalid = |r: han_vcs::Result<PathBuf>| {
        assert!(matches!(r, Err(VcsError::Invalid(_))), "{r:?}");
    };
    // another head, another base, garbage
    invalid(exec.import_bundle(&remote, None, &bundle, &ws.base, &c1));
    invalid(exec.import_bundle(&remote, None, &bundle, &c1, &c2));
    invalid(exec.import_bundle(&remote, None, b"# v2 git bundle\n\nPACK", &ws.base, &c2));
    // a bundle whose pack was tampered with is refused by git
    let mut bad = bundle.clone();
    let n = bad.len();
    bad[n - 30] ^= 0xff;
    assert!(
        exec.import_bundle(&remote, None, &bad, &ws.base, &c2)
            .is_err()
    );
    // nothing to bundle
    invalid(f.vcs.bundle(&ws.dir, &c2, &c2).map(|_| PathBuf::new()));
}

#[test]
fn a_remote_without_the_base_is_not_found() {
    let f = fixture();
    let ws = f.workspace("w", "han/b");
    write(&ws.dir.join("a.txt"), "a\n");
    let c1 = f.vcs.commit_all(&ws.dir, "one", "Agent").unwrap();
    write(&ws.dir.join("b.txt"), "b\n");
    let c2 = f.vcs.commit_all(&ws.dir, "two", "Agent").unwrap();
    // based on c1, which only the workspace holds
    let bundle = f.vcs.bundle(&ws.dir, &c1, &c2).unwrap();
    let exec = GitVcs::new(f.root.join("exec-store"));
    let r = exec.import_bundle(&f.source(), None, &bundle, &c1, &c2);
    assert!(matches!(r, Err(VcsError::NotFound(_))), "{r:?}");
}
