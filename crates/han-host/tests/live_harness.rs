//! Live, opt-in: a real ACP adapter, launched the way a session launches it (the session proxy, the
//! OS sandbox, the agent's own login dir, HAN's MCP server through the in-sandbox stub), completes ACP
//! `initialize`, opens a session and answers one tiny prompt. It spends a little of the login's quota, so it
//! runs only when asked:
//!
//! ```text
//! HAN_LIVE_DRIVER=<driver.json> HAN_LIVE_NODE=<node> HAN_LIVE_ADAPTER=<adapter package directory>
//! HAN_LIVE_LOGIN=<agent login root> cargo test -p han-host --test live_harness -- --ignored
//! ```
//!
//! `han agent setup-harness` installs the adapter; `han agent login` makes the login. The test never
//! reads the login dir: it only binds it into the sandbox, as a session does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use han_host::acp::{
    AcpDriver, AgentDriver, AgentEvent, Mode, SandboxPolicy, SessionConfig, TurnInput,
};
use han_host::driver::{DriverConfig, builtin};
use han_host::glue::Egress;
use han_host::host::{HostConfig, RepoConfig, SandboxChoice};
use han_host::mask::Masker;
use han_mcp::{HostBridge, ProposeAnswer, ProposeRequest};
use han_node::api::{BoxFuture, NodeApi};
use han_vcs::Workspace;

mod common;
mod production_shim;
use common::*;

struct Bridge(Arc<dyn NodeApi>);

impl HostBridge for Bridge {
    fn propose(&self, _req: ProposeRequest) -> BoxFuture<'_, ProposeAnswer> {
        Box::pin(async {
            ProposeAnswer::Refused {
                reason: "not in this test".into(),
            }
        })
    }
    fn node(&self) -> Arc<dyn NodeApi> {
        Arc::new(han_host::host::AgentApi(self.0.clone()))
    }
    fn space(&self) -> String {
        "live".into()
    }
    fn session(&self) -> String {
        "live-harness".into()
    }
    fn tool_used(&self, _tool: &str, _summary: &str) {}
    fn summaries(&self, _session: &str) -> Vec<String> {
        Vec::new()
    }
}

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "uses a real harness login and subscription quota"]
async fn adapter_answers_a_prompt_inside_the_os_sandbox() {
    let node = var("HAN_LIVE_NODE")
        .or_else(|| var("HAN_LIVE_CLAUDE_NODE"))
        .expect("set HAN_LIVE_NODE");
    let adapter = var("HAN_LIVE_ADAPTER")
        .or_else(|| var("HAN_LIVE_CLAUDE_ADAPTER"))
        .expect("set HAN_LIVE_ADAPTER");
    let login = var("HAN_LIVE_LOGIN")
        .or_else(|| var("HAN_LIVE_CLAUDE_LOGIN"))
        .expect("set HAN_LIVE_LOGIN");
    let driver = var("HAN_LIVE_DRIVER")
        .map(|path| DriverConfig::load(&path).unwrap())
        .unwrap_or_else(|| builtin("claude").unwrap());
    let tmp = tempfile::Builder::new()
        .prefix("han-live-")
        .tempdir()
        .unwrap();
    let root = tmp.path();
    let world = World::new(root);
    let api: Arc<dyn NodeApi> = Arc::new(world.node(CODER, 2));
    let session_dir = root.join("session");
    let (wt, agent_dir, temp) = (
        session_dir.join("wt"),
        session_dir.join("agent"),
        session_dir.join("tmp"),
    );
    for d in [&wt, &agent_dir, &temp] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(wt.join("README.md"), "live check\n").unwrap();
    let workspace = Workspace {
        dir: wt.clone(),
        base: String::new(),
        branch: "han/live".into(),
        shared_store: None,
    };
    let cfg = HostConfig {
        space: "live".into(),
        agent: CODER.into(),
        operator: None,
        driver,
        repo: RepoConfig {
            name: "local:live".into(),
            source: wt.to_string_lossy().into_owned(),
            base: "HEAD".into(),
            default_branch: "main".into(),
        },
        issue: None,
        task: None,
        executor: EXEC.into(),
        session_dir: session_dir.clone(),
        taint_db: root.join("taint.db"),
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: std::env::var("HAN_LIVE_SANDBOX")
                .or_else(|_| std::env::var("HAN_LIVE_CLAUDE_SANDBOX"))
                .unwrap_or_else(|_| "auto".into()),
            allow_advisory: false,
            ro: Vec::new(),
            login_dir: Some(login),
            allow_hosts: Vec::new(),
            toolchains: Default::default(),
        },
        shim: production_shim::shim(),
        node,
        adapter_dir: adapter,
        model_key: None,
        budget: None,
        turn_timeout: Duration::from_secs(180),
        exclusion: Vec::new(),
        finish: Default::default(),
    };
    let token = format!("live-token-{}", std::process::id());
    let bridge: Arc<dyn HostBridge> = Arc::new(Bridge(api));
    let egress = Egress::start(&cfg, None, &bridge, &token, &workspace, &agent_dir, &temp)
        .await
        .expect("the proxy starts");
    let mut launched = egress
        .launch(
            &cfg,
            &agent_dir,
            &workspace,
            Arc::new(Masker::new(vec![token.clone()])),
        )
        .await
        .expect("the agent launches");
    eprintln!("sandbox: {:?}", launched.sandbox);
    let (stdin, stdout) = (
        std::mem::replace(&mut launched.stdin, Box::new(tokio::io::sink())),
        std::mem::replace(&mut launched.stdout, Box::new(tokio::io::empty())),
    );
    let run = async {
        let mut agent = AcpDriver::connect(
            stdin,
            stdout,
            Arc::new(SandboxPolicy {
                worktree: wt.clone(),
            }),
        )
        .await?
        .with_turn_timeout(cfg.turn_timeout);
        let started = agent
            .start(SessionConfig {
                cwd: wt.clone(),
                mcp_servers: launched.mcp_servers.clone(),
                model: None,
                meta: launched.session_meta.clone(),
                strict_mode: cfg.driver.strict_mode.clone(),
                auto_mode: cfg.driver.auto_mode.clone(),
            })
            .await?;
        eprintln!("started: agent version {:?}", started.agent_version);
        let mut events = agent.events();
        let end = agent
            .turn(
                TurnInput {
                    text: "Reply with the single word pong. Do not use any tools.".into(),
                    data_blocks: Vec::new(),
                },
                Mode::Auto,
            )
            .await?;
        let mut text = String::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(100), events.next()).await
        {
            if let AgentEvent::Output { text: chunk } = event {
                text.push_str(&chunk);
            }
        }
        assert_eq!(
            text.trim().to_ascii_lowercase(),
            "pong",
            "the model must answer, not just end a turn"
        );
        agent.shutdown().await;
        Ok::<_, han_host::acp::AcpError>(end)
    };
    let result = tokio::time::timeout(Duration::from_secs(240), run).await;
    let log = launched.stderr.as_mut().expect("stderr is captured");
    let path = log.path().to_path_buf();
    let lines = log.tail();
    let why = han_host::stderr_log::StderrLog::describe(&lines, &path, 8);
    if !matches!(result, Ok(Ok(_))) {
        // The log is masked and this test's own.
        eprintln!(
            "--- {} ---\n{}",
            path.display(),
            std::fs::read_to_string(&path).unwrap_or_default()
        );
    }
    let end = match result {
        Ok(Ok(end)) => end,
        Ok(Err(e)) => panic!("the session failed: {e}; {why}"),
        Err(_) => panic!("the session timed out; {why}"),
    };
    assert_eq!(end.stop_reason, "end_turn", "{end:?}; {why}");
    assert!(end.produced_output, "{end:?}; {why}");
    launched.kill();
}
