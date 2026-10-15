//! `han dev prs`: the stateless reconciler. The invariant it keeps: every open auto-merge pull request is
//! either progressing or reported (the liveness invariant of r33 §3, for dogfood landing through GitHub).
//! Each round is one GraphQL query; a new session just runs it again.
//!
//! It exits (status 0) on the first round that has events, one line per event:
//!
//! - `MERGED #n`: a PR seen open earlier in this run has merged; the pruner then runs on that PR's
//!   branch only;
//! - `CLOSED #n`: closed without a merge; the pruner then runs on that PR's branch only;
//! - `CONFLICT #n`: the head conflicts with the base, and GitHub then runs no `pull_request` CI at all, so a
//!   required check never appears and the PR would wait for ever;
//! - `CI_FAILED #n checks`: a check failed, was cancelled or timed out;
//! - `STALLED #n reason`: nothing is pending and the PR has not merged for longer than `--grace` since the
//!   last push; also when auto-merge was switched off or `update-branch` failed;
//! - `GH_ERROR msg`.
//!
//! One event does not end the watch: `RETRIGGERED #n`. A PR with auto-merge on, mergeState CLEAN and every
//! check green that has been idle for more than `--grace` was missed by GitHub's auto-merge; it is switched
//! off and on again with the same merge method (never merged directly: branch protection still applies). If it
//! is still open and idle one more grace period later, it is `STALLED` as any other.
//!
//! Otherwise `OK watching: …` (`--once`) or `IDLE no open auto-merge PRs`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use clap::Args;
use serde::Serialize;

use super::Res;
use super::gh::{GhCli, Github, OpenPr, PrState, merge_flag};
use super::git::GitCli;
use super::prune::{self, Kind, Outcome, Scope};

/// What a PR needs a decision about.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    /// Merged.
    Merged,
    /// Closed without a merge.
    Closed,
    /// The head conflicts with the base.
    Conflict,
    /// A check failed.
    CiFailed,
    /// Nothing pending and not merging.
    Stalled,
    /// Auto-merge was switched off and on again; the watch goes on.
    Retriggered,
    /// GitHub could not be asked, or answered something unexpected.
    GhError,
}

/// One event of a round.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// What happened.
    #[serde(rename = "event")]
    pub kind: EventKind,
    /// The pull request, if the event is about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    /// Why, for people.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// For `CI_FAILED`: the checks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    /// For `MERGED` and `CLOSED`: the head branch, which the pruner is then limited to.
    #[serde(skip)]
    pub branch: String,
}

impl Event {
    fn new(kind: EventKind, pr: Option<u64>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            pr,
            detail: detail.into(),
            checks: Vec::new(),
            branch: String::new(),
        }
    }

    fn finished(kind: EventKind, pr: u64, branch: String) -> Self {
        Self {
            branch,
            ..Self::new(kind, Some(pr), "")
        }
    }

    /// The line for people.
    pub fn text(&self) -> String {
        let name = match self.kind {
            EventKind::Merged => "MERGED",
            EventKind::Closed => "CLOSED",
            EventKind::Conflict => "CONFLICT",
            EventKind::CiFailed => "CI_FAILED",
            EventKind::Stalled => "STALLED",
            EventKind::Retriggered => "RETRIGGERED",
            EventKind::GhError => "GH_ERROR",
        };
        let pr = self.pr.map(|n| format!(" #{n}")).unwrap_or_default();
        let sep = if self.detail.is_empty() { "" } else { " " };
        format!("{name}{pr}{sep}{}", self.detail)
    }
}

/// One line of the command's output.
#[derive(Clone, Debug)]
pub enum Line {
    /// An event that needs a decision.
    Event(Event),
    /// What the pruner did after a merge.
    Prune(Outcome),
    /// The PRs that are still open, after the events.
    Watching(Vec<u64>),
    /// Nothing is watched.
    Idle,
    /// `--once` and nothing to report.
    Ok(Vec<u64>),
}

impl Line {
    fn text(&self) -> String {
        let list = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        match self {
            Line::Event(e) => e.text(),
            Line::Prune(o) => o.text(),
            Line::Watching(v) => format!("watching: {}", list(v)),
            Line::Idle => "IDLE no open auto-merge PRs".into(),
            Line::Ok(v) => format!("OK watching: {}", list(v)),
        }
    }

    fn json(&self) -> serde_json::Value {
        match self {
            Line::Event(e) => serde_json::to_value(e),
            Line::Prune(o) => serde_json::to_value(o),
            Line::Watching(v) => Ok(serde_json::json!({"event": "WATCHING", "prs": v})),
            Line::Idle => Ok(serde_json::json!({"event": "IDLE"})),
            Line::Ok(v) => Ok(serde_json::json!({"event": "OK", "prs": v})),
        }
        .unwrap_or_default()
    }

    /// Prints it, as a line of text or as a JSON object.
    pub fn print(&self, json: bool) {
        if json {
            println!("{}", self.json());
        } else {
            println!("{}", self.text());
        }
    }
}

/// What a round found: the events, and the PRs still open.
pub struct Round {
    /// Things that need a decision.
    pub events: Vec<Event>,
    /// The watched PRs that are open.
    pub open: Vec<u64>,
}

/// One round: asks GitHub once, classifies every watched PR, and keeps `seen` (the PRs seen open in this run).
/// `only` is the set of PRs to watch (empty: all with auto-merge). A failed query is the error.
pub fn round(
    gh: &dyn Github,
    repo: &str,
    only: &BTreeSet<u64>,
    seen: &mut BTreeSet<u64>,
    retriggered: &mut BTreeMap<u64, i64>,
    now: i64,
    grace: i64,
) -> Result<Round, Event> {
    let prs = gh
        .auto_merge_prs(repo)
        .map_err(|e| Event::new(EventKind::GhError, None, e.to_string()))?;
    let mut events = Vec::new();
    let mut open = BTreeSet::new();
    for pr in prs
        .iter()
        .filter(|p| only.is_empty() || only.contains(&p.number))
    {
        open.insert(pr.number);
        seen.insert(pr.number);
        if let Some(e) = judge(gh, repo, pr, retriggered, now, grace) {
            events.push(e);
        }
    }
    // PRs that were open and no longer are: ask what became of them.
    for n in seen
        .iter()
        .copied()
        .filter(|n| !open.contains(n))
        .collect::<Vec<_>>()
    {
        events.push(match gh.pr_state(repo, n) {
            Ok((PrState::Merged, b)) => Event::finished(EventKind::Merged, n, b),
            Ok((PrState::Closed, b)) => Event::finished(EventKind::Closed, n, b),
            Ok((PrState::Open, _)) => {
                Event::new(EventKind::Stalled, Some(n), "auto-merge was disabled")
            }
            Err(e) => Event::new(EventKind::GhError, Some(n), e.to_string()),
        });
        seen.remove(&n);
        retriggered.remove(&n);
    }
    Ok(Round {
        events,
        open: open.into_iter().collect(),
    })
}

/// The verdict on one open PR. A conflicting PR gets no `pull_request` CI, so its required check never
/// appears: it is reported before anything is waited for.
fn judge(
    gh: &dyn Github,
    repo: &str,
    pr: &OpenPr,
    retriggered: &mut BTreeMap<u64, i64>,
    now: i64,
    grace: i64,
) -> Option<Event> {
    let n = Some(pr.number);
    if pr.mergeable == "CONFLICTING" {
        return Some(Event::new(
            EventKind::Conflict,
            n,
            format!(
                "(mergeState {}; no pull_request CI will run)",
                pr.merge_state
            ),
        ));
    }
    if !pr.failed.is_empty() {
        let mut e = Event::new(EventKind::CiFailed, n, pr.failed.join(","));
        e.checks = pr.failed.clone();
        return Some(e);
    }
    if pr.merge_state == "BEHIND" {
        return match gh.update_branch(repo, pr.number) {
            Ok(()) => None,
            Err(_) => Some(Event::new(
                EventKind::Stalled,
                n,
                "behind base and update-branch failed",
            )),
        };
    }
    let age = now - pr.pushed;
    if pr.running + pr.pending == 0 && age > grace {
        // CLEAN and green but idle: GitHub missed the merge. Switch auto-merge off and on once per grace
        // period; the second time round it is a stall like any other.
        let last = retriggered.get(&pr.number).copied();
        if pr.merge_state == "CLEAN" && last.is_none_or(|t| now - t <= grace) {
            if last.is_some() {
                return None;
            }
            if merge_flag(&pr.merge_method).is_some() {
                retriggered.insert(pr.number, now);
                return Some(retrigger(gh, repo, pr));
            }
        }
        return Some(Event::new(
            EventKind::Stalled,
            n,
            format!(
                "mergeable={} mergeState={}, nothing pending for {age}s since last push",
                pr.mergeable, pr.merge_state
            ),
        ));
    }
    None
}

/// Auto-merge off, then at once on again with the same method. Failing to re-enable leaves it off, and says so.
fn retrigger(gh: &dyn Github, repo: &str, pr: &OpenPr) -> Event {
    let n = Some(pr.number);
    if let Err(e) = gh.disable_auto_merge(repo, pr.number) {
        return Event::new(
            EventKind::Stalled,
            n,
            format!("clean and idle, and disabling auto-merge to retrigger it failed: {e}"),
        );
    }
    match gh.enable_auto_merge(repo, pr.number, &pr.merge_method) {
        Ok(()) => Event::new(EventKind::Retriggered, n, ""),
        Err(e) => Event::new(
            EventKind::Stalled,
            n,
            format!("auto-merge is now OFF: re-enabling it after the retrigger failed: {e}"),
        ),
    }
}

/// How `watch` runs.
pub struct Watch<'a> {
    /// GitHub.
    pub gh: &'a dyn Github,
    /// `owner/name`.
    pub repo: &'a str,
    /// The PRs to watch; empty means every open one with auto-merge.
    pub only: BTreeSet<u64>,
    /// Seconds without a push, with nothing pending, before a PR is stalled.
    pub grace: i64,
    /// Time between rounds.
    pub interval: Duration,
    /// One round, then stop.
    pub once: bool,
}

/// Rounds until one has events (or nothing is watched, or `once`). After a merge or a close, `prune` runs on the
/// head branches of the PRs that finished, and what it reports is shown, except that open or PR-less branches are not news.
pub fn watch(
    w: &Watch,
    now: &dyn Fn() -> i64,
    sleep: &dyn Fn(Duration),
    prune: &mut dyn FnMut(&[String]) -> Res<Vec<Outcome>>,
    emit: &mut dyn FnMut(Line),
) {
    let mut seen = BTreeSet::new();
    let mut retriggered = BTreeMap::new();
    loop {
        let mut r = match round(
            w.gh,
            w.repo,
            &w.only,
            &mut seen,
            &mut retriggered,
            now(),
            w.grace,
        ) {
            Ok(r) => r,
            Err(e) => return emit(Line::Event(e)),
        };
        // A retrigger is news but not an end: say it and keep watching.
        let (news, events): (Vec<_>, Vec<_>) = r
            .events
            .into_iter()
            .partition(|e| e.kind == EventKind::Retriggered);
        r.events = events;
        for e in news {
            emit(Line::Event(e));
        }
        if !r.events.is_empty() {
            // Only the branches of the PRs that finished in this round: other sessions' worktrees are theirs
            // to prune (`han dev prune` with no branch does the full sweep). No known branch, no pruning, as
            // an empty filter would mean every branch.
            let finished: Vec<String> = r
                .events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::Merged | EventKind::Closed))
                .map(|e| e.branch.clone())
                .filter(|b| !b.is_empty())
                .collect();
            for e in r.events {
                emit(Line::Event(e));
            }
            // A merge frees its worktree, Rust target and branch; the pruner keeps anything dirty or unpushed.
            if !finished.is_empty() {
                match prune(&finished) {
                    Ok(outcomes) => outcomes
                        .into_iter()
                        .filter(|o| !matches!(o.kind, Kind::KeptOpen | Kind::KeptNoPr))
                        .for_each(|o| emit(Line::Prune(o))),
                    Err(e) => emit(Line::Event(Event::new(
                        EventKind::GhError,
                        None,
                        format!("prune: {e}"),
                    ))),
                }
            }
            return emit(Line::Watching(r.open));
        }
        if r.open.is_empty() {
            return emit(Line::Idle);
        }
        if w.once {
            return emit(Line::Ok(r.open));
        }
        sleep(w.interval);
    }
}

/// Options of `han dev prs`.
#[derive(Args)]
pub struct PrsArgs {
    #[command(flatten)]
    scope: Scope,
    /// Seconds between rounds.
    #[arg(long, default_value_t = 120)]
    interval: u64,
    /// Seconds after the last push with nothing pending before a PR counts as stalled.
    #[arg(long, default_value_t = 900)]
    grace: u64,
    /// One round, then stop.
    #[arg(long)]
    once: bool,
    /// Print one JSON object per line.
    #[arg(long)]
    json: bool,
    /// Only these PRs; default: every open PR with auto-merge enabled.
    prs: Vec<u64>,
}

/// Runs `han dev prs`.
pub fn run(a: PrsArgs) -> Res<()> {
    let json = a.json;
    let r = a.scope.resolve(false, Vec::new())?;
    let w = Watch {
        gh: &GhCli,
        repo: &r.repo,
        only: a.prs.into_iter().collect(),
        grace: i64::try_from(a.grace)?,
        interval: Duration::from_secs(a.interval),
        once: a.once,
    };
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    };
    let git: &GitCli = &r.git;
    let mut prune_all = |branches: &[String]| {
        let mut out = Vec::new();
        let cfg = prune::Config {
            only: branches.to_vec(),
            ..r.cfg.clone()
        };
        prune::prune(git, &GhCli, &r.repo, &cfg, &mut |o| out.push(o))?;
        Ok(out)
    };
    watch(&w, &now, &std::thread::sleep, &mut prune_all, &mut |l| {
        l.print(json)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::dev::gh::fake::FakeGh;

    const NOW: i64 = 1_000_000;

    fn pr(n: u64) -> OpenPr {
        OpenPr {
            number: n,
            mergeable: "MERGEABLE".into(),
            merge_state: "BLOCKED".into(),
            pushed: NOW - 10,
            running: 1,
            pending: 0,
            failed: Vec::new(),
            merge_method: "SQUASH".into(),
        }
    }

    fn gh(rounds: Vec<Vec<OpenPr>>) -> FakeGh {
        FakeGh {
            rounds: std::sync::Mutex::new(rounds.into_iter().map(Ok).collect()),
            ..Default::default()
        }
    }

    /// Runs `watch` with a clock that never moves; returns the lines and how often it slept.
    fn run_watch(gh: &FakeGh, only: &[u64], once: bool, prune: Vec<Outcome>) -> (Vec<Line>, u32) {
        let (lines, slept, _) = run_watch_branches(gh, only, once, prune);
        (lines, slept)
    }

    /// Like `run_watch`; also returns the branches the pruner was limited to.
    fn run_watch_branches(
        gh: &FakeGh,
        only: &[u64],
        once: bool,
        prune: Vec<Outcome>,
    ) -> (Vec<Line>, u32, Vec<String>) {
        let asked = RefCell::new(Vec::new());
        let slept = Cell::new(0);
        let lines = RefCell::new(Vec::new());
        let pruned = Cell::new(false);
        let w = Watch {
            gh,
            repo: "o/n",
            only: only.iter().copied().collect(),
            grace: 900,
            interval: Duration::from_secs(1),
            once,
        };
        watch(
            &w,
            &|| NOW,
            &|_| slept.set(slept.get() + 1),
            &mut |branches| {
                assert!(!pruned.replace(true), "pruned twice");
                asked.borrow_mut().extend_from_slice(branches);
                Ok(prune.clone())
            },
            &mut |l| lines.borrow_mut().push(l),
        );
        (lines.into_inner(), slept.get(), asked.into_inner())
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::text).collect()
    }

    #[test]
    fn a_conflicting_pr_with_no_checks_is_reported_not_waited_for() {
        // PR #99: a conflict gets no pull_request CI, so the required check never shows up. Nothing is
        // running, nothing pending, nothing failed, and the push is recent (inside the grace period).
        let mut p = pr(99);
        p.mergeable = "CONFLICTING".into();
        p.merge_state = "DIRTY".into();
        p.running = 0;
        let (lines, slept) = run_watch(&gh(vec![vec![p]]), &[], false, vec![]);
        assert_eq!(slept, 0, "it did not wait");
        assert_eq!(
            texts(&lines),
            [
                "CONFLICT #99 (mergeState DIRTY; no pull_request CI will run)",
                "watching: 99"
            ]
        );
    }

    #[test]
    fn a_pr_with_no_checks_that_never_merges_stalls_after_the_grace_period() {
        let mut p = pr(5);
        p.running = 0;
        p.pushed = NOW - 901;
        let (lines, _) = run_watch(&gh(vec![vec![p.clone()]]), &[], false, vec![]);
        assert!(texts(&lines)[0].starts_with(
            "STALLED #5 mergeable=MERGEABLE mergeState=BLOCKED, nothing pending for 901s"
        ));
        // inside the grace period it is just waiting; pending checks are never stalled
        p.pushed = NOW - 900;
        let (lines, _) = run_watch(&gh(vec![vec![p.clone()]]), &[], true, vec![]);
        assert_eq!(texts(&lines), ["OK watching: 5"]);
        p.pushed = NOW - 99_999;
        p.pending = 1;
        let (lines, _) = run_watch(&gh(vec![vec![p]]), &[], true, vec![]);
        assert_eq!(texts(&lines), ["OK watching: 5"]);
    }

    /// A green, CLEAN PR that GitHub did not merge.
    fn missed(n: u64, idle: i64) -> OpenPr {
        let mut p = pr(n);
        p.merge_state = "CLEAN".into();
        p.running = 0;
        p.pushed = NOW - idle;
        p.merge_method = "REBASE".into();
        p
    }

    /// `watch` over several rounds with a clock that reads `times` (one per round); returns the lines.
    fn run_clock(g: &FakeGh, times: &[i64]) -> Vec<Line> {
        let round = Cell::new(0);
        let lines = RefCell::new(Vec::new());
        let w = Watch {
            gh: g,
            repo: "o/n",
            only: BTreeSet::new(),
            grace: 900,
            interval: Duration::from_secs(1),
            once: false,
        };
        watch(
            &w,
            &|| times[round.get().min(times.len() - 1)],
            &|_| round.set(round.get() + 1),
            &mut |_| Ok(Vec::new()),
            &mut |l| lines.borrow_mut().push(l),
        );
        lines.into_inner()
    }

    #[test]
    fn a_clean_green_idle_pr_is_retriggered_once_then_stalls_a_grace_period_later() {
        let g = gh(vec![vec![missed(116, 901)]]);
        let lines = run_clock(&g, &[NOW, NOW + 60, NOW + 900, NOW + 901]);
        assert_eq!(
            *g.toggled.lock().unwrap(),
            ["off #116", "on #116 REBASE"],
            "retriggered once, with the same method"
        );
        let t = texts(&lines);
        assert_eq!(t[0], "RETRIGGERED #116");
        assert_eq!(
            lines[0].json(),
            serde_json::json!({"event": "RETRIGGERED", "pr": 116})
        );
        assert!(
            t[1].starts_with("STALLED #116 mergeable=MERGEABLE mergeState=CLEAN"),
            "{t:?}"
        );
        assert_eq!(t[2], "watching: 116");
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn a_failed_re_enable_stalls_and_says_auto_merge_is_off() {
        let mut g = gh(vec![vec![missed(116, 901)]]);
        g.enable_fails = true;
        let (lines, _) = run_watch(&g, &[], false, vec![]);
        assert_eq!(
            texts(&lines)[0],
            "STALLED #116 auto-merge is now OFF: re-enabling it after the retrigger failed: enable refused"
        );
        assert_eq!(g.toggled.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_pr_with_pending_checks_or_inside_the_grace_period_is_not_retriggered() {
        let mut p = missed(1, 99_999);
        p.pending = 1;
        let mut q = missed(2, 99_999);
        q.running = 1;
        let g = gh(vec![vec![p, q, missed(3, 900)]]);
        let (lines, _) = run_watch(&g, &[], true, vec![]);
        assert_eq!(texts(&lines), ["OK watching: 1 2 3"]);
        assert!(g.toggled.lock().unwrap().is_empty());
    }

    #[test]
    fn a_blocked_idle_pr_and_one_with_an_unknown_method_stall_without_a_retrigger() {
        let mut blocked = missed(1, 901);
        blocked.merge_state = "BLOCKED".into();
        let mut odd = missed(2, 901);
        odd.merge_method = String::new();
        let g = gh(vec![vec![blocked, odd]]);
        let (lines, _) = run_watch(&g, &[], false, vec![]);
        let t = texts(&lines);
        assert!(t[0].starts_with("STALLED #1 "), "{t:?}");
        assert!(t[1].starts_with("STALLED #2 "), "{t:?}");
        assert!(g.toggled.lock().unwrap().is_empty());
    }

    #[test]
    fn a_behind_pr_still_goes_to_update_branch_not_a_retrigger() {
        let mut p = missed(3, 99_999);
        p.merge_state = "BEHIND".into();
        let g = gh(vec![vec![p]]);
        let (lines, _) = run_watch(&g, &[], true, vec![]);
        assert_eq!(*g.updated.lock().unwrap(), [3]);
        assert!(g.toggled.lock().unwrap().is_empty());
        assert_eq!(texts(&lines), ["OK watching: 3"]);
    }

    #[test]
    fn failed_checks_are_named() {
        let mut p = pr(7);
        p.failed = vec!["lint".into(), "test (windows)".into()];
        let (lines, _) = run_watch(&gh(vec![vec![p]]), &[], false, vec![]);
        assert_eq!(texts(&lines)[0], "CI_FAILED #7 lint,test (windows)");
        let Line::Event(e) = &lines[0] else { panic!() };
        let j = serde_json::to_value(e).unwrap();
        assert_eq!(j["event"], "CI_FAILED");
        assert_eq!(j["pr"], 7);
        assert_eq!(j["checks"][1], "test (windows)");
    }

    #[test]
    fn a_pr_behind_its_base_is_updated_and_a_failed_update_stalls_it() {
        let mut p = pr(3);
        p.merge_state = "BEHIND".into();
        let g = gh(vec![vec![p.clone()]]);
        let (lines, _) = run_watch(&g, &[], true, vec![]);
        assert_eq!(*g.updated.lock().unwrap(), [3]);
        assert_eq!(texts(&lines), ["OK watching: 3"]);

        let mut g = gh(vec![vec![p]]);
        g.update_fails = true;
        let (lines, _) = run_watch(&g, &[], true, vec![]);
        assert_eq!(
            texts(&lines)[0],
            "STALLED #3 behind base and update-branch failed"
        );
    }

    #[test]
    fn a_merge_ends_the_watch_and_runs_the_pruner() {
        let g = gh(vec![vec![pr(1), pr(2)], vec![pr(2)]]);
        g.states.lock().unwrap().insert(1, PrState::Merged);
        let news = Outcome::failed(Some("H:/HAN/wt-x"), "feat/x", None, "boom");
        let noise = Outcome::new(Kind::KeptOpen, Some("H:/HAN/wt-y"), "feat/y", None);
        let (lines, slept) = run_watch(&g, &[], false, vec![noise, news]);
        assert_eq!(slept, 1, "one quiet round, then the merge");
        assert_eq!(
            texts(&lines),
            [
                "MERGED #1",
                "PRUNE_FAILED H:/HAN/wt-x (feat/x): boom",
                "watching: 2"
            ]
        );
    }

    #[test]
    fn only_the_branches_of_the_finished_prs_are_pruned() {
        // #1 and #2 both merged, but only #1 was watched; #2's worktree belongs to another session.
        let g = gh(vec![vec![pr(1)], vec![]]);
        {
            let mut s = g.states.lock().unwrap();
            s.insert(1, PrState::Merged);
            s.insert(2, PrState::Merged);
            let mut b = g.branches.lock().unwrap();
            b.insert(1, "feat/one".into());
            b.insert(2, "feat/two".into());
        }
        let (lines, _, asked) = run_watch_branches(&g, &[1], false, vec![]);
        assert_eq!(texts(&lines)[0], "MERGED #1");
        assert_eq!(asked, ["feat/one"]);
    }

    #[test]
    fn closed_disabled_and_unknown_prs_are_told_apart() {
        let g = gh(vec![vec![pr(1), pr(2), pr(3)], vec![]]);
        {
            let mut s = g.states.lock().unwrap();
            s.insert(1, PrState::Closed);
            s.insert(2, PrState::Open);
        }
        let (lines, _) = run_watch(&g, &[], false, vec![]);
        assert_eq!(
            texts(&lines)[..3],
            [
                "CLOSED #1",
                "STALLED #2 auto-merge was disabled",
                "GH_ERROR #3 not found"
            ]
        );
    }

    #[test]
    fn nothing_to_watch_is_idle_and_a_query_failure_is_an_error_event() {
        let (lines, _) = run_watch(&gh(vec![vec![]]), &[], false, vec![]);
        assert_eq!(texts(&lines), ["IDLE no open auto-merge PRs"]);
        assert_eq!(lines[0].json()["event"], "IDLE");

        let g = FakeGh::default();
        g.rounds
            .lock()
            .unwrap()
            .push_back(Err("rate limited".into()));
        let (lines, _) = run_watch(&g, &[], false, vec![]);
        assert_eq!(texts(&lines), ["GH_ERROR rate limited"]);
    }

    #[test]
    fn named_prs_are_the_only_ones_watched() {
        let mut conflicting = pr(2);
        conflicting.mergeable = "CONFLICTING".into();
        let g = gh(vec![vec![pr(1), conflicting]]);
        let (lines, _) = run_watch(&g, &[1], true, vec![]);
        assert_eq!(texts(&lines), ["OK watching: 1"]);
    }

    #[test]
    fn json_lines_carry_the_event_name() {
        let (lines, _) = run_watch(&gh(vec![vec![pr(4)]]), &[], true, vec![]);
        assert_eq!(
            lines[0].json(),
            serde_json::json!({"event": "OK", "prs": [4]})
        );
    }
}
