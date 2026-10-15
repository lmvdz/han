//! Effects through the node API (M4): an agent proposes, a person approves, an executor claims once and reports;
//! the `Effects` view carries the plan an executor checks, and the `Guard` view what a host's R-24 reads.

mod common;

use common::{World, call, confirm_operator, id_of, space_of, until};
use han_node::api::{CallResult, Command, CommandClass, NodeApi, NodeError, Query, Row, Window};

const ALICE: &str = "@alice:a.localhost";
const CODER: &str = "@coder:a.localhost";
const EXEC: &str = "@exec:a.localhost";

fn effects(api: &dyn NodeApi, executor: &str, include_done: bool) -> Vec<han_node::api::EffectRow> {
    let mut v = api.view(Query::Effects {
        space: None,
        executor: Some(executor.into()),
        include_done,
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

/// The default policy with every action needing an owner's approval.
fn gated_policy() -> String {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        m.insert(
            "approvals".into(),
            han_core::json::parse(br#"[{"action":"*","approvers":["owner"]}]"#).unwrap(),
        );
    }
    String::from_utf8(han_core::json::canonicalize(&p).unwrap()).unwrap()
}

fn guarded(api: &dyn NodeApi, space: &str, actor: &str) -> bool {
    let mut v = api.view(Query::Guard {
        space: space.into(),
        actor: actor.into(),
        window: Window::default(),
    });
    match v.try_next() {
        Some(han_node::api::ViewEvent::Snapshot { rows, .. }) => rows.iter().any(|r| match r {
            Row::Guard(g) => g.guarded,
            _ => false,
        }),
        other => panic!("no guard snapshot: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_effect_is_proposed_approved_claimed_once_and_reported() {
    let world = World::new(&[(ALICE, 41)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(42), Some(ALICE))
        .unwrap();
    world
        .home
        .register(EXEC, "service", "d1", &common::pubkey(43), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 41);
    let coder = world.node(CODER, 42);
    let exec = world.node(EXEC, 43);
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
            policy: gated_policy(),
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
    let plan =
        r#"{"action":"git_push","target":"github:o/r","args":{"branch":"han/1-coder","to":"abc"}}"#;
    let propose = Command::EffectPropose {
        space: space.clone(),
        executor: EXEC.into(),
        plan: plan.into(),
        task: None,
        cost: None,
        repeatable: false,
        expires_at: None,
    };
    assert_eq!(propose.class(), CommandClass::Agent);
    let proposal = id_of(&call(&coder, propose).await);

    until("the executor sees the proposal with its plan", || {
        effects(&exec, EXEC, false)
            .iter()
            .any(|e| e.proposal == proposal && e.plan.is_some())
    })
    .await;
    let row = effects(&exec, EXEC, false)
        .into_iter()
        .find(|e| e.proposal == proposal)
        .unwrap();
    let parsed = han_core::json::parse(row.plan.as_deref().unwrap().as_bytes()).unwrap();
    assert_eq!(
        han_core::ids::json_digest(&parsed).unwrap(),
        row.plan_digest
    );
    assert_eq!(
        (row.phase.as_str(), row.claims, row.launch_window),
        ("open", 0, 30)
    );

    call(
        &alice,
        Command::EffectApprove {
            space: space.clone(),
            proposal: proposal.clone(),
            expires_at: None,
            note: None,
        },
    )
    .await;
    until("the executor sees the approval", || {
        effects(&exec, EXEC, false)
            .iter()
            .any(|e| e.proposal == proposal && e.approvals == vec![ALICE.to_owned()])
    })
    .await;

    // The row says which entry gave the approvals their latest member, and when: a landing lane orders its
    // changes by it and dates its commits with it.
    let approved = effects(&exec, EXEC, false)
        .into_iter()
        .find(|e| e.proposal == proposal)
        .unwrap();
    assert!(approved.approved_seq.is_some_and(|n| n > row.seq));
    assert!(
        approved
            .approved_at
            .as_deref()
            .is_some_and(han_core::grammar::is_time)
    );
    assert_eq!((row.approved_seq, row.approved_at), (None, None));

    // A claim that did not start returns the approvals it consumed (P-09), with their order and time, so the
    // executor can claim again.
    let first = id_of(
        &call(
            &exec,
            Command::EffectClaim {
                space: space.clone(),
                proposal: proposal.clone(),
                attempt: 1,
                window: 30,
            },
        )
        .await,
    );
    until("the claim consumed the approval", || {
        effects(&exec, EXEC, false)
            .iter()
            .any(|e| e.proposal == proposal && e.phase == "claimed" && e.approvals.is_empty())
    })
    .await;
    call(
        &exec,
        Command::EffectOutcome {
            space: space.clone(),
            claim: first,
            result: "failed".into(),
            started: "no".into(),
            cost: None,
        },
    )
    .await;
    until("the approval returned with the effect", || {
        effects(&exec, EXEC, false).iter().any(|e| {
            e.proposal == proposal
                && e.phase == "open"
                && e.approvals == vec![ALICE.to_owned()]
                && e.approved_seq == approved.approved_seq
        })
    })
    .await;

    let claim = Command::EffectClaim {
        space: space.clone(),
        proposal: proposal.clone(),
        attempt: 2,
        window: 30,
    };
    assert_eq!(claim.class(), CommandClass::Executor);
    let claim_id = id_of(&call(&exec, claim.clone()).await);
    // A second claim of the same attempt is refused by Policy (the phase is claimed).
    let again = exec.call(claim).await.unwrap_err();
    assert!(
        matches!(again, NodeError::Denied { .. } | NodeError::Invalid { .. }),
        "{again:?}"
    );
    let out = call(
        &exec,
        Command::EffectOutcome {
            space: space.clone(),
            claim: claim_id.clone(),
            result: "succeeded".into(),
            started: "yes".into(),
            cost: None,
        },
    )
    .await;
    assert!(matches!(out, CallResult::Receipt { .. }));
    until("the effect is done", || {
        effects(&exec, EXEC, true)
            .iter()
            .any(|e| e.proposal == proposal && e.phase == "done" && e.claims == 2)
    })
    .await;
    // The claim consumed the approvals, and with them their order.
    assert!(
        effects(&exec, EXEC, true)
            .iter()
            .all(|e| e.approved_seq.is_none() && e.approved_at.is_none())
    );
    assert!(effects(&exec, EXEC, false).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_that_opens_a_guarded_session_is_guarded() {
    let world = World::new(&[(ALICE, 51)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(52), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 51);
    let coder = world.node(CODER, 52);
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
    call(
        &alice,
        Command::MemberSet {
            space: space.clone(),
            actor: CODER.into(),
            role: "member".into(),
        },
    )
    .await;
    call(
        &coder,
        Command::SpaceFollow {
            home: world.home_ref.clone(),
            space: space.clone(),
        },
    )
    .await;
    assert!(!guarded(&coder, &space, CODER));
    call(
        &coder,
        Command::SessionOpen {
            space: space.clone(),
            steer_mode: Some("guarded".into()),
            task: None,
            title: None,
        },
    )
    .await;
    until("alice sees the agent guarded", || {
        guarded(&alice, &space, CODER)
    })
    .await;
    assert!(guarded(&coder, &space, CODER));
    assert!(!guarded(&alice, &space, ALICE));
    let bad = coder
        .call(Command::SessionOpen {
            space: space.clone(),
            steer_mode: Some("loose".into()),
            task: None,
            title: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(bad, NodeError::Invalid { .. }));
}
