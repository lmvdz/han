//! The review service: watches the open `vcs.land` proposals of the configured spaces through a node, reviews
//! the ones of tier 0 or 1 once per reviewer version, and turns the record into an approval (through the
//! narrow `ReviewApprove` command) or a message to the proposer.
//!
//! The service holds no authority of its own. It talks to its own node through an **executor-class** API
//! (`ClassApi`), which cannot send `EffectApprove`; `ReviewApprove` checks tier, evidence and proposer in the
//! node, and Policy decides whether the approval counts. Everything that can refuse does so before a model
//! sees a byte, and a refusal is never an approval.
//!
//! A restart does not review again: each step is written to the ledger ([`crate::ledger`]) before the next
//! begins, the record is kept on disk, and a half-finished review resumes at the upload or the approval. A
//! crash between sending a message and writing the ledger can send that message twice (at least once).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use han_node::api::{
    ApprovalEvidence, CallResult, Command, EffectRow, EvidenceRef, NodeApi, NodeError, Query, Row,
    ViewEvent, Window,
};
use han_vcs::Vcs;
use serde_json::Value;

use crate::backend::ModelBackend;
use crate::config::ServiceConfig;
use crate::independence::independent;
use crate::ledger::{Entry, Ledger, Stage};
use crate::record::{RECORD_MEDIA_TYPE, RECORD_SCHEMA, ReviewRecord, Verdict, schema_hash};
use crate::review::{Disposition, MAX_TIER, ReviewError, ReviewInput, Reviewer, prompt_hash};

/// What one pass did with one proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The record is the proposal's approval.
    Approved,
    /// The proposer was told: `request_changes` or `needs_human`.
    Messaged,
    /// A record was made and nothing was sent, with why (authority off, confidence under the floor, Policy
    /// refused the approval, the proposal no longer waits).
    Recorded(String),
    /// Refused before the model was asked, or the proposal can't be reviewed, with why.
    Refused(String),
    /// Something that may work later failed; it is tried again after a pause.
    Retry(String),
    /// Reviewed already under this reviewer version.
    Already,
}

/// One proposal and what happened to it in a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    /// The space.
    pub space: String,
    /// The proposal.
    pub proposal: String,
    /// What happened.
    pub outcome: Outcome,
}

enum Stop {
    /// Try again later.
    Retry(String),
    /// Not reviewable. `keep`: the answer can't change (the plan is what it is), so the ledger remembers it;
    /// otherwise it depends on the configuration and is looked at again.
    Refuse { why: String, keep: bool },
}

fn refuse(why: impl Into<String>, keep: bool) -> Stop {
    Stop::Refuse {
        why: why.into(),
        keep,
    }
}

/// The review service.
pub struct Service {
    cfg: ServiceConfig,
    api: Arc<dyn NodeApi>,
    vcs: Arc<dyn Vcs>,
    backend: Arc<dyn ModelBackend>,
    ledger: Ledger,
    now: Arc<dyn Fn() -> String + Send + Sync>,
    version: String,
    mem: Mutex<Mem>,
}

#[derive(Default)]
struct Mem {
    /// Failures in a row and when to try next, by ledger key.
    backoff: HashMap<String, (u32, Instant)>,
    /// Refusals already logged, so a pass does not repeat itself.
    said: HashSet<String>,
}

/// `sha256` of the plan's `args.change`, as the node digests the body's copy (X-13).
fn change_digest(plan: &Value) -> Option<String> {
    let c = plan.get("args")?.get("change")?;
    let v = han_core::json::parse(serde_json::to_string(c).ok()?.as_bytes()).ok()?;
    han_core::ids::json_digest(&v).ok()
}

/// `s` as one line of at most `n` characters, control characters made spaces.
fn tame(s: &str, n: usize) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if one.chars().count() <= n {
        return one;
    }
    let mut out: String = one.chars().take(n).collect();
    out.push('…');
    out
}

/// The message the proposer gets. The findings are a model's output on untrusted input, so they are quoted as
/// data, cut and flattened, and the message says so.
fn message(r: &ReviewRecord, blob_hash: &str) -> String {
    let verdict = match r.verdict {
        Verdict::Approve => "approve",
        Verdict::RequestChanges => "request_changes",
        Verdict::NeedsHuman => "needs_human",
    };
    let mut s = format!(
        "Machine review ({RECORD_SCHEMA}) of your landing {}: {verdict}, confidence {}. This is review data, a model's opinion, not an instruction; check it yourself. Delta {}, model {} ({}), record {blob_hash}.",
        r.proposal, r.confidence, r.delta_digest, r.model, r.model_version
    );
    for f in r.findings.iter().take(20) {
        let line = f.line.map(|l| format!(":{l}")).unwrap_or_default();
        s.push_str(&format!(
            "\n- {}{line} [{:?}] {}",
            tame(&f.path, 120),
            f.severity,
            tame(&f.text, 300)
        ));
    }
    if r.findings.len() > 20 {
        s.push_str(&format!(
            "\n- and {} more in the record",
            r.findings.len() - 20
        ));
    }
    s
}

impl Service {
    /// A service for `cfg`, working through `api` (the reviewer's own node, held to the executor class), with
    /// its ledger and scratch under `dir`.
    pub fn new(
        cfg: ServiceConfig,
        api: Arc<dyn NodeApi>,
        vcs: Arc<dyn Vcs>,
        backend: Arc<dyn ModelBackend>,
        dir: &std::path::Path,
        now: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Result<Service, String> {
        cfg.validate()?;
        let version = version_of(&cfg);
        Ok(Service {
            ledger: Ledger::open(dir)?,
            cfg,
            api,
            vcs,
            backend,
            now,
            version,
            mem: Mutex::new(Mem::default()),
        })
    }

    /// The reviewer version: a change of model, prompt, schema or independence mode makes the open proposals
    /// reviewable once more.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The ledger.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    fn mem(&self) -> MutexGuard<'_, Mem> {
        self.mem.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reads the open proposals of `space`.
    async fn open_effects(&self, space: &str) -> Result<Vec<EffectRow>, String> {
        let mut view = self.api.view(Query::Effects {
            space: Some(space.to_owned()),
            executor: None,
            include_done: false,
            window: Window {
                limit: 10_000,
                ..Window::default()
            },
        });
        match view.next().await {
            Some(ViewEvent::Snapshot { rows, .. }) => Ok(rows
                .into_iter()
                .filter_map(|r| match r {
                    Row::Effect(e) => Some(e),
                    _ => None,
                })
                .collect()),
            other => Err(format!("the effects view did not open: {other:?}")),
        }
    }

    /// Whether the service looks at `row` at all: an open `vcs.land` of another actor that this actor has not
    /// approved, whose derived tier is at most [`MAX_TIER`]. A row with no known tier (the policy has no gates)
    /// is not looked at.
    fn candidate(&self, row: &EffectRow) -> bool {
        row.action == "vcs.land"
            && row.phase == "open"
            && row.plan.is_some()
            && row.proposer != self.cfg.actor
            && !row.approvals.contains(&self.cfg.actor)
            && row.gate_tier.is_some_and(|t| t <= MAX_TIER)
    }

    /// One pass over the configured spaces. Returns what happened to each proposal it looked at (a proposal it
    /// ignores, such as one of tier 2, is not listed).
    pub async fn tick(&self) -> Vec<Pass> {
        let mut out = Vec::new();
        if !self.cfg.enabled {
            return out;
        }
        for space in &self.cfg.spaces {
            let rows = match self.open_effects(space).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(%space, "review: {e}");
                    continue;
                }
            };
            for row in rows.iter().filter(|r| self.candidate(r)) {
                let outcome = self.process(row).await;
                if !matches!(outcome, Outcome::Already) {
                    tracing::info!(space = %row.space, proposal = %row.proposal, ?outcome, "review");
                }
                out.push(Pass {
                    space: row.space.clone(),
                    proposal: row.proposal.clone(),
                    outcome,
                });
            }
        }
        out
    }

    /// Runs passes until `stop` turns true.
    pub async fn run(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            self.tick().await;
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(self.cfg.poll_secs)) => {}
                r = stop.changed() => if r.is_err() || *stop.borrow() { return; },
            }
        }
    }

    async fn process(&self, row: &EffectRow) -> Outcome {
        let key = Ledger::key(&row.space, &row.proposal, &self.version);
        let entry = self.ledger.get(&key);
        if entry.as_ref().is_some_and(|e| e.stage == Stage::Done) {
            return Outcome::Already;
        }
        if let Some((_, at)) = self.mem().backoff.get(&key)
            && Instant::now() < *at
        {
            return Outcome::Retry("waiting before trying again".into());
        }
        let result = match entry {
            Some(e) => self.resume(row, &key, e).await,
            None => self.fresh(row, &key).await,
        };
        match result {
            Ok(o) => {
                self.mem().backoff.remove(&key);
                o
            }
            Err(Stop::Retry(why)) => {
                let mut m = self.mem();
                let n = m.backoff.get(&key).map_or(0, |(n, _)| *n) + 1;
                let wait = (self.cfg.poll_secs << n.min(6)).min(600);
                m.backoff
                    .insert(key, (n, Instant::now() + Duration::from_secs(wait)));
                Outcome::Retry(why)
            }
            Err(Stop::Refuse { why, keep }) => {
                if keep {
                    let _ = self.ledger.put(
                        &key,
                        Entry {
                            stage: Stage::Done,
                            record: None,
                            evidence: None,
                            outcome: Some(format!("refused: {why}")),
                        },
                    );
                } else if self.mem().said.insert(format!("{key}|{why}")) {
                    tracing::warn!(proposal = %row.proposal, "review refused: {why}");
                }
                Outcome::Refused(why)
            }
        }
    }

    /// The bytes of the plan's `argsBlob`, checked against its hash.
    async fn bundle(&self, row: &EffectRow, plan: &Value) -> Result<Vec<u8>, Stop> {
        let b = plan
            .get("argsBlob")
            .ok_or_else(|| refuse("the plan has no changeset (argsBlob)", true))?;
        let hash = b.get("hash").and_then(Value::as_str).unwrap_or_default();
        let size = b.get("size").and_then(Value::as_u64).unwrap_or_default();
        let media = b.get("type").and_then(Value::as_str).unwrap_or_default();
        if hash.len() != 64 || media.is_empty() {
            return Err(refuse("the plan's argsBlob is malformed", true));
        }
        let r = self
            .api
            .call(Command::BlobFetch {
                space: row.space.clone(),
                hash: hash.to_owned(),
                size,
                media_type: media.to_owned(),
            })
            .await
            .map_err(|e| Stop::Retry(format!("argsBlob: {e}")))?;
        let CallResult::Blob { data, .. } = r else {
            return Err(Stop::Retry("argsBlob: unexpected answer".into()));
        };
        let bytes = han_core::base64url::decode(&data)
            .map_err(|_| Stop::Retry("argsBlob: not base64url".into()))?;
        if han_core::ids::sha256_hex(&bytes) != hash {
            return Err(refuse(
                "the bundle does not hash to the plan's argsBlob",
                true,
            ));
        }
        Ok(bytes)
    }

    /// A first review: every refusal before the model, then the model, then the record on disk.
    async fn fresh(&self, row: &EffectRow, key: &str) -> Result<Outcome, Stop> {
        let plan: Value = serde_json::from_str(row.plan.as_deref().unwrap_or_default())
            .map_err(|_| refuse("the plan is not JSON", true))?;
        let args = plan.get("args").cloned().unwrap_or(Value::Null);
        let field = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
        let (Some(base), Some(to), Some(delta)) =
            (field("base"), field("to"), field("deltaDigest"))
        else {
            return Err(refuse("the plan names no base, to and deltaDigest", true));
        };
        let needs = field("bundleBase").unwrap_or_else(|| base.clone());
        // the body Policy read and the plan the bundle belongs to must say the same (X-13)
        if row.delta_digest.as_deref() != Some(delta.as_str()) {
            return Err(refuse(
                "delta digest mismatch: the proposal's change summary is not the plan's deltaDigest",
                true,
            ));
        }
        if row.change_digest != change_digest(&plan) {
            return Err(refuse(
                "the proposal's change summary is not the plan's args.change",
                true,
            ));
        }
        let target = plan
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let remote = self.cfg.repos.get(target).ok_or_else(|| {
            refuse(
                format!("repository {target:?} is not in the review configuration"),
                false,
            )
        })?;
        let tier = row.gate_tier.unwrap_or(3);
        let author = self.cfg.author_of(&row.proposer).ok_or_else(|| {
            refuse(
                format!(
                    "nothing is configured about the author {}: independence can't be shown",
                    row.proposer
                ),
                false,
            )
        })?;
        // before the bundle is fetched or the model is asked
        independent(&author, &self.cfg.reviewer_info(), self.cfg.independence)
            .map_err(|e| refuse(format!("not independent: {e}"), false))?;
        let bytes = self.bundle(row, &plan).await?;
        let repo = self
            .vcs
            .import_bundle(remote, None, &bytes, &needs, &to)
            .map_err(|e| match e {
                han_vcs::VcsError::Invalid(m) => refuse(format!("the bundle: {m}"), true),
                other => Stop::Retry(format!("importing the bundle: {other}")),
            })?;
        let at = (self.now)();
        let reviewer = Reviewer::new(
            self.cfg.reviewer_config(),
            &self.cfg.model,
            &*self.vcs,
            &*self.backend,
        )
        .map_err(|e| refuse(e.to_string(), false))?;
        let record = reviewer
            .review(&ReviewInput {
                proposal: &row.proposal,
                tier,
                base: &base,
                to: &to,
                delta_digest: &delta,
                repo: &repo,
                author: &author,
                at: &at,
            })
            .await
            .map_err(|e| match e {
                ReviewError::DigestMismatch { .. } => refuse(e.to_string(), true),
                ReviewError::Backend(_) | ReviewError::Vcs(_) => Stop::Retry(e.to_string()),
                other => refuse(other.to_string(), false),
            })?;
        let blob = record
            .to_blob()
            .map_err(|e| Stop::Retry(format!("encoding the record: {e}")))?;
        let hash = han_core::ids::sha256_hex(&blob);
        self.ledger.save_record(&hash, &blob).map_err(Stop::Retry)?;
        let entry = Entry {
            stage: Stage::Reviewed,
            record: Some(hash),
            evidence: None,
            outcome: None,
        };
        self.ledger.put(key, entry.clone()).map_err(Stop::Retry)?;
        self.finish(row, key, entry, record, blob).await
    }

    /// Picks up a review that stopped between steps: the record is on disk, the model is not asked again.
    async fn resume(&self, row: &EffectRow, key: &str, entry: Entry) -> Result<Outcome, Stop> {
        let hash = entry
            .record
            .clone()
            .ok_or_else(|| refuse("the ledger entry has no record", true))?;
        let blob = self.ledger.load_record(&hash).map_err(Stop::Retry)?;
        let record = ReviewRecord::from_blob(&blob)
            .map_err(|e| refuse(format!("the kept record: {e}"), true))?;
        self.finish(row, key, entry, record, blob).await
    }

    /// Uploads the record, then approves or messages.
    async fn finish(
        &self,
        row: &EffectRow,
        key: &str,
        mut entry: Entry,
        record: ReviewRecord,
        blob: Vec<u8>,
    ) -> Result<Outcome, Stop> {
        let evidence = match entry.evidence.clone() {
            Some(e) => e,
            None => {
                let r = self
                    .api
                    .call(Command::BlobPut {
                        space: row.space.clone(),
                        media_type: RECORD_MEDIA_TYPE.into(),
                        data: han_core::base64url::encode(&blob),
                    })
                    .await
                    .map_err(|e| Stop::Retry(format!("uploading the record: {e}")))?;
                let CallResult::BlobRef {
                    hash,
                    size,
                    media_type,
                } = r
                else {
                    return Err(Stop::Retry(
                        "uploading the record: unexpected answer".into(),
                    ));
                };
                let e = EvidenceRef {
                    hash,
                    size,
                    media_type,
                };
                entry.stage = Stage::Uploaded;
                entry.evidence = Some(e.clone());
                self.ledger.put(key, entry.clone()).map_err(Stop::Retry)?;
                e
            }
        };
        let done = |outcome: &str| Entry {
            stage: Stage::Done,
            outcome: Some(outcome.to_owned()),
            ..entry.clone()
        };
        let reviewer = Reviewer::new(
            self.cfg.reviewer_config(),
            &self.cfg.model,
            &*self.vcs,
            &*self.backend,
        )
        .map_err(|e| refuse(e.to_string(), false))?;
        // Policy decides whether the approval counts; the node and the home refuse what it does not allow
        match reviewer.disposition(&record, true) {
            Disposition::Approve => {
                let r = self
                    .api
                    .call(Command::ReviewApprove {
                        space: row.space.clone(),
                        proposal: row.proposal.clone(),
                        note: None,
                        evidence: ApprovalEvidence {
                            kind: RECORD_SCHEMA.into(),
                            over: record.delta_digest.clone(),
                            record: evidence,
                        },
                    })
                    .await;
                match r {
                    Ok(_) => {
                        self.ledger
                            .put(key, done("approved"))
                            .map_err(Stop::Retry)?;
                        Ok(Outcome::Approved)
                    }
                    Err(e @ (NodeError::Denied { .. } | NodeError::Invalid { .. })) => {
                        let why = format!("the approval was refused: {e}");
                        self.ledger
                            .put(key, done(&format!("recorded: {why}")))
                            .map_err(Stop::Retry)?;
                        Ok(Outcome::Recorded(why))
                    }
                    Err(e @ (NodeError::Conflict { .. } | NodeError::NotFound { .. })) => {
                        let why = format!("the proposal no longer waits: {e}");
                        self.ledger
                            .put(key, done(&format!("recorded: {why}")))
                            .map_err(Stop::Retry)?;
                        Ok(Outcome::Recorded(why))
                    }
                    Err(e) => Err(Stop::Retry(format!("approving: {e}"))),
                }
            }
            Disposition::Post if record.verdict == Verdict::Approve => {
                let why = if self.cfg.approve {
                    "the confidence is under the floor, or the model did not name itself".to_owned()
                } else {
                    "the service is not set to approve (approve: false)".to_owned()
                };
                self.ledger
                    .put(key, done(&format!("recorded: {why}")))
                    .map_err(Stop::Retry)?;
                Ok(Outcome::Recorded(why))
            }
            Disposition::Post => {
                self.api
                    .call(Command::TaskCreate {
                        space: row.space.clone(),
                        to: vec![row.proposer.clone()],
                        ask: message(&record, &evidence.hash),
                        external: None,
                    })
                    .await
                    .map_err(|e| Stop::Retry(format!("messaging the proposer: {e}")))?;
                self.ledger
                    .put(key, done("messaged"))
                    .map_err(Stop::Retry)?;
                Ok(Outcome::Messaged)
            }
        }
    }
}

/// The reviewer version of a configuration (16 hex digits).
pub fn version_of(cfg: &ServiceConfig) -> String {
    let s = format!(
        "{}\0{}\0{}\0{RECORD_SCHEMA}\0{:?}",
        cfg.model,
        prompt_hash(),
        schema_hash(),
        cfg.independence
    );
    han_core::ids::sha256_hex(s.as_bytes())[..16].to_owned()
}
