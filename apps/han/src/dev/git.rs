//! Git access for `han dev prune`: the git CLI behind a small trait, so the pruner is tested against real
//! temporary repositories and could one day run on another backend.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::Res;

/// One entry of `git worktree list` that has a branch checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// Where it is, as git spells it (forward slashes on Windows).
    pub path: String,
    /// The branch checked out, without `refs/heads/`.
    pub branch: String,
}

/// A commit named by its short id and subject.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Commit {
    /// The first 8 hex digits.
    pub id: String,
    /// The first line of the message.
    pub subject: String,
}

/// The git operations of the pruner, all on the repository of the main checkout.
pub trait Git {
    /// The absolute git directory of the main checkout (where the pruner's lock lives).
    fn git_dir(&self) -> Res<PathBuf>;
    /// Updates the remote-tracking refs.
    fn fetch(&self) -> Res<()>;
    /// Forgets worktrees whose directory is gone.
    fn prune_worktrees(&self) -> Res<()>;
    /// The worktrees that have a branch checked out.
    fn worktrees(&self) -> Res<Vec<Worktree>>;
    /// The commit a checkout is at.
    fn head(&self, dir: &Path) -> Res<String>;
    /// Whether `a` is an ancestor of (or equal to) `b`.
    fn is_ancestor(&self, a: &str, b: &str) -> bool;
    /// The commits of `tip` (after `after`, if given) whose patch is not on `base`: `git cherry` marks them `+`.
    fn unlanded(&self, base: &str, tip: &str, after: Option<&str>) -> Res<Vec<Commit>>;
    /// Whether a checkout has changes; untracked files count only when `untracked`.
    fn is_dirty(&self, dir: &Path, untracked: bool) -> Res<bool>;
    /// Local branches and their tips.
    fn branches(&self) -> Res<Vec<(String, String)>>;
    /// Deletes a local branch; one that is already gone is done.
    fn delete_branch(&self, name: &str) -> Res<()>;
    /// Removes a clean worktree; one that is already gone is done. An error says why it stayed.
    fn remove_worktree(&self, dir: &Path) -> Res<()>;
    /// In a checkout: switches to `branch`, then fast-forwards it to `to`.
    fn switch_ff(&self, dir: &Path, branch: &str, to: &str) -> Res<()>;
}

/// The git CLI.
pub struct GitCli {
    /// Any checkout of the repository; the main one.
    pub main: PathBuf,
}

impl GitCli {
    fn run_in(dir: &Path, args: &[&str]) -> Res<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("git {}: {}", args.join(" "), err.trim()).into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run(&self, args: &[&str]) -> Res<String> {
        Self::run_in(&self.main, args)
    }

    /// The main checkout of the repository that contains `dir`: the first entry of `git worktree list`.
    pub fn main_of(dir: &Path) -> Res<PathBuf> {
        let text = Self::run_in(dir, &["worktree", "list", "--porcelain"])?;
        text.lines()
            .find_map(|l| l.strip_prefix("worktree "))
            .map(PathBuf::from)
            .ok_or_else(|| format!("{} is not in a git repository", dir.display()).into())
    }

    /// The URL of the `origin` remote.
    pub fn origin_url(&self) -> Res<String> {
        Ok(self
            .run(&["remote", "get-url", "origin"])?
            .trim()
            .to_owned())
    }

    fn branch_exists(&self, name: &str) -> bool {
        self.run(&["show-ref", "-q", "--verify", &format!("refs/heads/{name}")])
            .is_ok()
    }
}

impl Git for GitCli {
    fn git_dir(&self) -> Res<PathBuf> {
        Ok(PathBuf::from(
            self.run(&["rev-parse", "--absolute-git-dir"])?.trim(),
        ))
    }

    fn fetch(&self) -> Res<()> {
        self.run(&["fetch", "-q", "origin"]).map(|_| ())
    }

    fn prune_worktrees(&self) -> Res<()> {
        self.run(&["worktree", "prune"]).map(|_| ())
    }

    fn worktrees(&self) -> Res<Vec<Worktree>> {
        Ok(parse_worktrees(&self.run(&[
            "worktree",
            "list",
            "--porcelain",
        ])?))
    }

    fn head(&self, dir: &Path) -> Res<String> {
        Ok(Self::run_in(dir, &["rev-parse", "HEAD"])?.trim().to_owned())
    }

    fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.run(&["merge-base", "--is-ancestor", a, b]).is_ok()
    }

    fn unlanded(&self, base: &str, tip: &str, after: Option<&str>) -> Res<Vec<Commit>> {
        let mut args = vec!["cherry", "-v", base, tip];
        args.extend(after);
        Ok(self
            .run(&args)?
            .lines()
            .filter_map(|l| {
                let rest = l.strip_prefix("+ ")?;
                let (id, subject) = rest.split_once(' ').unwrap_or((rest, ""));
                Some(Commit {
                    id: id.chars().take(8).collect(),
                    subject: subject.to_owned(),
                })
            })
            .collect())
    }

    fn is_dirty(&self, dir: &Path, untracked: bool) -> Res<bool> {
        let mut args = vec!["status", "--porcelain"];
        if !untracked {
            args.push("--untracked-files=no");
        }
        Ok(!Self::run_in(dir, &args)?.trim().is_empty())
    }

    fn branches(&self) -> Res<Vec<(String, String)>> {
        let text = self.run(&[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads",
        ])?;
        Ok(text
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(b, o)| (b.to_owned(), o.to_owned()))
            .collect())
    }

    fn delete_branch(&self, name: &str) -> Res<()> {
        if !self.branch_exists(name) {
            return Ok(());
        }
        match self.run(&["branch", "-D", name]) {
            // a concurrent run may have deleted it between the check and the delete
            Err(_) if !self.branch_exists(name) => Ok(()),
            r => r.map(|_| ()),
        }
    }

    fn remove_worktree(&self, dir: &Path) -> Res<()> {
        let err = match Self::run_in(&self.main, &["worktree", "remove", &dir.to_string_lossy()]) {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        // Already gone (a concurrent run, or the directory was deleted by hand): forget it and look again.
        self.prune_worktrees()?;
        let want = norm(&dir.to_string_lossy());
        if self.worktrees()?.iter().any(|w| norm(&w.path) == want) {
            return Err(err);
        }
        Ok(())
    }

    fn switch_ff(&self, dir: &Path, branch: &str, to: &str) -> Res<()> {
        Self::run_in(dir, &["switch", "-q", branch])?;
        Self::run_in(dir, &["merge", "-q", "--ff-only", to]).map(|_| ())
    }
}

/// The worktrees of `git worktree list --porcelain` that have a branch (a detached or bare one has none).
pub fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    let mut path = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_owned());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/")
            && let Some(p) = path.take()
        {
            out.push(Worktree {
                path: p,
                branch: b.to_owned(),
            });
        }
    }
    out
}

/// A path in one spelling for comparing: forward slashes, no trailing slash, and case-folded on Windows.
pub fn norm(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.trim_end_matches('/');
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_listings_are_parsed() {
        let text = "worktree H:/HAN/v1\nHEAD aaa\nbranch refs/heads/main\n\n\
                    worktree H:/HAN/wt-a b\nHEAD bbb\nbranch refs/heads/feat/x\n\n\
                    worktree H:/HAN/wt-d\nHEAD ccc\ndetached\n\n";
        assert_eq!(
            parse_worktrees(text),
            [
                Worktree {
                    path: "H:/HAN/v1".into(),
                    branch: "main".into()
                },
                Worktree {
                    path: "H:/HAN/wt-a b".into(),
                    branch: "feat/x".into()
                },
            ]
        );
    }

    #[test]
    fn paths_compare_across_spellings() {
        assert_eq!(norm("H:\\HAN\\v1\\"), norm("H:/HAN/v1"));
    }
}
