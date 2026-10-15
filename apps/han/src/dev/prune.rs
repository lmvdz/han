//! `han dev prune`: removes agent worktrees and local branches whose pull request has merged or closed, with
//! the worktree's Rust target directory. One `gh pr list` call per run.
//!
//! It never removes: a dirty worktree; a worktree or branch whose tip is not the PR's recorded head (commits
//! made after the PR) unless the base has them by patch id; the main checkout; a worktree outside `--root`
//! (so another tool's workspaces stay untouchable). Work made after a PR merged never lands by itself, so
//! that is reported as `UNLANDED`, an event and not housekeeping.
//!
//! This is the reference for the host's pruning duty (r33 §7): the logic is in [`prune`], over the [`Git`]
//! and [`Github`] traits; the command is a thin shell around it.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Args;
use serde::Serialize;

use super::Res;
use super::gh::{GhCli, Github, PrRecord, PrState, repo_of_remote, split_repo};
use super::git::{Commit, Git, GitCli, norm};

/// What happened to one candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Kind {
    /// A worktree, its target directory and its branch are gone.
    Pruned,
    /// A local branch with no worktree is gone.
    PrunedBranch,
    /// The main checkout went back to the base and its landed branch was deleted.
    Switched,
    /// Commits made after the PR are not on the base: an event.
    Unlanded,
    /// Removal failed; the detail says why.
    PruneFailed,
    /// Another pruner holds the lock.
    PruneBusy,
    /// Uncommitted changes.
    KeptDirty,
    /// The PR is still open.
    KeptOpen,
    /// The branch never had a PR.
    KeptNoPr,
    /// `--dry-run`: a worktree would be pruned.
    WouldPrune,
    /// `--dry-run`: a branch would be pruned.
    WouldPruneBranch,
    /// `--dry-run`: the main checkout would be switched.
    WouldSwitch,
}

/// One line of output.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// What happened.
    #[serde(rename = "event")]
    pub kind: Kind,
    /// The worktree; none for a branch with no worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The branch.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub branch: String,
    /// The branch's newest PR.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    /// That PR's state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<&'static str>,
    /// An error, a note or the ref a checkout went to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// For `UNLANDED`: the commits the base lacks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<Commit>,
}

impl Outcome {
    pub(super) fn new(
        kind: Kind,
        path: Option<&str>,
        branch: &str,
        rec: Option<&PrRecord>,
    ) -> Self {
        Self {
            kind,
            path: path.map(str::to_owned),
            branch: branch.to_owned(),
            pr: rec.map(|r| r.number),
            pr_state: rec.map(|r| r.state.as_str()),
            detail: None,
            commits: Vec::new(),
        }
    }

    pub(super) fn with(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(super) fn failed(
        path: Option<&str>,
        branch: &str,
        rec: Option<&PrRecord>,
        err: &str,
    ) -> Self {
        Self::new(Kind::PruneFailed, path, branch, rec).with(err)
    }

    /// The line for people (several for `UNLANDED`).
    pub fn text(&self) -> String {
        let name = match self.kind {
            Kind::Pruned => "PRUNED",
            Kind::PrunedBranch => "PRUNED_BRANCH",
            Kind::Switched => "SWITCHED",
            Kind::Unlanded => "UNLANDED",
            Kind::PruneFailed => "PRUNE_FAILED",
            Kind::PruneBusy => "PRUNE_BUSY",
            Kind::KeptDirty => "KEPT_DIRTY",
            Kind::KeptOpen => "KEPT_OPEN",
            Kind::KeptNoPr => "KEPT_NO_PR",
            Kind::WouldPrune => "WOULD_PRUNE",
            Kind::WouldPruneBranch => "WOULD_PRUNE_BRANCH",
            Kind::WouldSwitch => "WOULD_SWITCH",
        };
        let b = &self.branch;
        let place = self.path.as_deref().unwrap_or("branch");
        let detail = self.detail.as_deref().unwrap_or_default();
        let state = self.pr_state.unwrap_or("NONE");
        let n = self.pr.unwrap_or(0);
        let rest = match self.kind {
            Kind::Pruned | Kind::WouldPrune => {
                let note = if detail.is_empty() {
                    String::new()
                } else {
                    format!("; {detail}")
                };
                format!("{place} ({b}, PR {state}){note}")
            }
            Kind::PrunedBranch | Kind::WouldPruneBranch => format!("{b} (PR {state})"),
            Kind::Switched => {
                format!("{place} from {b} (PR {state}) to {detail}; branch deleted")
            }
            Kind::WouldSwitch => format!("{place} from {b} to {detail}"),
            Kind::Unlanded => {
                let mut s = format!("{place} ({b}, PR #{n} {state}, {detail}:");
                for c in &self.commits {
                    s.push_str(&format!("\n    {} {}", c.id, c.subject));
                }
                s.push_str("\n  )");
                s
            }
            Kind::PruneFailed => match &self.path {
                Some(p) => format!("{p} ({b}): {detail}"),
                None => format!("branch {b}: {detail}"),
            },
            Kind::PruneBusy => detail.to_owned(),
            Kind::KeptDirty => format!("{place} ({b}, PR {state})"),
            Kind::KeptOpen | Kind::KeptNoPr => format!("{place} ({b})"),
        };
        format!("{name} {rest}")
    }

    /// Prints it, as a line of text or as a JSON object.
    pub fn print(&self, json: bool) {
        if json {
            println!("{}", serde_json::to_string(self).unwrap_or_default());
        } else {
            println!("{}", self.text());
        }
    }
}

/// What to prune and where; shared with `han dev prs`, which prunes after a merge.
#[derive(Clone)]
pub struct Config {
    /// The main checkout: never removed.
    pub main: PathBuf,
    /// Only worktrees whose path starts with this are touched.
    pub root: String,
    /// The ref work must be on to count as landed, for example `origin/main`.
    pub base: String,
    /// Report what would happen and change nothing.
    pub dry_run: bool,
    /// Only these branches; empty means all.
    pub only: Vec<String>,
    /// How long to wait for another pruner.
    pub lock_wait: Duration,
}

/// The local name of the base: `main` for `origin/main`.
fn local_base(base: &str) -> &str {
    base.split_once('/').map_or(base, |(_, b)| b)
}

/// A cross-process lock: an advisory lock on a file in the git directory. The operating system releases it
/// when its holder exits, however it exits, so a dead owner cannot wedge the next run.
struct Lock {
    _file: File,
}

fn acquire(path: &Path, wait: Duration) -> Res<Option<Lock>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(Lock { _file: file })),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

/// The verdict for a branch at `tip`.
enum Verdict {
    /// Everything on it has landed or was never ours to keep.
    Go,
    /// Keep it, and say why.
    Keep(Box<Outcome>),
}

fn judge(
    git: &dyn Git,
    base: &str,
    rec: Option<&PrRecord>,
    branch: &str,
    tip: &str,
    path: Option<&str>,
) -> Res<Verdict> {
    let keep = |k| Ok(Verdict::Keep(Box::new(Outcome::new(k, path, branch, rec))));
    let Some(rec) = rec else {
        return keep(Kind::KeptNoPr);
    };
    if rec.state == PrState::Open {
        return keep(Kind::KeptOpen);
    }
    if tip == rec.head || git.is_ancestor(tip, base) {
        return Ok(Verdict::Go);
    }
    // `gh pr update-branch` puts a merge commit on GitHub only: the PR's head then contains the local tip, and
    // the head may not exist here, so it is fetched once. A closed PR's head never landed.
    if rec.state == PrState::Merged
        && (git.is_ancestor(tip, &rec.head)
            || (git.fetch_pr_head(rec.number).is_ok() && git.is_ancestor(tip, &rec.head)))
    {
        return Ok(Verdict::Go);
    }
    // A squash merge never matches the PR's own commits by patch id, so only what came after the PR's head
    // is judged; a branch that was rewritten (the head is not an ancestor) is judged whole.
    let after = git.is_ancestor(&rec.head, tip).then_some(rec.head.as_str());
    let lost = git.unlanded(base, tip, after)?;
    if lost.is_empty() {
        return Ok(Verdict::Go);
    }
    let mut o = Outcome::new(Kind::Unlanded, path, branch, Some(rec))
        .with(format!("commits not on {base}"));
    o.commits = lost;
    Ok(Verdict::Keep(Box::new(o)))
}

/// Removes a directory tree; one that is not there is done.
fn remove_tree(p: &Path) -> Res<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("cannot remove {}: {e}", p.display()).into())
        }
        _ => Ok(()),
    }
}

/// Sweeps the repository of `git`: every worktree under `cfg.root` and every local branch whose PR is final.
/// Each candidate is reported through `emit`; a failure on one is reported and the sweep goes on.
pub fn prune(
    git: &dyn Git,
    gh: &dyn Github,
    repo: &str,
    cfg: &Config,
    emit: &mut dyn FnMut(Outcome),
) -> Res<()> {
    // branch -> its newest PR (gh lists newest first, so the first row per branch wins)
    let mut prs: HashMap<String, PrRecord> = HashMap::new();
    for r in gh.pr_records(repo)? {
        prs.entry(r.branch.clone()).or_insert(r);
    }
    // One pruner per repository at a time, across sessions: concurrent runs race on worktree removal and on
    // .git/config's lock.
    let lock_path = git.git_dir()?.join("han-prune.lock");
    let Some(_lock) = acquire(&lock_path, cfg.lock_wait)? else {
        emit(
            Outcome::new(Kind::PruneBusy, None, "", None)
                .with(format!("another pruner holds {}", lock_path.display())),
        );
        return Ok(());
    };
    // A stale base only ever makes more look unlanded, never fewer, so a failed fetch is not fatal.
    let _ = git.fetch();
    git.prune_worktrees()?;

    let wanted = |b: &str| cfg.only.is_empty() || cfg.only.iter().any(|o| o == b);
    let base = cfg.base.as_str();
    let main_n = norm(&cfg.main.to_string_lossy());
    let root_n = norm(&cfg.root);
    let mut checked_out = HashSet::new();

    for wt in git.worktrees()? {
        checked_out.insert(wt.branch.clone());
        if !wanted(&wt.branch) {
            continue;
        }
        let rec = prs.get(&wt.branch);
        let result = if norm(&wt.path) == main_n {
            // The main checkout is never removed, but a clean one left on a landed branch goes back to the base.
            if wt.branch == local_base(base) || rec.is_none() {
                continue;
            }
            switch_main(git, cfg, &wt.path, &wt.branch, rec, emit)
        } else if norm(&wt.path).starts_with(&root_n) {
            prune_worktree(git, cfg, &wt.path, &wt.branch, rec, emit)
        } else {
            continue;
        };
        if let Err(e) = result {
            emit(Outcome::failed(
                Some(&wt.path),
                &wt.branch,
                rec,
                &e.to_string(),
            ));
        }
    }

    // Local branches with no worktree.
    for (b, tip) in git.branches()? {
        if checked_out.contains(&b) || !wanted(&b) {
            continue;
        }
        let Some(rec) = prs.get(&b) else { continue }; // never had a PR: not ours to judge
        let outcome = match judge(git, base, Some(rec), &b, &tip, None) {
            Ok(Verdict::Keep(o)) => *o,
            Ok(Verdict::Go) if cfg.dry_run => {
                Outcome::new(Kind::WouldPruneBranch, None, &b, Some(rec))
            }
            Ok(Verdict::Go) => match git.delete_branch(&b) {
                Ok(()) => Outcome::new(Kind::PrunedBranch, None, &b, Some(rec)),
                Err(e) => Outcome::failed(None, &b, Some(rec), &e.to_string()),
            },
            Err(e) => Outcome::failed(None, &b, Some(rec), &e.to_string()),
        };
        emit(outcome);
    }
    Ok(())
}

fn switch_main(
    git: &dyn Git,
    cfg: &Config,
    path: &str,
    branch: &str,
    rec: Option<&PrRecord>,
    emit: &mut dyn FnMut(Outcome),
) -> Res<()> {
    let dir = Path::new(path);
    let to = local_base(&cfg.base);
    if let Verdict::Keep(o) = judge(git, &cfg.base, rec, branch, &git.head(dir)?, Some(path))? {
        emit(*o);
        return Ok(());
    }
    if git.is_dirty(dir, false)? {
        emit(Outcome::new(Kind::KeptDirty, Some(path), branch, rec));
    } else if cfg.dry_run {
        emit(Outcome::new(Kind::WouldSwitch, Some(path), branch, rec).with(to));
    } else {
        git.switch_ff(dir, to, &cfg.base)?;
        git.delete_branch(branch)?;
        emit(Outcome::new(Kind::Switched, Some(path), branch, rec).with(to));
    }
    Ok(())
}

fn prune_worktree(
    git: &dyn Git,
    cfg: &Config,
    path: &str,
    branch: &str,
    rec: Option<&PrRecord>,
    emit: &mut dyn FnMut(Outcome),
) -> Res<()> {
    let dir = Path::new(path);
    let tip = match git.head(dir) {
        Ok(t) => t,
        // Removed by someone else since the listing: done.
        Err(_) if !dir.exists() => return Ok(()),
        Err(e) => return Err(e),
    };
    if let Verdict::Keep(o) = judge(git, &cfg.base, rec, branch, &tip, Some(path))? {
        emit(*o);
        return Ok(());
    }
    if git.is_dirty(dir, true)? {
        emit(Outcome::new(Kind::KeptDirty, Some(path), branch, rec));
        return Ok(());
    }
    if cfg.dry_run {
        emit(Outcome::new(Kind::WouldPrune, Some(path), branch, rec));
        return Ok(());
    }
    remove_tree(&dir.join("target"))?;
    remove_tree(Path::new(&format!("{path}-target")))?;
    let removed = git.remove_worktree(dir);
    // A directory the OS keeps (a shell's cwd on Windows) may stay behind empty; remove it when it is.
    if dir.is_dir() && std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none()) {
        let _ = std::fs::remove_dir(dir);
    }
    removed?;
    git.delete_branch(branch)?;
    let mut o = Outcome::new(Kind::Pruned, Some(path), branch, rec);
    if dir.exists() {
        o = o.with(format!("directory {path} is held open, remove it later"));
    }
    emit(o);
    Ok(())
}

/// Where a command looks: the repository, the main checkout, the worktree prefix, the base.
#[derive(Args)]
pub struct Scope {
    /// The GitHub repository, `owner/name`. Default: the main checkout's `origin` remote.
    #[arg(long)]
    pub repo: Option<String>,
    /// The main checkout, which is never removed. Default: the first worktree of the repository in the current
    /// directory.
    #[arg(long)]
    pub main: Option<PathBuf>,
    /// Only worktrees whose path starts with this are touched. Default: `wt-` next to the main checkout.
    #[arg(long)]
    pub root: Option<String>,
    /// The ref that work must be on to count as landed.
    #[arg(long, default_value = "origin/main")]
    pub base: String,
}

/// A [`Scope`] with its defaults filled in.
pub struct Resolved {
    /// Git on the main checkout.
    pub git: GitCli,
    /// `owner/name`.
    pub repo: String,
    /// The pruner's configuration.
    pub cfg: Config,
}

impl Scope {
    /// Finds the main checkout, the repository and the worktree prefix.
    pub fn resolve(self, dry_run: bool, only: Vec<String>) -> Res<Resolved> {
        let main = match self.main {
            Some(m) => m,
            None => GitCli::main_of(&std::env::current_dir()?)?,
        };
        let git = GitCli { main: main.clone() };
        let repo = match self.repo {
            Some(r) => {
                split_repo(&r)?;
                r
            }
            None => {
                let url = git.origin_url()?;
                repo_of_remote(&url).ok_or_else(|| {
                    format!("the origin remote {url:?} is not a GitHub repository: pass --repo")
                })?
            }
        };
        let root = match self.root {
            Some(r) => r,
            None => {
                let parent = main
                    .parent()
                    .ok_or("the main checkout has no parent directory")?;
                format!(
                    "{}/wt-",
                    parent
                        .to_string_lossy()
                        .replace('\\', "/")
                        .trim_end_matches('/')
                )
            }
        };
        Ok(Resolved {
            git,
            repo,
            cfg: Config {
                main,
                root,
                base: self.base,
                dry_run,
                only,
                lock_wait: Duration::from_secs(60),
            },
        })
    }
}

/// Options of `han dev prune`.
#[derive(Args)]
pub struct PruneArgs {
    #[command(flatten)]
    scope: Scope,
    /// Say what would be pruned and change nothing.
    #[arg(long)]
    dry_run: bool,
    /// Print one JSON object per outcome.
    #[arg(long)]
    json: bool,
    /// Only these branches.
    branches: Vec<String>,
}

/// Runs `han dev prune`.
pub fn run(a: PruneArgs) -> Res<()> {
    let json = a.json;
    let r = a.scope.resolve(a.dry_run, a.branches)?;
    prune(&r.git, &GhCli, &r.repo, &r.cfg, &mut |o| o.print(json))
}

#[cfg(test)]
mod tests;
