//! A face connection is locked for the commands that govern or steer until a person confirms on the device
//! (SR-106). Any process of the person's OS user can open the face pipe; the pipe proves nothing about who is
//! at the machine. The presence backend is a test double injected through the node's config.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{World, call, space_of};
use han_node::api::{
    Command, ConnectionClass, HomeRef, NodeApi, NodeError, Query, ViewEvent, Window,
};
use han_node::rpc::{RpcClient, ServerClock, default_endpoint, serve_as, serve_with_clock};
use han_node::{Node, presence};
use han_presence::PresenceError;
use han_presence::testing::Scripted;

const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";

type N = Node<han_net::MemTransport>;

/// What a command that governs or steers is, one of each kind.
fn locked_commands(space: &str) -> Vec<(&'static str, Command)> {
    vec![
        (
            "member",
            Command::MemberSet {
                space: space.into(),
                actor: BOB.into(),
                role: "member".into(),
            },
        ),
        (
            "policy",
            Command::PolicySet {
                space: space.into(),
                policy: "{}".into(),
            },
        ),
        (
            "invite",
            Command::Invite {
                space: space.into(),
                ttl_secs: None,
                expires_at: None,
                auto_admit: false,
            },
        ),
        ("device offer", Command::DeviceOffer),
        (
            "device revoke",
            Command::DeviceRevoke {
                key_id: "d1".into(),
            },
        ),
        ("device pair", Command::DevicePair { code: "x".into() }),
        (
            "home pin",
            Command::HomePin {
                ticket: "han1home.x".into(),
            },
        ),
        (
            "join",
            Command::SpaceJoin {
                ticket: "han1.x".into(),
            },
        ),
        (
            "agent key",
            Command::AgentCreate {
                name: "x".into(),
                server: "a.localhost".into(),
            },
        ),
        (
            "agent start",
            Command::AgentStart {
                space: space.into(),
                agent: "@coder:a.localhost".into(),
                harness: None,
                repo: None,
                issue: None,
                home: None,
            },
        ),
        // a ticket given with a command is pinned, which is `HomePin` by another door
        (
            "follow by ticket",
            Command::SpaceFollow {
                home: Some(HomeRef::Ticket("han1home.x".into())),
                space: space.into(),
            },
        ),
        (
            "create by ticket",
            Command::SpaceCreate {
                home: Some(HomeRef::Ticket("han1home.x".into())),
                name: "x".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        ),
        (
            "steer",
            Command::SessionInput {
                space: space.into(),
                session: "nothing".into(),
                text: "go".into(),
            },
        ),
    ]
}

struct Stage {
    world: World,
    _alice: N,
    space: String,
    face: String,
    /// The time the face server counts unlock windows by.
    clock: Arc<ServerClock>,
    _servers: Vec<han_node::rpc::RpcServer>,
}

async fn stage(script: Arc<Scripted>, window: Option<Duration>) -> Stage {
    stage_with(script, window, |_, _| {}).await
}

async fn stage_with(
    script: Arc<Scripted>,
    window: Option<Duration>,
    tweak: impl FnOnce(&std::path::Path, &mut han_node::NodeConfig),
) -> Stage {
    let world = World::new(&[(ALICE, 41), (BOB, 42)]);
    let data = world.dir.join("alice-data");
    let alice = world.node_with(ALICE, 41, |c| {
        tweak(&data, c);
        c.data_dir = Some(data);
        c.set_presence(script);
        c.unlock_window = window;
    });
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
    let dir = world.dir.join("face-lock");
    std::fs::create_dir_all(&dir).unwrap();
    let face = default_endpoint(&dir);
    let api: Arc<dyn NodeApi> = Arc::new(alice.clone());
    let clock = Arc::new(ServerClock::default());
    let servers = vec![
        serve_with_clock(
            Arc::clone(&api),
            &face,
            ConnectionClass::Face,
            Arc::clone(&clock),
        )
        .unwrap(),
        serve_as(api, &format!("{face}-host"), ConnectionClass::Host).unwrap(),
    ];
    Stage {
        world,
        _alice: alice,
        space,
        face,
        clock,
        _servers: servers,
    }
}

fn presence_required(r: &Result<han_node::api::CallResult, NodeError>) -> bool {
    matches!(r, Err(NodeError::PresenceRequired { .. }))
}

fn set_bob(s: &Stage) -> Command {
    Command::MemberSet {
        space: s.space.clone(),
        actor: BOB.into(),
        role: "member".into(),
    }
}

/// Run as a child by `a_separate_process_on_the_face_pipe_cannot_govern_or_steer`: a plain local process that
/// opens the face pipe (the endpoint is in the environment) and tries what an unsandboxed agent would. It
/// prints one line per attempt.
#[test]
#[ignore]
fn locked_child() {
    let (Ok(endpoint), Ok(space)) = (
        std::env::var("HAN_FACE_LOCK_ENDPOINT"),
        std::env::var("HAN_FACE_LOCK_SPACE"),
    ) else {
        return;
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let raw = RpcClient::connect_locked(&endpoint).await.unwrap();
            for (name, cmd) in locked_commands(&space) {
                let r = raw.call(cmd).await;
                println!(
                    "child {name}: {}",
                    match r {
                        Err(NodeError::PresenceRequired { .. }) => "presence_required".to_owned(),
                        other => format!("{other:?}"),
                    }
                );
            }
            // reads and own work still go through
            let mut v = raw.view(Query::Spaces {
                window: Window::default(),
            });
            let first = v.next().await;
            println!(
                "child read: {}",
                if matches!(first, Some(ViewEvent::Snapshot { .. })) {
                    "ok"
                } else {
                    "failed"
                }
            );
            // a client that does ask for the unlock gets the person's answer, which here is no
            let asking = RpcClient::connect(&endpoint).await.unwrap();
            let e = asking
                .call(Command::DeviceOffer)
                .await
                .expect_err("nobody confirms");
            println!("child asking: {e}");
        });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_separate_process_on_the_face_pipe_cannot_govern_or_steer() {
    let script = Scripted::new();
    let s = stage(script.clone(), None).await;
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "locked_child", "--nocapture"])
        .env("HAN_FACE_LOCK_ENDPOINT", &s.face)
        .env("HAN_FACE_LOCK_SPACE", &s.space)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (name, _) in locked_commands(&s.space) {
        assert!(
            text.contains(&format!("child {name}: presence_required")),
            "{name} was not refused as presence_required:\n{text}"
        );
    }
    assert!(text.contains("child read: ok"), "{text}");
    assert!(
        text.contains("child asking: denied: not unlocked: "),
        "{text}"
    );
    // the only prompt was the one the asking client caused, and the person said no (an empty script)
    let seen = script.prompts();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].action, "unlock");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unlock_is_for_one_connection_and_for_the_window() {
    let script = Scripted::new();
    let s = stage(script.clone(), Some(Duration::from_millis(1500))).await;
    let a = RpcClient::connect_locked(&s.face).await.unwrap();
    let b = RpcClient::connect_locked(&s.face).await.unwrap();
    assert!(presence_required(&a.call(set_bob(&s)).await));
    // the person confirms on connection A
    script.allow();
    let window = a.unlock().await.expect("a person confirmed");
    assert_eq!(window, Duration::from_millis(1500));
    a.call(set_bob(&s))
        .await
        .expect("A governs inside the window");
    // steering is open on A too: the error is the session's, not the lock's
    let steer = a
        .call(Command::SessionInput {
            space: s.space.clone(),
            session: "nothing".into(),
            text: "go".into(),
        })
        .await;
    assert!(!presence_required(&steer), "{steer:?}");
    // B shares the pipe and the process, and gets nothing from A's unlock
    assert!(presence_required(&b.call(set_bob(&s)).await));
    assert_eq!(script.prompts().len(), 1, "B was not asked or let through");
    // the window ends: A is locked again
    s.clock.advance(Duration::from_millis(1600));
    assert!(presence_required(&a.call(set_bob(&s)).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_face_client_unlocks_when_it_is_refused_and_asks_once_per_window() {
    let script = Scripted::new();
    let s = stage(script.clone(), None).await;
    let face = RpcClient::connect(&s.face).await.unwrap();
    script.allow();
    face.call(set_bob(&s)).await.expect("confirmed once");
    // a second governing command in the window asks nobody
    face.call(Command::Invite {
        space: s.space.clone(),
        ttl_secs: None,
        expires_at: None,
        auto_admit: false,
    })
    .await
    .expect("inside the window");
    let seen = script.prompts();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].action, "unlock");
    assert!(
        seen[0]
            .message()
            .starts_with("Unlock governing and steering")
    );
    // a cancelled prompt is the command's error, typed, and nothing was done
    let other = RpcClient::connect(&s.face).await.unwrap();
    script.fail(PresenceError::Cancelled);
    let e = other.call(Command::DeviceOffer).await.unwrap_err();
    assert!(
        matches!(&e, NodeError::Denied { message } if message.starts_with(presence::NOT_UNLOCKED)
            && message.contains("cancelled")),
        "{e:?}"
    );
    // the refusal spaces the next prompt out, and says exactly how long to wait
    let soon = RpcClient::connect_locked(&s.face).await.unwrap();
    let e = soon.unlock().await.unwrap_err();
    assert!(
        matches!(e, NodeError::Unavailable { retry_after_ms: Some(ms), .. } if ms > 0 && ms <= 2001),
        "{e:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_ask_per_action_and_hosts_cannot_unlock() {
    let script = Scripted::new();
    let s = stage(script.clone(), None).await;
    // an approval is not behind the unlock: it is refused by its own check (a person, bound to the plan), and
    // by the node's lookup, not by the lock
    let raw = RpcClient::connect_locked(&s.face).await.unwrap();
    let r = raw
        .call(Command::EffectApprove {
            space: s.space.clone(),
            proposal: "nothing".into(),
            expires_at: None,
            note: None,
        })
        .await;
    assert!(!presence_required(&r), "{r:?}");
    // an integration (host class) connection never unlocks and never governs or steers
    let host = RpcClient::connect_locked(&format!("{}-host", s.face))
        .await
        .unwrap();
    let e = host.unlock().await.unwrap_err();
    assert!(matches!(e, NodeError::Denied { .. }), "{e:?}");
    for (name, cmd) in locked_commands(&s.space) {
        // the host endpoint signs as the person, so it refuses steering like every other face command
        let e = host.call(cmd).await.unwrap_err();
        assert!(
            matches!(&e, NodeError::Denied { message } if message.contains("may not send")),
            "{name}: {e:?}"
        );
    }
    // an agent's own node in process may still send input to a session, and the node (the policy) decides
    let own = han_node::ClassApi::new(Arc::new(s._alice.clone()), ConnectionClass::Host);
    let steer = Command::SessionInput {
        space: s.space.clone(),
        session: "nothing".into(),
        text: "go".into(),
    };
    let e = own.call(steer.clone()).await.unwrap_err();
    assert!(
        !matches!(&e, NodeError::Denied { message } if message.contains("may not send")),
        "in process it reaches the node: {e:?}"
    );
    let exec = han_node::ClassApi::new(Arc::new(s._alice.clone()), ConnectionClass::Executor);
    let e = exec.call(steer).await.unwrap_err();
    assert!(
        matches!(&e, NodeError::Denied { message } if message.contains("may not send")),
        "{e:?}"
    );
    assert!(script.prompts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_setting_decides_and_weakening_it_needs_a_person() {
    let script = Scripted::new();
    let s = stage(script.clone(), None).await;
    let raw = RpcClient::connect_locked(&s.face).await.unwrap();
    assert!(presence_required(&raw.call(set_bob(&s)).await));
    let db = han_node::db::NodeDb::open(&s.world.dir.join("unused.db")).unwrap();
    let t = Duration::from_secs(1);
    // with the default (everything required) a window change needs a person
    let e =
        presence::change(&db, None, None, None, Some(3000), Some(script.clone()), t).unwrap_err();
    assert!(matches!(e, NodeError::Denied { .. }), "{e:?}");
    script.allow();
    presence::change(&db, None, None, None, Some(3000), Some(script.clone()), t).unwrap();
    assert_eq!(
        presence::unlock_window(&db).unwrap(),
        Duration::from_secs(3000)
    );
    // out of range is invalid, not clamped
    let e = presence::change(&db, None, None, None, Some(1), Some(script.clone()), t).unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    db.set_config(presence::UNLOCK_SECS_KEY, "soon").unwrap();
    assert!(presence::unlock_window(&db).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_unlock_is_refused_while_one_waits_and_an_unlocked_connection_is_not_asked_again()
{
    let script = Scripted::new();
    let s = stage(script.clone(), None).await;
    // the person takes as long as the test likes: the prompt tells the test it is open, then waits to be
    // released
    let (open_tx, open_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (open_tx, release_rx) = (
        std::sync::Mutex::new(open_tx),
        std::sync::Mutex::new(release_rx),
    );
    script.during(move || {
        open_tx.lock().unwrap().send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    });
    script.allow();
    let a = RpcClient::connect_locked(&s.face).await.unwrap();
    let (first, second) = tokio::join!(a.unlock(), async {
        tokio::task::spawn_blocking(move || open_rx.recv().unwrap())
            .await
            .unwrap();
        let second = a.unlock().await;
        release_tx.send(()).unwrap();
        second
    });
    first.expect("the person confirmed");
    assert!(
        matches!(second, Err(NodeError::Conflict { .. })),
        "{second:?}"
    );
    // unlocked: a further unlock asks nobody
    a.unlock().await.expect("still unlocked");
    assert_eq!(script.prompts().len(), 1, "{:?}", script.prompts());
    // a face client whose calls were refused together asks the person once
    let script2 = Scripted::new();
    script2.allow();
    let s2 = stage(script2.clone(), None).await;
    let face = Arc::new(RpcClient::connect(&s2.face).await.unwrap());
    let mut calls = Vec::new();
    for _ in 0..4 {
        let (face, cmd) = (Arc::clone(&face), set_bob(&s2));
        calls.push(tokio::spawn(async move { face.call(cmd).await }));
    }
    for c in calls {
        c.await
            .unwrap()
            .expect("every call goes through after one confirmation");
    }
    assert_eq!(script2.prompts().len(), 1, "{:?}", script2.prompts());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_node_ignores_an_edit_of_its_db() {
    let script = Scripted::new();
    let s = stage(script.clone(), None).await;
    let raw = RpcClient::connect_locked(&s.face).await.unwrap();
    assert!(presence_required(&raw.call(set_bob(&s)).await));
    // a process of the same user opens node.db and switches the requirement off
    let db = han_node::db::NodeDb::open(&s.world.dir.join("alice-data").join("node.db")).unwrap();
    db.set_config(presence::REQUIRE_KEY, "").unwrap();
    assert!(
        presence_required(&raw.call(set_bob(&s)).await),
        "the node keeps the settings it started with"
    );
    assert!(script.prompts().is_empty());
}

/// A service node set up like the runner's (`han runner setup`): root's marker names its data folder. Nobody
/// sits at it, so a prompt would never be answered: it has no unlock, and it never approves. A person's node
/// next to it still locks. Needs a Unix file owner to stand in for root.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_headless_service_node_starts_agents_without_a_prompt_and_a_persons_node_still_locks() {
    use std::os::unix::fs::MetadataExt as _;
    let start = |agent: &str, space: &str| Command::AgentStart {
        space: space.into(),
        agent: agent.into(),
        harness: None,
        repo: None,
        issue: None,
        home: None,
    };
    let script = Scripted::new();
    // (the marker's folder and its parent must be owned by the user standing in for root and not writable by
    // others, so not directly in the shared temp folder)
    let base = std::env::temp_dir().join(format!("han-headless-{}", std::process::id()));
    let markers = base.join("headless.d");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&markers).unwrap();
    let owner = std::fs::metadata(&markers).unwrap().uid();
    let headless = stage_with(script.clone(), None, |data, c| {
        std::fs::write(
            markers.join(presence::seal_name(data)),
            "headless
",
        )
        .unwrap();
        c.headless_dir = Some(markers.clone());
        c.headless_owner = owner;
    })
    .await;
    let raw = RpcClient::connect_locked(&headless.face).await.unwrap();
    let r = raw.call(start("@coder:a.localhost", &headless.space)).await;
    assert!(
        !presence_required(&r),
        "a headless node does not lock agent starts: {r:?}"
    );
    raw.call(set_bob(&headless))
        .await
        .expect("governs without a prompt");
    assert!(script.prompts().is_empty(), "nobody was asked");
    // it never approves: the check is a backend that has none
    let approve = raw
        .call(Command::EffectApprove {
            space: headless.space.clone(),
            proposal: "nothing".into(),
            expires_at: None,
            note: None,
        })
        .await;
    assert!(!presence_required(&approve), "{approve:?}");
    // and it says so
    let mut v = raw.view(Query::NodeStatus {
        window: Window::default(),
    });
    let Some(ViewEvent::Snapshot { rows, .. }) = v.next().await else {
        panic!("no snapshot")
    };
    assert!(
        rows.iter()
            .any(|r| matches!(r, han_node::api::Row::NodeStatus(n) if n.headless)),
        "{rows:?}"
    );

    // a marker that is not owned by the right user (a process of the service user wrote it) does not count
    let forged = stage_with(Scripted::new(), None, |data, c| {
        std::fs::write(
            markers.join(presence::seal_name(data)),
            "headless
",
        )
        .unwrap();
        c.headless_dir = Some(markers.clone());
        c.headless_owner = owner.wrapping_add(1);
    })
    .await;
    let raw = RpcClient::connect_locked(&forged.face).await.unwrap();
    assert!(presence_required(
        &raw.call(start("@coder:a.localhost", &forged.space)).await
    ));

    // a person's node: no marker, so it locks
    let person = stage(Scripted::new(), None).await;
    let raw = RpcClient::connect_locked(&person.face).await.unwrap();
    assert!(presence_required(
        &raw.call(start("@coder:a.localhost", &person.space)).await
    ));
    let _ = std::fs::remove_dir_all(&base);
}
