//! A fake node (the `Effects` view and the effect commands, folded like Policy would), a fake version-control
//! backend, a counting action, and a stoppable clock.

#![allow(dead_code, missing_docs)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_exec::{
    Action, ActionCtx, Elapsed, ExecConfig, Executor, ExecutorConfig, IdempotencyKey,
    NeedsApproval, Outcome, PlanError, RepoTarget,
};
use han_node::api::{
    BoxFuture, CallResult, Command, EffectRow, NodeApi, NodeError, Query, Row, ViewEvent,
    ViewStream,
};
use han_vcs::{
    ChangeSet, LandOutcome, LandSpec, MergeSim, RemoteAuth, Vcs, VcsError, Workspace, WorkspaceSpec,
};
use serde_json::{Value, json};

pub const EXEC: &str = "@exec:a.localhost";
pub const SPACE: &str = "s1:a.localhost";

// ───────────────────────── the clock ─────────────────────────

#[derive(Default)]
pub struct TestClock(pub AtomicU64);

impl TestClock {
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Elapsed for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn name(&self) -> &'static str {
        "test"
    }
}

/// A wall clock a test moves.
pub struct TestWall(pub AtomicI64);

impl Default for TestWall {
    fn default() -> Self {
        TestWall(AtomicI64::new(1_792_000_000))
    }
}

impl TestWall {
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl han_exec::Wall for TestWall {
    fn now_secs(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

// ───────────────────────── the node ─────────────────────────

/// How a call misbehaves once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Never answers, without applying (a crash before the send).
    HangBefore,
    /// Applies, then never answers (a crash after the send: the reply is lost).
    HangAfter,
    /// Applies, then answers `unavailable` (a lost reply the caller sees).
    LostReply,
}

#[derive(Default)]
pub struct NodeState {
    pub rows: Vec<EffectRow>,
    pub saved_approvals: Vec<(String, Vec<String>)>,
    pub head: u64,
    pub commands: Vec<Command>,
    pub outcomes: Vec<(String, String, String)>,
    pub claim_faults: Vec<Fault>,
    pub outcome_faults: Vec<Fault>,
    /// Advance this clock by this much when a claim is appended (a stalled executor).
    pub stall_on_claim: Option<(Arc<TestClock>, u64)>,
    /// Blobs of the space, by hash.
    pub blobs: HashMap<String, Vec<u8>>,
    /// Profile stream items published: `(session, profile name, body)`.
    pub published: Vec<(String, String, Value)>,
    /// Runs once, just before the next claim is applied (the world moves under the claim).
    pub on_claim: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Default, Clone)]
pub struct FakeNode(pub Arc<Mutex<NodeState>>);

impl FakeNode {
    pub fn state(&self) -> std::sync::MutexGuard<'_, NodeState> {
        self.0.lock().unwrap()
    }

    /// Proposes a plan; returns the proposal id.
    pub fn propose(&self, plan: &Value) -> String {
        let text =
            String::from_utf8(han_core::json::canonicalize(&to_core(plan)).unwrap()).unwrap();
        let digest = han_core::ids::json_digest(&to_core(plan)).unwrap();
        self.propose_raw(plan["action"].as_str().unwrap(), &text, &digest)
    }

    pub fn propose_raw(&self, action: &str, plan: &str, digest: &str) -> String {
        let mut st = self.state();
        st.head += 1;
        let seq = st.head;
        let id = format!("p{seq}");
        st.rows.push(EffectRow {
            key: format!("{SPACE}|{seq:012}|{id}"),
            space: SPACE.into(),
            proposal: id.clone(),
            proposer: "@agent:a.localhost".into(),
            executor: EXEC.into(),
            action: action.into(),
            plan_digest: digest.into(),
            plan: Some(plan.into()),
            task: None,
            phase: "open".into(),
            approvals: vec![],
            claims: 0,
            last_claim: None,
            launch_window: 30,
            repeatable: false,
            cost: None,
            expires_at: None,
            approved_seq: None,
            approved_at: None,
            seq,
            head: seq,
            ended: None,
        });
        id
    }

    pub fn approve(&self, proposal: &str) {
        self.approve_at(proposal, "2026-10-15T10:00:00Z");
    }

    pub fn approve_at(&self, proposal: &str, time: &str) {
        let mut st = self.state();
        st.head += 1;
        let head = st.head;
        let r = st.rows.iter_mut().find(|r| r.proposal == proposal).unwrap();
        r.approvals.push("@lars:a.localhost".into());
        r.approved_seq = Some(head);
        r.approved_at = Some(time.into());
        for r in &mut st.rows {
            r.head = head;
        }
    }

    pub fn row(&self, proposal: &str) -> EffectRow {
        self.state()
            .rows
            .iter()
            .find(|r| r.proposal == proposal)
            .unwrap()
            .clone()
    }

    pub fn claims(&self) -> usize {
        self.state()
            .commands
            .iter()
            .filter(|c| matches!(c, Command::EffectClaim { .. }))
            .count()
    }

    fn bump(st: &mut NodeState) {
        st.head += 1;
        let head = st.head;
        for r in &mut st.rows {
            r.head = head;
        }
    }

    /// Puts a blob into the space; returns its hash.
    pub fn put_blob(&self, bytes: &[u8]) -> String {
        let hash = han_core::ids::sha256_hex(bytes);
        self.state().blobs.insert(hash.clone(), bytes.to_vec());
        hash
    }

    /// The profile items published on the lane's stream with this profile name, in order.
    pub fn items(&self, name: &str) -> Vec<Value> {
        self.state()
            .published
            .iter()
            .filter(|(_, n, _)| n == name)
            .map(|(_, _, b)| b.clone())
            .collect()
    }

    fn apply(&self, cmd: &Command) -> Result<CallResult, NodeError> {
        if let Command::EffectClaim { .. } = cmd {
            let hook = self.state().on_claim.take();
            if let Some(h) = hook {
                h();
            }
        }
        let mut st = self.state();
        st.commands.push(cmd.clone());
        match cmd {
            Command::EffectClaim {
                proposal,
                attempt,
                window,
                ..
            } => {
                let r = st
                    .rows
                    .iter_mut()
                    .find(|r| &r.proposal == proposal)
                    .ok_or_else(|| NodeError::not_found("proposal"))?;
                if r.phase != "open" || *attempt != r.claims + 1 || *window != r.launch_window {
                    return Err(NodeError::denied("E5"));
                }
                if r.approvals.is_empty() {
                    return Err(NodeError::denied("E3: no approval"));
                }
                r.claims += 1;
                let id = format!("c{}-{}", r.proposal, r.claims);
                r.last_claim = Some(id.clone());
                r.phase = "claimed".into();
                r.approved_seq = None;
                r.approved_at = None;
                let taken = std::mem::take(&mut r.approvals);
                let p = r.proposal.clone();
                st.saved_approvals.push((p, taken));
                Self::bump(&mut st);
                if let Some((clock, ms)) = st.stall_on_claim.clone() {
                    clock.advance(ms);
                }
                Ok(CallResult::Receipt {
                    space: SPACE.into(),
                    seq: st.head,
                    id,
                    time: String::new(),
                })
            }
            Command::EffectOutcome {
                claim,
                result,
                started,
                ..
            } => {
                let idx = st
                    .rows
                    .iter()
                    .position(|r| {
                        r.last_claim.as_deref() == Some(claim.as_str()) && r.phase == "claimed"
                    })
                    .ok_or_else(|| NodeError::denied("not the latest open claim"))?;
                let proposal = st.rows[idx].proposal.clone();
                let phase = match (result.as_str(), started.as_str()) {
                    ("failed" | "cancelled", "no") => {
                        // The approvals return with the effect (P-09); their order and time with them.
                        let head = st.head;
                        st.rows[idx].approved_seq = Some(head);
                        st.rows[idx].approved_at = Some("2026-10-15T10:00:00Z".into());
                        let back = st
                            .saved_approvals
                            .iter()
                            .rev()
                            .find(|(p, _)| *p == proposal)
                            .map(|(_, a)| a.clone())
                            .unwrap_or_default();
                        st.rows[idx].approvals = back;
                        "open"
                    }
                    _ => "done",
                };
                st.rows[idx].phase = phase.into();
                st.outcomes
                    .push((proposal, result.clone(), started.clone()));
                Self::bump(&mut st);
                Ok(CallResult::Receipt {
                    space: SPACE.into(),
                    seq: st.head,
                    id: format!("o{}", st.head),
                    time: String::new(),
                })
            }
            Command::EffectCancel { proposal, .. } => {
                // P-07a: the executor may cancel its own effect while it is open.
                let r = st
                    .rows
                    .iter_mut()
                    .find(|r| &r.proposal == proposal)
                    .ok_or_else(|| NodeError::not_found("proposal"))?;
                if r.phase != "open" {
                    return Err(NodeError::conflict("the proposal is no longer open"));
                }
                r.phase = "done".into();
                Self::bump(&mut st);
                Ok(CallResult::Receipt {
                    space: SPACE.into(),
                    seq: st.head,
                    id: format!("x{}", st.head),
                    time: String::new(),
                })
            }
            Command::BlobFetch {
                hash, media_type, ..
            } => {
                let data = st
                    .blobs
                    .get(hash)
                    .ok_or_else(|| NodeError::not_found("blob"))?;
                Ok(CallResult::Blob {
                    hash: hash.clone(),
                    media_type: media_type.clone(),
                    data: han_core::base64url::encode(data),
                })
            }
            Command::BlobPut {
                media_type, data, ..
            } => {
                let bytes = han_core::base64url::decode(data)
                    .map_err(|_| NodeError::invalid("base64url"))?;
                let hash = han_core::ids::sha256_hex(&bytes);
                let size = bytes.len() as u64;
                st.blobs.insert(hash.clone(), bytes);
                Ok(CallResult::BlobRef {
                    hash,
                    size,
                    media_type: media_type.clone(),
                })
            }
            Command::SessionOpen { .. } => {
                st.head += 1;
                Ok(CallResult::Receipt {
                    space: SPACE.into(),
                    seq: st.head,
                    id: "lane-stream".into(),
                    time: String::new(),
                })
            }
            Command::StreamPublish {
                session,
                name,
                body,
                ..
            } => {
                let v: Value =
                    serde_json::from_str(body).map_err(|_| NodeError::invalid("body"))?;
                st.published.push((session.clone(), name.clone(), v));
                Ok(CallResult::Status {
                    n: st.published.len() as u64,
                })
            }
            other => Err(NodeError::denied(format!(
                "not an executor command: {other:?}"
            ))),
        }
    }
}

impl NodeApi for FakeNode {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        Box::pin(async move {
            if !matches!(
                cmd,
                Command::EffectClaim { .. } | Command::EffectOutcome { .. }
            ) {
                return self.apply(&cmd);
            }
            let fault = {
                let mut st = self.state();
                let list = match &cmd {
                    Command::EffectClaim { .. } => &mut st.claim_faults,
                    _ => &mut st.outcome_faults,
                };
                if list.is_empty() {
                    None
                } else {
                    Some(list.remove(0))
                }
            };
            match fault {
                None => self.apply(&cmd),
                Some(Fault::HangBefore) => std::future::pending().await,
                Some(Fault::HangAfter) => {
                    let _ = self.apply(&cmd);
                    std::future::pending().await
                }
                Some(Fault::LostReply) => {
                    let _ = self.apply(&cmd);
                    Err(NodeError::unavailable("reply lost"))
                }
            }
        })
    }

    fn view(&self, query: Query) -> ViewStream {
        let Query::Effects { executor, .. } = query else {
            return ViewStream::failed(NodeError::unavailable("only effects"));
        };
        let rows: Vec<Row> = self
            .state()
            .rows
            .iter()
            .filter(|r| executor.as_deref().is_none_or(|x| r.executor == x))
            .cloned()
            .map(Row::Effect)
            .collect();
        let (sink, stream) = ViewStream::channel();
        sink.send(ViewEvent::Snapshot {
            seq: 1,
            total: rows.len() as u32,
            rows,
            offset: 0,
            next_cursor: None,
            now_ms: 0,
            totals: None,
        });
        stream
    }
}

pub fn to_core(v: &Value) -> han_core::Value {
    han_core::json::parse(v.to_string().as_bytes()).unwrap()
}

// ───────────────────────── version control ─────────────────────────

#[derive(Default)]
pub struct VcsState {
    pub digest: String,
    pub remote: Option<String>,
    pub lands: u32,
    pub auth_seen: Vec<String>,
}

#[derive(Default, Clone)]
pub struct FakeVcs(pub Arc<Mutex<VcsState>>);

fn nope() -> VcsError {
    VcsError::Invalid("fake".into())
}

impl Vcs for FakeVcs {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn create_workspace(&self, _: &WorkspaceSpec) -> han_vcs::Result<Workspace> {
        Err(nope())
    }
    fn changeset(&self, _: &Path, _: &str) -> han_vcs::Result<ChangeSet> {
        Err(nope())
    }
    fn changeset_between(&self, _: &Path, base: &str, head: &str) -> han_vcs::Result<ChangeSet> {
        Ok(ChangeSet {
            base: base.into(),
            head: Some(head.into()),
            files: vec![],
            digest: self.0.lock().unwrap().digest.clone(),
        })
    }
    fn commit_all(&self, _: &Path, _: &str, _: &str) -> han_vcs::Result<String> {
        Err(nope())
    }
    fn remote_ref(
        &self,
        _: &Path,
        _: &str,
        _: &str,
        _: Option<&RemoteAuth>,
    ) -> han_vcs::Result<Option<String>> {
        Ok(self.0.lock().unwrap().remote.clone())
    }
    fn land(
        &self,
        _: &Path,
        spec: &LandSpec,
        auth: Option<&RemoteAuth>,
    ) -> han_vcs::Result<LandOutcome> {
        let mut st = self.0.lock().unwrap();
        if let Some(a) = auth {
            st.auth_seen.push(a.secret().to_owned());
        }
        if st.remote.as_deref() == Some(spec.new.as_str()) {
            return Ok(LandOutcome::AlreadyThere);
        }
        if st.remote != spec.expected {
            return Ok(LandOutcome::Conflict {
                actual: st.remote.clone(),
            });
        }
        st.remote = Some(spec.new.clone());
        st.lands += 1;
        Ok(LandOutcome::Landed)
    }
    fn merge_simulate(&self, _: &Path, _: &str, _: &str) -> han_vcs::Result<MergeSim> {
        Err(nope())
    }
    fn remove_workspace(&self, _: &Workspace) -> han_vcs::Result<()> {
        Ok(())
    }
}

// ───────────────────────── a counting action ─────────────────────────

/// Where the counting action crashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunFault {
    None,
    /// Hangs before touching the world.
    Before,
    /// Touches the world, then hangs.
    After,
}

pub struct Counter {
    pub world: Arc<AtomicU64>,
    pub runs: Arc<AtomicU64>,
    pub fault: Mutex<RunFault>,
}

impl Counter {
    pub fn new() -> Arc<Counter> {
        Arc::new(Counter {
            world: Arc::default(),
            runs: Arc::default(),
            fault: Mutex::new(RunFault::None),
        })
    }
}

#[async_trait::async_trait]
impl Action for Counter {
    fn kind(&self) -> &'static str {
        "count"
    }
    fn plan_check(&self, plan: &Value, _: &ExecConfig) -> Result<(), PlanError> {
        let obj = plan.as_object().ok_or(PlanError::Schema("object".into()))?;
        if obj
            .keys()
            .any(|k| !["action", "target", "args"].contains(&k.as_str()))
        {
            return Err(PlanError::Schema("unknown member".into()));
        }
        Ok(())
    }
    async fn preflight(&self, _: &Value, _: &ActionCtx) -> Result<(), PlanError> {
        Ok(())
    }
    async fn run(&self, _: &Value, _: &IdempotencyKey, _: &ActionCtx) -> Outcome {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let fault = std::mem::replace(&mut *self.fault.lock().unwrap(), RunFault::None);
        if fault == RunFault::Before {
            std::future::pending::<()>().await;
        }
        self.world.fetch_add(1, Ordering::SeqCst);
        if fault == RunFault::After {
            std::future::pending::<()>().await;
        }
        Outcome::Succeeded {
            detail: "counted".into(),
        }
    }
    async fn lookup(&self, _: &Value, _: &IdempotencyKey, _: &ActionCtx) -> Option<Outcome> {
        (self.world.load(Ordering::SeqCst) > 0).then(|| Outcome::Succeeded {
            detail: "found".into(),
        })
    }
}

pub fn count_plan(n: u64) -> Value {
    json!({"action": "count", "target": "world", "args": {"n": n}})
}

// ───────────────────────── wiring ─────────────────────────

pub struct Rig {
    pub node: FakeNode,
    pub vcs: FakeVcs,
    pub clock: Arc<TestClock>,
    pub dir: tempfile::TempDir,
    pub actions: Vec<Arc<dyn Action>>,
    pub config: ExecConfig,
}

impl Rig {
    pub fn new(actions: Vec<Arc<dyn Action>>) -> Rig {
        Rig {
            node: FakeNode::default(),
            vcs: FakeVcs::default(),
            clock: Arc::new(TestClock::default()),
            dir: tempfile::tempdir().unwrap(),
            actions,
            config: ExecConfig {
                repos: vec![RepoTarget::new("repo", "remote")],
                branch_prefixes: vec!["han/".into()],
                ..ExecConfig::default()
            },
        }
    }

    pub fn executor(&self) -> Executor {
        self.executor_with(None)
    }

    pub fn executor_with(&self, github: Option<Arc<han_exec::github::GithubMinter>>) -> Executor {
        Executor::open(
            ExecutorConfig {
                actor: EXEC.into(),
                actions: self.actions.clone(),
                gate: Arc::new(NeedsApproval),
                config: Arc::new(self.config.clone()),
                vcs: Arc::new(self.vcs.clone()),
                github,
                state_dir: self.dir.path().to_path_buf(),
                stall_bound: Duration::from_secs(10),
            },
            Arc::new(self.node.clone()),
            self.clock.clone(),
        )
        .unwrap()
    }
}

/// Runs a future; `None` if it does not finish in 300 ms (a "crash": the future is dropped).
pub async fn or_crash<T>(f: impl std::future::Future<Output = T>) -> Option<T> {
    tokio::time::timeout(Duration::from_millis(300), f)
        .await
        .ok()
}
