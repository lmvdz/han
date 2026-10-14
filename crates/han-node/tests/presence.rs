//! The human-presence check before an approval is signed (SR-106), on a real node with an in-process team and
//! the local JSON-RPC server. The presence backend is a test double injected through the node's config; the
//! real Windows Hello backend has a manual check (`crates/han-presence/examples/hello.rs`).

mod common;

use std::sync::Arc;

use common::{World, call, confirm_operator, id_of, space_of, until};
use han_node::api::{
    APPROVAL_NOT_SIGNED, Command, CommandClass, ConnectionClass, NodeApi, NodeError, Query, Row,
    ViewEvent, Window,
};
use han_node::rpc::{RpcClient, default_endpoint, serve_as};
use han_node::{Node, presence};
use han_presence::PresenceError;
use han_presence::testing::Scripted;

const ALICE: &str = "@alice:a.localhost";
const CODER: &str = "@coder:a.localhost";
const EXEC: &str = "@exec:a.localhost";

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

type N = Node<han_net::MemTransport>;

/// Alice's node, a proposal waiting in a gated space, and the agent's node that proposed it.
struct Stage {
    world: World,
    alice: N,
    space: String,
    proposal: String,
}

async fn stage(tweak: impl FnOnce(&mut han_node::NodeConfig)) -> Stage {
    let world = World::new(&[(ALICE, 41)]);
    for (who, n) in [(CODER, 42), (EXEC, 43)] {
        world
            .home
            .register(who, "agent", "d1", &common::pubkey(n), Some(ALICE))
            .unwrap();
    }
    let alice = world.node_with(ALICE, 41, tweak);
    let coder = world.node(CODER, 42);
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
        Command::PolicySet {
            space: space.clone(),
            policy: gated_policy(),
        },
    )
    .await;
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
    let plan = r#"{"action":"git_push","target":"github:o/r","args":{"to":"abc"}}"#;
    let proposal = id_of(
        &call(
            &coder,
            Command::EffectPropose {
                space: space.clone(),
                executor: EXEC.into(),
                plan: plan.into(),
                task: None,
                cost: None,
                repeatable: false,
                expires_at: None,
            },
        )
        .await,
    );
    let s = Stage {
        world,
        alice,
        space,
        proposal,
    };
    until("alice sees the proposal", || {
        !effects(&s.alice, &s.space).is_empty()
    })
    .await;
    s
}

fn effects(api: &dyn NodeApi, space: &str) -> Vec<han_node::api::EffectRow> {
    let mut v = api.view(Query::Effects {
        space: Some(space.into()),
        executor: None,
        include_done: true,
        window: Window::default(),
    });
    let mut out = Vec::new();
    while let Some(ev) = v.try_next() {
        if let ViewEvent::Snapshot { rows, .. } = ev {
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

fn approve(s: &Stage) -> Command {
    Command::EffectApprove {
        space: s.space.clone(),
        proposal: s.proposal.clone(),
        expires_at: None,
        note: None,
    }
}

fn approvals(s: &Stage) -> usize {
    effects(&s.alice, &s.space)
        .iter()
        .map(|e| e.approvals.len())
        .sum()
}

fn denied_with(e: &NodeError, what: &str) {
    match e {
        NodeError::Denied { message } => {
            assert!(message.starts_with(APPROVAL_NOT_SIGNED), "{message}");
            assert!(message.contains(what), "{message}");
        }
        other => panic!("not denied: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_is_signed_only_after_a_person_confirms_the_plan() {
    let script = Scripted::new();
    let s = stage(|c| c.set_presence(script.clone())).await;

    // nobody answers: nothing is signed
    let e = s.alice.call(approve(&s)).await.unwrap_err();
    denied_with(&e, "presence check");
    // the person cancels the prompt: nothing is signed, and the error says so
    script.fail(PresenceError::Cancelled);
    let e = s.alice.call(approve(&s)).await.unwrap_err();
    denied_with(&e, "presence check cancelled");
    script.fail(PresenceError::TimedOut);
    let e = s.alice.call(approve(&s)).await.unwrap_err();
    denied_with(&e, "timed out");
    assert_eq!(approvals(&s), 0, "nothing was signed");

    // the prompt named the action and the short digest the sign panel shows
    let row = effects(&s.alice, &s.space).remove(0);
    let seen = script.prompts();
    let p = seen.last().unwrap();
    assert_eq!(p.action, "git_push");
    assert_eq!(p.target, "github:o/r");
    assert_eq!(p.plan_digest_short, row.plan_digest[..12]);
    assert_eq!(p.requester, CODER);

    // a proof for another plan is refused
    script.confirm_other_digest();
    let e = s.alice.call(approve(&s)).await.unwrap_err();
    denied_with(&e, "another plan");
    assert_eq!(approvals(&s), 0);

    // the person confirms: signed
    script.allow();
    call(&s.alice, approve(&s)).await;
    until("the approval lands", || approvals(&s) == 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proposal_cancelled_while_the_person_confirms_is_not_signed() {
    let script = Scripted::new();
    let s = stage(|c| c.set_presence(script.clone())).await;
    let (alice, space, proposal) = (s.alice.clone(), s.space.clone(), s.proposal.clone());
    let handle = tokio::runtime::Handle::current();
    script.allow().during(move || {
        // the prompt is open, and the proposal is cancelled under the person's hand
        handle
            .block_on(alice.call(Command::EffectCancel {
                space: space.clone(),
                proposal: proposal.clone(),
            }))
            .unwrap();
    });
    let e = s.alice.call(approve(&s)).await.unwrap_err();
    assert!(matches!(e, NodeError::Conflict { .. }), "{e:?}");
    assert_eq!(approvals(&s), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_without_a_presence_check_cannot_approve() {
    // The backend is the device's own setting in node.db; there is none on this device.
    let dir = std::env::temp_dir().join(format!("han-presence-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = han_node::db::NodeDb::open(&dir.join("node.db")).unwrap();
    db.set_config(presence::BACKEND_KEY, "none").unwrap();
    drop(db);
    let d = dir.clone();
    let s = stage(move |c| {
        c.data_dir = Some(d);
        c.presence = None;
    })
    .await;
    let e = s.alice.call(approve(&s)).await.unwrap_err();
    denied_with(&e, "this device has no presence check");
    assert_eq!(approvals(&s), 0);
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_the_pipe_a_face_gets_the_prompt_and_a_host_connection_cannot_approve() {
    let script = Scripted::new();
    let s = stage(|c| c.set_presence(script.clone())).await;
    let dir = s.world.dir.join("presence-rpc");
    std::fs::create_dir_all(&dir).unwrap();
    let faces = default_endpoint(&dir);
    let hosts = format!("{faces}-host");
    let api: Arc<dyn NodeApi> = Arc::new(s.alice.clone());
    let _faces = serve_as(Arc::clone(&api), &faces, ConnectionClass::Face).unwrap();
    let _hosts = serve_as(Arc::clone(&api), &hosts, ConnectionClass::Host).unwrap();
    let _execs = serve_as(
        Arc::clone(&api),
        &format!("{faces}-exec"),
        ConnectionClass::Executor,
    )
    .unwrap();

    // a host (and an executor) connection is refused before the node sees the command, even though a person
    // would confirm: no prompt is shown
    for endpoint in [hosts, format!("{faces}-exec")] {
        let conn = RpcClient::connect(&endpoint).await.unwrap();
        let e = conn.call(approve(&s)).await.unwrap_err();
        assert!(matches!(e, NodeError::Denied { .. }), "{e:?}");
        assert!(e.to_string().contains("may not send"), "{e}");
    }
    assert!(script.prompts().is_empty(), "the node never asked a person");
    assert_eq!(approvals(&s), 0);
    // (host and executor commands still pass their own classes)
    assert!(ConnectionClass::Host.permits(CommandClass::Agent));
    assert!(!ConnectionClass::Host.permits(CommandClass::Executor));
    assert!(ConnectionClass::Executor.permits(CommandClass::Executor));
    assert!(!ConnectionClass::Executor.permits(CommandClass::Face));

    // the face sends the same command: the node prompts, and a cancel comes back typed over the wire
    let face = RpcClient::connect(&faces).await.unwrap();
    script.fail(PresenceError::Cancelled);
    let e = face.call(approve(&s)).await.unwrap_err();
    denied_with(&e, "presence check cancelled");
    assert_eq!(approvals(&s), 0);
    script.allow();
    face.call(approve(&s)).await.expect("confirmed");
    until("the approval lands", || approvals(&s) == 1).await;
}
