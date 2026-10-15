//! Delta, path-disjointness, squash-rebase and conflict records of the git backend, against real
//! repositories in temporary directories. Commits are built with plumbing in a private index, so no
//! checkout is involved unless a test wants one.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use han_vcs::{
    CandidateSource, ConflictKind, ConflictRecord, Delta, DeltaKind, GitVcs, RebaseOutcome,
    Rebased, Vcs, delta_digest,
};
use sha2::{Digest, Sha256};

fn run_git(dir: &Path, env: &[(&str, &str)], args: &[&str], stdin: Option<&[u8]>) -> (i32, String) {
    let empty = std::env::temp_dir().join("han-vcs-test-empty.gitconfig");
    if !empty.exists() {
        std::fs::write(&empty, b"").unwrap();
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .args(["-c", "core.autocrlf=false", "-c", "safe.directory=*"])
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out) = run_git(dir, &[], args, None);
    assert_eq!(code, 0, "git {args:?}: {out}");
    out
}

/// One edit of a commit.
enum Edit<'a> {
    Put(&'a str, &'a [u8], &'a str),
    Rm(&'a str),
}

fn put<'a>(path: &'a str, content: &'a str) -> Edit<'a> {
    Edit::Put(path, content.as_bytes(), "100644")
}

fn exec<'a>(path: &'a str, content: &'a str) -> Edit<'a> {
    Edit::Put(path, content.as_bytes(), "100755")
}

fn link<'a>(path: &'a str, target: &'a str) -> Edit<'a> {
    Edit::Put(path, target.as_bytes(), "120000")
}

fn rm(path: &str) -> Edit<'_> {
    Edit::Rm(path)
}

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    vcs: GitVcs,
}

fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let vcs = GitVcs::new(tmp.path().join("store"));
    Repo {
        _tmp: tmp,
        dir,
        vcs,
    }
}

impl Repo {
    /// A commit on `parent` (or a root) with `edits` applied.
    fn commit(&self, parent: Option<&str>, edits: &[Edit]) -> String {
        let idx = self.dir.join(".git").join("test-index");
        let _ = std::fs::remove_file(&idx);
        let env = [("GIT_INDEX_FILE", idx.to_str().unwrap())];
        let sh = |args: &[&str], stdin: Option<&[u8]>| {
            let (code, out) = run_git(&self.dir, &env, args, stdin);
            assert_eq!(code, 0, "git {args:?}: {out}");
            out
        };
        match parent {
            Some(p) => {
                sh(&["read-tree", p], None);
            }
            None => {
                sh(&["read-tree", "--empty"], None);
            }
        }
        for e in edits {
            match e {
                Edit::Put(path, content, mode) => {
                    let blob = sh(&["hash-object", "-w", "--stdin"], Some(content));
                    sh(
                        &[
                            "update-index",
                            "--add",
                            "--replace",
                            "--cacheinfo",
                            &format!("{mode},{blob},{path}"),
                        ],
                        None,
                    );
                }
                Edit::Rm(path) => {
                    sh(&["update-index", "--force-remove", path], None);
                }
            }
        }
        let tree = sh(&["write-tree"], None);
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "test"];
        if let Some(p) = parent {
            args.extend(["-p", p]);
        }
        let commit = sh(&args, None);
        let _ = std::fs::remove_file(&idx);
        commit
    }

    fn blob(&self, commit: &str, path: &str) -> String {
        git(&self.dir, &["rev-parse", &format!("{commit}:{path}")])
    }

    fn text(&self, commit: &str, path: &str) -> String {
        git(&self.dir, &["cat-file", "-p", &format!("{commit}:{path}")])
    }

    fn tree(&self, commit: &str) -> String {
        git(&self.dir, &["rev-parse", &format!("{commit}^{{tree}}")])
    }

    fn rebase(&self, base: &str, head: &str, onto: &str) -> RebaseOutcome {
        self.vcs
            .squash_rebase(
                &self.dir,
                base,
                head,
                onto,
                "land: change\n",
                "Lane <lane@example.invalid>",
                1_700_000_123,
            )
            .unwrap()
    }

    fn rebased(&self, base: &str, head: &str, onto: &str) -> Rebased {
        match self.rebase(base, head, onto) {
            RebaseOutcome::Rebased(r) => r,
            RebaseOutcome::Conflict(c) => panic!("conflict: {c:#?}"),
        }
    }

    fn conflict(&self, base: &str, head: &str, onto: &str) -> ConflictRecord {
        match self.rebase(base, head, onto) {
            RebaseOutcome::Conflict(c) => c,
            RebaseOutcome::Rebased(r) => panic!("rebased: {r:#?}"),
        }
    }
}

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn summary(d: &Delta) -> Vec<(String, String)> {
    d.entries
        .iter()
        .map(|e| {
            let kind = match &e.kind {
                DeltaKind::Add => "add".to_owned(),
                DeltaKind::Modify => "modify".to_owned(),
                DeltaKind::Delete => "delete".to_owned(),
                DeltaKind::Rename { from_path } => format!("rename<-{from_path}"),
            };
            (e.path.clone(), kind)
        })
        .collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

fn base_files() -> Vec<Edit<'static>> {
    vec![
        put("a.txt", "a1\na2\n"),
        put("b.txt", "b1\n"),
        put("c.txt", "tgt"),
        put("d/e.txt", "e1\n"),
        put("run.sh", "#!/bin/sh\n"),
        put("mv_src.txt", "moving content\nline two\n"),
        put("keep.txt", "keep\n"),
    ]
}

fn change_files() -> Vec<Edit<'static>> {
    vec![
        put("a.txt", "a1\nA2\n"),
        rm("b.txt"),
        put("new.txt", "brand new\n"),
        rm("mv_src.txt"),
        put("moved/mv.txt", "moving content\nline two\n"),
        exec("run.sh", "#!/bin/sh\n"),
        link("c.txt", "tgt"),
    ]
}

#[test]
fn delta_has_every_kind_sorted_by_path_and_a_jcs_digest() {
    let r = repo();
    let base = r.commit(None, &base_files());
    let head = r.commit(Some(&base), &change_files());
    let d = r.vcs.delta(&r.dir, &base, &head).unwrap();
    assert_eq!(
        summary(&d),
        pairs(&[
            ("a.txt", "modify"),
            ("b.txt", "delete"),
            ("c.txt", "modify"),
            ("moved/mv.txt", "rename<-mv_src.txt"),
            ("new.txt", "add"),
            ("run.sh", "modify"),
        ])
    );
    let by = |p: &str| d.entries.iter().find(|e| e.path == p).unwrap();
    // mode change only: same content id, the modes tell
    let run = by("run.sh");
    assert_eq!(run.from, run.to);
    assert_eq!(run.from_mode.as_deref(), Some("100644"));
    assert_eq!(run.mode.as_deref(), Some("100755"));
    // type change: file to symlink
    assert_eq!(by("c.txt").mode.as_deref(), Some("120000"));
    assert_eq!(by("c.txt").from_mode.as_deref(), Some("100644"));
    // add and delete carry one side only
    let new = by("new.txt");
    assert!(new.from.is_none() && new.from_mode.is_none());
    assert_eq!(new.to.as_deref(), Some(r.blob(&head, "new.txt").as_str()));
    let gone = by("b.txt");
    assert!(gone.to.is_none() && gone.mode.is_none());
    assert_eq!(gone.from.as_deref(), Some(r.blob(&base, "b.txt").as_str()));
    // a rename names its source and keeps the content id
    let mv = by("moved/mv.txt");
    assert_eq!(mv.from, mv.to);
    assert_eq!(
        mv.from.as_deref(),
        Some(r.blob(&base, "mv_src.txt").as_str())
    );

    // the wire form: camelCase, a flat kind, absent fields left out, as a bare array
    let json = serde_json::to_value(&d).unwrap();
    let mv_json = json
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "moved/mv.txt")
        .unwrap();
    assert_eq!(mv_json["kind"], "rename");
    assert_eq!(mv_json["fromPath"], "mv_src.txt");
    assert!(mv_json.get("fromMode").is_some());
    let add_json = json
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "new.txt")
        .unwrap();
    assert!(add_json.get("from").is_none() && add_json.get("fromMode").is_none());
    let back: Delta = serde_json::from_value(json).unwrap();
    assert_eq!(back, d);

    // the digest is sha256 over the RFC 8785 form (checked against an independent implementation)
    let oracle = serde_json_canonicalizer::to_string(&d).unwrap();
    let want: String = Sha256::digest(oracle.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(delta_digest(&d).unwrap(), want);
    assert_eq!(d.digest().unwrap(), want);
}

#[test]
fn delta_digest_does_not_depend_on_the_base_commit() {
    let r = repo();
    let base = r.commit(None, &base_files());
    let head = r.commit(Some(&base), &change_files());
    let other_base = r.commit(Some(&base), &[put("unrelated.txt", "x\n")]);
    let other_head = r.commit(Some(&other_base), &change_files());
    let a = r.vcs.delta(&r.dir, &base, &head).unwrap();
    let b = r.vcs.delta(&r.dir, &other_base, &other_head).unwrap();
    assert_ne!(base, other_base);
    assert_eq!(a, b);
    assert_eq!(a.digest().unwrap(), b.digest().unwrap());
    // and it is a function of the entries: another content gives another digest
    let third = r.commit(Some(&base), &[put("a.txt", "a1\nOTHER\n")]);
    assert_ne!(
        r.vcs
            .delta(&r.dir, &base, &third)
            .unwrap()
            .digest()
            .unwrap(),
        a.digest().unwrap()
    );
    // an empty delta is the digest of `[]`
    let none = r.vcs.delta(&r.dir, &base, &base).unwrap();
    assert!(none.entries.is_empty());
    assert_eq!(serde_json::to_string(&none).unwrap(), "[]");
}

#[test]
fn renames_are_exact_and_pair_in_path_order() {
    let r = repo();
    let base = r.commit(
        None,
        &[
            put("x1.txt", "same\n"),
            put("x2.txt", "same\n"),
            put("e.txt", "edited\nmoved\nand more\nlines\n"),
            put("f.txt", "tgt"),
            put("lonely.txt", "only deleted\n"),
        ],
    );
    let head = r.commit(
        Some(&base),
        &[
            rm("x1.txt"),
            rm("x2.txt"),
            put("y2.txt", "same\n"),
            put("y1.txt", "same\n"),
            put("y3.txt", "same\n"),
            // moved and edited: a delete and an add, not a rename
            rm("e.txt"),
            put("e2.txt", "edited\nmoved\nand more\nlines\nplus one\n"),
            // a symlink never pairs with a file of the same content
            rm("f.txt"),
            link("g.txt", "tgt"),
            rm("lonely.txt"),
        ],
    );
    let d = r.vcs.delta(&r.dir, &base, &head).unwrap();
    assert_eq!(
        summary(&d),
        pairs(&[
            ("e.txt", "delete"),
            ("e2.txt", "add"),
            ("f.txt", "delete"),
            ("g.txt", "add"),
            ("lonely.txt", "delete"),
            ("y1.txt", "rename<-x1.txt"),
            ("y2.txt", "rename<-x2.txt"),
            ("y3.txt", "add"),
        ])
    );
    // deterministic: the same inputs give the same bytes
    let again = r.vcs.delta(&r.dir, &base, &head).unwrap();
    assert_eq!(
        serde_json::to_vec(&d).unwrap(),
        serde_json::to_vec(&again).unwrap()
    );
}

fn disjoint(r: &Repo, base: &str, head: &str, onto: &str) -> (bool, Vec<String>) {
    let d = r.vcs.delta(&r.dir, base, head).unwrap();
    let p = r.vcs.path_disjoint(&r.dir, &d, onto).unwrap();
    assert_eq!(p.ok, p.touched.is_empty());
    (p.ok, p.touched)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn path_disjoint_holds_when_the_new_base_leaves_the_delta_paths_alone() {
    let r = repo();
    let base = r.commit(None, &base_files());
    let head = r.commit(Some(&base), &change_files());
    // the base itself, and a base that changed other files, other directories and added others
    assert_eq!(disjoint(&r, &base, &head, &base), (true, vec![]));
    let onto = r.commit(
        Some(&base),
        &[
            put("keep.txt", "changed elsewhere\n"),
            put("d/other.txt", "o\n"),
            put("z/deep/er.txt", "z\n"),
        ],
    );
    assert_eq!(disjoint(&r, &base, &head, &onto), (true, vec![]));
    // an empty delta is disjoint from anything
    assert_eq!(disjoint(&r, &base, &base, &onto), (true, vec![]));
    // the same change already on the new base is not disjoint: its paths differ from `from`
    let (ok, touched) = disjoint(&r, &base, &head, &head);
    assert!(!ok);
    assert!(touched.contains(&"a.txt".to_owned()), "{touched:?}");
}

#[test]
fn path_disjoint_names_every_path_that_broke_it() {
    let r = repo();
    let base = r.commit(None, &base_files());
    let head = r.commit(Some(&base), &change_files());
    // modify: content changed on the target
    let onto = r.commit(Some(&base), &[put("a.txt", "a1\na2\na3\n")]);
    assert_eq!(disjoint(&r, &base, &head, &onto), (false, strs(&["a.txt"])));
    // delete: already deleted there
    let onto = r.commit(Some(&base), &[rm("b.txt")]);
    assert_eq!(disjoint(&r, &base, &head, &onto), (false, strs(&["b.txt"])));
    // add: the path appeared there
    let onto = r.commit(Some(&base), &[put("new.txt", "someone else\n")]);
    assert_eq!(
        disjoint(&r, &base, &head, &onto),
        (false, strs(&["new.txt"]))
    );
    // rename: both sides count, the source changed or the destination appeared
    let onto = r.commit(Some(&base), &[put("mv_src.txt", "edited source\n")]);
    assert_eq!(
        disjoint(&r, &base, &head, &onto),
        (false, strs(&["mv_src.txt"]))
    );
    let onto = r.commit(Some(&base), &[put("moved/mv.txt", "squatter\n")]);
    assert_eq!(
        disjoint(&r, &base, &head, &onto),
        (false, strs(&["moved/mv.txt"]))
    );
    // mode: only the executable bit changed on the target, and the delta edits the same file
    let onto = r.commit(Some(&base), &[exec("a.txt", "a1\na2\n")]);
    assert_eq!(disjoint(&r, &base, &head, &onto), (false, strs(&["a.txt"])));
    // several at once, sorted
    let onto = r.commit(
        Some(&base),
        &[put("new.txt", "x\n"), put("a.txt", "y\n"), rm("b.txt")],
    );
    assert_eq!(
        disjoint(&r, &base, &head, &onto),
        (false, strs(&["a.txt", "b.txt", "new.txt"]))
    );
}

#[test]
fn path_disjoint_checks_that_ancestors_keep_their_kind() {
    let r = repo();
    let base = r.commit(None, &[put("keep.txt", "k\n"), put("d/e.txt", "e\n")]);
    // an add below directories that do not exist yet
    let head = r.commit(Some(&base), &[put("a/b/c.txt", "c\n")]);
    // `a` became a file on the target
    let onto = r.commit(Some(&base), &[put("a", "now a file\n")]);
    assert_eq!(disjoint(&r, &base, &head, &onto), (false, strs(&["a"])));
    // `a/b` became a file
    let onto = r.commit(Some(&base), &[put("a/b", "now a file\n")]);
    assert_eq!(disjoint(&r, &base, &head, &onto), (false, strs(&["a/b"])));
    // a directory appearing where the delta expects none is fine for its ancestors, but not for the path
    let onto = r.commit(Some(&base), &[put("a/other.txt", "o\n")]);
    assert_eq!(disjoint(&r, &base, &head, &onto), (true, vec![]));
    let onto = r.commit(Some(&base), &[put("a/b/c.txt/inner", "i\n")]);
    assert_eq!(
        disjoint(&r, &base, &head, &onto),
        (false, strs(&["a/b/c.txt"]))
    );
    // a modify below `d`, and `d` replaced by a file on the target: the path and the ancestor both break
    let head = r.commit(Some(&base), &[put("d/e.txt", "e2\n")]);
    let onto = r.commit(Some(&base), &[put("d", "file now\n")]);
    assert_eq!(
        disjoint(&r, &base, &head, &onto),
        (false, strs(&["d", "d/e.txt"]))
    );
    // the delta itself flips a directory to a file: `d` is then a path of the delta, not an ancestor to keep
    let flip = r.commit(Some(&base), &[rm("d/e.txt"), put("d", "a file\n")]);
    assert_eq!(disjoint(&r, &base, &flip, &base), (true, vec![]));
    let onto = r.commit(Some(&base), &[put("keep.txt", "elsewhere\n")]);
    assert_eq!(disjoint(&r, &base, &flip, &onto), (true, vec![]));
    // and the other way: a file becomes a directory
    let b2 = r.commit(Some(&base), &[put("f", "file\n")]);
    let h2 = r.commit(Some(&b2), &[rm("f"), put("f/g.txt", "g\n")]);
    assert_eq!(disjoint(&r, &b2, &h2, &b2), (true, vec![]));
    let onto = r.commit(Some(&b2), &[put("f", "file edited\n")]);
    assert_eq!(disjoint(&r, &b2, &h2, &onto), (false, strs(&["f"])));
    // a symlink where a directory is expected
    let onto = r.commit(Some(&base), &[link("a", "d")]);
    assert_eq!(
        disjoint(
            &r,
            &base,
            &r.commit(Some(&base), &[put("a/x", "x\n")]),
            &onto
        ),
        (false, strs(&["a"]))
    );
}

fn ls(r: &Repo, commit: &str) -> BTreeSet<String> {
    git(&r.dir, &["ls-tree", "-r", "--name-only", commit])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_path_disjoint_squash_rebase_lands_exactly_the_approved_delta() {
    let r = repo();
    let base = r.commit(None, &base_files());
    // the change is several commits
    let mid = r.commit(Some(&base), &[put("a.txt", "a1\nmid\n")]);
    let head = r.commit(Some(&mid), &change_files());
    let onto = r.commit(
        Some(&base),
        &[put("keep.txt", "moved on\n"), put("d/other.txt", "o\n")],
    );
    let want = r.vcs.delta(&r.dir, &base, &head).unwrap();
    let out = r.rebased(&base, &head, &onto);
    assert!(out.path_disjoint);
    // byte for byte, and so the same digest
    assert_eq!(out.delta, want);
    assert_eq!(
        serde_json::to_vec(&out.delta).unwrap(),
        serde_json::to_vec(&want).unwrap()
    );
    assert_eq!(out.delta.digest().unwrap(), want.digest().unwrap());
    // and the delta read back from the repository agrees
    assert_eq!(r.vcs.delta(&r.dir, &onto, &out.commit).unwrap(), want);
    // one commit on the target, with its own message, author and both dates from the arguments
    assert_eq!(
        git(
            &r.dir,
            &["rev-list", "--count", &format!("{onto}..{}", out.commit)]
        ),
        "1"
    );
    assert_eq!(
        git(&r.dir, &["rev-parse", &format!("{}^", out.commit)]),
        onto
    );
    assert_eq!(
        git(
            &r.dir,
            &[
                "log",
                "-1",
                "--format=%an|%ae|%cn|%ce|%ad|%cd",
                "--date=raw",
                &out.commit
            ]
        ),
        "Lane|lane@example.invalid|Lane|lane@example.invalid|1700000123 +0000|1700000123 +0000"
    );
    assert_eq!(
        git(&r.dir, &["log", "-1", "--format=%B", &out.commit]),
        "land: change"
    );
    // the target's own changes are kept, the approved ones applied
    assert_eq!(r.text(&out.commit, "keep.txt"), "moved on");
    assert_eq!(r.text(&out.commit, "d/other.txt"), "o");
    assert_eq!(r.text(&out.commit, "a.txt"), "a1\nA2");
    assert_eq!(
        r.text(&out.commit, "moved/mv.txt"),
        "moving content\nline two"
    );
    assert!(!ls(&r, &out.commit).contains("mv_src.txt"));
    assert!(!ls(&r, &out.commit).contains("b.txt"));
    assert_eq!(
        git(&r.dir, &["ls-tree", &out.commit, "run.sh"])
            .split(' ')
            .next(),
        Some("100755")
    );
    assert_eq!(
        git(&r.dir, &["ls-tree", &out.commit, "c.txt"])
            .split(' ')
            .next(),
        Some("120000")
    );
}

#[test]
fn squash_rebase_is_a_function_of_its_arguments() {
    let r = repo();
    let base = r.commit(None, &base_files());
    let head = r.commit(Some(&base), &change_files());
    let onto = r.commit(Some(&base), &[put("keep.txt", "moved on\n")]);
    let first = r.rebased(&base, &head, &onto);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = r.rebased(&base, &head, &onto);
    assert_eq!(first.commit, second.commit, "the clock never enters");
    // every argument counts
    let call = |msg: &str, who: &str, t: i64| match r
        .vcs
        .squash_rebase(&r.dir, &base, &head, &onto, msg, who, t)
        .unwrap()
    {
        RebaseOutcome::Rebased(x) => x.commit,
        RebaseOutcome::Conflict(_) => unreachable!(),
    };
    let same = call(
        "land: change\n",
        "Lane <lane@example.invalid>",
        1_700_000_123,
    );
    assert_eq!(same, first.commit);
    assert_ne!(
        call("other\n", "Lane <lane@example.invalid>", 1_700_000_123),
        same
    );
    assert_ne!(
        call("land: change\n", "Other <o@example.invalid>", 1_700_000_123),
        same
    );
    assert_ne!(
        call(
            "land: change\n",
            "Lane <lane@example.invalid>",
            1_700_000_124
        ),
        same
    );
    // the same in a repository that started from other configuration and environment
    let (_, _) = run_git(
        &r.dir,
        &[],
        &["config", "i18n.commitEncoding", "latin1"],
        None,
    );
    let (_, _) = run_git(&r.dir, &[], &["config", "commit.gpgsign", "true"], None);
    assert_eq!(
        call(
            "land: change\n",
            "Lane <lane@example.invalid>",
            1_700_000_123
        ),
        same
    );
}

#[test]
fn squash_rebase_leaves_every_checkout_alone() {
    let r = repo();
    let base = r.commit(None, &base_files());
    let head = r.commit(Some(&base), &change_files());
    let onto = r.commit(Some(&base), &[put("keep.txt", "moved on\n")]);
    git(&r.dir, &["checkout", "-q", "-b", "work", &base]);
    std::fs::write(r.dir.join("dirty.txt"), "uncommitted\n").unwrap();
    std::fs::write(r.dir.join("a.txt"), "locally edited\n").unwrap();
    let head_ref = git(&r.dir, &["rev-parse", "HEAD"]);
    let index = std::fs::read(r.dir.join(".git/index")).unwrap();
    let status = git(&r.dir, &["status", "--porcelain"]);
    let refs = git(&r.dir, &["for-each-ref"]);
    let out = r.rebased(&base, &head, &onto);
    assert!(out.path_disjoint);
    let conflicted = r.commit(Some(&base), &[put("a.txt", "x\n")]);
    let _ = r.rebase(
        &base,
        &r.commit(Some(&base), &[put("a.txt", "y\n")]),
        &conflicted,
    );
    assert_eq!(git(&r.dir, &["rev-parse", "HEAD"]), head_ref);
    assert_eq!(std::fs::read(r.dir.join(".git/index")).unwrap(), index);
    assert_eq!(git(&r.dir, &["status", "--porcelain"]), status);
    assert_eq!(git(&r.dir, &["for-each-ref"]), refs);
    assert_eq!(
        std::fs::read_to_string(r.dir.join("a.txt")).unwrap(),
        "locally edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(r.dir.join("dirty.txt")).unwrap(),
        "uncommitted\n"
    );
    // and nothing is left behind in the backend's scratch directory
    let left: Vec<_> = std::fs::read_dir(r.vcs.store_root().join("tmp"))
        .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_clean_merge_in_a_shared_file_is_not_path_disjoint() {
    let r = repo();
    let body = lines(12);
    let base = r.commit(None, &[put("f.txt", &body), put("other.txt", "o\n")]);
    let head = r.commit(
        Some(&base),
        &[put("f.txt", &body.replace("line 2\n", "line two\n"))],
    );
    let onto = r.commit(
        Some(&base),
        &[put("f.txt", &body.replace("line 11\n", "line eleven\n"))],
    );
    let out = r.rebased(&base, &head, &onto);
    assert!(!out.path_disjoint, "the caller must gate it again");
    let merged = body
        .replace("line 2\n", "line two\n")
        .replace("line 11\n", "line eleven\n");
    assert_eq!(r.text(&out.commit, "f.txt"), merged.trim_end());
    assert_eq!(
        git(&r.dir, &["rev-parse", &format!("{}^", out.commit)]),
        onto
    );
    // the delta of the result is the change as it now reads: `from` is the target's content
    assert_eq!(out.delta.entries.len(), 1);
    assert_eq!(
        out.delta.entries[0].from.as_deref(),
        Some(r.blob(&onto, "f.txt").as_str())
    );
    assert_ne!(
        out.delta,
        r.vcs.delta(&r.dir, &base, &head).unwrap(),
        "the approved delta did not survive"
    );
}

fn no_markers(rec: &ConflictRecord) {
    let json = serde_json::to_string(rec).unwrap();
    for m in ["<<<<<<<", ">>>>>>>", "=======", "|||||||"] {
        assert!(!json.contains(m), "{m} in {json}");
    }
}

#[test]
fn a_text_conflict_is_data_with_hunks_and_candidates_stored_as_blobs() {
    let r = repo();
    let body = lines(10);
    let base = r.commit(None, &[put("f.txt", &body), put("clean.txt", "c\n")]);
    let ours = body
        .replace("line 2\n", "ours 2\n")
        .replace("line 8\n", "ours 8a\nours 8b\n");
    let theirs = body
        .replace("line 2\n", "theirs 2\n")
        .replace("line 8\n", "theirs 8\n");
    let head = r.commit(
        Some(&base),
        &[put("f.txt", &theirs), put("clean.txt", "c2\n")],
    );
    let onto = r.commit(Some(&base), &[put("f.txt", &ours)]);
    let rec = r.conflict(&base, &head, &onto);
    no_markers(&rec);
    assert_eq!(rec.onto, onto);
    assert_eq!(rec.clean, strs(&["clean.txt"]));
    assert_eq!(rec.conflicts.len(), 1);
    let c = &rec.conflicts[0];
    assert_eq!((c.path.as_str(), c.kind), ("f.txt", ConflictKind::Content));
    assert_eq!(c.base.as_ref().unwrap().id, r.blob(&base, "f.txt"));
    assert_eq!(c.ours.as_ref().unwrap().id, r.blob(&onto, "f.txt"));
    assert_eq!(c.theirs.as_ref().unwrap().id, r.blob(&head, "f.txt"));
    assert_eq!(c.hunks.len(), 2);
    let h = &c.hunks[0];
    assert_eq!(
        (
            h.base_range.start,
            h.base_range.len,
            h.ours_range.start,
            h.ours_range.len,
            h.theirs_range.start,
            h.theirs_range.len
        ),
        (2, 1, 2, 1, 2, 1)
    );
    // the second region sits after the first, in each file's own numbering: ours gained a line at 8
    let h = &c.hunks[1];
    assert_eq!((h.base_range.start, h.base_range.len), (8, 1));
    assert_eq!((h.ours_range.start, h.ours_range.len), (8, 2));
    assert_eq!((h.theirs_range.start, h.theirs_range.len), (8, 1));
    let cand = |h: &han_vcs::ConflictHunk, src: &CandidateSource| {
        let c = h.candidates.iter().find(|c| &c.source == src).unwrap();
        git(&r.dir, &["cat-file", "blob", &c.content_id])
    };
    assert_eq!(cand(&c.hunks[0], &CandidateSource::Ours), "ours 2");
    assert_eq!(cand(&c.hunks[0], &CandidateSource::Theirs), "theirs 2");
    assert_eq!(
        cand(&c.hunks[1], &CandidateSource::Ours),
        "ours 8a\nours 8b"
    );
    assert_eq!(cand(&c.hunks[1], &CandidateSource::Theirs), "theirs 8");
    // no tool has filled its slot, and candidate ids are unique within the record
    let ids: BTreeSet<_> = c
        .hunks
        .iter()
        .flat_map(|h| h.candidates.iter().map(|c| c.id.clone()))
        .collect();
    assert_eq!(ids.len(), 4);
    assert!(c.hunks.iter().all(|h| {
        h.candidates.len() == 2
            && !h
                .candidates
                .iter()
                .any(|c| matches!(c.source, CandidateSource::Tool { .. }))
    }));

    // the wire form
    let json = serde_json::to_value(&rec).unwrap();
    assert_eq!(json["conflicts"][0]["kind"], "content");
    assert!(json["conflicts"][0]["hunks"][0].get("baseRange").is_some());
    assert_eq!(
        json["conflicts"][0]["hunks"][0]["candidates"][0]["source"]["kind"],
        "ours"
    );
    assert!(
        json["conflicts"][0]["hunks"][0]["candidates"][0]
            .get("contentId")
            .is_some()
    );
    let back: ConflictRecord = serde_json::from_value(json).unwrap();
    assert_eq!(back, rec);
    // the tool slot is a variant of the same enum
    let tool = CandidateSource::Tool {
        name: "mergiraf".into(),
        version: "0.1".into(),
    };
    assert_eq!(
        serde_json::to_value(&tool).unwrap(),
        serde_json::json!({"kind": "tool", "name": "mergiraf", "version": "0.1"})
    );
}

#[test]
fn every_kind_of_conflict_is_reported_with_its_sides() {
    let r = repo();
    let base = r.commit(
        None,
        &[
            put("m.txt", "1\n2\n3\n"),
            put("md1.txt", "x\ny\n"),
            put("md2.txt", "x\ny\n"),
            put("bin.dat", "q\0base\n"),
            put("l", "base-target"),
            put("d/inner.txt", "i\n"),
            put("ok.txt", "fine\n"),
        ],
    );
    let ours_base = r.commit(
        Some(&base),
        &[
            put("m.txt", "1\nours\n3\n"),
            put("aa.txt", "ours added\n"),
            rm("md1.txt"),
            put("md2.txt", "x\nchanged by ours\n"),
            Edit::Put("bin.dat", b"q\0ours\n", "100644"),
            link("l", "ours-target"),
            rm("d/inner.txt"),
            put("d", "ours made it a file\n"),
        ],
    );
    let head = r.commit(
        Some(&base),
        &[
            put("m.txt", "1\ntheirs\n3\n"),
            put("aa.txt", "theirs added\n"),
            put("md1.txt", "x\nchanged by theirs\n"),
            rm("md2.txt"),
            Edit::Put("bin.dat", b"q\0theirs\n", "100644"),
            link("l", "theirs-target"),
            put("d/other.txt", "theirs added below\n"),
            put("ok.txt", "fine, edited\n"),
        ],
    );
    let rec = r.conflict(&base, &head, &ours_base);
    no_markers(&rec);
    let kinds: Vec<(&str, ConflictKind)> = rec
        .conflicts
        .iter()
        .map(|c| (c.path.as_str(), c.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("aa.txt", ConflictKind::AddAdd),
            ("bin.dat", ConflictKind::Binary),
            ("d", ConflictKind::FileDirectory),
            ("l", ConflictKind::Symlink),
            ("m.txt", ConflictKind::Content),
            ("md1.txt", ConflictKind::ModifyDelete),
            ("md2.txt", ConflictKind::ModifyDelete),
        ]
    );
    assert_eq!(
        rec.clean,
        strs(&["ok.txt"]),
        "a path under a conflicted one is not clean"
    );
    let by = |p: &str| rec.conflicts.iter().find(|c| c.path == p).unwrap();
    let id = |s: &Option<han_vcs::ConflictSide>| s.as_ref().map(|s| s.id.clone());
    // add/add: no base
    let aa = by("aa.txt");
    assert!(aa.base.is_none() && aa.hunks.is_empty());
    assert_eq!(id(&aa.ours), Some(r.blob(&ours_base, "aa.txt")));
    assert_eq!(id(&aa.theirs), Some(r.blob(&head, "aa.txt")));
    // binary: all three, no hunks
    let bin = by("bin.dat");
    assert_eq!(id(&bin.base), Some(r.blob(&base, "bin.dat")));
    assert_eq!(id(&bin.ours), Some(r.blob(&ours_base, "bin.dat")));
    assert_eq!(id(&bin.theirs), Some(r.blob(&head, "bin.dat")));
    assert!(bin.hunks.is_empty());
    // symlink: the modes say so
    let l = by("l");
    assert_eq!(l.ours.as_ref().unwrap().mode, "120000");
    assert_eq!(id(&l.theirs), Some(r.blob(&head, "l")));
    // modify/delete: the side that deleted has none
    let md1 = by("md1.txt");
    assert!(md1.ours.is_none() && md1.base.is_some() && md1.theirs.is_some());
    let md2 = by("md2.txt");
    assert!(md2.theirs.is_none() && md2.base.is_some() && md2.ours.is_some());
    // file/directory: ours has a file, theirs a directory
    let d = by("d");
    assert_eq!(d.ours.as_ref().unwrap().mode, "100644");
    assert_eq!(d.theirs.as_ref().unwrap().mode, "040000");
    assert_eq!(d.base.as_ref().unwrap().mode, "040000");
    assert_eq!(id(&d.ours), Some(r.blob(&ours_base, "d")));
    // content: hunks
    assert_eq!(by("m.txt").hunks.len(), 1);
    // `merge_simulate` keeps its old shape over the same data
    let sim = r.vcs.merge_simulate(&r.dir, &ours_base, &head).unwrap();
    assert!(!sim.clean);
    assert_eq!(
        sim.conflicts,
        strs(&["aa.txt", "bin.dat", "d", "l", "m.txt", "md1.txt", "md2.txt"])
    );
    let rec2 = r.vcs.merge_record(&r.dir, &ours_base, &head).unwrap();
    assert_eq!(rec2.conflicts, rec.conflicts);
    assert_eq!(rec2.onto, ours_base);
}

#[test]
fn a_file_where_theirs_has_a_directory_and_the_reverse_are_file_directory_conflicts() {
    let r = repo();
    let base = r.commit(None, &[put("keep.txt", "k\n")]);
    let file = r.commit(Some(&base), &[put("x", "a file\n")]);
    let dir = r.commit(Some(&base), &[put("x/y.txt", "below\n")]);
    for (head, onto, ours_mode, theirs_mode) in [
        (&dir, &file, "100644", "040000"),
        (&file, &dir, "040000", "100644"),
    ] {
        let rec = r.conflict(&base, head, onto);
        assert_eq!(rec.conflicts.len(), 1, "{rec:#?}");
        let c = &rec.conflicts[0];
        assert_eq!(
            (c.path.as_str(), c.kind),
            ("x", ConflictKind::FileDirectory)
        );
        assert_eq!(c.ours.as_ref().unwrap().mode, ours_mode);
        assert_eq!(c.theirs.as_ref().unwrap().mode, theirs_mode);
        assert!(c.base.is_none());
        no_markers(&rec);
    }
}

#[test]
fn conflicts_come_from_the_merge_base_given_not_the_one_git_would_find() {
    let r = repo();
    let base = r.commit(None, &[put("f.txt", &lines(6))]);
    let step = r.commit(
        Some(&base),
        &[put("f.txt", &lines(6).replace("line 1\n", "step\n"))],
    );
    // head was made on `step`, but the approved change is only base..head's delta from `step` on
    let head = r.commit(
        Some(&step),
        &[put(
            "f.txt",
            &lines(6)
                .replace("line 1\n", "step\n")
                .replace("line 5\n", "five\n"),
        )],
    );
    let onto = r.commit(
        Some(&step),
        &[put(
            "f.txt",
            &lines(6)
                .replace("line 1\n", "step\n")
                .replace("line 5\n", "FIVE\n"),
        )],
    );
    let rec = r.conflict(&step, &head, &onto);
    assert_eq!(
        rec.conflicts[0].base.as_ref().unwrap().id,
        r.blob(&step, "f.txt")
    );
    let sim = r.vcs.merge_record(&r.dir, &onto, &head).unwrap();
    assert_eq!(
        sim.conflicts[0].base.as_ref().unwrap().id,
        r.blob(&step, "f.txt")
    );
}

// ───────────────────────── differential against plain git ─────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn word(&mut self) -> String {
        format!("w{:012x}", self.next() & 0xffff_ffff_ffff)
    }

    fn file(&mut self, n: usize) -> String {
        (0..n).map(|_| self.word() + "\n").collect()
    }
}

const POOL: [&str; 8] = [
    "a.txt",
    "b.txt",
    "d/x.txt",
    "d/y.txt",
    "e/f/z.txt",
    "g.txt",
    "h.txt",
    "k/m.txt",
];

const NEW: [&str; 4] = ["n1.txt", "d/new.txt", "e/g.txt", "k/deep/n2.txt"];

/// A random set of edits: modify one line, delete, or leave each file, and maybe add new ones. Returns the
/// edits' paths too.
fn random_edits(
    rng: &mut Rng,
    current: &dyn Fn(&str) -> String,
) -> (Vec<(String, Option<String>)>, BTreeSet<String>) {
    let mut edits = Vec::new();
    let mut paths = BTreeSet::new();
    for p in POOL {
        match rng.below(6) {
            0 | 1 => {
                let old = current(p);
                let mut ls: Vec<String> = old.lines().map(str::to_owned).collect();
                let i = rng.below(ls.len() as u64) as usize;
                ls[i] = rng.word();
                edits.push((p.to_owned(), Some(ls.join("\n") + "\n")));
                paths.insert(p.to_owned());
            }
            2 => {
                edits.push((p.to_owned(), None));
                paths.insert(p.to_owned());
            }
            _ => {}
        }
    }
    for p in NEW {
        if rng.below(4) == 0 {
            edits.push((p.to_owned(), Some(rng.file(5))));
            paths.insert(p.to_owned());
        }
    }
    (edits, paths)
}

#[test]
fn squash_rebase_agrees_with_plain_git_on_random_histories() {
    let r = repo();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let contents: Vec<(&str, String)> = POOL.iter().map(|p| (*p, rng.file(8))).collect();
    let base_edits: Vec<Edit> = contents.iter().map(|(p, c)| put(p, c)).collect();
    let base = r.commit(None, &base_edits);
    let cur = |p: &str| r.text(&base, p) + "\n";
    let (mut n_disjoint, mut n_clean, mut n_conflict) = (0, 0, 0);
    for _ in 0..14 {
        let (h_edits, h_paths) = random_edits(&mut rng, &cur);
        let (o_edits, o_paths) = random_edits(&mut rng, &cur);
        let apply = |edits: &[(String, Option<String>)]| {
            let es: Vec<Edit> = edits
                .iter()
                .map(|(p, c)| match c {
                    Some(c) => put(p, c),
                    None => rm(p),
                })
                .collect();
            r.commit(Some(&base), &es)
        };
        let head = apply(&h_edits);
        let onto = apply(&o_edits);
        // plain git (both sides are children of `base`, so it finds the same merge base)
        let (code, plain) = run_git(
            &r.dir,
            &[],
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                &onto,
                &head,
            ],
            None,
        );
        let plain_tree = plain.lines().next().unwrap().to_owned();
        let plain_conflicts: BTreeSet<String> = plain.lines().skip(1).map(str::to_owned).collect();
        match r.rebase(&base, &head, &onto) {
            RebaseOutcome::Rebased(out) => {
                assert_eq!(code, 0, "git saw a conflict: {plain}");
                assert_eq!(r.tree(&out.commit), plain_tree);
                let overlap = h_paths.intersection(&o_paths).next().is_some();
                assert_eq!(
                    out.path_disjoint, !overlap,
                    "head {h_paths:?} onto {o_paths:?}"
                );
                if out.path_disjoint {
                    n_disjoint += 1;
                    assert_eq!(out.delta, r.vcs.delta(&r.dir, &base, &head).unwrap());
                } else {
                    n_clean += 1;
                }
                assert_eq!(
                    git(&r.dir, &["rev-parse", &format!("{}^", out.commit)]),
                    onto
                );
            }
            RebaseOutcome::Conflict(rec) => {
                n_conflict += 1;
                assert_eq!(code, 1, "git merged it: {plain}");
                let got: BTreeSet<String> = rec.conflicts.iter().map(|c| c.path.clone()).collect();
                assert_eq!(got, plain_conflicts);
                no_markers(&rec);
                for c in &rec.conflicts {
                    // every conflict names what it is made of, and every candidate is a blob that exists
                    assert!(c.base.is_some() || c.ours.is_some() || c.theirs.is_some());
                    for h in &c.hunks {
                        for cand in &h.candidates {
                            git(&r.dir, &["cat-file", "-e", &cand.content_id]);
                        }
                    }
                }
            }
        }
    }
    // the generator reaches all three outcomes
    assert!(
        n_disjoint > 0 && n_clean + n_conflict > 0,
        "{n_disjoint} {n_clean} {n_conflict}"
    );
}

#[test]
fn diff_text_is_the_plain_unified_diff_between_two_commits() {
    let r = repo();
    let base = r.commit(None, &[put("a.txt", "one\ntwo\n"), put("gone.txt", "x\n")]);
    let head = r.commit(Some(&base), &[put("a.txt", "one\n2\n"), rm("gone.txt")]);
    let d = r.vcs.diff_text(&r.dir, &base, &head).unwrap();
    assert!(d.contains("--- a/a.txt\n+++ b/a.txt\n"), "{d}");
    assert!(d.contains("-two\n+2\n"), "{d}");
    assert!(d.contains("deleted file mode 100644"), "{d}");
    assert!(!d.contains('\u{1b}'), "no colour: {d}");
    assert_eq!(r.vcs.diff_text(&r.dir, &base, &base).unwrap(), "");
}
