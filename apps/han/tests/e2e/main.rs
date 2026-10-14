//! End-to-end tests of the dogfood loop, on the real binaries: `han dev up` builds a home, a person's node, a
//! space with the dogfood policy, an agent (`han-fake-agent` over the real ACP path, the production shim and
//! the real OS sandbox), an executor, a fake GitHub and a local bare remote, all with the commands a user
//! runs. Each test names the bug it would have caught.
//!
//! They are slow (a stack is about ten processes), so they are `#[ignore]`d: `cargo test --workspace` and the
//! quick CI skip them, and the nightly `full` workflow runs them. Run them by hand with
//! `cargo test -p han --test e2e -- --ignored --test-threads=1`. They need
//! `han-fake-agent` and `han-github` built next to `han` (`cargo build --workspace --bins`).

mod os_user;
mod stack;

use std::time::Duration;

use han_node::NodeApi;
use han_node::api::{
    Command, Fidelity, ItemBody, MessageRole, Query, Row, SessionPart, SessionState, SpaceLink,
    Window,
};
use stack::{Stack, git, han, rows, run, until};

/// The board row reads "waiting for input: <start of the last message>".
fn idle(b: &han_node::api::BoardRow) -> bool {
    b.state == SessionState::Idle && b.step.starts_with("waiting for input")
}

/// Bug: the in-sandbox shim verb was wrong because the tests used a different binary (`han-agent-shim`) than
/// production (`han`). Here the host hands the sandbox its own executable, `han listen …`, and the session
/// has to get through `initialize`, `session/new` and a prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn the_session_starts_under_the_real_sandbox_with_the_production_shim() {
    let s = Stack::up("simple", &[]);
    let c = s.client().await;
    s.start_agent();
    let row = s
        .board_until(&c, "the first turn to finish", 120, idle)
        .await;
    let sandbox = row.sandbox.expect("the sandbox is in status (SR-34)");
    if stack::os_sandbox() {
        assert_eq!(sandbox.fidelity, Fidelity::Enforced, "{sandbox:?}");
        assert_ne!(sandbox.backend, han_node::api::SandboxBackend::None);
    } else {
        assert_eq!(sandbox.fidelity, Fidelity::Advisory);
    }
    // the agent really ran: it logged the three requests of the start
    let events = s.agent_events();
    for event in ["initialize", "new_session", "prompt"] {
        assert!(events.iter().any(|e| e == event), "{event}: {events:?}");
    }
}

/// Bug: the agent's messages were never mirrored. Its messages, a thought and a tool call are Session items,
/// and the board step reads "waiting for input: …".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn the_agents_output_appears_as_session_items() {
    let s = Stack::up("simple", &[]);
    let c = s.client().await;
    s.start_agent();
    let row = s
        .board_until(&c, "the first turn to finish", 120, idle)
        .await;
    assert!(
        row.step.starts_with("waiting for input: I read the task"),
        "{}",
        row.step
    );
    let session = row.session.clone();
    let items = || async {
        rows(
            &c,
            Query::Session {
                id: session.clone(),
                part: SessionPart::Items,
                window: Window::default(),
            },
        )
        .await
    };
    until("the items to arrive", 60, || async {
        let all = items().await;
        all.iter()
            .any(|r| matches!(r, Row::Item(i) if matches!(&i.item, ItemBody::ToolCall { .. })))
    })
    .await;
    let all: Vec<_> = items()
        .await
        .into_iter()
        .filter_map(|r| match r {
            Row::Item(i) => Some(i.item),
            _ => None,
        })
        .collect();
    assert!(
        all.iter().any(|b| matches!(b, ItemBody::Message { role: MessageRole::Agent, text, .. } if text.contains("I read the task"))),
        "a message: {all:?}"
    );
    assert!(
        all.iter().any(
            |b| matches!(b, ItemBody::Reasoning { text, .. } if text.contains("Reading the task"))
        ),
        "a thought: {all:?}"
    );
    assert!(
        all.iter().any(|b| matches!(b, ItemBody::ToolCall { title: Some(t), .. } if t.contains("read README.md"))),
        "a tool call: {all:?}"
    );
}

/// Bug: the task's issue text did not reach the agent. It arrives as a data block after the instruction, not
/// inside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn the_issue_text_reaches_the_agent_as_a_data_block() {
    let s = Stack::up("simple", &[]);
    let c = s.client().await;
    s.start_agent();
    s.board_until(&c, "the first turn to finish", 120, idle)
        .await;
    let log = s.agent_log();
    let prompt = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["event"] == "prompt")
        .expect("the agent got a prompt");
    let blocks = prompt["prompt"].as_array().expect("content blocks");
    assert!(blocks.len() >= 2, "{blocks:?}");
    let text = |b: &serde_json::Value| b["text"].as_str().unwrap_or_default().to_owned();
    assert!(
        !text(&blocks[0]).contains("DEV-ISSUE-MARKER"),
        "the instruction carries no issue text"
    );
    let data = blocks[1..].iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(data.contains("DEV-ISSUE-MARKER"), "{data}");
    assert!(
        data.contains("untrusted"),
        "the issue text is marked as untrusted: {data}"
    );
}

/// Bugs: (a) the host showed "waiting-approval" for a proposal the home had not accepted; (b) the executor
/// claimed or pushed more than once. The proposal is accepted, shows in the person's Approvals, nothing lands
/// before the approval, the approval lands one push, and an executor restart pushes nothing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn an_approved_push_lands_exactly_once() {
    let s = Stack::up("propose", &[]);
    let c = s.client().await;
    s.start_agent();
    let row = s
        .board_until(&c, "waiting-approval", 120, |b| {
            b.state == SessionState::WaitingApproval
        })
        .await;
    let effects = || async {
        rows(
            &c,
            Query::Effects {
                space: Some(s.space()),
                executor: Some(s.executor()),
                include_done: true,
                window: Window::default(),
            },
        )
        .await
        .into_iter()
        .filter_map(|r| match r {
            Row::Effect(e) => Some(e),
            _ => None,
        })
        .collect::<Vec<_>>()
    };
    // waiting-approval means the home accepted it: it is in the log, open, with no claim
    until("the proposal in the log", 30, || async {
        !effects().await.is_empty()
    })
    .await;
    let e = effects().await.remove(0);
    assert_eq!((e.claims, e.phase.as_str()), (0, "open"), "{e:?}");
    assert_eq!(e.action, "git_push");
    assert_eq!(e.executor, s.executor());
    // step 1 of the approval: it is in the person's Approvals, with the plan to read
    until("the approval request", 30, || async {
        rows(
            &c,
            Query::Approvals {
                actor: s.person(),
                window: Window::default(),
            },
        )
        .await
        .iter()
        .any(
            |r| matches!(r, Row::Approval(a) if a.proposal == e.proposal && a.action == "git_push"),
        )
    })
    .await;
    assert!(
        row.needs_me,
        "the board says the session waits on the person"
    );
    let refname = "refs/heads/han/1-coder";
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !s.remote_refs().contains(refname),
        "nothing lands before the approval"
    );
    assert_eq!(effects().await[0].claims, 0, "no claim before the approval");
    // step 2a: the node's presence check stands between the face and the signature. This device has no check
    // (`none`) and requires one: the approval is refused with the typed message, and nothing lands.
    s.presence("all", "none");
    let refused = c
        .call(Command::EffectApprove {
            space: s.space(),
            proposal: e.proposal.clone(),
            expires_at: None,
            note: None,
        })
        .await
        .expect_err("no presence check, no approval");
    assert!(
        matches!(&refused, han_node::NodeError::Denied { message }
            if message.starts_with(han_node::api::APPROVAL_NOT_SIGNED)
            && message.contains("this device has no presence check")),
        "{refused:?}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        effects().await[0].claims,
        0,
        "a refused approval signs nothing"
    );
    // step 2b: approve, on a device where the person turned the requirement off
    s.presence("", "auto");
    c.call(Command::EffectApprove {
        space: s.space(),
        proposal: e.proposal.clone(),
        expires_at: None,
        note: None,
    })
    .await
    .expect("the approval is accepted");
    until("the effect to be done", 90, || async {
        effects()
            .await
            .iter()
            .any(|x| x.proposal == e.proposal && x.phase == "done")
    })
    .await;
    let plan: serde_json::Value = serde_json::from_str(e.plan.as_deref().unwrap()).unwrap();
    let to = plan["args"]["to"].as_str().unwrap().to_owned();
    let landed = git(&s.remote(), &["rev-parse", refname]);
    assert_eq!(landed, to);
    assert_eq!(effects().await[0].claims, 1);
    // an executor restart does not push again
    s.dev(&["restart", "exec"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(effects().await[0].claims, 1, "no second claim");
    assert_eq!(git(&s.remote(), &["rev-parse", refname]), to);
}

/// Bug: a proposal that the home had not accepted (the home was away) showed "waiting-approval" at once. It
/// has to read as queued until the home accepts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn a_proposal_waits_as_queued_until_the_home_accepts_it() {
    let s = Stack::up("propose-late", &[]);
    let c = s.client().await;
    s.start_agent();
    s.board_until(&c, "the session to be working", 120, |b| !b.closed)
        .await;
    s.dev(&["stop", "home"]);
    // the script proposes about nine seconds in; give it time, with the home away
    tokio::time::sleep(Duration::from_secs(14)).await;
    let c = s.client().await;
    let b = s
        .board_row(&c)
        .await
        .expect("the board still shows the session");
    assert_ne!(
        b.state,
        SessionState::WaitingApproval,
        "the home has not accepted the proposal: {b:?}"
    );
    s.dev(&["start", "home"]);
    s.board_until(&c, "waiting-approval once the home accepts", 120, |b| {
        b.state == SessionState::WaitingApproval
    })
    .await;
}

/// Bug: a proposal naming an executor that has no profile was retried 637 times by the outbox while the host
/// showed "waiting-approval". It needs a definite refusal, surfaced to the agent and to the board.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn a_proposal_to_an_unregistered_executor_is_refused_definitely() {
    let s = Stack::up("propose", &["--no-executor"]);
    let c = s.client().await;
    s.start_agent();
    // the agent is told, in its tool result
    until("the agent to hear the refusal", 120, || async {
        s.agent_events().iter().any(|e| e == "mcp_result")
    })
    .await;
    let log = s.agent_log();
    assert!(
        log.contains("Err") || log.to_lowercase().contains("refus"),
        "the tool result is a refusal: {log}"
    );
    // the board does not claim the session waits for approval
    tokio::time::sleep(Duration::from_secs(3)).await;
    let b = s.board_row(&c).await.expect("a board row");
    assert_ne!(b.state, SessionState::WaitingApproval, "{b:?}");
    // and the outbox does not retry for ever
    tokio::time::sleep(Duration::from_secs(10)).await;
    let outbox = rows(
        &c,
        Query::Outbox {
            space: Some(s.space()),
            window: Window::default(),
        },
    )
    .await;
    for r in outbox {
        if let Row::Outbox(o) = r {
            assert!(o.attempts <= 3, "retried {} times: {o:?}", o.attempts);
        }
    }
}

/// Setup path: a second person joins by ticket, and an operator on another device confirms an agent that
/// was added on the first. Only the setup commands are used, and running them again changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn joining_by_ticket_and_the_operators_confirm_use_only_the_setup_commands() {
    let s = Stack::up("simple", &["--no-executor"]);
    let server = s.get("server");
    let bob = format!("@bob:{server}");
    let bot = format!("@bobot:{server}");
    let bob_dir = s.dir.join("bob");
    let bob_data = bob_dir.to_string_lossy().into_owned();
    let key_file = bob_dir.join("keys").join("device.key");
    let home_data = s.home_data().to_string_lossy().into_owned();
    let person_data = s.person_data().to_string_lossy().into_owned();

    // Bob's device key, and his profile at the home (the home's admin)
    let key = han(&["key", &key_file.to_string_lossy()]).trim().to_owned();
    let registered = han(&[
        "home",
        "register",
        &bob,
        "--kind",
        "human",
        "--key-id",
        "d1",
        &format!("--public-key={key}"),
        "--data",
        &home_data,
    ]);
    assert!(registered.contains("registered"), "{registered}");
    let again = han(&[
        "home",
        "register",
        &bob,
        "--kind",
        "human",
        "--key-id",
        "d1",
        &format!("--public-key={key}"),
        "--data",
        &home_data,
    ]);
    assert!(again.contains("already registered"), "{again}");
    // the old name still works
    let old = han(&[
        "seq",
        "register",
        "--actor",
        &bob,
        "--kind",
        "human",
        "--key-id",
        "d1",
        &format!("--public-key={key}"),
        "--data",
        &home_data,
    ]);
    assert!(old.contains("registered"), "{old}");

    // Bob's node joins by ticket
    let log = std::fs::File::create(s.dir.join("logs").join("bob.log")).unwrap();
    let mut node = std::process::Command::new(stack::HAN)
        .args([
            "node",
            "--actor",
            &bob,
            "--data",
            &bob_data,
            "--file-keys",
            "--relay",
            "none",
            "--bind",
            "127.0.0.1:0",
            "--home",
            &s.ticket(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    until("bob's node", 60, || async {
        run(&["node", "status", "--data", &bob_data]).0
    })
    .await;
    // the owner admits him
    han(&[
        "space",
        "member",
        &s.space(),
        &bob,
        "member",
        "--data",
        &person_data,
    ]);
    let followed = han(&[
        "space",
        "follow",
        &s.space(),
        "--home",
        &s.ticket(),
        "--data",
        &bob_data,
    ]);
    assert!(followed.contains("following"), "{followed}");

    // the first person adds Bob's agent: the home's admin step is done here, the operator's is not
    let add = |expect_todo: bool| {
        let out = han(&[
            "agent",
            "add",
            "bobot",
            "--operator",
            &bob,
            "--space",
            &s.space(),
            "--home",
            &s.ticket(),
            "--home-data",
            &home_data,
            "--allow-advisory",
            "--sandbox",
            "none",
            "--data",
            &person_data,
        ]);
        assert_eq!(out.contains("to do:"), expect_todo, "{out}");
        out
    };
    let first = add(true);
    assert!(first.contains("changed:   registered"), "{first}");
    let command = first
        .lines()
        .find_map(|l| l.trim().strip_prefix("the operator runs: "))
        .expect("it prints what the operator runs")
        .to_owned();
    assert!(command.starts_with("han actor confirm"), "{command}");

    // Bob runs exactly that, on his device
    let mut args: Vec<&str> = command.split_whitespace().skip(1).collect();
    args.extend(["--data", &bob_data]);
    let confirmed = han(&args);
    assert!(confirmed.contains("confirmed"), "{confirmed}");
    let confirmed_again = han(&args);
    assert!(confirmed_again.contains("unchanged"), "{confirmed_again}");

    // the first person finishes; running it again after that changes nothing
    let second = add(false);
    assert!(second.contains("is now a member"), "{second}");
    assert!(second.contains(&format!("ready: {bot}")), "{second}");
    let third = add(false);
    assert!(
        !third
            .lines()
            .any(|l| l.trim_start().starts_with("changed:")),
        "{third}"
    );
    let _ = node.kill();
    let _ = node.wait();
}

/// Restarts: the node and the home are killed mid-session and come back; the board and the session carry on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn the_board_and_the_session_resume_after_the_node_and_the_home_restart() {
    let s = Stack::up("simple", &[]);
    let c = s.client().await;
    s.start_agent();
    let before = s
        .board_until(&c, "the first turn to finish", 120, idle)
        .await;
    drop(c);

    s.dev(&["restart", "node"]);
    let c = s.client().await;
    let after_node = s
        .board_until(&c, "the board after the node restart", 90, |b| {
            !b.session.is_empty()
        })
        .await;
    assert_eq!(after_node.session, before.session, "the same session");
    assert!(
        after_node.step.starts_with("waiting for input"),
        "{}",
        after_node.step
    );

    s.dev(&["restart", "home"]);
    until("the node to follow the space again", 90, || async {
        rows(&c, Query::Spaces { window: Window::default() })
            .await
            .iter()
            .any(|r| matches!(r, Row::SpaceStatus(x) if x.space == s.space() && x.link == SpaceLink::Live))
    })
    .await;
    let after_home = s
        .board_row(&c)
        .await
        .expect("the board still shows the session");
    assert_eq!(after_home.session, before.session);

    // the session still answers: steer it and it takes another turn
    c.call(Command::SessionInput {
        space: s.space(),
        session: before.session.clone(),
        text: "Please go on.".into(),
    })
    .await
    .expect("the steering input is accepted");
    until("the agent's second turn", 90, || async {
        s.agent_events().iter().filter(|e| *e == "prompt").count() >= 2
    })
    .await;
}
