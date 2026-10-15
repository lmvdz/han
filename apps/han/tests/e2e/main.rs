//! End-to-end tests of the dogfood loop, on the real binaries: `han dev up` builds a home, a person's node, a
//! space with the dogfood policy, an agent (`han-fake-agent` over the real ACP path, the production shim and
//! the real OS sandbox), an executor, a fake GitHub and a local bare remote, all with the commands a user
//! runs. Each test names the bug it would have caught.
//!
//! They are slow (a stack is about ten processes), so they are `#[ignore]`d: `cargo test --workspace` and the
//! quick CI skip them, and the nightly `full` workflow runs them. Run them by hand with
//! `cargo test -p han --test e2e -- --ignored --test-threads=1`. They need
//! `han-fake-agent` and `han-github` built next to `han` (`cargo build --workspace --bins`).

mod model;
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

/// Bug: on a Windows build where MXC gives a container no loopback port (10013), `han agent start` for an MXC
/// agent succeeded, the session started, and the agent failed mid-session when it could not reach its tools.
/// The start is now refused with the reason and no session starts; status says why.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn an_mxc_agent_is_refused_with_the_reason_when_the_loopback_port_does_not_work() {
    // The node's self-test is made to fail (`HAN_MXC_FORCE_UNAVAILABLE`), as it does on such a build.
    let s = Stack::up_confined(
        "simple",
        &["--sandbox", "mxc"],
        &[("HAN_MXC_FORCE_UNAVAILABLE", "loopback")],
    );
    let c = s.client().await;
    let started = std::time::Instant::now();
    let (ok, out, err) = run(&[
        "agent",
        "start",
        "--space",
        &s.space(),
        "--agent",
        &s.agent(),
        "--repo",
        &s.remote().to_string_lossy(),
        "--issue",
        &s.get("issue"),
        "--home",
        &s.ticket(),
        "--data",
        &s.person_data().to_string_lossy(),
    ]);
    assert!(
        !ok,
        "the start of an MXC agent must be refused:
{out}
{err}"
    );
    assert!(
        err.contains("loopback") && err.contains("10013"),
        "the refusal says why: {err}"
    );
    assert!(
        err.contains("WSL2+bubblewrap") && err.contains("--allow-advisory"),
        "the refusal says what to do: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "it fails fast: {:?}",
        started.elapsed()
    );
    // no session started: no host role, and the agent's events log was never written
    let status = |rows: Vec<Row>| {
        rows.into_iter().find_map(|r| match r {
            Row::NodeStatus(n) => Some(n),
            _ => None,
        })
    };
    let n = status(
        rows(
            &c,
            Query::NodeStatus {
                window: Window::default(),
            },
        )
        .await,
    )
    .expect("the node has a status");
    assert!(n.roles.is_empty(), "a host was started: {:?}", n.roles);
    assert!(s.agent_events().is_empty(), "the agent ran");
    // status shows it (the probe's detail is the node's problem, with the fix)
    until(
        "the node's status names the sandbox problem",
        60,
        || async {
            status(
                rows(
                    &c,
                    Query::NodeStatus {
                        window: Window::default(),
                    },
                )
                .await,
            )
            .is_some_and(|n| {
                n.problems
                    .iter()
                    .any(|p| p.contains("loopback port") && p.contains("fix:"))
            })
        },
    )
    .await;
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
    // nobody sits at the machine that runs the tests to confirm Bob's joining
    std::fs::create_dir_all(&bob_data).unwrap();
    han_node::db::NodeDb::open(&std::path::Path::new(&bob_data).join("node.db"))
        .unwrap()
        .set_config(han_node::presence::REQUIRE_KEY, "")
        .unwrap();
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

/// Bug: a space budget was never enforced in a live session (nothing created the ledger), and the status did
/// not carry spend. The fake agent runs in API-key mode against a priced fake model over TLS (one unit per
/// token; each call reports 40 tokens in and 60 out) in a space with a budget of 100: the first call spends all of it, the
/// second is refused with a 429, and the board says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn a_spent_budget_refuses_the_next_model_call_and_the_board_says_why() {
    let work = std::env::temp_dir().join(format!("han-e2e-model-{}", std::process::id()));
    let model = model::FakeModel::start(&work, 40, 60).await;
    let ca = model.ca_file.to_string_lossy().into_owned();
    let s = Stack::up(
        "model-calls",
        &[
            "--budget",
            "USD-cent=100",
            "--model-url",
            &model.url,
            "--model-ca",
            &ca,
        ],
    );
    let c = s.client().await;
    s.start_agent();
    let row = s
        .board_until(&c, "the budget to refuse a call", 120, |b| {
            b.budget.as_ref().is_some_and(|x| x.refused.is_some())
        })
        .await;
    let budget = row.budget.expect("the board carries the budget");
    assert_eq!(
        (budget.unit.as_str(), budget.limit, budget.spent),
        ("USD-cent", 100, 100)
    );
    assert_eq!(budget.not_enforced, None);
    assert_eq!(budget.refused.as_deref(), Some("budget-spent"));
    assert_eq!(
        budget.text(),
        "budget: a model call was refused (budget-spent), 100 of 100 USD-cent"
    );
    // the cost and the counts come from the proxy
    let tokens = row.tokens.expect("tokens in the status");
    assert_eq!((tokens.input, tokens.out, tokens.cost), (40, 60, 100));
    // the agent was paid once and refused once, in that order
    let log = s.agent_log();
    assert_eq!(
        model.calls(),
        1,
        "the refused call never reached the provider"
    );
    let ok = log.find("model call: 200").expect(&log);
    let refused = log.find("model call: 429 (budget-spent)").expect(&log);
    assert!(ok < refused, "{log}");
    // `han board` says it too
    let board = han(&[
        "board",
        &s.space(),
        "--data",
        &s.person_data().to_string_lossy(),
    ]);
    assert!(
        board.contains("budget: a model call was refused"),
        "{board}"
    );
}

/// Bug: a host proposed with `cost: None`, so a budgeted space always denied the proposal for having no cost.
/// The cost is the proxy's measurement: after a paid model call the proposal carries it (in the space's
/// unit), and a cost the agent claims is refused by the tool and never used. The proposal then gets past the
/// cost check and meets the next rule: an agent in a budgeted space spends from a budgeted task (the dev
/// stack's issue task has none). The exact amount is checked by the host's unit tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn a_proposal_after_a_priced_model_call_carries_the_measured_cost() {
    let work = std::env::temp_dir().join(format!("han-e2e-cost-{}", std::process::id()));
    let model = model::FakeModel::start(&work, 4, 6).await;
    let ca = model.ca_file.to_string_lossy().into_owned();
    let s = Stack::up(
        "model-propose",
        &[
            "--budget",
            "USD-cent=100",
            "--model-url",
            &model.url,
            "--model-ca",
            &ca,
        ],
    );
    let c = s.client().await;
    s.start_agent();
    let row = s
        .board_until(&c, "the model call to be counted", 120, |b| {
            b.tokens.is_some_and(|t| t.cost == 10)
        })
        .await;
    assert_eq!(row.budget.expect("the budget").spent, 10);
    let results = || {
        s.agent_log()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == "mcp_result")
            .map(|v| v["result"].to_string())
            .collect::<Vec<_>>()
    };
    until("both proposals to be answered", 120, || async {
        results().len() >= 2
    })
    .await;
    let answers = results();
    // the cost the agent claimed is refused by the tool, before the home sees it
    assert!(answers[0].contains("no `cost`"), "{answers:?}");
    // the other proposal carries the measured cost, so the home's first complaint is no longer the cost
    assert!(
        !answers[1].contains("cost in the space's unit required"),
        "{answers:?}"
    );
    assert!(
        answers[1].contains("agents spend only from a budgeted task"),
        "{answers:?}"
    );
}

/// Bug: a budget that cannot be enforced was invisible. With no price table for the space's unit nothing is
/// reserved, and the status says so instead of staying silent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored (the nightly full workflow does)"]
async fn a_budget_without_a_price_table_says_it_is_not_enforced() {
    let s = Stack::up("simple", &["--budget", "USD-cent=100"]);
    let c = s.client().await;
    s.start_agent();
    let row = s
        .board_until(&c, "the budget in the status", 120, |b| b.budget.is_some())
        .await;
    let budget = row.budget.unwrap();
    assert_eq!(budget.text(), "budget: not enforced (no price table)");
    assert_eq!((budget.limit, budget.spent), (100, 0));
}

#[cfg(feature = "conformance")]
/// Runs `han role-probe` as `role` with the keys in `keys`, and returns what it printed.
fn probe(s: &Stack, role: &str, actor: &str, keys: &std::path::Path) -> String {
    let data = s.dir.join(format!("probe-{role}"));
    let (ok, out, err) = run(&[
        "role-probe",
        role,
        "--actor",
        actor,
        "--keys",
        &keys.to_string_lossy(),
        "--data",
        &data.to_string_lossy(),
        "--space",
        &s.space(),
        "--home",
        &s.ticket(),
        "--relay",
        "none",
    ]);
    assert!(ok, "the {role} role let a command through:\n{out}\n{err}");
    out
}

/// Bug class: a host could approve or govern. The host's own node was handed to it whole, so nothing but the
/// home's policy stood between an agent's host and `EffectApprove`, `PolicySet` or `MemberSet`. A host now
/// works through its node held to the host class, which `han host` and the probe build the same way
/// (`rolenode::start`): every command that approves, governs or claims is refused before it reaches the node,
/// and the agent's own work still runs (the first turn finishes).
#[cfg(feature = "conformance")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored --features conformance (the nightly full workflow does)"]
async fn a_host_cannot_approve_or_govern_and_still_does_its_work() {
    let s = Stack::up("simple", &[]);
    let out = probe(&s, "host", &s.agent(), &s.person_data());
    for name in [
        "approve", "cancel", "member", "invite", "policy", "revoke", "claim",
    ] {
        assert!(
            out.contains(&format!("{name}: refused by class")),
            "{name}: {out}"
        );
    }
    assert!(out.contains("own: reached the node"), "{out}");
    // its normal work goes through the same wrapper
    let c = s.client().await;
    s.start_agent();
    s.board_until(&c, "the first turn to finish", 120, idle)
        .await;
}

/// Bug class: an executor could approve. It claims and reports, and nothing else: approvals, policy and
/// membership are refused before the node sees them, and the approved push still lands once the executor is
/// back (the executor's own work).
#[cfg(feature = "conformance")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored --features conformance (the nightly full workflow does)"]
async fn an_executor_cannot_approve_or_govern() {
    let s = Stack::up("propose", &[]);
    s.dev(&["stop", "exec"]);
    let out = probe(&s, "exec", &s.executor(), &s.dir.join("exec"));
    for name in ["approve", "cancel", "member", "invite", "policy", "revoke"] {
        assert!(
            out.contains(&format!("{name}: refused by class")),
            "{name}: {out}"
        );
    }
    assert!(!out.contains("claim:"), "an executor may claim: {out}");
    assert!(out.contains("own: reached the node"), "{out}");
    s.dev(&["start", "exec"]);
}

/// Bug class: any process of the person's OS user could open the node's face pipe and govern or steer as the
/// person. A connection now starts locked for members, policy, invites, devices and steering, and the node's
/// presence check (here the `test-allow` backend, which exists only in `conformance` builds) unlocks it. With
/// no check on the device, a plain local client and the CLI are refused with the typed error and nothing
/// changes; with one, the CLI and a face client govern and steer, and the loop goes on.
#[cfg(feature = "conformance")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end: run with --ignored --features conformance (the nightly full workflow does)"]
async fn a_local_process_cannot_govern_or_steer_until_a_person_confirms() {
    use han_node::NodeError;
    let s = Stack::up("simple", &[]);
    let c = s.client().await;
    s.start_agent();
    let before = s
        .board_until(&c, "the first turn to finish", 120, idle)
        .await;
    let data = s.person_data().to_string_lossy().into_owned();
    let member = |s: &Stack| {
        run(&[
            "space",
            "member",
            &s.space(),
            &s.agent(),
            "member",
            "--data",
            &data,
        ])
    };
    let steer = Command::SessionInput {
        space: s.space(),
        session: before.session.clone(),
        text: "Please go on.".into(),
    };

    // a device that requires a person and has no way to ask one: nothing governs or steers
    s.presence("all", "none");
    let endpoint = han_node::rpc::default_endpoint(&s.person_data());
    let plain = han_node::rpc::RpcClient::connect_locked(&endpoint)
        .await
        .unwrap();
    for cmd in [
        steer.clone(),
        Command::MemberSet {
            space: s.space(),
            actor: s.agent(),
            role: "member".into(),
        },
        Command::PolicySet {
            space: s.space(),
            policy: "{}".into(),
        },
        Command::Invite {
            space: s.space(),
            ttl_secs: None,
            expires_at: None,
            auto_admit: false,
        },
        Command::DeviceOffer,
        // pinning a home by the ticket of a command is `HomePin` by another door
        Command::SpaceFollow {
            home: Some(han_node::api::HomeRef::Ticket("han1home.x".into())),
            space: s.space(),
        },
        Command::AgentStart {
            space: s.space(),
            agent: s.agent(),
            harness: None,
            repo: None,
            issue: None,
            home: None,
        },
    ] {
        let e = plain.call(cmd.clone()).await.unwrap_err();
        assert!(
            matches!(e, NodeError::PresenceRequired { .. }),
            "{cmd:?}: {e:?}"
        );
    }
    assert!(
        s.board_row(&plain).await.is_some(),
        "reads work on a locked connection"
    );
    let (ok, _, err) = member(&s);
    assert!(
        !ok && err.contains("not unlocked") && err.contains("no presence check"),
        "the CLI is refused: {err}"
    );
    let refused = c.call(steer.clone()).await.unwrap_err();
    assert!(
        matches!(&refused, NodeError::Denied { message } if message.starts_with("not unlocked: ")),
        "{refused:?}"
    );
    assert_eq!(
        s.agent_events().iter().filter(|e| *e == "prompt").count(),
        1,
        "the agent was not steered"
    );

    // the person is there: the CLI asks once and the face client unlocks on its first steering input; a
    // second plain connection still gets nothing from either
    s.presence("all", "test-allow");
    // The refusals above space the next prompt out, on the node and on connection `c`. The refusal says how
    // long (`retry_after_ms`), and each wait here is exactly that.
    let probe = han_node::rpc::RpcClient::connect_locked(&endpoint)
        .await
        .unwrap();
    while let Err(NodeError::Unavailable {
        retry_after_ms: Some(ms),
        ..
    }) = probe.unlock().await
    {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    let (ok, _, err) = member(&s);
    assert!(ok, "the CLI governs after a confirmation: {err}");
    loop {
        match c.call(steer.clone()).await {
            Err(NodeError::Unavailable {
                retry_after_ms: Some(ms),
                ..
            }) => tokio::time::sleep(Duration::from_millis(ms)).await,
            r => {
                r.expect("the steering input is accepted after a confirmation");
                break;
            }
        }
    }
    until("the agent's second turn", 90, || async {
        s.agent_events().iter().filter(|e| *e == "prompt").count() >= 2
    })
    .await;
    let e = plain
        .call(Command::DeviceOffer)
        .await
        .expect_err("another connection stays locked");
    assert!(matches!(e, NodeError::PresenceRequired { .. }), "{e:?}");
}
