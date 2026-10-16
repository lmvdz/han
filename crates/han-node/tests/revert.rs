//! The land lane's own approval of its revert (A2, P-17 "Revert"): `Command::RevertApprove` against a real home.
//! The node signs as its own actor with no prompt only for an open revert that actor proposed, with `han.ci/1`
//! evidence over the delta of the change it undoes; Policy still decides, and no other connection may send it.

mod common;

use std::sync::Arc;

use common::{World, call, confirm_operator, id_of, space_of, until};
use han_node::ClassApi;
use han_node::api::{
    ApprovalEvidence, CallResult, Command, CommandClass, ConnectionClass, EffectRow, EvidenceRef,
    NodeApi, NodeError, Query, Row, Window,
};

const ALICE: &str = "@alice:a.localhost";
const CODER: &str = "@coder:a.localhost";
const EXEC: &str = "@exec:a.localhost";

fn effects(api: &dyn NodeApi) -> Vec<EffectRow> {
    let mut v = api.view(Query::Effects {
        space: None,
        executor: Some(EXEC.into()),
        include_done: true,
        window: Window::default(),
    });
    let mut out = Vec::new();
    while let Some(ev) = v.try_next() {
        if let han_node::api::ViewEvent::Snapshot { rows, .. } = ev {
            out = rows
                .into_iter()
                .filter_map(|r| match r {
                    Row::Effect(e) => Some(e),
                    _ => None,
                })
                .collect();
        }
    }
    out
}

/// A revert rule for tier 0-1 that names the lane executor, then humans for the rest.
fn policy() -> String {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        m.insert(
            "approvals".into(),
            han_core::json::parse(
                format!(
                    r#"[{{"action":"vcs.land","revert":true,"maxTier":1,"approvers":["{EXEC}","owner"],"evidence":"han.ci/1"}},
                        {{"action":"vcs.land","approvers":["owner"],"humans":true}}]"#
                )
                .as_bytes(),
            )
            .unwrap(),
        );
        m.insert("separateApprover".into(), han_core::Value::Bool(true));
        m.insert(
            "gates".into(),
            han_core::json::parse(br#"{"small":20,"large":500,"paths":[],"owners":[]}"#).unwrap(),
        );
    }
    String::from_utf8(han_core::json::canonicalize(&p).unwrap()).unwrap()
}

fn digest(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn land_plan(path: &str, delta: &str, revert_of: Option<&str>) -> String {
    let mut change = serde_json::json!({
        "repo": "github:o/r", "deltaDigest": delta, "total": 1,
        "files": [{"path": path, "kind": "modify", "lines": 3}],
    });
    if let Some(r) = revert_of {
        change["revertOf"] = r.into();
    }
    serde_json::json!({
        "action": "vcs.land", "target": "github:o/r",
        "args": {"ref": "refs/heads/main", "change": change},
    })
    .to_string()
}

struct Rig {
    _world: World,
    alice: han_node::Node<han_net::MemTransport>,
    coder: han_node::Node<han_net::MemTransport>,
    exec: han_node::Node<han_net::MemTransport>,
    space: String,
}

async fn rig() -> Rig {
    let world = World::new(&[(ALICE, 61)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(62), Some(ALICE))
        .unwrap();
    world
        .home
        .register(EXEC, "service", "d1", &common::pubkey(63), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 61);
    let coder = world.node(CODER, 62);
    let exec = world.node(EXEC, 63);
    let space = space_of(
        &call(
            &alice,
            Command::SpaceCreate {
                home: world.home_ref.clone(),
                name: "team".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        )
        .await,
    );
    confirm_operator(&world, &alice, ALICE, CODER).await;
    confirm_operator(&world, &alice, ALICE, EXEC).await;
    call(
        &alice,
        Command::PolicySet {
            space: space.clone(),
            policy: policy(),
        },
    )
    .await;
    for who in [CODER, EXEC] {
        call(
            &alice,
            Command::MemberSet {
                space: space.clone(),
                actor: who.into(),
                role: "member".into(),
            },
        )
        .await;
    }
    for node in [&coder, &exec] {
        call(
            node,
            Command::SpaceFollow {
                home: world.home_ref.clone(),
                space: space.clone(),
            },
        )
        .await;
    }
    Rig {
        _world: world,
        alice,
        coder,
        exec,
        space,
    }
}

impl Rig {
    async fn waits(&self, proposal: &str) {
        let p = proposal.to_owned();
        until("the executor sees the proposal", || {
            effects(&self.exec).iter().any(|e| e.proposal == p)
        })
        .await;
        // the nodes that act on it next (the owner approves, the coder proposes on) have it, too
        common::effect_known(&self.alice, &self.space, proposal).await;
        common::effect_known(&self.coder, &self.space, proposal).await;
    }

    /// A land the coder proposes, the owner approves and the executor runs to `succeeded`; returns its id.
    async fn landed(&self, path: &str, delta: &str) -> String {
        let id = id_of(
            &call(
                &self.coder,
                Command::EffectPropose {
                    space: self.space.clone(),
                    executor: EXEC.into(),
                    plan: land_plan(path, delta, None),
                    task: None,
                    cost: None,
                    repeatable: false,
                    expires_at: None,
                },
            )
            .await,
        );
        self.waits(&id).await;
        call(
            &self.alice,
            Command::EffectApprove {
                space: self.space.clone(),
                proposal: id.clone(),
                expires_at: None,
                note: None,
                evidence: None,
            },
        )
        .await;
        let p = id.clone();
        until("the executor sees the approval", || {
            effects(&self.exec)
                .iter()
                .any(|e| e.proposal == p && !e.approvals.is_empty())
        })
        .await;
        let claim = id_of(
            &call(
                &self.exec,
                Command::EffectClaim {
                    space: self.space.clone(),
                    proposal: id.clone(),
                    attempt: 1,
                    window: 30,
                },
            )
            .await,
        );
        call(
            &self.exec,
            Command::EffectOutcome {
                space: self.space.clone(),
                claim,
                result: "succeeded".into(),
                started: "yes".into(),
                cost: None,
                evidence: None,
            },
        )
        .await;
        let p = id.clone();
        until("the land succeeded", || {
            effects(&self.exec)
                .iter()
                .any(|e| e.proposal == p && e.phase == "done")
        })
        .await;
        id
    }

    /// A proposal `by` makes for the executor; waits until the executor sees it.
    async fn propose(&self, by: &dyn NodeApi, plan: String) -> String {
        let id = id_of(
            &call(
                by,
                Command::EffectPropose {
                    space: self.space.clone(),
                    executor: EXEC.into(),
                    plan,
                    task: None,
                    cost: None,
                    repeatable: false,
                    expires_at: None,
                },
            )
            .await,
        );
        self.waits(&id).await;
        id
    }

    async fn record(&self) -> EvidenceRef {
        let CallResult::BlobRef {
            hash,
            size,
            media_type,
        } = call(
            &self.exec,
            Command::BlobPut {
                space: self.space.clone(),
                media_type: "application/json".into(),
                data: han_core::base64url::encode(br#"{"checks":[]}"#),
            },
        )
        .await
        else {
            panic!("a blob reference")
        };
        EvidenceRef {
            hash,
            size,
            media_type,
        }
    }

    async fn approve(
        &self,
        proposal: &str,
        kind: &str,
        over: &str,
    ) -> Result<CallResult, NodeError> {
        self.exec
            .call(Command::RevertApprove {
                space: self.space.clone(),
                proposal: proposal.to_owned(),
                note: Some("the post-merge run failed".into()),
                evidence: ApprovalEvidence {
                    kind: kind.into(),
                    over: over.into(),
                    record: self.record().await,
                },
            })
            .await
    }

    fn approvals(&self, proposal: &str) -> Vec<String> {
        effects(&self.exec)
            .into_iter()
            .find(|e| e.proposal == proposal)
            .map(|e| e.approvals)
            .unwrap_or_default()
    }
}

fn denied(r: Result<CallResult, NodeError>, needle: &str) {
    match r {
        Err(NodeError::Denied { message }) => {
            assert!(message.contains(needle), "{message:?} lacks {needle:?}");
        }
        other => panic!("expected a refusal naming {needle:?}, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lane_approves_its_own_tier_one_revert_with_no_person() {
    let r = rig().await;
    let land = r.landed("src/a.rs", &digest(1)).await;
    let revert = r
        .propose(&r.exec, land_plan("src/a.rs", &digest(2), Some(&land)))
        .await;
    assert_eq!(
        Command::RevertApprove {
            space: r.space.clone(),
            proposal: revert.clone(),
            note: None,
            evidence: ApprovalEvidence {
                kind: "han.ci/1".into(),
                over: digest(1),
                record: r.record().await,
            },
        }
        .class(),
        CommandClass::Executor
    );
    let out = r.approve(&revert, "han.ci/1", &digest(1)).await;
    assert!(matches!(out, Ok(CallResult::Receipt { .. })), "{out:?}");
    until("the approval reaches the executor's view", || {
        r.approvals(&revert) == vec![EXEC.to_owned()]
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn it_refuses_what_is_not_the_lanes_own_revert_on_the_right_evidence() {
    let r = rig().await;
    let land = r.landed("src/a.rs", &digest(1)).await;

    // another actor's proposal, even a revert
    let theirs = r
        .propose(&r.coder, land_plan("src/a.rs", &digest(2), Some(&land)))
        .await;
    denied(
        r.approve(&theirs, "han.ci/1", &digest(1)).await,
        "its own actor",
    );

    // the lane's own proposal, but no revertOf
    let plain = r
        .propose(&r.exec, land_plan("src/b.rs", &digest(3), None))
        .await;
    denied(
        r.approve(&plain, "han.ci/1", &digest(1)).await,
        "no revertOf",
    );

    // a revert of the lane, with the wrong evidence
    let revert = r
        .propose(&r.exec, land_plan("src/a.rs", &digest(4), Some(&land)))
        .await;
    denied(
        r.approve(&revert, "han.review/1", &digest(1)).await,
        "han.ci/1",
    );
    denied(
        r.approve(&revert, "han.ci/1", &digest(4)).await,
        "not over the delta",
    );
    // a revert of something that is not a land of this space never gets to be a proposal (Policy: invalid)
    let bogus = r.exec.call(Command::EffectPropose {
        space: r.space.clone(),
        executor: EXEC.into(),
        plan: land_plan("src/a.rs", &digest(5), Some(&digest(9))),
        task: None,
        cost: None,
        repeatable: false,
        expires_at: None,
    });
    assert!(matches!(bogus.await, Err(NodeError::Invalid { .. })));

    // nothing was signed
    for p in [&theirs, &plain, &revert] {
        assert!(r.approvals(p).is_empty(), "{p}");
    }
    // and the right call still works afterwards
    assert!(r.approve(&revert, "han.ci/1", &digest(1)).await.is_ok());
    until("approved", || r.approvals(&revert) == vec![EXEC.to_owned()]).await;
    // a proposal that no longer waits (it is claimed) is refused
    call(
        &r.exec,
        Command::EffectClaim {
            space: r.space.clone(),
            proposal: revert.clone(),
            attempt: 1,
            window: 30,
        },
    )
    .await;
    let p = revert.clone();
    until("claimed", || {
        effects(&r.exec)
            .iter()
            .any(|e| e.proposal == p && e.phase == "claimed")
    })
    .await;
    let again = r.approve(&revert, "han.ci/1", &digest(1)).await;
    assert!(
        matches!(again, Err(NodeError::Conflict { .. })),
        "{again:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_an_executor_connection_may_send_it() {
    let r = rig().await;
    let land = r.landed("src/a.rs", &digest(1)).await;
    let revert = r
        .propose(&r.exec, land_plan("src/a.rs", &digest(2), Some(&land)))
        .await;
    let cmd = Command::RevertApprove {
        space: r.space.clone(),
        proposal: revert.clone(),
        note: None,
        evidence: ApprovalEvidence {
            kind: "han.ci/1".into(),
            over: digest(1),
            record: r.record().await,
        },
    };
    for class in [ConnectionClass::Face, ConnectionClass::Host] {
        let api = ClassApi::new(Arc::new(r.exec.clone()), class);
        denied(api.call(cmd.clone()).await, "may not send");
    }
    assert!(r.approvals(&revert).is_empty());
    // an executor connection may send it, and still may not send EffectApprove
    let api = ClassApi::new(Arc::new(r.exec.clone()), ConnectionClass::Executor);
    denied(
        api.call(Command::EffectApprove {
            space: r.space.clone(),
            proposal: revert.clone(),
            expires_at: None,
            note: None,
            evidence: None,
        })
        .await,
        "may not send",
    );
    assert!(api.call(cmd).await.is_ok());
    until("approved", || r.approvals(&revert) == vec![EXEC.to_owned()]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn policy_still_keeps_a_tier_two_revert_with_a_person() {
    let r = rig().await;
    // a never-auto path is at least tier 2, and so is its revert
    let land = r.landed(".github/workflows/ci.yml", &digest(1)).await;
    let revert = r
        .propose(
            &r.exec,
            land_plan(".github/workflows/ci.yml", &digest(2), Some(&land)),
        )
        .await;
    let out = r.approve(&revert, "han.ci/1", &digest(1)).await;
    assert!(matches!(out, Err(NodeError::Denied { .. })), "{out:?}");
    assert!(r.approvals(&revert).is_empty());
    // a person's approval is the way
    call(
        &r.alice,
        Command::EffectApprove {
            space: r.space.clone(),
            proposal: revert.clone(),
            expires_at: None,
            note: None,
            evidence: None,
        },
    )
    .await;
    until("the person approved", || {
        r.approvals(&revert) == vec![ALICE.to_owned()]
    })
    .await;
}
