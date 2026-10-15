//! The `land` finish flow (PLAN §6 "Finish flow", R33 §2, §3, §7): the host proposes one `vcs.land` effect to
//! the landing lane, freezes what it proposed, follows the lane's `han.land/1` items for it, and prunes the
//! session's workspace when the effect is final.
//!
//! It is off unless the host's spec turns it on ([`han_registry::LandFinish`]); then the agent may propose the
//! kind `land` (the host builds the plan, never the agent) and everything else here happens by itself.
//!
//! **Frozen changesets.** The first `land` proposal of a session is `base..to`, where `base` is the workspace's
//! base and `to` the commit the host made of the workspace for it. From then on that commit is *the proposed
//! state*:
//!
//! * the host keeps the proposal's `to`, `base` and delta digest in its ledger (`land.json` in the session's
//!   directory) and never builds another plan from the workspace's present state without first checking it
//!   against the ledger: the proposed plan is a value, not a view of the workspace, so a commit made after
//!   proposing cannot change it;
//! * work that goes on is a **new** changeset based on the old `to`, with `dependsOn` naming the old
//!   proposal. The host refuses to propose when the old `to` is no longer an ancestor of the workspace's head
//!   (a rewrite of proposed history), when nothing was added since, or when the old proposal was rejected
//!   (the way on is `rebaseOf`, which names the rejected proposal, keeps its base and depends on nothing);
//! * the lane's executor imports the bundle `old.to..new.to` on top of what it imported for the old proposal.
//!
//! The host cannot stop the agent writing in its own workspace (that is the agent's sandbox); what it
//! refuses is making the *proposal* from a changed past. Commits that follow a proposal and never become a
//! proposal are what pruning reports as `unlanded`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use han_node::api::{
    BoxFuture, CallResult, Command, Filter, ItemBody, NodeApi, Query, Row, SessionPart, Window,
};
use han_registry::LandFinish;
use han_vcs::{Commit, Vcs, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::host::HostConfig;
use crate::mirror::glob::glob_match;
use crate::status::bound;

/// The profile of the lane's items.
pub const PROFILE: &str = "han.land/1";

/// The ledger's file name in the session directory.
pub const LEDGER_FILE: &str = "land.json";

/// The informative tier of a change no rule touches.
pub const DEFAULT_TIER: u8 = 1;

const MAX_TITLE: usize = 200;
/// The longest conflict record handed to the agent whole; a longer one is cut to the paths it names.
const MAX_RECORD: usize = 32 * 1024;
/// The longest text of the lane's that is kept.
const MAX_LANE_TEXT: usize = 300;

// ───────────────────────── the lane's items ─────────────────────────

/// A `change` item of the lane, as the host reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneChange {
    /// The proposal.
    pub proposal: String,
    /// `queued`, `testing`, `landing`, `landed`, `confirmed`, `rejected`, `cancelled` (or `reverted`).
    pub state: String,
    /// The rejection reason: `conflict`, `test`, `dependency`, `policy`, `timeout`, `post_merge_failed`.
    pub reason: Option<String>,
    /// What the lane says about it.
    pub detail: Option<String>,
    /// Why it is stalled, if it is.
    pub stalled: Option<String>,
    /// Its place in the queue.
    pub position: Option<u32>,
    /// The conflict record, a `BlobRef`.
    pub conflict: Option<Value>,
    /// The evidence, a `BlobRef`.
    pub evidence: Option<Value>,
    /// The lane's count of this change's items.
    pub seq: u64,
}

impl LaneChange {
    /// Reads a `change` item's JSON (`None`: not a change item).
    pub fn parse(item: &Value) -> Option<LaneChange> {
        if item.get("item")?.as_str()? != "change" {
            return None;
        }
        let text = |k: &str| {
            item.get(k)
                .and_then(Value::as_str)
                .map(|s| bound(s, MAX_LANE_TEXT))
        };
        Some(LaneChange {
            proposal: item.get("proposal")?.as_str()?.to_owned(),
            state: item.get("state")?.as_str()?.to_owned(),
            reason: text("reason"),
            detail: text("detail"),
            stalled: text("stalled"),
            position: item
                .get("position")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok()),
            conflict: item.get("conflict").filter(|v| v.is_object()).cloned(),
            evidence: item.get("evidence").filter(|v| v.is_object()).cloned(),
            seq: item.get("seq").and_then(Value::as_u64).unwrap_or(0),
        })
    }
}

/// Where the host reads the lane from.
pub trait LaneFeed: Send + Sync {
    /// The latest item of every change the lane has published.
    fn changes(&self) -> BoxFuture<'_, Result<Vec<LaneChange>, String>>;

    /// The bytes a `BlobRef` of the lane names (a conflict record).
    fn blob<'a>(&'a self, blob: &'a Value) -> BoxFuture<'a, Result<Vec<u8>, String>>;
}

/// The lane as the node shows it: the lane's session (the executor's, in the space) has the `han.land/1`
/// items as `Unknown` items of its stream, and a blob is fetched through the node.
pub struct NodeLaneFeed {
    api: Arc<dyn NodeApi>,
    space: String,
    executor: String,
}

impl NodeLaneFeed {
    /// A feed over `api` for the lane of `executor` in `space`.
    pub fn new(api: Arc<dyn NodeApi>, space: String, executor: String) -> Self {
        NodeLaneFeed {
            api,
            space,
            executor,
        }
    }

    async fn first(&self, q: Query) -> Result<Vec<Row>, String> {
        crate::host::snapshot(self.api.as_ref(), q)
            .await
            .map_err(|e| e.to_string())
    }
}

impl LaneFeed for NodeLaneFeed {
    fn changes(&self) -> BoxFuture<'_, Result<Vec<LaneChange>, String>> {
        Box::pin(async move {
            let boards = self
                .first(Query::Board {
                    space: self.space.clone(),
                    group: Default::default(),
                    collapsed: Vec::new(),
                    window: Window {
                        filter: Filter {
                            actor: Some(self.executor.clone()),
                            ..Filter::default()
                        },
                        limit: 200,
                        ..Window::default()
                    },
                })
                .await?;
            let mut out = Vec::new();
            for session in boards.iter().filter_map(|r| match r {
                Row::Board(b) if b.agent == self.executor => Some(b.session.clone()),
                _ => None,
            }) {
                let items = self
                    .first(Query::Session {
                        id: session,
                        part: SessionPart::Items,
                        window: Window {
                            from_end: true,
                            limit: 200,
                            ..Window::default()
                        },
                    })
                    .await?;
                for r in items {
                    if let Row::Item(i) = r
                        && let ItemBody::Unknown { raw_kind, raw } = i.item
                        && raw_kind == PROFILE
                        && let Ok(v) = serde_json::from_str::<Value>(&raw)
                        && let Some(c) = LaneChange::parse(&v)
                    {
                        out.push(c);
                    }
                }
            }
            Ok(out)
        })
    }

    fn blob<'a>(&'a self, blob: &'a Value) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let (Some(hash), Some(size), Some(kind)) = (
                blob.get("hash").and_then(Value::as_str),
                blob.get("size").and_then(Value::as_u64),
                blob.get("type").and_then(Value::as_str),
            ) else {
                return Err("not a blob reference".into());
            };
            match self
                .api
                .call(Command::BlobFetch {
                    space: self.space.clone(),
                    hash: hash.to_owned(),
                    size,
                    media_type: kind.to_owned(),
                })
                .await
            {
                Ok(CallResult::Blob { data, .. }) => {
                    han_core::base64url::decode(&data).map_err(|e| e.to_string())
                }
                Ok(_) => Err("the node did not return the blob".into()),
                Err(e) => Err(e.to_string()),
            }
        })
    }
}

// ───────────────────────── the ledger ─────────────────────────

/// One `land` proposal of the session, frozen: what was proposed, and where the lane has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposed {
    /// The proposal's entry id.
    pub proposal: String,
    /// The changeset's base.
    pub base: String,
    /// The commit proposed: the frozen head.
    pub to: String,
    /// The delta digest in the plan.
    pub delta_digest: String,
    /// The plan's title.
    pub title: String,
    /// The plan's informative tier.
    pub tier: u8,
    /// The proposals this one waits for.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// The rejected proposal this one replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase_of: Option<String>,
    /// `proposed` until the lane says otherwise, then the lane's state.
    pub state: String,
    /// The rejection reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the lane said last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Why the lane says it is stalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled: Option<String>,
    /// Its place in the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    /// The conflict record's blob reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<Value>,
    /// The conflict record itself, fetched (bounded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_record: Option<Value>,
    /// The evidence's blob reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
    /// The last of the lane's item counts seen.
    #[serde(default)]
    pub lane_seq: u64,
}

impl Proposed {
    fn failed(&self) -> bool {
        matches!(self.state.as_str(), "rejected" | "cancelled" | "reverted")
    }
}

/// Commits (or changes) the workspace holds that no proposal carried to the target (R33 §7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unlanded {
    /// The workspace.
    pub workspace: String,
    /// The proposal the commits come after.
    pub proposal: String,
    /// The commits after the proposal's `to` that the target lacks by patch id.
    pub commits: Vec<Commit>,
    /// The workspace also has changes that were never committed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uncommitted: bool,
}

impl Unlanded {
    /// The item published for it (`han.land/1`, kind `unlanded`).
    pub fn item(&self) -> Value {
        let mut v = json!({
            "item": "unlanded",
            "workspace": self.workspace,
            "proposal": self.proposal,
            "commits": self.commits,
        });
        if self.uncommitted {
            v["uncommitted"] = json!(true);
        }
        v
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ledger {
    proposals: Vec<Proposed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unlanded: Option<Unlanded>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pruned: bool,
}

/// A plan drafted but not yet proposed.
#[derive(Clone, Debug)]
struct Pending {
    base: String,
    to: String,
    delta_digest: String,
    title: String,
    tier: u8,
    depends_on: Vec<String>,
    rebase_of: Option<String>,
}

#[derive(Default)]
struct Inner {
    ledger: Ledger,
    pending: Option<Pending>,
    /// Proposals the home refused after they were queued: never frozen, even if the news comes first.
    refused: std::collections::BTreeSet<String>,
}

/// What a plan is made of, for the host to propose.
pub struct Draft {
    /// The `vcs.land` plan, with its `argsBlob` reference.
    pub plan: Value,
    /// The bundle `base..to`, to upload before the proposal.
    pub bundle: Vec<u8>,
}

/// What [`Flow::prune`] did.
#[derive(Clone, Debug, PartialEq)]
pub enum Pruned {
    /// Not yet: no proposal, one still on its way, or a turn is on.
    Waiting(&'static str),
    /// The workspace, its build output and its refs are gone.
    Removed,
    /// Kept, and why: the `unlanded` report.
    Kept(Unlanded),
}

// ───────────────────────── the flow ─────────────────────────

/// One session's land finish flow.
pub struct Flow {
    land: LandFinish,
    repo_name: String,
    source: String,
    default_branch: String,
    session: String,
    agent: String,
    ask: Option<String>,
    issue: Option<String>,
    workspace: Workspace,
    session_dir: PathBuf,
    vcs: Arc<dyn Vcs>,
    feed: Arc<dyn LaneFeed>,
    /// Called just before the workspace is removed: lets go of what watches it (the worktree mirror).
    release: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    inner: Mutex<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Flow {
    /// The flow of a session, or `None` when the spec does not turn it on. A ledger left in the session
    /// directory by an earlier run of the same session is picked up.
    pub fn new(
        cfg: &HostConfig,
        session: &str,
        ask: Option<String>,
        workspace: Workspace,
        vcs: Arc<dyn Vcs>,
        feed: Arc<dyn LaneFeed>,
    ) -> Option<Flow> {
        let land = cfg.finish.land.clone()?;
        let ledger = std::fs::read_to_string(cfg.session_dir.join(LEDGER_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Some(Flow {
            land,
            repo_name: cfg.repo.name.clone(),
            source: cfg.repo.source.clone(),
            default_branch: cfg.repo.default_branch.clone(),
            session: session.to_owned(),
            agent: cfg.agent.clone(),
            ask,
            issue: cfg.issue.clone(),
            workspace,
            session_dir: cfg.session_dir.clone(),
            vcs,
            feed,
            release: Mutex::new(None),
            inner: Mutex::new(Inner {
                ledger,
                ..Inner::default()
            }),
        })
    }

    /// Sets what runs just before the workspace is removed (the host stops the worktree mirror: a watched
    /// directory cannot be deleted on every platform).
    pub fn on_release(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.release) = Some(f);
    }

    /// How long the host keeps following after the session stops.
    pub fn settle(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.land.settle_secs)
    }

    /// The ref the work lands on.
    pub fn target_ref(&self) -> String {
        self.land
            .target_ref
            .clone()
            .unwrap_or_else(|| format!("refs/heads/{}", self.default_branch))
    }

    fn save(&self, ledger: &Ledger) {
        let path = self.session_dir.join(LEDGER_FILE);
        let tmp = self.session_dir.join(format!("{LEDGER_FILE}.tmp"));
        let written = serde_json::to_vec_pretty(ledger)
            .map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            tracing::warn!("land ledger not saved: {e}");
        }
    }

    /// The informative tier of a change: the highest tier of any rule that matches a path it touches, else
    /// [`DEFAULT_TIER`]. Policy derives the real tier (R33).
    pub fn tier_of(&self, delta: &han_vcs::Delta) -> u8 {
        let mut best: Option<u8> = None;
        for p in delta.entries.iter().flat_map(|e| e.paths()) {
            for r in &self.land.tier_rules {
                if glob_match(&r.path, p) {
                    best = Some(best.map_or(r.tier, |b| b.max(r.tier)));
                }
            }
        }
        best.unwrap_or(DEFAULT_TIER).min(3)
    }

    /// The plan's title: the task's ask (`Work on <issue>: <text>` becomes `<text> (<issue>)`), else the
    /// issue, else the summary of the proposal. One line, bounded.
    pub fn title(&self, summary: &str) -> String {
        let line = |s: &str| {
            s.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| l.chars().filter(|c| !c.is_control()).collect::<String>())
        };
        let from_ask = self.ask.as_deref().and_then(line).map(|l| {
            match l.strip_prefix("Work on ").and_then(|r| r.split_once(": ")) {
                Some((issue, text)) if !text.trim().is_empty() => {
                    format!("{} ({issue})", text.trim())
                }
                _ => l,
            }
        });
        let t = from_ask
            .or_else(|| self.issue.clone())
            .or_else(|| line(summary))
            .unwrap_or_else(|| "land".into());
        bound(&t, MAX_TITLE)
    }

    /// Drafts the plan for a `land` proposal: commits the workspace, takes its delta from the right base,
    /// bundles it. It does not change what was proposed before (see the module doc); `proposed` records the
    /// draft once the proposal exists, `abandon` drops it.
    pub fn draft(&self, summary: &str, effect: &Map<String, Value>) -> Result<Draft, String> {
        let mut inner = lock(&self.inner);
        if inner.ledger.pruned {
            return Err("the workspace was pruned after its proposals were final".into());
        }
        let dir = &self.workspace.dir;
        let rebase_of = effect.get("rebaseOf").and_then(Value::as_str);
        let latest = inner.ledger.proposals.last().cloned();
        let (base, depends_on, rebase_of) = match (rebase_of, &latest) {
            (Some(old), _) => {
                let o = inner
                    .ledger
                    .proposals
                    .iter()
                    .find(|p| p.proposal == old)
                    .ok_or_else(|| {
                        format!("rebaseOf names {old}, which this session did not propose")
                    })?;
                if !o.failed() {
                    return Err(format!(
                        "{old} is {}: only a rejected, cancelled or reverted proposal is proposed again with rebaseOf",
                        o.state
                    ));
                }
                (o.base.clone(), Vec::new(), Some(old.to_owned()))
            }
            (None, Some(l)) => {
                if l.failed() {
                    return Err(format!(
                        "proposal {} was {}{}: propose again with rebaseOf: \"{}\"",
                        l.proposal,
                        l.state,
                        l.reason
                            .as_deref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default(),
                        l.proposal
                    ));
                }
                // The proposed commit must still be in the workspace's history: a rewrite of it is a change
                // to the proposed state, which is frozen.
                match self.vcs.is_ancestor(dir, &l.to, "HEAD") {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(format!(
                            "proposal {} is frozen at {}, which is no longer in this workspace's history: \
                             changes after a proposal are a new changeset on top of it, never a rewrite",
                            l.proposal,
                            &l.to[..l.to.len().min(8)]
                        ));
                    }
                    Err(e) => return Err(format!("the frozen proposal cannot be checked: {e}")),
                }
                (l.to.clone(), vec![l.proposal.clone()], None)
            }
            (None, None) => (self.workspace.base.clone(), Vec::new(), None),
        };
        let head = self
            .vcs
            .commit_all(
                dir,
                &format!("{}\n\nHAN session {}", summary.trim(), self.session),
                &self.agent,
            )
            .map_err(|e| e.to_string())?;
        if head == base || latest.as_ref().is_some_and(|l| l.to == head) {
            return Err(match &latest {
                Some(l) => format!(
                    "there is nothing new since proposal {}: it is frozen at {} and the workspace has no \
                     commit after it",
                    l.proposal,
                    &l.to[..l.to.len().min(8)]
                ),
                None => "there is nothing to land: the branch has no changes".into(),
            });
        }
        let delta = self
            .vcs
            .delta(dir, &base, &head)
            .map_err(|e| e.to_string())?;
        if delta.entries.is_empty() {
            return Err("there is nothing to land: the changes cancel out".into());
        }
        let digest = delta.digest().map_err(|e| e.to_string())?;
        let bundle = self
            .vcs
            .bundle(dir, &base, &head)
            .map_err(|e| format!("the changeset can't be bundled: {e}"))?;
        if bundle.len() > crate::host::MAX_BUNDLE {
            return Err(format!(
                "the changeset is too large to hand to the executor: {} bytes as a bundle, at most {}",
                bundle.len(),
                crate::host::MAX_BUNDLE
            ));
        }
        let tier = self.tier_of(&delta);
        let title = self.title(summary);
        let mut args = json!({
            "ref": self.target_ref(),
            "base": base,
            "to": head,
            "deltaDigest": digest,
            "tier": tier,
            "rebase": "path-disjoint",
            "title": title,
        });
        if !depends_on.is_empty() {
            args["dependsOn"] = depends_on
                .iter()
                .map(|p| json!({"repo": self.repo_name, "proposal": p}))
                .collect();
        }
        if let Some(r) = &rebase_of {
            args["rebaseOf"] = json!(r);
        }
        let plan = json!({
            "action": "vcs.land",
            "target": self.repo_name,
            "args": args,
            "argsBlob": {
                "hash": han_core::ids::sha256_hex(&bundle),
                "size": bundle.len(),
                "type": han_vcs::BUNDLE_MEDIA_TYPE,
            }
        });
        inner.pending = Some(Pending {
            base,
            to: head,
            delta_digest: digest,
            title,
            tier,
            depends_on,
            rebase_of,
        });
        Ok(Draft { plan, bundle })
    }

    /// The draft became proposal `id`: from now on its `to` is the proposed, frozen state.
    pub fn proposed(&self, id: &str) {
        let mut inner = lock(&self.inner);
        let Some(p) = inner.pending.take() else {
            return;
        };
        if inner.refused.contains(id) {
            return;
        }
        // New work is proposed: what was reported as left behind is judged again when this one is final.
        inner.ledger.unlanded = None;
        inner.ledger.proposals.push(Proposed {
            proposal: id.to_owned(),
            base: p.base,
            to: p.to,
            delta_digest: p.delta_digest,
            title: p.title,
            tier: p.tier,
            depends_on: p.depends_on,
            rebase_of: p.rebase_of,
            state: "proposed".into(),
            reason: None,
            detail: None,
            stalled: None,
            position: None,
            conflict: None,
            conflict_record: None,
            evidence: None,
            lane_seq: 0,
        });
        self.save(&inner.ledger);
    }

    /// The home refused proposal `id` after it was queued: it was never made, so it freezes nothing.
    pub fn withdrawn(&self, id: &str) {
        let mut inner = lock(&self.inner);
        inner.refused.insert(id.to_owned());
        let n = inner.ledger.proposals.len();
        inner.ledger.proposals.retain(|p| p.proposal != id);
        if inner.ledger.proposals.len() != n {
            self.save(&inner.ledger);
        }
    }

    /// The draft was not proposed.
    pub fn abandon(&self) {
        lock(&self.inner).pending = None;
    }

    /// The session's proposals, as the ledger has them.
    pub fn proposals(&self) -> Vec<Proposed> {
        lock(&self.inner).ledger.proposals.clone()
    }

    /// The `unlanded` report, if the workspace was kept.
    pub fn unlanded(&self) -> Option<Unlanded> {
        lock(&self.inner).ledger.unlanded.clone()
    }

    /// Whether every proposal is final (or there is none).
    pub fn settled(&self) -> bool {
        let inner = lock(&self.inner);
        inner.ledger.proposals.iter().all(|p| self.is_final(p))
    }

    /// Whether the workspace was pruned.
    pub fn pruned(&self) -> bool {
        lock(&self.inner).ledger.pruned
    }

    /// Whether a proposal is final. `landed` is final only where the repository has no full stage.
    fn is_final(&self, p: &Proposed) -> bool {
        match p.state.as_str() {
            "confirmed" | "rejected" | "cancelled" | "reverted" => true,
            "landed" => self.land.no_full_stage,
            _ => false,
        }
    }

    // ───── following the lane ─────

    /// Reads the lane's items for the session's open proposals and folds them in. Returns whether anything
    /// changed. A conflict record is fetched the first time it is named.
    pub async fn poll(&self) -> Result<bool, String> {
        let open: Vec<String> = {
            let inner = lock(&self.inner);
            inner
                .ledger
                .proposals
                .iter()
                .filter(|p| !self.is_final(p))
                .map(|p| p.proposal.clone())
                .collect()
        };
        if open.is_empty() {
            return Ok(false);
        }
        let seen = self.feed.changes().await?;
        let mut changed = false;
        for id in open {
            let Some(c) = seen
                .iter()
                .filter(|c| c.proposal == id)
                .max_by_key(|c| c.seq)
            else {
                continue;
            };
            let fetched = match &c.conflict {
                Some(r) => {
                    let need = lock(&self.inner)
                        .ledger
                        .proposals
                        .iter()
                        .any(|p| p.proposal == id && p.conflict_record.is_none());
                    if need {
                        self.fetch_record(r).await
                    } else {
                        None
                    }
                }
                None => None,
            };
            let mut inner = lock(&self.inner);
            let Some(p) = inner.ledger.proposals.iter_mut().find(|p| p.proposal == id) else {
                continue;
            };
            let before = p.clone();
            if c.seq >= p.lane_seq {
                p.state = c.state.clone();
                p.reason = c.reason.clone();
                p.detail = c.detail.clone();
                p.stalled = c.stalled.clone();
                p.position = c.position;
                p.lane_seq = c.seq;
                if c.conflict.is_some() {
                    p.conflict = c.conflict.clone();
                }
                if c.evidence.is_some() {
                    p.evidence = c.evidence.clone();
                }
            }
            if fetched.is_some() {
                p.conflict_record = fetched;
            }
            if *p != before {
                changed = true;
                let ledger = inner.ledger.clone();
                self.save(&ledger);
            }
        }
        Ok(changed)
    }

    async fn fetch_record(&self, r: &Value) -> Option<Value> {
        match self.feed.blob(r).await {
            Ok(bytes) => {
                let v: Value = serde_json::from_slice(&bytes).ok()?;
                Some(bounded_record(v))
            }
            Err(e) => {
                tracing::warn!("land: the conflict record could not be fetched: {e}");
                None
            }
        }
    }

    /// The line the session's status shows for the proposals, and whether it needs the agent or its operator
    /// (rejected, stalled): the first proposal still open, else the latest. `None` until the lane has said
    /// something about one.
    pub fn status_line(&self) -> Option<(String, bool)> {
        let inner = lock(&self.inner);
        // A proposal the lane has not mentioned is still waiting for its approval: that is the status's to say.
        let mut heard = inner
            .ledger
            .proposals
            .iter()
            .filter(|p| p.state != "proposed");
        let p = {
            let all: Vec<&Proposed> = heard.by_ref().collect();
            all.iter()
                .find(|p| !self.is_final(p))
                .or_else(|| all.last())
                .copied()?
        };
        let mut t = format!("land {}: {}", short(&p.proposal), p.state);
        if let Some(n) = p.position {
            t.push_str(&format!(" (position {n})"));
        }
        if let Some(r) = &p.reason {
            t.push_str(&format!(" ({r})"));
        }
        if let Some(s) = &p.stalled {
            t.push_str(&format!("; stalled: {s}"));
        }
        if let Some(u) = &inner.ledger.unlanded {
            t.push_str(&format!(
                "; {} commit(s) left in {} never landed",
                u.commits.len(),
                u.workspace
            ));
        }
        Some((
            bound(&t, 200),
            p.failed() && !self.superseded(&inner.ledger, p) || p.stalled.is_some(),
        ))
    }

    fn superseded(&self, l: &Ledger, p: &Proposed) -> bool {
        l.proposals
            .iter()
            .any(|q| q.rebase_of.as_deref() == Some(p.proposal.as_str()))
    }

    /// What the agent is told, as data: one object per proposal (state, reason, the conflict record when the
    /// lane rejected on one) and the `unlanded` report.
    pub fn notices(&self) -> Vec<Value> {
        let inner = lock(&self.inner);
        let mut out: Vec<Value> = inner
            .ledger
            .proposals
            .iter()
            .map(|p| {
                let mut v = json!({
                    "item": "proposal",
                    "proposal": p.proposal,
                    "state": p.state,
                    "base": p.base,
                    "to": p.to,
                    "title": p.title,
                    "tier": p.tier,
                    "final": self.is_final(p),
                });
                let o = v.as_object_mut().expect("an object");
                for (k, x) in [
                    ("reason", &p.reason),
                    ("detail", &p.detail),
                    ("stalled", &p.stalled),
                ] {
                    if let Some(x) = x {
                        o.insert(k.into(), json!(x));
                    }
                }
                if let Some(n) = p.position {
                    o.insert("position".into(), json!(n));
                }
                if !p.depends_on.is_empty() {
                    o.insert("dependsOn".into(), json!(p.depends_on));
                }
                if let Some(r) = &p.rebase_of {
                    o.insert("rebaseOf".into(), json!(r));
                }
                if let Some(r) = &p.conflict_record {
                    o.insert("conflictRecord".into(), r.clone());
                }
                if let Some(r) = &p.conflict {
                    o.insert("conflict".into(), r.clone());
                }
                if let Some(r) = &p.evidence {
                    o.insert("evidence".into(), r.clone());
                }
                if p.failed() && p.reason.as_deref() == Some("conflict") {
                    o.insert(
                        "next".into(),
                        json!(format!(
                            "resolve the conflict in your workspace, commit, and propose kind land with rebaseOf \"{}\"",
                            p.proposal
                        )),
                    );
                }
                v
            })
            .collect();
        if let Some(u) = &inner.ledger.unlanded {
            out.push(u.item());
        }
        out
    }

    // ───── pruning ─────

    /// Prunes the workspace when every proposal is final (R33 §7).
    ///
    /// * `busy`: a turn is running, so the agent is in the workspace: wait.
    /// * `session_over`: the session has stopped. A proposal that ended `rejected`, `cancelled` or `reverted`
    ///   (and no later one replaces it with `rebaseOf`) keeps the workspace while the session lives, because
    ///   the agent may re-propose; once the session is over it is pruned like the rest.
    ///
    /// The workspace is kept, and reported as `unlanded`, when it has uncommitted changes or commits after the
    /// last proposal's `to` that the target lacks by patch id. Otherwise the workspace, the session's build
    /// output (`CARGO_TARGET_DIR`, `prewarm`) and, with the workspace, its local refs are removed.
    pub fn prune(&self, busy: bool, session_over: bool) -> Pruned {
        let mut inner = lock(&self.inner);
        if inner.ledger.pruned {
            return Pruned::Waiting("already pruned");
        }
        if inner.ledger.unlanded.is_some() {
            return Pruned::Waiting("kept: reported as unlanded");
        }
        let Some(last) = inner.ledger.proposals.last().cloned() else {
            return Pruned::Waiting("nothing was proposed");
        };
        if inner.ledger.proposals.iter().any(|p| !self.is_final(p)) {
            return Pruned::Waiting("a proposal is not final");
        }
        if busy {
            return Pruned::Waiting("the agent is working in the workspace");
        }
        let ledger = &inner.ledger;
        if !session_over
            && ledger
                .proposals
                .iter()
                .any(|p| p.failed() && !self.superseded(ledger, p))
        {
            return Pruned::Waiting("the agent may propose again with rebaseOf");
        }
        let dir = &self.workspace.dir;
        let report = match self.unlanded_of(&last) {
            Ok(r) => r,
            Err(e) => {
                // Not knowing is not a reason to delete someone's commits.
                tracing::warn!("land: the workspace is kept, its state could not be read: {e}");
                return Pruned::Waiting("the workspace could not be examined");
            }
        };
        if let Some(u) = report {
            inner.ledger.unlanded = Some(u.clone());
            self.save(&inner.ledger);
            return Pruned::Kept(u);
        }
        for extra in ["target", "prewarm"] {
            if let Err(e) = han_vcs::remove_tree(&self.session_dir.join(extra)) {
                tracing::warn!("land: cannot remove {extra}: {e}");
            }
        }
        if let Some(f) = lock(&self.release).clone() {
            f();
        }
        if let Err(e) = self.vcs.remove_workspace(&self.workspace) {
            // A directory the OS keeps (the agent's working directory, on Windows) may stay behind empty: the
            // work is gone, and the empty directory is not worth another look.
            let emptied =
                !dir.exists() || std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
            if !emptied {
                tracing::warn!("land: cannot remove {}: {e}", dir.display());
                return Pruned::Waiting("the workspace could not be removed");
            }
            tracing::debug!("land: {} is held open and empty: {e}", dir.display());
        }
        inner.ledger.unlanded = None;
        inner.ledger.pruned = true;
        self.save(&inner.ledger);
        Pruned::Removed
    }

    /// The `unlanded` report of the workspace against the last proposal, or `None` when everything is on the
    /// target or in the proposal.
    fn unlanded_of(&self, last: &Proposed) -> Result<Option<Unlanded>, String> {
        let dir = &self.workspace.dir;
        let dirty = self.vcs.is_dirty(dir).map_err(|e| e.to_string())?;
        // What is after the proposal's `to`: the commits, unless the workspace's history no longer has it (it
        // was rewritten), when everything since the base is judged.
        let descends = self
            .vcs
            .is_ancestor(dir, &last.to, "HEAD")
            .map_err(|e| e.to_string())?;
        let tip = self.tip()?;
        let commits = if tip == last.to {
            Vec::new()
        } else {
            let target = self
                .vcs
                .fetch_target(dir, &self.source, &self.target_ref(), None)
                .ok()
                .flatten();
            // Without the target's objects the best that can be said is "not in what was proposed".
            let against = target.unwrap_or_else(|| {
                if descends {
                    last.to.clone()
                } else {
                    self.workspace.base.clone()
                }
            });
            let after = descends.then_some(last.to.as_str());
            self.vcs
                .unlanded(dir, &against, &tip, after)
                .map_err(|e| e.to_string())?
        };
        Ok((dirty || !commits.is_empty()).then(|| Unlanded {
            workspace: dir.display().to_string(),
            proposal: last.proposal.clone(),
            commits,
            uncommitted: dirty,
        }))
    }

    fn tip(&self) -> Result<String, String> {
        self.vcs
            .head(&self.workspace.dir)
            .map_err(|e| e.to_string())
    }

    /// The workspace's directory.
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace.dir
    }
}

fn short(s: &str) -> &str {
    &s[..s.len().min(12)]
}

/// A conflict record that fits the agent's context: whole when it is small, else the paths it names.
fn bounded_record(v: Value) -> Value {
    if v.to_string().len() <= MAX_RECORD {
        return v;
    }
    let paths: Vec<Value> = v
        .get("conflicts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("path").cloned())
        .collect();
    json!({
        "truncated": true,
        "onto": v.get("onto").cloned().unwrap_or(Value::Null),
        "clean": v.get("clean").cloned().unwrap_or(Value::Null),
        "conflictPaths": paths,
        "against": v.get("against").cloned().unwrap_or(Value::Null),
    })
}
