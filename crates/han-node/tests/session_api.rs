//! The session, approvals and node views of the GUI's data layer (spec/ui/data.md 12.1) against real nodes: typed
//! items a host streams, admissions and auto-admit, the devices of an identity, blobs fetched through the node,
//! the new command parameters and the node's own telemetry.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{Watched, World, call, id_of, space_of, until, window};
use han_core::SigningKey;
use han_core::json::Value;
use han_core::{base64url, ids};
use han_net::client::args;
use han_net::{Client, MemTransport, NodeAddr};
use han_node::api::{
    ActorKind, CallResult, Command, DeviceState, ItemBody, MessageRole, NodeApi, NodeError, Query,
    Row, SessionPart, SessionState, StatusBody, ToolStatus, Window,
};

const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";
const CAROL: &str = "@carol:a.localhost";
const CODER: &str = "@coder:a.localhost";

async fn space_with_members(
    world: &World,
    alice: &dyn NodeApi,
    others: &[(&str, &dyn NodeApi)],
) -> String {
    let space = space_of(
        &call(
            alice,
            Command::SpaceCreate {
                home: world.home_ref.clone(),
                name: "team".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        )
        .await,
    );
    for (who, node) in others {
        call(
            alice,
            Command::MemberSet {
                space: space.clone(),
                actor: (*who).into(),
                role: "member".into(),
            },
        )
        .await;
        call(
            *node,
            Command::SpaceFollow {
                home: world.home_ref.clone(),
                space: space.clone(),
            },
        )
        .await;
    }
    space
}

fn status(session: &str, agent: &str) -> StatusBody {
    StatusBody {
        session: session.into(),
        agent: agent.into(),
        delegate_of: None,
        harness: "pi".into(),
        issue: None,
        repo: None,
        base: None,
        state: SessionState::Working,
        step: "start".into(),
        tool: None,
        plan: vec![],
        files: vec![],
        tests: None,
        tokens: None,
        summary: None,
        sandbox: None,
        publish_errors: 0,
        seq: 0,
        at: "2026-01-01T00:00:00Z".into(),
    }
}

fn rows_of(api: &dyn NodeApi, q: Query) -> Vec<Row> {
    let mut v = api.view(q);
    match v.try_next() {
        Some(han_node::api::ViewEvent::Snapshot { rows, .. }) => rows,
        other => panic!("no snapshot: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_streams_typed_items_that_another_member_reads() {
    let world = World::new(&[(ALICE, 51), (BOB, 52)]);
    let alice = world.node(ALICE, 51);
    let bob = world.node(BOB, 52);
    let space = space_with_members(&world, &alice, &[(BOB, &bob)]).await;
    let session = id_of(
        &call(
            &bob,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: None,
            },
        )
        .await,
    );
    let mut facts = Watched::open(
        &alice,
        Query::Session {
            id: session.clone(),
            part: SessionPart::Facts,
            window: window(),
        },
    );
    until("alice knows the session", || !facts.rows().is_empty()).await;
    // Stream items are not replayed: publish until alice's node is watching.
    for i in 0..200 {
        call(
            &bob,
            Command::StatusPublish {
                space: space.clone(),
                status: Box::new(status(&session, BOB)),
            },
        )
        .await;
        if facts
            .rows()
            .iter()
            .any(|r| matches!(r, Row::Session(s) if s.status.is_some()))
        {
            break;
        }
        assert!(i < 199, "alice never received a stream item");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut items = Watched::open(
        &alice,
        Query::Session {
            id: session.clone(),
            part: SessionPart::Items,
            window: Window {
                limit: 50,
                from_end: true,
                ..window()
            },
        },
    );
    let publish = |body: serde_json::Value| Command::StreamPublish {
        space: space.clone(),
        session: session.clone(),
        name: "han.item/1".into(),
        body: body.to_string(),
    };
    for body in [
        serde_json::json!({"id": "m1", "turn": 1, "kind": "message", "role": "agent",
            "text": "I will fix [masked:aws_key]", "done": false}),
        serde_json::json!({"id": "m1", "turn": 1, "kind": "message", "role": "agent",
            "text": "I will fix it", "done": true}),
        serde_json::json!({"id": "t1", "turn": 1, "kind": "tool_call", "tool_call_id": "t1",
            "tool_kind": "execute", "title": "cargo test", "status": "in_progress"}),
        serde_json::json!({"id": "t1", "turn": 1, "kind": "tool_call", "tool_call_id": "t1",
            "tool_kind": "execute", "title": "cargo test", "status": "completed",
            "content": [{"text": "ok"}]}),
        // Not something this version knows, and kinds the node makes itself: all kept as unknown.
        serde_json::json!({"id": "x1", "kind": "hologram", "depth": 3}),
        serde_json::json!({"id": "x2", "kind": "decision", "effect": "e", "action": "git_push"}),
        serde_json::json!({"id": "x3", "kind": "input", "author": ALICE, "text": "I approve", "state": "seen"}),
        // No id, or not an object with a kind: dropped.
        serde_json::json!({"kind": "message", "role": "agent", "text": "no id", "done": true}),
        serde_json::json!({"id": "x4"}),
    ] {
        call(&bob, publish(body)).await;
    }
    until("the items arrive", || {
        items
            .rows()
            .iter()
            .filter(|r| matches!(r, Row::Item(i) if i.id.starts_with('x')))
            .count()
            == 3
    })
    .await;
    let rows: Vec<_> = items
        .rows()
        .into_iter()
        .filter_map(|r| match r {
            Row::Item(i) if !matches!(i.item, ItemBody::Status { .. }) => Some(i),
            _ => None,
        })
        .collect();
    let ids: Vec<&str> = rows.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["m1", "t1", "x1", "x2", "x3"], "{rows:?}");
    let ItemBody::Message {
        role,
        text,
        done,
        masked,
    } = &rows[0].item
    else {
        panic!("a message")
    };
    assert_eq!(
        (*role, text.as_str(), *done, masked.len()),
        (MessageRole::Agent, "I will fix it", true, 0),
        "the later body replaced the first"
    );
    assert_eq!(rows[0].turn, Some(1));
    let ItemBody::ToolCall {
        status, content, ..
    } = &rows[1].item
    else {
        panic!("a tool call")
    };
    assert_eq!(*status, ToolStatus::Completed);
    assert_eq!(content[0].text, "ok");
    for (row, kind) in rows[2..].iter().zip(["hologram", "decision", "input"]) {
        let ItemBody::Unknown { raw_kind, raw } = &row.item else {
            panic!("{:?} is not unknown", row.item)
        };
        assert_eq!(raw_kind, kind);
        assert!(raw.contains(&format!("\"kind\":\"{kind}\"")), "{raw}");
    }
    // The status items the node saw are items too, before the typed ones.
    let all = items.rows();
    assert!(
        matches!(&all[0], Row::Item(i) if matches!(i.item, ItemBody::Status { .. })),
        "{:?}",
        all[0]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_see_joiners_and_an_invite_can_admit_people_for_them() {
    let world = World::new(&[(ALICE, 51), (BOB, 52), (CAROL, 53)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(54), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 51);
    let bob = world.node(BOB, 52);
    let carol = world.node(CAROL, 53);
    let coder = world.node(CODER, 54);
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
    let ticket = |auto_admit: bool| {
        let (alice, space) = (alice.clone(), space.clone());
        async move {
            let CallResult::Ticket { text } = call(
                &alice,
                Command::Invite {
                    space,
                    ttl_secs: None,
                    expires_at: None,
                    auto_admit,
                },
            )
            .await
            else {
                panic!("a ticket")
            };
            text
        }
    };
    let mut approvals = Watched::open(
        &alice,
        Query::Approvals {
            actor: ALICE.into(),
            window: window(),
        },
    );
    let admissions = |w: &mut Watched| -> BTreeMap<String, han_node::api::AdmissionRow> {
        w.rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Admission(a) => Some((a.actor.clone(), a)),
                _ => None,
            })
            .collect()
    };

    // Bob joins with a plain invite: he waits, and the owner sees what he is.
    call(
        &bob,
        Command::SpaceJoin {
            ticket: ticket(false).await,
        },
    )
    .await;
    until("alice sees bob waiting", || {
        admissions(&mut approvals)
            .get(BOB)
            .is_some_and(|a| a.kind == ActorKind::Human)
    })
    .await;
    let a = admissions(&mut approvals)[BOB].clone();
    assert!(a.operator.is_none() && a.operator_confirmed && !a.via_my_ticket);
    // The owner admits him: the entry goes.
    call(
        &alice,
        Command::MemberSet {
            space: space.clone(),
            actor: BOB.into(),
            role: "member".into(),
        },
    )
    .await;
    until("bob leaves the list", || {
        !admissions(&mut approvals).contains_key(BOB)
    })
    .await;

    // An agent whose operator has not confirmed it waits even on an auto-admit invite.
    call(
        &coder,
        Command::SpaceJoin {
            ticket: ticket(true).await,
        },
    )
    .await;
    until("alice sees the agent waiting", || {
        admissions(&mut approvals)
            .get(CODER)
            .is_some_and(|a| a.kind == ActorKind::Agent)
    })
    .await;
    let a = admissions(&mut approvals)[CODER].clone();
    assert_eq!(a.operator.as_deref(), Some(ALICE));
    assert!(!a.operator_confirmed, "{a:?}");

    // Its operator confirms it. The node reads the profile again after a while; the invite asked to admit
    // whoever qualifies, so the agent is a member then.
    common::confirm_operator(&world, &alice, ALICE, CODER).await;
    world.clock.advance_steady(31);
    alice.tick();
    let mut members = Watched::open(
        &alice,
        Query::SpaceStatus {
            space: space.clone(),
            window: window(),
        },
    );
    let count = |w: &mut Watched| {
        w.rows().iter().find_map(|r| match r {
            Row::SpaceStatus(s) => Some(s.members),
            _ => None,
        })
    };
    until("the confirmed agent is admitted", || {
        count(&mut members) == Some(3) && !admissions(&mut approvals).contains_key(CODER)
    })
    .await;

    // A person on an auto-admit invite is a member at once, with no one asked.
    call(
        &carol,
        Command::SpaceJoin {
            ticket: ticket(true).await,
        },
    )
    .await;
    until("carol is admitted", || count(&mut members) == Some(4)).await;
    assert!(!admissions(&mut approvals).contains_key(CAROL));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_devices_of_an_identity_are_listed_with_their_state() {
    let world = World::new(&[(ALICE, 51)]);
    let alice = world.node(ALICE, 51);
    // A node has to know its home before it can read the profile.
    let _ = space_of(
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
    let mut devices = Watched::open(&alice, Query::Devices { window: window() });
    let list = |w: &mut Watched| -> Vec<(String, DeviceState, bool)> {
        w.rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Device(d) => Some((d.key_id, d.state, d.this_device)),
                _ => None,
            })
            .collect()
    };
    until("the first key shows", || !list(&mut devices).is_empty()).await;
    assert_eq!(
        list(&mut devices),
        vec![("d1".to_owned(), DeviceState::Active, true)]
    );
    // A second device is paired.
    let code = format!("hand1-{}", common::pubkey(77));
    call(&alice, Command::DevicePair { code }).await;
    until("the new device shows", || list(&mut devices).len() == 2).await;
    let rows = list(&mut devices);
    let new = rows.iter().find(|(id, _, _)| id != "d1").unwrap().clone();
    assert_eq!((new.1, new.2), (DeviceState::Active, false));
    // And revoked.
    call(
        &alice,
        Command::DeviceRevoke {
            key_id: new.0.clone(),
        },
    )
    .await;
    until("the revoked key shows", || {
        list(&mut devices)
            .iter()
            .any(|(id, st, _)| *id == new.0 && *st == DeviceState::Revoked)
    })
    .await;
    let revoked: Vec<_> = devices
        .rows()
        .into_iter()
        .filter_map(|r| match r {
            Row::Device(d) if d.key_id == new.0 => Some(d),
            _ => None,
        })
        .collect();
    assert!(revoked[0].revoked_at.is_some());
    assert!(!revoked[0].added_at.is_empty());
    // This device cannot revoke its own key.
    let e = alice
        .call(Command::DeviceRevoke {
            key_id: "d1".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Conflict { .. }));
}

fn json_obj(pairs: &[(&str, Value)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blob_is_fetched_through_the_node_and_checked() {
    let world = World::new(&[(ALICE, 51)]);
    let alice = world.node(ALICE, 51);
    let space = space_with_members(&world, &alice, &[]).await;
    // The proposer uploads the arguments as a blob (over a connection of its own: the node API has no upload).
    let data = br#"{"title":"a long title","body":"a long body"}"#.to_vec();
    let hash = ids::sha256_hex(&data);
    let blob = json_obj(&[
        ("hash", Value::Str(hash.clone())),
        ("size", Value::Int(data.len() as i64)),
        ("type", Value::Str("application/json".into())),
    ]);
    let transport = MemTransport::new(&world.net, &common::pubkey(51));
    let addr = NodeAddr {
        node: common::pubkey(200),
        addrs: vec![],
        relays: vec!["https://mem.invalid".into()],
    };
    let client = Client::connect(&transport, &addr, common::SERVER, &[common::pubkey(200)])
        .await
        .unwrap();
    client
        .hello(ALICE, "d1", &SigningKey::from_seed(common::seed(51)))
        .await
        .unwrap();
    client
        .call(
            "blobPut",
            args([
                ("ref", blob.clone()),
                ("offset", Value::Int(0)),
                ("data", Value::Str(base64url::encode(&data))),
            ]),
        )
        .await
        .unwrap();
    let plan = format!(
        r#"{{"action":"open_pr","target":"github:o/r","argsBlob":{{"hash":"{hash}","size":{},"type":"application/json"}}}}"#,
        data.len()
    );
    call(
        &alice,
        Command::EffectPropose {
            space: space.clone(),
            executor: ALICE.into(),
            plan,
            task: None,
            cost: None,
            repeatable: false,
            expires_at: None,
        },
    )
    .await;
    let fetch = |hash: &str, size: u64| Command::BlobFetch {
        space: space.clone(),
        hash: hash.into(),
        size,
        media_type: "application/json".into(),
    };
    let CallResult::Blob {
        hash: got,
        media_type,
        data: bytes,
    } = alice
        .call(fetch(&hash, data.len() as u64))
        .await
        .expect("the blob")
    else {
        panic!("a blob")
    };
    assert_eq!((got, media_type), (hash.clone(), "application/json".into()));
    assert_eq!(base64url::decode(&bytes).unwrap(), data);
    // Wrong size, unknown hash, bad hash, too big, a space not followed.
    let e = alice
        .call(fetch(&hash, data.len() as u64 + 1))
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    let e = alice
        .call(fetch(&ids::sha256_hex(b"other"), 5))
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::NotFound { .. }), "{e:?}");
    let e = alice.call(fetch("xyz", 5)).await.unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    let e = alice.call(fetch(&hash, 100_000_000)).await.unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    let e = alice
        .call(Command::BlobFetch {
            space: "nope:a.localhost".into(),
            hash,
            size: 1,
            media_type: "text/plain".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::NotFound { .. }), "{e:?}");
}

/// R15: the agent's host uploads a `git_push`'s bundle through its node (`BlobPut`). The home supplies it to
/// the space's readers because the proposal's author uploaded it, even when the upload comes after the
/// proposal was ordered (the home was away: the proposal waited in the outbox, the upload was tried again).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blob_put_through_the_node_after_its_proposal_is_supplied() {
    let world = World::new(&[(ALICE, 51), (BOB, 52)]);
    let alice = world.node(ALICE, 51);
    let bob = world.node(BOB, 52);
    let space = space_with_members(&world, &alice, &[(BOB, &bob)]).await;
    let data = b"# v2 git bundle

not really a pack"
        .to_vec();
    let hash = ids::sha256_hex(&data);
    let plan = format!(
        r#"{{"action":"git_push","target":"repo","argsBlob":{{"hash":"{hash}","size":{},"type":"application/x-git-bundle"}}}}"#,
        data.len()
    );
    call(
        &alice,
        Command::EffectPropose {
            space: space.clone(),
            executor: BOB.into(),
            plan,
            task: None,
            cost: None,
            repeatable: false,
            expires_at: None,
        },
    )
    .await;
    let fetch = || Command::BlobFetch {
        space: space.clone(),
        hash: hash.clone(),
        size: data.len() as u64,
        media_type: "application/x-git-bundle".into(),
    };
    // nothing uploaded yet
    let e = bob.call(fetch()).await.unwrap_err();
    assert!(matches!(e, NodeError::NotFound { .. }), "{e:?}");
    let put = alice
        .call(Command::BlobPut {
            space: space.clone(),
            media_type: "application/x-git-bundle".into(),
            data: base64url::encode(&data),
        })
        .await
        .expect("the upload");
    assert_eq!(
        put,
        CallResult::BlobRef {
            hash: hash.clone(),
            size: data.len() as u64,
            media_type: "application/x-git-bundle".into(),
        }
    );
    let CallResult::Blob { data: got, .. } = bob.call(fetch()).await.expect("the blob") else {
        panic!("a blob")
    };
    assert_eq!(base64url::decode(&got).unwrap(), data);
    // a media type is checked before anything is sent
    let e = alice
        .call(Command::BlobPut {
            space: space.clone(),
            media_type: "not a type".into(),
            data: base64url::encode(&data),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invites_and_approvals_take_the_times_a_face_collects() {
    let world = World::new(&[(ALICE, 51), (BOB, 52)]);
    let alice = world.node(ALICE, 51);
    let bob = world.node(BOB, 52);
    let space = space_with_members(&world, &alice, &[(BOB, &bob)]).await;
    let mut policy = han_seq::build::space_policy("members", "invite");
    if let Value::Object(m) = &mut policy {
        m.insert(
            "approvals".into(),
            han_core::json::parse(br#"[{"action":"*","approvers":["owner"]}]"#).unwrap(),
        );
    }
    call(
        &alice,
        Command::PolicySet {
            space: space.clone(),
            policy: String::from_utf8(han_core::json::canonicalize(&policy).unwrap()).unwrap(),
        },
    )
    .await;
    let invite = |ttl_secs: Option<u64>, expires_at: Option<String>| Command::Invite {
        space: space.clone(),
        ttl_secs,
        expires_at,
        auto_admit: false,
    };
    let at =
        |secs: u64| han_seq::time::format_time(i64::try_from(common::START_SECS + secs).unwrap());
    // Both, a time in the past, a time past a week, and a bad time: refused. A day ahead: a ticket.
    for bad in [
        invite(Some(60), Some(at(3600))),
        invite(None, Some(at(0))),
        invite(None, Some(at(8 * 86_400))),
        invite(None, Some("tomorrow".into())),
    ] {
        let e = alice.call(bad).await.unwrap_err();
        assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    }
    assert!(matches!(
        call(&alice, invite(None, Some(at(86_400)))).await,
        CallResult::Ticket { .. }
    ));

    // An approval takes `expires_at`: a future HAN Time only.
    let proposal = id_of(
        &call(
            &bob,
            Command::EffectPropose {
                space: space.clone(),
                executor: BOB.into(),
                plan: r#"{"action":"git_push","target":"github:o/r","args":{"branch":"b"}}"#.into(),
                task: None,
                cost: None,
                repeatable: false,
                expires_at: None,
            },
        )
        .await,
    );
    until("alice sees the proposal", || {
        rows_of(
            &alice,
            Query::Approvals {
                actor: ALICE.into(),
                window: window(),
            },
        )
        .iter()
        .any(|r| matches!(r, Row::Approval(a) if a.proposal == proposal))
    })
    .await;
    let approve = |expires_at: Option<String>| Command::EffectApprove {
        space: space.clone(),
        proposal: proposal.clone(),
        expires_at,
        note: None,
    };
    for bad in [at(0), "soon".to_owned()] {
        let e = alice.call(approve(Some(bad))).await.unwrap_err();
        assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    }
    let done = call(&alice, approve(Some(at(24 * 3600)))).await;
    assert!(matches!(done, CallResult::Receipt { .. }), "{done:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_node_status_lists_recent_errors_and_open_views() {
    let world = World::new(&[(ALICE, 51)]);
    let alice = world.node(ALICE, 51);
    let mut status = Watched::open(&alice, Query::NodeStatus { window: window() });
    let panel = |w: &mut Watched| match &w.rows()[0] {
        Row::NodeStatus(n) => n.clone(),
        other => panic!("{other:?}"),
    };
    let first = panel(&mut status);
    assert_eq!((first.subscriptions, first.resyncs), (1, 0));
    assert!(first.recent_errors.is_empty());
    // A refused command and a view that cannot open show up with their codes.
    let _ = alice
        .call(Command::EffectCancel {
            space: "nope:a.localhost".into(),
            proposal: "x".into(),
        })
        .await
        .unwrap_err();
    let _ = alice.view(Query::Board {
        space: "nope:a.localhost".into(),
        group: han_node::api::Group::Issue,
        collapsed: vec![],
        window: window(),
    });
    until("the panel lists both", || {
        panel(&mut status).recent_errors.len() == 2
    })
    .await;
    let errors = panel(&mut status).recent_errors;
    assert_eq!(
        errors
            .iter()
            .map(|e| (e.code.as_str(), e.what.as_str()))
            .collect::<Vec<_>>(),
        vec![("not_found", "effect_cancel"), ("not_found", "view")]
    );
    // Another view opens and closes: the count follows.
    let other = alice.view(Query::Spaces { window: window() });
    until("two views", || panel(&mut status).subscriptions == 2).await;
    drop(other);
    until("one view", || panel(&mut status).subscriptions == 1).await;
    alice.shutdown();
    status.drain();
    assert!(status.mirror.ended(), "the node says goodbye with End");
}
