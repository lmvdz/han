//! GitHub access for `han dev prs` and `han dev prune`: the contributor's own `gh` login behind a small trait,
//! so the reconciler and the pruner run against a fake in tests.

use std::process::Command;

use serde_json::Value;

use super::Res;

/// What GitHub says a pull request is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrState {
    /// Still open.
    Open,
    /// Merged into its base.
    Merged,
    /// Closed without a merge.
    Closed,
}

impl PrState {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "OPEN" => Some(Self::Open),
            "MERGED" => Some(Self::Merged),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }

    /// GitHub's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Merged => "MERGED",
            Self::Closed => "CLOSED",
        }
    }
}

/// A pull request as the pruner needs it: which branch it came from and where that branch stood.
#[derive(Clone, Debug)]
pub struct PrRecord {
    /// The head branch.
    pub branch: String,
    /// Open, merged or closed.
    pub state: PrState,
    /// The head commit when GitHub last saw it.
    pub head: String,
    /// The pull request number.
    pub number: u64,
}

/// An open pull request with auto-merge enabled, reduced to what the reconciler judges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPr {
    /// The pull request number.
    pub number: u64,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
    /// `BEHIND`, `BLOCKED`, `CLEAN` and so on.
    pub merge_state: String,
    /// Unix time of the last push.
    pub pushed: i64,
    /// Check runs that have not completed.
    pub running: usize,
    /// Commit statuses that are pending or expected.
    pub pending: usize,
    /// Names of checks that failed, were cancelled or timed out.
    pub failed: Vec<String>,
    /// How auto-merge will merge: `SQUASH`, `MERGE` or `REBASE` (empty when GitHub did not say).
    pub merge_method: String,
}

/// The `gh pr merge` flag of an auto-merge method, if it is one GitHub knows.
pub fn merge_flag(method: &str) -> Option<&'static str> {
    match method {
        "SQUASH" => Some("--squash"),
        "MERGE" => Some("--merge"),
        "REBASE" => Some("--rebase"),
        _ => None,
    }
}

/// The GitHub calls of the two commands.
pub trait Github {
    /// The open pull requests with auto-merge enabled, in one query.
    fn auto_merge_prs(&self, repo: &str) -> Res<Vec<OpenPr>>;
    /// The state of one pull request, and its head branch.
    fn pr_state(&self, repo: &str, number: u64) -> Res<(PrState, String)>;
    /// Merges the base into the pull request's branch.
    fn update_branch(&self, repo: &str, number: u64) -> Res<()>;
    /// Switches auto-merge off. Callers re-enable it at once; this is never the end of an action.
    fn disable_auto_merge(&self, repo: &str, number: u64) -> Res<()>;
    /// Switches auto-merge on with `method` (`SQUASH`, `MERGE` or `REBASE`). GitHub still applies branch
    /// protection: this never merges.
    fn enable_auto_merge(&self, repo: &str, number: u64, method: &str) -> Res<()>;
    /// Every pull request of the repository, newest first.
    fn pr_records(&self, repo: &str) -> Res<Vec<PrRecord>>;
}

/// `gh`, as the contributor is logged in.
pub struct GhCli;

impl GhCli {
    fn run(args: &[&str]) -> Res<String> {
        let out = Command::new("gh").args(args).output().map_err(|e| {
            format!("cannot run gh: {e} (install the GitHub CLI and run `gh auth login`)")
        })?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = if err.trim().is_empty() {
                String::from_utf8_lossy(&out.stdout)
            } else {
                err
            };
            return Err(format!("gh {}: {}", args.join(" "), msg.trim()).into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

const QUERY: &str = "query($o:String!,$n:String!){repository(owner:$o,name:$n){pullRequests(states:OPEN,last:50){nodes{
  number autoMergeRequest{enabledAt mergeMethod} mergeable mergeStateStatus
  commits(last:1){nodes{commit{committedDate pushedDate statusCheckRollup{state contexts(first:50){nodes{
    __typename ...on CheckRun{name status conclusion} ...on StatusContext{context state}}}}}}}}}}}";

impl Github for GhCli {
    fn auto_merge_prs(&self, repo: &str) -> Res<Vec<OpenPr>> {
        let (owner, name) = split_repo(repo)?;
        let text = Self::run(&[
            "api",
            "graphql",
            "-f",
            &format!("query={QUERY}"),
            "-F",
            &format!("o={owner}"),
            "-F",
            &format!("n={name}"),
        ])?;
        parse_auto_merge_prs(&serde_json::from_str(&text)?)
    }

    fn pr_state(&self, repo: &str, number: u64) -> Res<(PrState, String)> {
        let text = Self::run(&[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "state,headRefName",
        ])?;
        let v: Value = serde_json::from_str(&text)?;
        let state = v["state"].as_str().unwrap_or_default();
        let state = PrState::parse(state).ok_or_else(|| format!("unexpected state {state:?}"))?;
        Ok((
            state,
            v["headRefName"].as_str().unwrap_or_default().to_owned(),
        ))
    }

    fn update_branch(&self, repo: &str, number: u64) -> Res<()> {
        Self::run(&["pr", "update-branch", &number.to_string(), "--repo", repo]).map(|_| ())
    }

    fn disable_auto_merge(&self, repo: &str, number: u64) -> Res<()> {
        let n = number.to_string();
        Self::run(&["pr", "merge", &n, "--repo", repo, "--disable-auto"]).map(|_| ())
    }

    fn enable_auto_merge(&self, repo: &str, number: u64, method: &str) -> Res<()> {
        let flag = merge_flag(method).ok_or_else(|| format!("unknown merge method {method:?}"))?;
        let n = number.to_string();
        Self::run(&["pr", "merge", &n, "--repo", repo, "--auto", flag]).map(|_| ())
    }

    fn pr_records(&self, repo: &str) -> Res<Vec<PrRecord>> {
        let text = Self::run(&[
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "all",
            "--limit",
            "500",
            "--json",
            "headRefName,state,headRefOid,number",
        ])?;
        let v: Value = serde_json::from_str(&text)?;
        let rows = v.as_array().ok_or("unexpected reply from gh pr list")?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(PrRecord {
                    branch: r["headRefName"].as_str()?.to_owned(),
                    state: PrState::parse(r["state"].as_str()?)?,
                    head: r["headRefOid"].as_str()?.to_owned(),
                    number: r["number"].as_u64()?,
                })
            })
            .collect())
    }
}

/// `owner/name`, checked: it ends up in a `gh` command line.
pub fn split_repo(repo: &str) -> Res<(&str, &str)> {
    let word = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match repo.split_once('/') {
        Some((o, n)) if word(o) && word(n) => Ok((o, n)),
        _ => Err(format!("{repo:?} is not owner/name").into()),
    }
}

/// `owner/name` of a GitHub remote URL (`https://github.com/o/n(.git)`, `git@github.com:o/n.git`, `ssh://…`).
pub fn repo_of_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let tail = url.split_once("github.com")?.1;
    let tail = tail.trim_start_matches([':', '/']);
    split_repo(tail).ok().map(|_| tail.to_owned())
}

/// The reconciler's view of a GraphQL reply: the open pull requests that have auto-merge enabled.
pub fn parse_auto_merge_prs(reply: &Value) -> Res<Vec<OpenPr>> {
    if let Some(errors) = reply.get("errors") {
        return Err(format!("GraphQL errors: {errors}").into());
    }
    let nodes = reply
        .pointer("/data/repository/pullRequests/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("unexpected reply: {reply}"))?;
    let mut out = Vec::new();
    for node in nodes {
        if node["autoMergeRequest"].is_null() {
            continue;
        }
        let number = node["number"]
            .as_u64()
            .ok_or("a pull request has no number")?;
        let commit = &node["commits"]["nodes"][0]["commit"];
        let pushed = ["pushedDate", "committedDate"]
            .iter()
            .filter_map(|k| commit[k].as_str())
            .find_map(parse_iso)
            .ok_or_else(|| format!("PR #{number} has no push date"))?;
        let empty = Vec::new();
        let contexts = commit
            .pointer("/statusCheckRollup/contexts/nodes")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let (mut running, mut pending, mut failed) = (0, 0, Vec::new());
        for c in contexts {
            if c["status"].as_str().is_some_and(|s| s != "COMPLETED") {
                running += 1;
            }
            let state = c["state"].as_str().unwrap_or_default();
            if matches!(state, "PENDING" | "EXPECTED") {
                pending += 1;
            }
            let bad = matches!(
                c["conclusion"].as_str().unwrap_or_default(),
                "FAILURE" | "CANCELLED" | "TIMED_OUT"
            ) || matches!(state, "FAILURE" | "ERROR");
            if bad {
                let name = c["name"].as_str().or(c["context"].as_str());
                failed.push(name.unwrap_or("?").to_owned());
            }
        }
        out.push(OpenPr {
            number,
            mergeable: node["mergeable"].as_str().unwrap_or("UNKNOWN").to_owned(),
            merge_state: node["mergeStateStatus"]
                .as_str()
                .unwrap_or("UNKNOWN")
                .to_owned(),
            pushed,
            running,
            pending,
            failed,
            merge_method: node["autoMergeRequest"]["mergeMethod"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(out)
}

/// Unix seconds of `YYYY-MM-DDTHH:MM:SSZ` (the only form GitHub sends).
pub fn parse_iso(text: &str) -> Option<i64> {
    let t = text.strip_suffix('Z')?;
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut c = time.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, s) = (c.next()??, c.next()??, c.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s)
}

/// A scripted GitHub for tests.
#[cfg(test)]
pub mod fake {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;

    use super::*;

    /// Answers from lists the test fills in; records the `update-branch` calls.
    #[derive(Default)]
    pub struct FakeGh {
        /// One answer per `auto_merge_prs` call; the last one repeats.
        pub rounds: Mutex<VecDeque<Result<Vec<OpenPr>, String>>>,
        /// What `pr_state` says; the head branch is `branch-N` unless `branches` has it.
        pub states: Mutex<BTreeMap<u64, PrState>>,
        /// Head branches by pull request number.
        pub branches: Mutex<BTreeMap<u64, String>>,
        /// Pull requests `update_branch` is called for.
        pub updated: Mutex<Vec<u64>>,
        /// Whether `update_branch` fails.
        pub update_fails: bool,
        /// The calls to `disable_auto_merge` and `enable_auto_merge`, as `off #n` and `on #n METHOD`.
        pub toggled: Mutex<Vec<String>>,
        /// Whether `enable_auto_merge` fails.
        pub enable_fails: bool,
        /// What `pr_records` says.
        pub records: Vec<PrRecord>,
    }

    impl Github for FakeGh {
        fn auto_merge_prs(&self, _: &str) -> Res<Vec<OpenPr>> {
            let mut q = self.rounds.lock().unwrap();
            let next = if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front().cloned().unwrap_or(Ok(Vec::new()))
            };
            next.map_err(Into::into)
        }

        fn pr_state(&self, _: &str, number: u64) -> Res<(PrState, String)> {
            let s = self.states.lock().unwrap().get(&number).copied();
            let b = self.branches.lock().unwrap().get(&number).cloned();
            let state = s.ok_or("not found")?;
            Ok((state, b.unwrap_or_else(|| format!("branch-{number}"))))
        }

        fn update_branch(&self, _: &str, number: u64) -> Res<()> {
            self.updated.lock().unwrap().push(number);
            if self.update_fails {
                return Err("cannot update".into());
            }
            Ok(())
        }

        fn disable_auto_merge(&self, _: &str, number: u64) -> Res<()> {
            self.toggled.lock().unwrap().push(format!("off #{number}"));
            Ok(())
        }

        fn enable_auto_merge(&self, _: &str, number: u64, method: &str) -> Res<()> {
            self.toggled
                .lock()
                .unwrap()
                .push(format!("on #{number} {method}"));
            if self.enable_fails {
                return Err("enable refused".into());
            }
            Ok(())
        }

        fn pr_records(&self, _: &str) -> Res<Vec<PrRecord>> {
            Ok(self.records.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates_are_unix_seconds() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2000-03-01T00:00:01Z"), Some(951_868_801));
        assert_eq!(parse_iso("2026-10-15T12:34:56Z"), Some(1_792_067_696));
        assert_eq!(parse_iso("yesterday"), None);
    }

    #[test]
    fn repos_come_from_remote_urls() {
        for url in [
            "https://github.com/lmvdz/han.git",
            "https://github.com/lmvdz/han",
            "git@github.com:lmvdz/han.git",
            "ssh://git@github.com/lmvdz/han.git\n",
        ] {
            assert_eq!(repo_of_remote(url).as_deref(), Some("lmvdz/han"), "{url}");
        }
        assert_eq!(repo_of_remote("/srv/git/han.git"), None);
        assert!(split_repo("a/b;rm").is_err());
        assert!(split_repo("nobar").is_err());
    }

    #[test]
    fn the_graphql_reply_is_classified() {
        let reply: Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequests":{"nodes":[
              {"number":1,"autoMergeRequest":null,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","commits":{"nodes":[]}},
              {"number":2,"autoMergeRequest":{"enabledAt":"x","mergeMethod":"SQUASH"},"mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED",
               "commits":{"nodes":[{"commit":{"committedDate":"2026-01-01T00:00:00Z","pushedDate":"2026-01-01T00:01:00Z",
                 "statusCheckRollup":{"state":"PENDING","contexts":{"nodes":[
                   {"__typename":"CheckRun","name":"build","status":"IN_PROGRESS","conclusion":null},
                   {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"FAILURE"},
                   {"__typename":"CheckRun","name":"slow","status":"COMPLETED","conclusion":"TIMED_OUT"},
                   {"__typename":"CheckRun","name":"fine","status":"COMPLETED","conclusion":"SUCCESS"},
                   {"__typename":"StatusContext","context":"ext","state":"PENDING"},
                   {"__typename":"StatusContext","context":"ext2","state":"ERROR"}]}}}}]}},
              {"number":3,"autoMergeRequest":{"enabledAt":"x"},"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY",
               "commits":{"nodes":[{"commit":{"committedDate":"2026-01-01T00:00:00Z","pushedDate":null,"statusCheckRollup":null}}]}}
            ]}}}}"#,
        )
        .unwrap();
        let prs = parse_auto_merge_prs(&reply).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!((prs[0].number, prs[0].running, prs[0].pending), (2, 1, 1));
        assert_eq!(prs[0].failed, ["lint", "slow", "ext2"]);
        assert_eq!(prs[0].pushed, parse_iso("2026-01-01T00:01:00Z").unwrap());
        // no pushedDate: the commit date; no checks at all
        assert_eq!(prs[1].pushed, parse_iso("2026-01-01T00:00:00Z").unwrap());
        assert_eq!(prs[1].mergeable, "CONFLICTING");
        assert_eq!(prs[0].merge_method, "SQUASH");
        assert_eq!(prs[1].merge_method, "");
        assert_eq!(merge_flag("REBASE"), Some("--rebase"));
        assert_eq!(merge_flag(""), None);
        assert_eq!(
            (prs[1].running, prs[1].pending, prs[1].failed.len()),
            (0, 0, 0)
        );
    }

    #[test]
    fn a_reply_without_data_is_an_error() {
        let bad: Value = serde_json::from_str(r#"{"errors":[{"message":"nope"}]}"#).unwrap();
        assert!(parse_auto_merge_prs(&bad).is_err());
        assert!(parse_auto_merge_prs(&serde_json::json!({})).is_err());
    }
}
