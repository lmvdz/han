//! The `Checks` seam (R33 §4 "Where checks run"): how a landing lane learns whether the commit it is about to
//! land passes the repository's checks.
//!
//! A backend answers two questions about a commit: which check runs exist for it and how each ended
//! ([`Checks::poll`]), and, where it can, run the red ones again ([`Checks::rerun`]). The lane decides what to
//! do with the answer; it never trusts a backend to say "green" for it, it reads the runs and compares them
//! with the repository's required names ([`crate::ChecksConfig`]).
//!
//! * [`GithubChecks`]: GitHub Actions. It runs by itself once the lane has pushed the commit to its lane
//!   branch (and, for the post-merge stage, once the commit is on the target ref), so [`Checks::start`] has
//!   nothing to do; `poll` lists the commit's check runs and commit statuses with the App's installation
//!   token (`checks: read`, `statuses: read`, one repository, SR-67). `rerun` runs the failed jobs of the
//!   workflow runs behind the named check runs (`actions: write`, minted for that call alone) and remembers the
//!   attempt it asked for, so `poll` does not answer with the attempt that was red (`actions: read` for that).
//! * A warm runner (PR 4) has to be told to run a commit, which is what [`Checks::start`] is for, and it can
//!   answer `poll` from its own results. It does not need GitHub.
//!
//! What a lane keeps of an answer is a [`ChecksEvidence`]: `{commit, checks: [{name, conclusion, url}],
//! backend}`, a plain value whose canonical form ([`ChecksEvidence::canonical`]) is what an executor instance
//! key signs, so the evidence can travel with the landed change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RepoTarget;
use crate::github::GithubMinter;

pub mod warm;

pub use warm::{RoutedChecks, WarmChecks};

/// Which stage of a repository's checks a request is about (R33 §4 "Two stages").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The pre-merge checks, on the exact commit the lane lands (`checks.required`).
    Fast,
    /// The post-merge checks, on a target that contains the change (`checks.full`).
    Full,
}

/// What a backend is asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRequest {
    /// The repository, as configured.
    pub repo: RepoTarget,
    /// The commit (full id).
    pub commit: String,
    /// The branch the commit was pushed to for testing, when it was (the lane branch for `Fast`, the target
    /// ref's short name for `Full`).
    pub branch: Option<String>,
    /// The stage.
    pub stage: Stage,
    /// The check names the lane is waiting for.
    pub names: Vec<String>,
}

/// One check run of a commit, as the backend reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    /// The check's name (GitHub: the job's check run name, or a status context).
    pub name: String,
    /// How it ended: `success`, `failure`, `neutral`, `skipped`, `cancelled`, `timed_out`, … as the backend
    /// words it, or `pending` while it has not ended.
    pub conclusion: String,
    /// A link to the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The command a step ran (a runner that runs steps itself; GitHub has none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The step's exit status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// How long the step took, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Where the step's log is: a `BlobRef` as JSON, or a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<Value>,
}

impl CheckRun {
    /// A run with a name, a conclusion and a link, and no step details.
    pub fn new(name: &str, conclusion: &str, url: Option<String>) -> CheckRun {
        CheckRun {
            name: name.to_owned(),
            conclusion: conclusion.to_owned(),
            url,
            command: None,
            exit: None,
            duration_ms: None,
            log: None,
        }
    }
}

/// How the lane reads a conclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Passed.
    Green,
    /// Ended, and not in a way that passes.
    Red,
    /// Not ended yet.
    Pending,
    /// No run of that name exists (yet).
    Missing,
}

impl CheckRun {
    /// What the run's conclusion means for the lane: `success`, `neutral` and `skipped` pass; a run that
    /// has not ended (`pending`, `queued`, `in_progress`, `waiting`, `requested`) is pending; everything else
    /// (`failure`, `error`, `cancelled`, `timed_out`, `action_required`, `stale`, …) is red.
    pub fn verdict(&self) -> Verdict {
        match self.conclusion.as_str() {
            "success" | "neutral" | "skipped" => Verdict::Green,
            "pending" | "queued" | "in_progress" | "waiting" | "requested" => Verdict::Pending,
            _ => Verdict::Red,
        }
    }
}

/// The verdict on one required name over a list of runs: red if any run of that name is red, else pending if
/// any is pending, else green if there is one, else missing.
pub fn verdict_of(runs: &[CheckRun], name: &str) -> Verdict {
    let mut seen = false;
    let mut pending = false;
    for r in runs.iter().filter(|r| r.name == name) {
        seen = true;
        match r.verdict() {
            Verdict::Red => return Verdict::Red,
            Verdict::Pending => pending = true,
            Verdict::Green | Verdict::Missing => {}
        }
    }
    match (seen, pending) {
        (false, _) => Verdict::Missing,
        (true, true) => Verdict::Pending,
        (true, false) => Verdict::Green,
    }
}

/// What a backend said about a commit, as a value that can be signed: `{commit, checks, backend}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksEvidence {
    /// The commit the checks ran on.
    pub commit: String,
    /// The runs, sorted by name.
    pub checks: Vec<CheckRun>,
    /// The backend's name.
    pub backend: String,
}

impl ChecksEvidence {
    /// The evidence for `commit` from `runs`, sorted by name (then url) so the value is the same however the
    /// backend ordered them.
    pub fn new(commit: &str, backend: &str, runs: &[CheckRun]) -> ChecksEvidence {
        let mut checks = runs.to_vec();
        checks.sort_by(|a, b| {
            (&a.name, &a.url, &a.conclusion).cmp(&(&b.name, &b.url, &b.conclusion))
        });
        ChecksEvidence {
            commit: commit.to_owned(),
            checks,
            backend: backend.to_owned(),
        }
    }

    /// The canonical (JCS) bytes: what an instance key signs and what a blob carries.
    pub fn canonical(&self) -> Result<Vec<u8>, CheckError> {
        let value = han_core::json::to_value(self).map_err(|e| CheckError(e.to_string()))?;
        han_core::json::canonicalize(&value).map_err(|e| CheckError(e.to_string()))
    }

    /// `sha256` of [`ChecksEvidence::canonical`], lowercase hex.
    pub fn digest(&self) -> Result<String, CheckError> {
        Ok(han_core::ids::sha256_hex(&self.canonical()?))
    }
}

/// Why a backend could not answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CheckError(pub String);

/// Where checks run.
#[async_trait::async_trait]
pub trait Checks: Send + Sync + 'static {
    /// The backend's name, recorded in the evidence (`github-actions`, `warm-runner`).
    fn name(&self) -> &'static str;

    /// The backend's name for evidence about `repo`. A backend that routes between others answers per repository;
    /// the default is [`Checks::name`].
    fn name_for(&self, repo: &RepoTarget) -> &'static str {
        let _ = repo;
        self.name()
    }

    /// Asks the backend to run the checks of `req` on `req.commit`, once the lane has made the commit
    /// available (pushed). Backends that run by themselves on a push (GitHub Actions) do nothing. Calling it
    /// again for the same commit must be harmless.
    async fn start(&self, req: &CheckRequest) -> Result<(), CheckError> {
        let _ = req;
        Ok(())
    }

    /// The check runs that exist now for `req.commit`.
    async fn poll(&self, req: &CheckRequest) -> Result<Vec<CheckRun>, CheckError>;

    /// Asks for the named runs to run again. Returns whether the backend did (default: it cannot).
    async fn rerun(&self, req: &CheckRequest, names: &[String]) -> Result<bool, CheckError> {
        let _ = (req, names);
        Ok(false)
    }
}

/// GitHub Actions (and any other app that reports check runs or commit statuses) on the repository's GitHub
/// side.
pub struct GithubChecks {
    minter: Arc<GithubMinter>,
    /// The re-runs asked for, by `(repository, commit)`: until `poll` sees the new attempt of a run, the old
    /// attempt's conclusion is not the answer.
    asked: Mutex<HashMap<(String, String), Vec<Asked>>>,
}

/// A named check run whose workflow run was asked to run its failed jobs again.
#[derive(Clone, Debug)]
struct Asked {
    name: String,
    run_id: u64,
    /// The `run_attempt` the re-run starts (the one before, plus one).
    attempt: u64,
}

impl GithubChecks {
    /// A backend that reads with the App's installation tokens.
    pub fn new(minter: Arc<GithubMinter>) -> GithubChecks {
        GithubChecks {
            minter,
            asked: Mutex::new(HashMap::new()),
        }
    }
}

/// The workflow run behind a check run: its `details_url` is `…/actions/runs/<run>/job/<job>`.
fn run_id_of(c: &Value) -> Option<u64> {
    let url = c.get("details_url").and_then(Value::as_str)?;
    let rest = url.split("/actions/runs/").nth(1)?;
    rest.split('/').next()?.parse().ok()
}

/// Pages of at most 100 are read up to this many.
const MAX_PAGES: u32 = 10;

fn gh_of(req: &CheckRequest) -> Result<&str, CheckError> {
    req.repo
        .github
        .as_deref()
        .ok_or_else(|| CheckError(format!("{} is not on GitHub", req.repo.name)))
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

#[async_trait::async_trait]
impl Checks for GithubChecks {
    fn name(&self) -> &'static str {
        "github-actions"
    }

    async fn poll(&self, req: &CheckRequest) -> Result<Vec<CheckRun>, CheckError> {
        let gh = gh_of(req)?;
        let octo = self
            .minter
            .client(gh, &[("checks", "read"), ("statuses", "read")])
            .await
            .map_err(CheckError)?;
        let sha = &req.commit;
        let asked: Vec<Asked> = self
            .asked
            .lock()
            .expect("asked lock")
            .get(&(gh.to_owned(), sha.clone()))
            .cloned()
            .unwrap_or_default();
        let mut actions: Option<octocrab::Octocrab> = None;
        let mut seen_new: Vec<(u64, String)> = Vec::new();
        let mut runs = Vec::new();
        for page in 1..=MAX_PAGES {
            let list: Value = octo
                .get(
                    format!(
                        "/repos/{gh}/commits/{sha}/check-runs?filter=latest&per_page=100&page={page}"
                    ),
                    None::<&()>,
                )
                .await
                .map_err(|_| CheckError("listing check runs failed".into()))?;
            let items = list
                .get("check_runs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let total = list.get("total_count").and_then(Value::as_u64).unwrap_or(0);
            for c in &items {
                let Some(name) = str_of(c, "name") else {
                    continue;
                };
                // A run that has not completed has no conclusion: its status says where it is.
                let conclusion = match (str_of(c, "status").as_deref(), str_of(c, "conclusion")) {
                    (Some("completed"), Some(done)) => done,
                    (Some(status), _) if status != "completed" => status.to_owned(),
                    (_, Some(done)) => done,
                    _ => "pending".to_owned(),
                };
                let mut conclusion = conclusion;
                if let Some(a) = asked
                    .iter()
                    .find(|a| a.name == name && run_id_of(c) == Some(a.run_id))
                {
                    // The check run of a job asked to run again: its own run attempt says which attempt it is.
                    let id = c.get("id").and_then(Value::as_u64).unwrap_or(0);
                    if actions.is_none() {
                        actions = Some(
                            self.minter
                                .client(gh, &[("actions", "read")])
                                .await
                                .map_err(CheckError)?,
                        );
                    }
                    let job: Value = actions
                        .as_ref()
                        .expect("minted above")
                        .get(format!("/repos/{gh}/actions/jobs/{id}"), None::<&()>)
                        .await
                        .map_err(|_| CheckError("reading a job's run attempt failed".into()))?;
                    let attempt = job.get("run_attempt").and_then(Value::as_u64).unwrap_or(0);
                    if attempt < a.attempt {
                        conclusion = "pending".to_owned();
                    } else {
                        seen_new.push((a.run_id, a.name.clone()));
                    }
                }
                runs.push(CheckRun::new(&name, &conclusion, str_of(c, "html_url")));
            }
            if items.len() < 100 || runs.len() as u64 >= total {
                break;
            }
        }
        if !seen_new.is_empty() {
            // Every name asked about whose new attempt is in: the asking is over.
            let mut g = self.asked.lock().expect("asked lock");
            if let Some(v) = g.get_mut(&(gh.to_owned(), sha.clone())) {
                v.retain(|a| !seen_new.contains(&(a.run_id, a.name.clone())));
                if v.is_empty() {
                    g.remove(&(gh.to_owned(), sha.clone()));
                }
            }
        }
        let status: Value = octo
            .get(
                format!("/repos/{gh}/commits/{sha}/status?per_page=100"),
                None::<&()>,
            )
            .await
            .map_err(|_| CheckError("reading commit statuses failed".into()))?;
        for s in status
            .get("statuses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = str_of(s, "context") else {
                continue;
            };
            let conclusion = match str_of(s, "state").as_deref() {
                Some("success") => "success",
                Some("failure" | "error") => "failure",
                _ => "pending",
            };
            runs.push(CheckRun::new(&name, conclusion, str_of(s, "target_url")));
        }
        Ok(runs)
    }

    /// Runs the failed jobs of the workflow runs behind the named check runs (`POST
    /// /repos/{o}/{r}/actions/runs/{run}/rerun-failed-jobs`, which needs `actions: write`; the check runs are
    /// listed with `checks: read`). The token is minted for this call and the one repository. A check run that
    /// is not an Actions job (a commit status, another app's check) cannot be run again here and is skipped.
    async fn rerun(&self, req: &CheckRequest, names: &[String]) -> Result<bool, CheckError> {
        let gh = gh_of(req)?;
        let octo = self
            .minter
            .client(gh, &[("checks", "read"), ("actions", "write")])
            .await
            .map_err(CheckError)?;
        let sha = &req.commit;
        let list: Value = octo
            .get(
                format!("/repos/{gh}/commits/{sha}/check-runs?filter=latest&per_page=100"),
                None::<&()>,
            )
            .await
            .map_err(|_| CheckError("listing check runs failed".into()))?;
        // The runs to run again, with the names that sit in each.
        let mut by_run: Vec<(u64, Vec<String>)> = Vec::new();
        for c in list
            .get("check_runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(name), Some(run)) = (str_of(c, "name"), run_id_of(c)) else {
                continue;
            };
            if !names.contains(&name) {
                continue;
            }
            match by_run.iter_mut().find(|(r, _)| *r == run) {
                Some((_, ns)) => ns.push(name),
                None => by_run.push((run, vec![name])),
            }
        }
        let mut asked = Vec::new();
        for (run, ns) in by_run {
            let before: Value = octo
                .get(format!("/repos/{gh}/actions/runs/{run}"), None::<&()>)
                .await
                .map_err(|_| CheckError("reading a workflow run failed".into()))?;
            let attempt = before
                .get("run_attempt")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            let _: Value = octo
                .post(
                    format!("/repos/{gh}/actions/runs/{run}/rerun-failed-jobs"),
                    None::<&()>,
                )
                .await
                .map_err(|_| {
                    CheckError(format!(
                        "asking for the failed jobs of workflow run {run} to run again failed"
                    ))
                })?;
            asked.extend(ns.into_iter().map(|name| Asked {
                name,
                run_id: run,
                attempt: attempt + 1,
            }));
        }
        if asked.is_empty() {
            return Ok(false);
        }
        let mut g = self.asked.lock().expect("asked lock");
        let slot = g.entry((gh.to_owned(), sha.clone())).or_default();
        slot.retain(|a| {
            !asked
                .iter()
                .any(|n| n.run_id == a.run_id && n.name == a.name)
        });
        slot.extend(asked);
        Ok(true)
    }
}
