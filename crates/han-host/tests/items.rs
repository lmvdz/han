//! What the agent says and does reaches the node (PLAN §5, spec/ui/data.md §4.2, §12.1): its messages,
//! thoughts and tool calls are `han.item/1` items of the `Session` view (masked), the board's idle step shows
//! the start of its last message, and the first prompt frames the task: the ask rides as a data block, and the
//! prompt says how to ask for more context. The agent is `han-fake-agent`; the node is a real one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_node::api::{
    CallResult, Command, ItemBody, MessageRole, Query, Row, SessionPart, SessionState, ToolStatus,
    Window,
};
use han_vcs::GitVcs;

mod common;
use common::*;

const KEY: &str = "AKIAIOSFODNN7EXAMPLE";

fn log_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_agents_messages_thoughts_and_tool_calls_reach_the_node_masked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = origin(root);
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let space = setup_space(&world, &alice, &[(CODER, &coder)], &[]).await;

    let ask = "Work on github:o/r#12: Fix the login\n\n----- BEGIN ISSUE TEXT (untrusted, from the issue's author) -----\n\
               The login check is missing.\n----- END ISSUE TEXT -----";
    let CallResult::Receipt { id: task, .. } = call(
        &alice,
        Command::TaskCreate {
            space: space.clone(),
            to: vec![CODER.into()],
            ask: ask.into(),
            external: Some(ISSUE.into()),
        },
    )
    .await
    else {
        panic!("a task")
    };
    until("the coder sees its task", 15, || {
        rows(
            &coder,
            Query::Tasks {
                space: space.clone(),
                assignee: Some(CODER.into()),
                window: Window::default(),
            },
        )
        .iter()
        .any(|r| matches!(r, Row::Task(t) if t.task == task && t.ask.is_some()))
    })
    .await;

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/items-session.json");
    let log = root.join("agent-log.jsonl");
    let mut driver = fake_driver(&script);
    driver.env.push((
        "HAN_FAKE_AGENT_LOG".into(),
        log.to_string_lossy().into_owned(),
    ));
    let taint_db = root.join("coder").join("taint.db");
    std::fs::create_dir_all(taint_db.parent().unwrap()).unwrap();
    let cfg = HostConfig {
        space: space.clone(),
        agent: CODER.into(),
        operator: Some(ALICE.into()),
        driver,
        repo: RepoConfig {
            name: "local:origin".into(),
            source: remote.to_string_lossy().into_owned(),
            base: "main".into(),
            default_branch: "main".into(),
        },
        issue: Some(ISSUE.into()),
        task: None,
        executor: EXEC.into(),
        session_dir: root.join("coder").join("s1"),
        taint_db,
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: "none".into(),
            allow_advisory: true,
            ro: vec![script.clone()],
            login_dir: None,
            allow_hosts: Vec::new(),
            test_loopback_upstream: false,
            test_roots: Vec::new(),
        },
        shim: PathBuf::from(env!("CARGO_BIN_EXE_han-agent-shim")),
        node: PathBuf::from("node"),
        adapter_dir: PathBuf::new(),
        model_key: None,
        turn_timeout: Duration::from_secs(60),
        exclusion: Vec::new(),
    };
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let host = tokio::spawn(run_session(cfg, deps, rx));

    // The board shows what the agent waits for, not just that it waits.
    let board = || {
        rows(
            &alice,
            Query::Board {
                space: space.clone(),
                group: Default::default(),
                collapsed: vec![],
                window: Window::default(),
            },
        )
        .into_iter()
        .find_map(|r| match r {
            Row::Board(b)
                if b.agent == CODER
                    && b.state == SessionState::Idle
                    && b.step.starts_with("waiting for input") =>
            {
                Some(b)
            }
            _ => None,
        })
    };
    let mut row = None;
    for _ in 0..600 {
        row = board();
        if row.is_some() {
            break;
        }
        assert!(!host.is_finished(), "the session ended early");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let row = row.expect("the session goes idle");
    assert_eq!(
        row.step,
        "waiting for input: Which branch should I target? I can use main or the release branch."
    );

    // The Session view's items: the thought, the messages, the tool call and its output.
    let items = || -> Vec<han_node::api::ItemRow> {
        rows(
            &alice,
            Query::Session {
                id: row.session.clone(),
                part: SessionPart::Items,
                window: Window::default(),
            },
        )
        .into_iter()
        .filter_map(|r| match r {
            Row::Item(i) => Some(i),
            _ => None,
        })
        .collect()
    };
    until("the final message is an item", 30, || {
        items().iter().any(|i| {
            matches!(&i.item, ItemBody::Message { text, done: true, .. } if text.starts_with("Which branch"))
        })
    })
    .await;
    let items = items();
    let dump = format!("{items:?}");
    assert!(!dump.contains(KEY), "a secret reached the node: {dump}");

    let reasoning: Vec<&str> = items
        .iter()
        .filter_map(|i| match &i.item {
            ItemBody::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasoning,
        ["The issue wants a check; I should look at the config first."]
    );

    let messages: Vec<(&str, bool, bool)> = items
        .iter()
        .filter_map(|i| match &i.item {
            ItemBody::Message {
                role: MessageRole::Agent,
                text,
                done,
                masked,
            } => Some((text.as_str(), *done, !masked.is_empty())),
            _ => None,
        })
        .collect();
    assert_eq!(
        messages,
        [
            (
                "Looking at the config. The key [masked:aws-access-key] is in it.",
                true,
                true
            ),
            (
                "Which branch should I target?\nI can use main or the release branch.",
                true,
                false
            ),
        ]
    );

    let tool = items
        .iter()
        .find_map(|i| match &i.item {
            ItemBody::ToolCall {
                tool_call_id,
                title,
                status,
                content,
                ..
            } => Some((
                tool_call_id.clone(),
                title.clone(),
                *status,
                content.clone(),
            )),
            _ => None,
        })
        .expect("a tool call item");
    assert_eq!(tool.0, "toolu_r1");
    assert_eq!(tool.1.as_deref(), Some("read config.toml"));
    assert_eq!(tool.2, ToolStatus::Completed);
    assert_eq!(tool.3[0].text, "key = [masked:aws-access-key]\nmode = fast");
    assert!(!tool.3[0].masked.is_empty());

    // The first prompt: the framing in the text, the ask in a data block after it.
    let first = log_lines(&log)
        .into_iter()
        .find(|v| v["event"] == "prompt")
        .expect("a prompt")["prompt"]
        .clone();
    let blocks = first.as_array().expect("blocks");
    let text = blocks[0]["text"].as_str().unwrap();
    assert!(text.contains(ISSUE), "{text}");
    assert!(text.contains("team_read"), "{text}");
    assert!(text.contains("end your turn with your question"), "{text}");
    assert!(
        text.contains("steering"),
        "the user answers by steering: {text}"
    );
    assert!(!text.contains("The login check is missing"), "{text}");
    let data = blocks[1]["text"].as_str().unwrap();
    assert!(data.contains("<han-data id="), "{data}");
    assert!(data.contains("The login check is missing."), "{data}");
    assert!(data.contains("not an instruction"), "{data}");

    stop.send(true).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(30), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(end.state, SessionState::Done);
}
