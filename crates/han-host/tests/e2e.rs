//! The M4 story end to end, in one process on the in-memory network (PLAN §10 M4 "done when"):
//!
//! 1. an agent session on an issue streams `han.status/1` and a live timeline to its operator's node;
//! 2. another agent's `team.who_is_on` (han-mcp) sees it working on `src/auth/`;
//! 3. the agent's proposed push waits for approval: the executor claims nothing until the operator approves;
//! 4. after the approval the push lands exactly once on the remote;
//! 5. an executor restart never lands it twice, and the session's final update closes the task.
//!
//! The agent is `han-fake-agent` replaying `fixtures/e2e-session.json` over ACP; its `propose` goes through the
//! in-sandbox MCP stub, the session proxy's tunnel and han-mcp, exactly as a real harness's would. The sandbox
//! is `none` (advisory) so the test runs on every CI runner; `tests/sandbox_probe.rs` covers confinement.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_exec::{ExecConfig, Executor, ExecutorConfig, NeedsApproval, RepoTarget, SystemElapsed};
use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_mcp::{HanTools, HostBridge, ProposeAnswer, ProposeRequest};
use han_node::api::{BoxFuture, CallResult, Command, NodeApi, Query, Row, SessionState, Window};
use han_vcs::GitVcs;

mod common;
mod production_shim;
use common::*;

/// A bridge for the second agent's tools: only reads matter here.
struct ScoutBridge {
    api: Arc<dyn NodeApi>,
    space: String,
}

impl HostBridge for ScoutBridge {
    fn propose(&self, _req: ProposeRequest) -> BoxFuture<'_, ProposeAnswer> {
        Box::pin(async {
            ProposeAnswer::Refused {
                reason: "not in this test".into(),
            }
        })
    }
    fn node(&self) -> Arc<dyn NodeApi> {
        Arc::new(han_host::host::AgentApi(self.api.clone()))
    }
    fn space(&self) -> String {
        self.space.clone()
    }
    fn session(&self) -> String {
        "scout-session".into()
    }
    fn tool_used(&self, _tool: &str, _summary: &str) {}
    fn summaries(&self, _session: &str) -> Vec<String> {
        Vec::new()
    }
}

fn exec_config(remote: &Path) -> ExecConfig {
    ExecConfig {
        repos: vec![RepoTarget {
            name: "local:origin".into(),
            remote: remote.to_string_lossy().into_owned(),
            github: None,
        }],
        http_hosts: Vec::new(),
        branch_prefixes: vec!["han/".into()],
        landing_refs: Vec::new(),
        github: None,
        test_allow_private_http: false,
    }
}

fn open_executor(api: Arc<dyn NodeApi>, state: &Path, remote: &Path) -> Executor {
    Executor::open(
        ExecutorConfig {
            actor: EXEC.into(),
            actions: han_exec::actions::builtin(),
            gate: Arc::new(NeedsApproval),
            config: Arc::new(exec_config(remote)),
            vcs: Arc::new(GitVcs::new(state.join("objects"))),
            github: None,
            state_dir: state.to_path_buf(),
            stall_bound: Duration::from_secs(10),
        },
        api,
        Arc::new(SystemElapsed::new()),
    )
    .expect("the executor opens")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_session_streams_is_seen_and_its_push_lands_once_after_approval() {
    story("none").await;
}

/// The same story with the agent confined by the OS sandbox (MXC on Windows, bubblewrap on Linux). Skips,
/// saying why, where neither is available.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_story_holds_inside_the_os_sandbox() {
    let available = han_host::sandbox::probe(&production_shim::shim());
    let Some(b) = available
        .iter()
        .find(|a| a.available && a.backend != han_node::api::SandboxBackend::None)
    else {
        let why: Vec<String> = available
            .iter()
            .filter(|a| a.backend != han_node::api::SandboxBackend::None)
            .map(|a| format!("{:?}: {}", a.backend, a.detail))
            .collect();
        eprintln!("SKIP: no usable OS sandbox here ({})", why.join("; "));
        return;
    };
    eprintln!("running inside {b:?}");
    story("auto").await;
}

async fn story(backend: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = origin(root);
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let scout = world.node(SCOUT, 3);
    let exec_node = world.node(EXEC, 4);

    // The space: Alice owns it, gates every action on an owner's approval, confirms her agents.
    let CallResult::Space { space } = call(
        &alice,
        Command::SpaceCreate {
            home: world.home_ref.clone(),
            name: "team".into(),
            visibility: "members".into(),
            join: "invite".into(),
        },
    )
    .await
    else {
        panic!("a space")
    };
    for agent in [CODER, SCOUT, EXEC] {
        confirm_operator(&world, &alice, ALICE, agent).await;
        call(
            &alice,
            Command::MemberSet {
                space: space.clone(),
                actor: agent.into(),
                role: "member".into(),
            },
        )
        .await;
    }
    call(
        &alice,
        Command::PolicySet {
            space: space.clone(),
            policy: gated_policy(),
        },
    )
    .await;
    for n in [&coder, &scout, &exec_node] {
        call(
            n,
            Command::SpaceFollow {
                home: world.home_ref.clone(),
                space: space.clone(),
            },
        )
        .await;
    }

    // The executor runs from the start: it must claim nothing until the approval.
    let exec_state = root.join("exec");
    std::fs::create_dir_all(&exec_state).unwrap();
    let exec_api: Arc<dyn NodeApi> = Arc::new(exec_node.clone());
    let (exec_stop, exec_rx) = tokio::sync::watch::channel(false);
    let exec_task = {
        let api = exec_api.clone();
        let state = exec_state.clone();
        let remote = remote.clone();
        tokio::spawn(async move {
            let ex = open_executor(api, &state, &remote);
            ex.run(exec_rx).await.unwrap();
        })
    };

    // The session.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e-session.json");
    let session_dir = root.join("coder").join("s1");
    // Inside the agent directory: writable in the sandbox.
    let agent_log = session_dir.join("agent").join("fake-agent.jsonl");
    let cfg = HostConfig {
        space: space.clone(),
        agent: CODER.into(),
        operator: Some(ALICE.into()),
        driver: {
            let mut d = fake_driver(&script);
            d.env.push((
                "HAN_FAKE_AGENT_LOG".into(),
                agent_log.to_string_lossy().into_owned(),
            ));
            d
        },
        repo: RepoConfig {
            name: "local:origin".into(),
            source: remote.to_string_lossy().into_owned(),
            base: "main".into(),
            default_branch: "main".into(),
        },
        issue: Some(ISSUE.into()),
        task: None,
        executor: EXEC.into(),
        session_dir: session_dir.clone(),
        taint_db: root.join("coder").join("taint.db"),
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: backend.into(),
            allow_advisory: backend == "none",
            // The replay script is the agent's own input: readable inside the sandbox.
            ro: vec![script.clone()],
            login_dir: None,
            allow_hosts: Vec::new(),
            toolchains: Default::default(),
        },
        shim: production_shim::shim(),
        node: PathBuf::from("node"),
        adapter_dir: PathBuf::new(),
        model_key: None,
        budget: None,
        turn_timeout: Duration::from_secs(120),
        exclusion: Vec::new(),
    };
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    let (host_stop, host_rx) = tokio::sync::watch::channel(false);
    let host_task = tokio::spawn(run_session(cfg, deps, host_rx));

    // 1. Alice's board shows the session waiting for approval, with its files, tests and sandbox.
    let board = |api: &dyn NodeApi| {
        rows(
            api,
            Query::Board {
                space: space.clone(),
                group: Default::default(),
                collapsed: vec![],
                window: Window::default(),
            },
        )
        .into_iter()
        .find_map(|r| match r {
            Row::Board(b) if b.agent == CODER => Some(b),
            _ => None,
        })
    };
    for i in 0..900 {
        let b = board(&alice);
        if b.as_ref()
            .is_some_and(|b| b.state == SessionState::WaitingApproval)
        {
            break;
        }
        if i % 100 == 99 {
            eprintln!("board: {b:?}");
        }
        if host_task.is_finished() {
            panic!("the session ended early: {:?}", host_task.await);
        }
        assert!(
            i < 899,
            "timed out waiting for the board to show the session waiting for approval; agent log:
{}
adapter stderr:
{}",
            std::fs::read_to_string(&agent_log).unwrap_or_default(),
            std::fs::read_to_string(session_dir.join("adapter-stderr.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let row = board(&alice).unwrap();
    assert_eq!(row.issue.as_deref(), Some(ISSUE));
    assert!(
        row.files.iter().any(|f| f.path == "src/auth/login.rs"),
        "{:?}",
        row.files
    );
    assert!(row.files.iter().any(|f| f.path == "README.md"));
    assert_eq!(row.tests.map(|t| (t.pass, t.fail)), Some((3, 0)));
    let sandbox = row.sandbox.expect("the sandbox is in status (SR-34)");
    if backend == "none" {
        assert_eq!(sandbox.fidelity, han_node::api::Fidelity::Advisory);
    } else {
        assert_eq!(sandbox.fidelity, han_node::api::Fidelity::Enforced);
        assert_ne!(sandbox.backend, han_node::api::SandboxBackend::None);
    }
    let session = row.session.clone();

    // ... and a live timeline of the worktree.
    until("the timeline has points", 30, || {
        !rows(
            &alice,
            Query::Timeline {
                session: session.clone(),
                at: None,
                window: Window::default(),
            },
        )
        .is_empty()
    })
    .await;

    // 2. Another agent's team.who_is_on sees the session on src/auth/.
    let tools = HanTools::new(Arc::new(ScoutBridge {
        api: Arc::new(scout.clone()),
        space: space.clone(),
    }));
    let mut seen = false;
    for _ in 0..100 {
        let mut args = serde_json::Map::new();
        args.insert("summary".into(), "who works on auth".into());
        args.insert("path".into(), "src/auth/".into());
        let r = tools.call("team_who_is_on", args).await;
        let text = serde_json::to_string(&r.structured_content).unwrap_or_default();
        if text.contains(&session) && text.contains("\"trust\":\"untrusted\"") {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen, "team.who_is_on names the coder's session");

    // 3. The proposal waits: no claim without an approval.
    let effects = |api: &dyn NodeApi| -> Vec<han_node::api::EffectRow> {
        rows(
            api,
            Query::Effects {
                space: Some(space.clone()),
                executor: Some(EXEC.into()),
                include_done: true,
                window: Window::default(),
            },
        )
        .into_iter()
        .filter_map(|r| match r {
            Row::Effect(e) => Some(e),
            _ => None,
        })
        .collect()
    };
    until("the executor sees the proposal", 30, || {
        !effects(&exec_node).is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let e = effects(&exec_node).remove(0);
    assert_eq!((e.claims, e.phase.as_str()), (0, "open"), "{e:?}");
    assert_eq!(e.action, "git_push");
    let refname = "refs/heads/han/12-coder";
    assert_eq!(
        git(&remote, &["for-each-ref", refname]),
        "",
        "nothing landed"
    );

    // The plan carries the changeset summary tiers will read.
    let plan: serde_json::Value = serde_json::from_str(e.plan.as_deref().unwrap()).unwrap();
    let changes = plan["args"]["changes"]["paths"].to_string();
    assert!(changes.contains("src/auth/login.rs"), "{changes}");

    // 4. Alice approves; the push lands once.
    until("alice sees the approval request", 15, || {
        !rows(
            &alice,
            Query::Approvals {
                actor: ALICE.into(),
                window: Window::default(),
            },
        )
        .is_empty()
    })
    .await;
    call(
        &alice,
        Command::EffectApprove {
            space: space.clone(),
            proposal: e.proposal.clone(),
            expires_at: None,
            note: None,
        },
    )
    .await;
    until("the effect is done", 60, || {
        effects(&exec_node)
            .iter()
            .any(|x| x.proposal == e.proposal && x.phase == "done")
    })
    .await;
    let to = plan["args"]["to"].as_str().unwrap().to_owned();
    assert_eq!(git(&remote, &["rev-parse", refname]), to);
    assert_eq!(effects(&exec_node)[0].claims, 1);

    // 5. A restart of the executor never lands it twice.
    exec_stop.send(true).unwrap();
    exec_task.await.unwrap();
    let (exec_stop2, exec_rx2) = tokio::sync::watch::channel(false);
    let exec_task2 = {
        let api = exec_api.clone();
        let state = exec_state.clone();
        let remote = remote.clone();
        tokio::spawn(async move {
            let ex = open_executor(api, &state, &remote);
            ex.run(exec_rx2).await.unwrap();
        })
    };
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(effects(&exec_node)[0].claims, 1, "no second claim");
    assert_eq!(git(&remote, &["rev-parse", refname]), to);
    exec_stop2.send(true).unwrap();
    exec_task2.await.unwrap();

    // The session stops: its final update closes the task, and the board shows it done.
    host_stop.send(true).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(60), host_task)
        .await
        .expect("the host stops")
        .unwrap()
        .expect("the session ends cleanly");
    assert_eq!(end.state, SessionState::Done);
    let task = end.task.expect("the issue's task");
    until("the task is closed by the agent's final update", 15, || {
        rows(
            &alice,
            Query::Tasks {
                space: space.clone(),
                assignee: Some(CODER.into()),
                window: Window::default(),
            },
        )
        .iter()
        .any(|r| matches!(r, Row::Task(t) if t.task == task && !t.open))
    })
    .await;
    drop(world);
}
