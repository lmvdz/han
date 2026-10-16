//! The pruner against real temporary git repositories (worktrees, a squash merge, patch-equivalent commits)
//! and a fake GitHub.

use han_proc::NoWindow as _;
use std::path::PathBuf;
use std::process::Command;

use super::*;
use crate::dev::gh::fake::FakeGh;
use crate::dev::git::Worktree;

/// Runs git in `dir`, with an identity, and returns its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .no_window()
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.autocrlf=false"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare `origin`, the main checkout `v1`, a second clone `other` that lands work on origin, and room for
/// worktrees named `wt-*` and for a workspace outside the root.
struct Repo {
    _tmp: tempfile::TempDir,
    /// `tmp` as git spells it: symlinks resolved (macOS /var is /private/var) and Windows 8.3 names expanded.
    dir: PathBuf,
    main: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = {
            let c = tmp.path().canonicalize().unwrap();
            let c = c.to_string_lossy().into_owned();
            PathBuf::from(c.strip_prefix(r"\\?\").unwrap_or(&c))
        };
        let t = dir.as_path();
        let (origin, main) = (t.join("origin.git"), t.join("v1"));
        std::fs::create_dir_all(&main).unwrap();
        git(t, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&main, &["init", "-q", "-b", "main"]);
        git(
            &main,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        std::fs::write(main.join("README.md"), "base\n").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "base"]);
        git(&main, &["push", "-q", "-u", "origin", "main"]);
        git(t, &["clone", "-q", &origin.to_string_lossy(), "other"]);
        Self {
            _tmp: tmp,
            dir,
            main,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn root(&self) -> String {
        format!("{}/wt-", norm(&self.dir.to_string_lossy()))
    }

    fn cfg(&self) -> Config {
        Config {
            main: self.main.clone(),
            root: self.root(),
            base: "origin/main".into(),
            dry_run: false,
            only: Vec::new(),
            lock_wait: Duration::from_millis(300),
        }
    }

    /// A worktree on a new branch from origin/main, at `<tmp>/<dir>`.
    fn worktree(&self, branch: &str, dir: &str) -> PathBuf {
        let p = self.path(dir);
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                &p.to_string_lossy(),
                "origin/main",
            ],
        );
        p
    }

    /// A commit touching `file`; returns its id.
    fn commit(dir: &Path, file: &str, subject: &str) -> String {
        std::fs::write(dir.join(file), format!("{subject}\n")).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", subject]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A squash merge of `branch` into origin/main, done by someone else.
    fn squash_land(&self, branch: &str) {
        let other = self.path("other");
        git(&other, &["pull", "-q"]);
        git(
            &other,
            &["fetch", "-q", &self.main.to_string_lossy(), branch],
        );
        git(&other, &["merge", "-q", "--squash", "FETCH_HEAD"]);
        git(&other, &["commit", "-q", "-m", &format!("squash {branch}")]);
        git(&other, &["push", "-q", "origin", "main"]);
        git(&self.main, &["fetch", "-q", "origin"]);
    }

    /// What `gh pr update-branch` does: in the second clone, merges origin/main into `tip` (after base moved on)
    /// and publishes the result as `refs/pull/<number>/head` of origin only. Returns the merge commit, which
    /// the main repository does not have.
    fn update_branch_on_github(&self, tip: &str, number: u64) -> String {
        let other = self.path("other");
        git(&other, &["pull", "-q"]);
        Self::commit(&other, "moved.txt", "base moved on");
        git(&other, &["push", "-q", "origin", "main"]);
        git(&other, &["fetch", "-q", &self.main.to_string_lossy(), tip]);
        git(&other, &["checkout", "-q", "-b", "pr-head", tip]);
        git(&other, &["merge", "-q", "--no-edit", "origin/main"]);
        let m = git(&other, &["rev-parse", "HEAD"]);
        git(
            &other,
            &[
                "push",
                "-q",
                "origin",
                &format!("HEAD:refs/pull/{number}/head"),
            ],
        );
        git(&other, &["checkout", "-q", "main"]);
        m
    }

    /// Lands one commit of the main repository on origin/main as a patch-equivalent copy.
    fn cherry_pick_land(&self, sha: &str) {
        let other = self.path("other");
        git(&other, &["pull", "-q"]);
        git(&other, &["fetch", "-q", &self.main.to_string_lossy(), sha]);
        git(&other, &["cherry-pick", sha]);
        git(&other, &["push", "-q", "origin", "main"]);
        git(&self.main, &["fetch", "-q", "origin"]);
    }

    fn branch_exists(&self, b: &str) -> bool {
        Command::new("git")
            .no_window()
            .arg("-C")
            .arg(&self.main)
            .args(["show-ref", "-q", "--verify", &format!("refs/heads/{b}")])
            .status()
            .unwrap()
            .success()
    }

    fn sweep(&self, records: Vec<PrRecord>) -> Vec<Outcome> {
        self.sweep_with(&self.cfg(), records)
    }

    fn sweep_with(&self, cfg: &Config, records: Vec<PrRecord>) -> Vec<Outcome> {
        let gh = FakeGh {
            records,
            ..Default::default()
        };
        let mut out = Vec::new();
        prune(
            &GitCli {
                main: self.main.clone(),
            },
            &gh,
            "o/n",
            cfg,
            &mut |o| out.push(o),
        )
        .unwrap();
        out
    }
}

fn rec(branch: &str, state: PrState, head: &str, number: u64) -> PrRecord {
    PrRecord {
        branch: branch.into(),
        state,
        head: head.into(),
        number,
    }
}

fn kinds(out: &[Outcome]) -> Vec<(Kind, String)> {
    out.iter().map(|o| (o.kind, o.branch.clone())).collect()
}

#[test]
fn a_merged_worktree_goes_with_its_target_and_branch() {
    let r = Repo::new();
    let wt = r.worktree("feat/a", "wt-a");
    let head = Repo::commit(&wt, "a.txt", "a");
    // build output inside the worktree (ignored, as in the real repository) and the sibling form
    std::fs::write(
        r.main.join(".git/info/exclude"),
        "target
",
    )
    .unwrap();
    std::fs::create_dir_all(wt.join("target/debug")).unwrap();
    std::fs::write(wt.join("target/debug/x"), "x").unwrap();
    let sibling = r.path("wt-a-target");
    std::fs::create_dir_all(&sibling).unwrap();
    r.squash_land("feat/a");

    let out = r.sweep(vec![rec("feat/a", PrState::Merged, &head, 5)]);
    assert_eq!(kinds(&out), [(Kind::Pruned, "feat/a".into())], "{out:?}");
    assert!(out[0].text().starts_with("PRUNED "), "{}", out[0].text());
    assert!(!wt.exists(), "worktree directory");
    assert!(!sibling.exists(), "sibling target directory");
    assert!(!r.branch_exists("feat/a"));
    // running it again finds nothing
    assert!(
        r.sweep(vec![rec("feat/a", PrState::Merged, &head, 5)])
            .is_empty()
    );
}

#[test]
fn commits_after_the_pr_merged_are_unlanded_with_their_subjects() {
    let r = Repo::new();
    let wt = r.worktree("feat/b", "wt-b");
    let head = Repo::commit(&wt, "b.txt", "b first");
    r.squash_land("feat/b");
    Repo::commit(&wt, "late.txt", "made after the merge");

    let out = r.sweep(vec![rec("feat/b", PrState::Merged, &head, 81)]);
    assert_eq!(kinds(&out), [(Kind::Unlanded, "feat/b".into())], "{out:?}");
    assert_eq!(out[0].commits.len(), 1);
    assert_eq!(out[0].commits[0].subject, "made after the merge");
    let text = out[0].text();
    assert!(text.contains("PR #81 MERGED"), "{text}");
    assert!(text.contains("made after the merge"), "{text}");
    assert!(wt.exists());
    assert!(r.branch_exists("feat/b"));
    let json = serde_json::to_value(&out[0]).unwrap();
    assert_eq!(json["event"], "UNLANDED");
    assert_eq!(json["commits"][0]["subject"], "made after the merge");
}

#[test]
fn a_commit_after_the_head_whose_patch_is_on_the_base_counts_as_landed() {
    let r = Repo::new();
    let wt = r.worktree("feat/c", "wt-c");
    let head = Repo::commit(&wt, "c.txt", "c first");
    let late = Repo::commit(&wt, "late.txt", "late but cherry-picked");
    // the PR's own commit is squashed (never patch-matches a multi-commit branch's commits); the late one
    // is copied onto the base with a different commit id
    let other = r.path("other");
    git(&other, &["pull", "-q"]);
    git(&other, &["fetch", "-q", &r.main.to_string_lossy(), &head]);
    git(&other, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    git(&other, &["commit", "-q", "-m", "squash c"]);
    git(&other, &["push", "-q", "origin", "main"]);
    r.cherry_pick_land(&late);

    let out = r.sweep(vec![rec("feat/c", PrState::Merged, &head, 7)]);
    assert_eq!(kinds(&out), [(Kind::Pruned, "feat/c".into())], "{out:?}");
}

#[test]
fn a_rewritten_branch_is_judged_whole() {
    let r = Repo::new();
    let wt = r.worktree("feat/d", "wt-d");
    Repo::commit(&wt, "d1.txt", "d one");
    let head = Repo::commit(&wt, "d2.txt", "d two");
    r.squash_land("feat/d");
    // the PR head is no longer an ancestor of the tip
    git(&wt, &["commit", "-q", "--amend", "-m", "d two rewritten"]);

    let out = r.sweep(vec![rec("feat/d", PrState::Merged, &head, 9)]);
    assert_eq!(kinds(&out), [(Kind::Unlanded, "feat/d".into())], "{out:?}");
    let subjects: Vec<_> = out[0].commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["d one", "d two rewritten"]);
}

#[test]
fn open_unknown_and_dirty_worktrees_are_kept() {
    let r = Repo::new();
    let open = r.worktree("feat/open", "wt-open");
    let open_head = Repo::commit(&open, "o.txt", "o");
    let _none = r.worktree("feat/none", "wt-none");
    let dirty = r.worktree("feat/dirty", "wt-dirty");
    let dirty_head = Repo::commit(&dirty, "x.txt", "x");
    std::fs::write(dirty.join("untracked.txt"), "keep me").unwrap();

    let out = r.sweep(vec![
        rec("feat/open", PrState::Open, &open_head, 1),
        rec("feat/dirty", PrState::Closed, &dirty_head, 3),
    ]);
    let mut got = kinds(&out);
    got.sort_by_key(|k| k.1.clone());
    assert_eq!(
        got,
        [
            (Kind::KeptDirty, "feat/dirty".into()),
            (Kind::KeptNoPr, "feat/none".into()),
            (Kind::KeptOpen, "feat/open".into()),
        ]
    );
    assert!(open.exists() && dirty.exists() && r.path("wt-none").exists());
    assert!(dirty.join("untracked.txt").exists());
}

#[test]
fn a_worktree_outside_the_root_is_never_touched() {
    let r = Repo::new();
    let ws = r.worktree("feat/ws", "workspaces-ws");
    let head = Repo::commit(&ws, "w.txt", "w");
    r.squash_land("feat/ws");

    let out = r.sweep(vec![rec("feat/ws", PrState::Merged, &head, 2)]);
    // neither the worktree nor, with it checked out, its branch
    assert!(out.is_empty(), "{out:?}");
    assert!(ws.exists());
    assert!(r.branch_exists("feat/ws"));
}

#[test]
fn a_merged_branch_without_a_worktree_is_deleted_and_a_branch_without_a_pr_is_not() {
    let r = Repo::new();
    git(&r.main, &["branch", "feat/gone", "origin/main"]);
    git(&r.main, &["branch", "feat/mine", "origin/main"]);
    let tip = git(&r.main, &["rev-parse", "feat/gone"]);

    let out = r.sweep(vec![rec("feat/gone", PrState::Merged, &tip, 4)]);
    assert_eq!(kinds(&out), [(Kind::PrunedBranch, "feat/gone".into())]);
    assert_eq!(out[0].text(), "PRUNED_BRANCH feat/gone (PR MERGED)");
    assert!(!r.branch_exists("feat/gone"));
    assert!(r.branch_exists("feat/mine"));
}

#[test]
fn the_newest_pr_of_a_branch_decides() {
    let r = Repo::new();
    git(&r.main, &["branch", "feat/again", "origin/main"]);
    let tip = git(&r.main, &["rev-parse", "feat/again"]);
    // gh lists newest first: the reopened PR is open
    let out = r.sweep(vec![
        rec("feat/again", PrState::Open, &tip, 12),
        rec("feat/again", PrState::Closed, &tip, 11),
    ]);
    assert_eq!(kinds(&out), [(Kind::KeptOpen, "feat/again".into())]);
    assert!(r.branch_exists("feat/again"));
}

#[test]
fn a_clean_main_checkout_on_a_landed_branch_goes_back_to_the_base() {
    let r = Repo::new();
    git(&r.main, &["switch", "-q", "-c", "feat/m"]);
    let head = Repo::commit(&r.main, "m.txt", "m");
    r.squash_land("feat/m");
    // untracked files do not make the main checkout dirty
    std::fs::write(r.main.join("scratch.txt"), "x").unwrap();

    let out = r.sweep(vec![rec("feat/m", PrState::Merged, &head, 6)]);
    assert_eq!(kinds(&out), [(Kind::Switched, "feat/m".into())], "{out:?}");
    assert!(
        out[0].text().contains("to main; branch deleted"),
        "{}",
        out[0].text()
    );
    assert!(r.main.exists() && r.main.join("scratch.txt").exists());
    assert_eq!(git(&r.main, &["branch", "--show-current"]), "main");
    assert_eq!(
        git(&r.main, &["rev-parse", "HEAD"]),
        git(&r.main, &["rev-parse", "origin/main"]),
        "fast-forwarded"
    );
    assert!(!r.branch_exists("feat/m"));
}

#[test]
fn a_dirty_or_unlanded_main_checkout_stays_on_its_branch() {
    let r = Repo::new();
    git(&r.main, &["switch", "-q", "-c", "feat/m"]);
    let head = Repo::commit(&r.main, "m.txt", "m");
    r.squash_land("feat/m");
    std::fs::write(r.main.join("README.md"), "edited\n").unwrap();
    let out = r.sweep(vec![rec("feat/m", PrState::Merged, &head, 6)]);
    assert_eq!(kinds(&out), [(Kind::KeptDirty, "feat/m".into())], "{out:?}");

    git(&r.main, &["checkout", "-q", "--", "README.md"]);
    Repo::commit(&r.main, "after.txt", "after the merge");
    let out = r.sweep(vec![rec("feat/m", PrState::Merged, &head, 6)]);
    assert_eq!(kinds(&out), [(Kind::Unlanded, "feat/m".into())], "{out:?}");
    assert_eq!(git(&r.main, &["branch", "--show-current"]), "feat/m");
}

#[test]
fn the_main_checkout_is_never_removed_even_under_the_root() {
    let r = Repo::new();
    let mut cfg = r.cfg();
    // a root that covers the main checkout itself
    cfg.root = format!("{}/", norm(&r.dir.to_string_lossy()));
    let tip = git(&r.main, &["rev-parse", "HEAD"]);
    let out = r.sweep_with(&cfg, vec![rec("main", PrState::Merged, &tip, 1)]);
    assert!(out.is_empty(), "{out:?}");
    assert!(r.main.join("README.md").exists());
    assert!(r.branch_exists("main"));
}

#[test]
fn only_the_named_branches_are_considered() {
    let r = Repo::new();
    git(&r.main, &["branch", "feat/x", "origin/main"]);
    git(&r.main, &["branch", "feat/y", "origin/main"]);
    let tip = git(&r.main, &["rev-parse", "feat/x"]);
    let mut cfg = r.cfg();
    cfg.only = vec!["feat/x".into()];
    let out = r.sweep_with(
        &cfg,
        vec![
            rec("feat/x", PrState::Merged, &tip, 1),
            rec("feat/y", PrState::Merged, &tip, 2),
        ],
    );
    assert_eq!(kinds(&out), [(Kind::PrunedBranch, "feat/x".into())]);
    assert!(r.branch_exists("feat/y"));
}

#[test]
fn a_dry_run_changes_nothing() {
    let r = Repo::new();
    let wt = r.worktree("feat/dry", "wt-dry");
    let head = Repo::commit(&wt, "d.txt", "d");
    r.squash_land("feat/dry");
    let mut cfg = r.cfg();
    cfg.dry_run = true;
    let out = r.sweep_with(&cfg, vec![rec("feat/dry", PrState::Merged, &head, 3)]);
    assert_eq!(kinds(&out), [(Kind::WouldPrune, "feat/dry".into())]);
    assert!(wt.exists() && r.branch_exists("feat/dry"));
}

#[test]
fn a_worktree_deleted_by_hand_is_not_a_failure() {
    let r = Repo::new();
    let wt = r.worktree("feat/hand", "wt-hand");
    let head = Repo::commit(&wt, "h.txt", "h");
    r.squash_land("feat/hand");
    std::fs::remove_dir_all(&wt).unwrap();

    let out = r.sweep(vec![rec("feat/hand", PrState::Merged, &head, 8)]);
    assert_eq!(
        kinds(&out),
        [(Kind::PrunedBranch, "feat/hand".into())],
        "{out:?}"
    );
    assert!(!r.branch_exists("feat/hand"));
}

#[test]
fn another_pruner_holding_the_lock_is_reported_busy() {
    let r = Repo::new();
    git(&r.main, &["branch", "feat/l", "origin/main"]);
    let tip = git(&r.main, &["rev-parse", "feat/l"]);
    let lock_path = GitCli {
        main: r.main.clone(),
    }
    .git_dir()
    .unwrap()
    .join("han-prune.lock");
    let held = acquire(&lock_path, Duration::from_secs(1))
        .unwrap()
        .unwrap();

    let out = r.sweep(vec![rec("feat/l", PrState::Merged, &tip, 1)]);
    assert_eq!(kinds(&out), [(Kind::PruneBusy, String::new())], "{out:?}");
    assert!(out[0].text().starts_with("PRUNE_BUSY another pruner holds"));
    assert!(r.branch_exists("feat/l"));

    drop(held);
    let out = r.sweep(vec![rec("feat/l", PrState::Merged, &tip, 1)]);
    assert_eq!(kinds(&out), [(Kind::PrunedBranch, "feat/l".into())]);
}

#[test]
fn a_lock_left_by_a_dead_owner_does_not_wedge_the_next_run() {
    let r = Repo::new();
    git(&r.main, &["branch", "feat/s", "origin/main"]);
    let tip = git(&r.main, &["rev-parse", "feat/s"]);
    // what a crashed pruner leaves behind: the file, with its pid in it, and nobody holding it
    let lock_path = GitCli {
        main: r.main.clone(),
    }
    .git_dir()
    .unwrap()
    .join("han-prune.lock");
    std::fs::write(&lock_path, "999999").unwrap();
    let out = r.sweep(vec![rec("feat/s", PrState::Merged, &tip, 1)]);
    assert_eq!(
        kinds(&out),
        [(Kind::PrunedBranch, "feat/s".into())],
        "{out:?}"
    );
}

#[test]
fn two_pruners_at_once_remove_each_worktree_once_and_fail_none() {
    let r = Repo::new();
    let mut records = Vec::new();
    for i in 0..4 {
        let b = format!("feat/p{i}");
        let wt = r.worktree(&b, &format!("wt-p{i}"));
        let head = Repo::commit(&wt, &format!("p{i}.txt"), &format!("p{i}"));
        r.squash_land(&b);
        records.push(rec(&b, PrState::Merged, &head, 20 + i));
    }
    let mut cfg_a = r.cfg();
    cfg_a.lock_wait = Duration::from_secs(60);
    let results: Vec<Vec<Outcome>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..2)
            .map(|_| s.spawn(|| r.sweep_with(&cfg_a, records.clone())))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let all: Vec<&Outcome> = results.iter().flatten().collect();
    assert!(
        all.iter().all(|o| o.kind == Kind::Pruned),
        "no failure, no busy: {all:?}"
    );
    assert_eq!(all.len(), 4, "each exactly once: {all:?}");
    for i in 0..4 {
        assert!(!r.path(&format!("wt-p{i}")).exists());
        assert!(!r.branch_exists(&format!("feat/p{i}")));
    }
}

/// Git whose worktree removal misbehaves like a Windows shell keeping the directory open.
struct Flaky {
    inner: GitCli,
    mode: Flake,
}

enum Flake {
    /// Removes the worktree but leaves an empty directory behind.
    LeavesEmptyDir,
    /// Leaves the directory behind with something in it, and says it is still there.
    Fails,
}

impl Git for Flaky {
    fn git_dir(&self) -> Res<PathBuf> {
        self.inner.git_dir()
    }
    fn fetch(&self) -> Res<()> {
        self.inner.fetch()
    }
    fn fetch_pr_head(&self, number: u64) -> Res<()> {
        self.inner.fetch_pr_head(number)
    }
    fn prune_worktrees(&self) -> Res<()> {
        self.inner.prune_worktrees()
    }
    fn worktrees(&self) -> Res<Vec<Worktree>> {
        self.inner.worktrees()
    }
    fn head(&self, dir: &Path) -> Res<String> {
        self.inner.head(dir)
    }
    fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.inner.is_ancestor(a, b)
    }
    fn unlanded(&self, base: &str, tip: &str, after: Option<&str>) -> Res<Vec<Commit>> {
        self.inner.unlanded(base, tip, after)
    }
    fn is_dirty(&self, dir: &Path, untracked: bool) -> Res<bool> {
        self.inner.is_dirty(dir, untracked)
    }
    fn branches(&self) -> Res<Vec<(String, String)>> {
        self.inner.branches()
    }
    fn delete_branch(&self, name: &str) -> Res<()> {
        self.inner.delete_branch(name)
    }
    fn remove_worktree(&self, dir: &Path) -> Res<()> {
        match self.mode {
            Flake::LeavesEmptyDir => {
                self.inner.remove_worktree(dir)?;
                std::fs::create_dir_all(dir)?;
                Ok(())
            }
            Flake::Fails => Err("the directory is in use".into()),
        }
    }
    fn switch_ff(&self, dir: &Path, branch: &str, to: &str) -> Res<()> {
        self.inner.switch_ff(dir, branch, to)
    }
}

fn flaky_sweep(r: &Repo, mode: Flake, records: Vec<PrRecord>) -> Vec<Outcome> {
    let gh = FakeGh {
        records,
        ..Default::default()
    };
    let git = Flaky {
        inner: GitCli {
            main: r.main.clone(),
        },
        mode,
    };
    let mut out = Vec::new();
    prune(&git, &gh, "o/n", &r.cfg(), &mut |o| out.push(o)).unwrap();
    out
}

#[test]
fn a_leftover_empty_directory_is_removed() {
    let r = Repo::new();
    let wt = r.worktree("feat/e", "wt-e");
    let head = Repo::commit(&wt, "e.txt", "e");
    r.squash_land("feat/e");
    let out = flaky_sweep(
        &r,
        Flake::LeavesEmptyDir,
        vec![rec("feat/e", PrState::Merged, &head, 1)],
    );
    assert_eq!(kinds(&out), [(Kind::Pruned, "feat/e".into())], "{out:?}");
    assert!(
        out[0].detail.is_none(),
        "not reported as held open: {out:?}"
    );
    assert!(!wt.exists());
}

#[test]
fn a_failed_removal_is_reported_and_keeps_the_branch() {
    let r = Repo::new();
    let wt = r.worktree("feat/f", "wt-f");
    let head = Repo::commit(&wt, "f.txt", "f");
    r.squash_land("feat/f");
    let out = flaky_sweep(
        &r,
        Flake::Fails,
        vec![rec("feat/f", PrState::Merged, &head, 1)],
    );
    assert_eq!(
        kinds(&out),
        [(Kind::PruneFailed, "feat/f".into())],
        "{out:?}"
    );
    assert!(out[0].text().contains("the directory is in use"));
    assert!(wt.exists());
    assert!(
        r.branch_exists("feat/f"),
        "a failed prune never drops the branch"
    );
}

#[test]
fn a_branch_inside_the_updated_head_of_a_merged_pr_is_pruned() {
    let r = Repo::new();
    let wt = r.worktree("feat/upd", "wt-upd");
    Repo::commit(&wt, "u1.txt", "u1");
    let tip = Repo::commit(&wt, "u2.txt", "u2");
    let m = r.update_branch_on_github(&tip, 9);
    assert!(
        !GitCli {
            main: r.main.clone()
        }
        .is_ancestor(&tip, &m),
        "the merge commit is not known locally yet"
    );
    r.squash_land("feat/upd");
    let out = r.sweep(vec![rec("feat/upd", PrState::Merged, &m, 9)]);
    assert_eq!(kinds(&out), [(Kind::Pruned, "feat/upd".into())], "{out:?}");
    assert!(!wt.exists());
    assert!(!r.branch_exists("feat/upd"));
}

#[test]
fn a_branch_inside_the_updated_head_of_a_closed_pr_is_kept() {
    let r = Repo::new();
    let wt = r.worktree("feat/cupd", "wt-cupd");
    let tip = Repo::commit(&wt, "c1.txt", "c1");
    let m = r.update_branch_on_github(&tip, 10);
    let out = r.sweep(vec![rec("feat/cupd", PrState::Closed, &m, 10)]);
    assert_eq!(
        kinds(&out),
        [(Kind::Unlanded, "feat/cupd".into())],
        "{out:?}"
    );
    assert!(wt.exists());
    assert!(r.branch_exists("feat/cupd"));
}

#[test]
fn commits_after_the_updated_head_are_still_unlanded() {
    let r = Repo::new();
    let wt = r.worktree("feat/lateupd", "wt-lateupd");
    let tip = Repo::commit(&wt, "l1.txt", "l1");
    let m = r.update_branch_on_github(&tip, 11);
    r.squash_land("feat/lateupd");
    Repo::commit(&wt, "l2.txt", "after the update");
    let out = r.sweep(vec![rec("feat/lateupd", PrState::Merged, &m, 11)]);
    assert_eq!(
        kinds(&out),
        [(Kind::Unlanded, "feat/lateupd".into())],
        "{out:?}"
    );
    assert!(wt.exists());
}
