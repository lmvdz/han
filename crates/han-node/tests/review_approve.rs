//! The machine reviewer's approval (P-17, PLAN §12 "Machine review"): `Command::ReviewApprove` against a real
//! home. The node signs as its own actor with no prompt only for an open tier 0-1 `vcs.land` another actor
//! proposed, with `han.review/1` evidence over the landing's delta digest; Policy still decides, and no other
//! connection may send it.

mod common;

use std::sync::Arc;

use common::{World, call, confirm_operator, id_of, space_of, until};
use han_node::ClassApi;
use han_node::api::{
    ApprovalEvidence, CallResult, Command, ConnectionClass, EffectRow, EvidenceRef, NodeApi,
    NodeError, Query, Row, Window,
};

const ALICE: &str = "@alice:a.localhost";
const CODER: &str = "@coder:a.localhost";
const EXEC: &str = "@exec:a.localhost";
const REVIEWER: &str = "@reviewer:a.localhost";

fn effects(api: &dyn NodeApi) -> Vec<EffectRow> {
    let mut v = api.view(Query::Effects {
        space: None,
        executor: None,
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

/// The reviewer's rule for tier 0-1 (evidence required), then humans for the rest.
fn policy() -> String {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        m.insert(
            "approvals".into(),
            han_core::json::parse(
                format!(
                    r#"[{{"action":"vcs.land","maxTier":1,"approvers":["{REVIEWER}","owner"],"evidence":"han.review/1"}},
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

fn land_plan(path: &str, delta: &str, lines: u64) -> String {
    serde_json::json!({
        "action": "vcs.land", "target": "github:o/r",
        "args": {"ref": "refs/heads/main", "change": {
            "repo": "github:o/r", "deltaDigest": delta, "total": 1,
            "files": [{"path": path, "kind": "modify", "lines": lines}],
        }},
    })
    .to_string()
}

struct Rig {
    _world: World,
    alice: han_node::Node<han_net::MemTransport>,
    coder: han_node::Node<han_net::MemTransport>,
    reviewer: han_node::Node<han_net::MemTransport>,
    space: String,
}

async fn rig() -> Rig {
    let world = World::new(&[(ALICE, 71)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(72), Some(ALICE))
        .unwrap();
    world
        .home
        .register(REVIEWER, "service", "d1", &common::pubkey(73), Some(ALICE))
        .unwrap();
    world
        .home
        .register(EXEC, "service", "d1", &common::pubkey(74), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 71);
    let coder = world.node(CODER, 72);
    let reviewer = world.node(REVIEWER, 73);
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
    confirm_operator(&world, &alice, ALICE, REVIEWER).await;
    call(
        &alice,
        Command::PolicySet {
            space: space.clone(),
            policy: policy(),
        },
    )
    .await;
    for who in [CODER, REVIEWER] {
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
    for node in [&coder, &reviewer] {
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
        reviewer,
        space,
    }
}

impl Rig {
    async fn propose_action(&self, by: &dyn NodeApi, plan: String) -> String {
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
        let p = id.clone();
        until("the reviewer sees the proposal", || {
            effects(&self.reviewer).iter().any(|e| e.proposal == p)
        })
        .await;
        // the nodes that act on it next (the owner approves, the coder is the stranger) have it, too
        common::effect_known(&self.alice, &self.space, &id).await;
        common::effect_known(&self.coder, &self.space, &id).await;
        id
    }

    async fn record(&self) -> EvidenceRef {
        let CallResult::BlobRef {
            hash,
            size,
            media_type,
        } = call(
            &self.reviewer,
            Command::BlobPut {
                space: self.space.clone(),
                media_type: "application/vnd.han.review+json".into(),
                data: han_core::base64url::encode(br#"{"schema":"han.review/1"}"#),
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
        api: &dyn NodeApi,
        proposal: &str,
        kind: &str,
        over: &str,
    ) -> Result<CallResult, NodeError> {
        api.call(Command::ReviewApprove {
            space: self.space.clone(),
            proposal: proposal.to_owned(),
            note: None,
            evidence: ApprovalEvidence {
                kind: kind.into(),
                over: over.into(),
                record: self.record().await,
            },
        })
        .await
    }

    fn row(&self, proposal: &str) -> EffectRow {
        effects(&self.reviewer)
            .into_iter()
            .find(|e| e.proposal == proposal)
            .unwrap()
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

fn exec_api(r: &Rig) -> ClassApi {
    ClassApi::new(Arc::new(r.reviewer.clone()), ConnectionClass::Executor)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_row_carries_the_tier_and_the_delta_digest() {
    let r = rig().await;
    let small = r
        .propose_action(&r.coder, land_plan("src/a.rs", &digest(1), 3))
        .await;
    let big = r
        .propose_action(&r.coder, land_plan("src/b.rs", &digest(2), 900))
        .await;
    let sensitive = r
        .propose_action(
            &r.coder,
            land_plan(".github/workflows/ci.yml", &digest(3), 1),
        )
        .await;
    // 3 lines of code is tier 1 (code floors at 1; only docs and tests are tier 0)
    assert_eq!(r.row(&small).gate_tier, Some(1));
    assert_eq!(r.row(&small).delta_digest, Some(digest(1)));
    assert_eq!(r.row(&big).gate_tier, Some(2));
    assert_eq!(r.row(&sensitive).gate_tier, Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reviewer_approves_a_tier_one_land_with_no_person() {
    let r = rig().await;
    let id = r
        .propose_action(&r.coder, land_plan("src/a.rs", &digest(1), 30))
        .await;
    assert_eq!(r.row(&id).gate_tier, Some(1));
    let api = exec_api(&r);
    let out = r.approve(&api, &id, "han.review/1", &digest(1)).await;
    assert!(matches!(out, Ok(CallResult::Receipt { .. })), "{out:?}");
    until("the approval reaches the view", || {
        r.row(&id).approvals == vec![REVIEWER.to_owned()]
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn it_refuses_what_is_not_a_tier_one_land_on_the_right_evidence() {
    let r = rig().await;
    let api = exec_api(&r);
    let ok = r
        .propose_action(&r.coder, land_plan("src/a.rs", &digest(1), 30))
        .await;
    denied(
        r.approve(&api, &ok, "han.ci/1", &digest(1)).await,
        "han.review/1",
    );
    denied(
        r.approve(&api, &ok, "han.review/1", &digest(9)).await,
        "not over the delta",
    );
    // tier 2 is a person's
    let big = r
        .propose_action(&r.coder, land_plan("src/b.rs", &digest(2), 900))
        .await;
    denied(
        r.approve(&api, &big, "han.review/1", &digest(2)).await,
        "tier 1 at most",
    );
    // its own actor's landing
    let own = r
        .propose_action(&r.reviewer, land_plan("src/c.rs", &digest(3), 30))
        .await;
    denied(
        r.approve(&api, &own, "han.review/1", &digest(3)).await,
        "its own actor",
    );
    // not a vcs.land
    let other = r
        .propose_action(
            &r.coder,
            serde_json::json!({"action": "open_pr", "target": "github:o/r"}).to_string(),
        )
        .await;
    denied(
        r.approve(&api, &other, "han.review/1", &digest(1)).await,
        "only a vcs.land",
    );
    for p in [&ok, &big, &own, &other] {
        assert!(r.row(p).approvals.is_empty(), "{p}");
    }
    // the right call still works afterwards
    assert!(
        r.approve(&api, &ok, "han.review/1", &digest(1))
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_an_executor_connection_may_send_it_and_policy_still_decides() {
    let r = rig().await;
    let id = r
        .propose_action(&r.coder, land_plan("src/a.rs", &digest(1), 30))
        .await;
    for class in [ConnectionClass::Face, ConnectionClass::Host] {
        let api = ClassApi::new(Arc::new(r.reviewer.clone()), class);
        denied(
            r.approve(&api, &id, "han.review/1", &digest(1)).await,
            "may not send",
        );
    }
    assert!(r.row(&id).approvals.is_empty());
    // an executor connection of an actor the rule does not list: the node signs, Policy refuses
    let stranger = ClassApi::new(Arc::new(r.coder.clone()), ConnectionClass::Executor);
    let other = r
        .propose_action(&r.reviewer, land_plan("src/z.rs", &digest(5), 30))
        .await;
    let out = r
        .approve(&stranger, &other, "han.review/1", &digest(5))
        .await;
    assert!(matches!(out, Err(NodeError::Denied { .. })), "{out:?}");
    assert!(r.row(&other).approvals.is_empty());
    // a person's approval is the way for what the rule does not cover
    call(
        &r.alice,
        Command::EffectApprove {
            space: r.space.clone(),
            proposal: other.clone(),
            expires_at: None,
            note: None,
            evidence: None,
        },
    )
    .await;
}
