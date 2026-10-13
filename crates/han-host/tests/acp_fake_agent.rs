//! The ACP client against `han-fake-agent`: recorded-session replay (golden event sequences), permission
//! policy per mode with the reject reason in the next prompt, hidden provider errors, crashes, timeouts and
//! other edge cases. No Node, no network.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::{FutureExt, StreamExt};
use han_host::acp::{
    AcpDriver, AcpError, AgentDriver, AgentEvent, GateFidelity, McpServerSpec, Mode,
    PermissionPolicy, SandboxPolicy, SessionConfig, TokenCounts, TurnInput,
};
use serde_json::Value;

const FAKE: &str = env!("CARGO_BIN_EXE_han-fake-agent");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

struct Run {
    driver: AcpDriver,
    events: futures::stream::BoxStream<'static, AgentEvent>,
    dir: tempfile::TempDir,
    log: PathBuf,
}

impl Run {
    async fn new(args: &[&str], policy: Arc<dyn PermissionPolicy>) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("agent.log");
        let mut cmd = tokio::process::Command::new(FAKE);
        cmd.args(args)
            .arg("--log")
            .arg(&log)
            .current_dir(dir.path());
        let mut driver = AcpDriver::spawn(cmd, policy).await.unwrap();
        let events = driver.events();
        Run {
            driver,
            events,
            dir,
            log,
        }
    }

    fn worktree(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn cfg(&self, mcp: bool) -> SessionConfig {
        SessionConfig {
            cwd: self.worktree(),
            mcp_servers: if mcp {
                vec![McpServerSpec::Stdio {
                    name: "han".into(),
                    command: PathBuf::from(FAKE),
                    args: vec!["mcp-echo".into()],
                    env: vec![],
                }]
            } else {
                vec![]
            },
            ..SessionConfig::default()
        }
    }

    fn drain(&mut self) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Some(Some(e)) = self.events.next().now_or_never() {
            out.push(e);
        }
        out
    }

    fn log_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn prompts(&self) -> Vec<Vec<String>> {
        self.log_lines()
            .iter()
            .filter(|v| v["event"] == "prompt")
            .map(|v| {
                v["prompt"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b["text"].as_str().unwrap_or("").to_owned())
                    .collect()
            })
            .collect()
    }
}

fn policy(dir: &Path) -> Arc<dyn PermissionPolicy> {
    Arc::new(SandboxPolicy {
        worktree: dir.to_path_buf(),
    })
}

/// One line per event, the deterministic parts only.
fn summary(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Output { text } => Some(format!("output {text}")),
            AgentEvent::Thought { text } => Some(format!("thought {text}")),
            AgentEvent::ToolCall {
                id,
                title,
                kind,
                locations,
                ..
            } => Some(format!(
                "tool_call {id} {} {title} {locations:?}",
                kind.as_deref().unwrap_or("-")
            )),
            AgentEvent::ToolCallUpdate {
                id, status, output, ..
            } => Some(format!(
                "update {id} {} {}",
                status.as_deref().unwrap_or("-"),
                output.as_deref().unwrap_or("").replace('\n', "|")
            )),
            AgentEvent::Plan { entries } => Some(format!("plan {entries:?}")),
            AgentEvent::Usage {
                used, size, cost, ..
            } => Some(format!("usage {used}/{size} {cost:?}")),
            AgentEvent::TurnEnded { stop_reason, .. } => Some(format!("end {stop_reason}")),
            AgentEvent::PermissionDecided { title, allowed, .. } => {
                Some(format!("permission {title} allowed={allowed}"))
            }
            AgentEvent::Stderr { .. } | AgentEvent::Exited { .. } | AgentEvent::Error { .. } => {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn replays_a_recorded_pi_session_deterministically() {
    let script = fixture("pi-session.json");
    let mut golden: Option<Vec<String>> = None;
    for _ in 0..2 {
        let tmp = tempfile::tempdir().unwrap();
        let mut run = Run::new(&["replay", script.to_str().unwrap()], policy(tmp.path())).await;
        let started = run.driver.start(run.cfg(true)).await.unwrap();
        assert_eq!(started.agent_name.as_deref(), Some("pi-acp"));
        assert_eq!(started.agent_version.as_deref(), Some("0.0.34"));
        // pi offers no ask-before-acting mode: its prompts gate nothing.
        assert_eq!(started.gate_fidelity, GateFidelity::PromptOnly);
        let t1 = run
            .driver
            .turn(
                TurnInput {
                    text: "hi, which tools do you have?".into(),
                    data_blocks: vec![],
                },
                Mode::Auto,
            )
            .await
            .unwrap();
        assert!(t1.produced_output && !t1.suspect_failure);
        let t2 = run
            .driver
            .turn(
                TurnInput {
                    text: "fix the token check, then push".into(),
                    data_blocks: vec!["<han-data id=\"r1\">{\"han_data\":{}}</han-data>".into()],
                },
                Mode::Auto,
            )
            .await
            .unwrap();
        assert_eq!(t2.tool_calls, 3);
        let events = summary(&run.drain());
        let wt = run.worktree();
        assert_eq!(
            std::fs::read_to_string(wt.join("src/auth/token.rs")).unwrap(),
            "pub fn check(t: &str) -> bool {\n    t.len() >= 8\n}\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("made.txt")).unwrap(),
            "hi-from-tool\n"
        );
        // The data block travels as its own content block after the instruction.
        let prompts = run.prompts();
        assert_eq!(prompts[1].len(), 2);
        assert!(prompts[1][1].starts_with("<han-data"));
        match &golden {
            None => golden = Some(events),
            Some(g) => assert_eq!(g, &events, "replay is deterministic"),
        }
    }
    let expected: Vec<String> = [
        "usage 0/200000 None",
        "output Hello. Offered tools: read, bash, edit, write, mcp__han__propose",
        "usage 1765/200000 None",
        "end end_turn",
        "plan [(\"Fix the token check in auth\", \"in_progress\"), (\"Push the branch\", \"pending\")]",
        "tool_call toolu_edit1 edit write src/auth/token.rs [\"src/auth/token.rs\"]",
        "update toolu_edit1 completed Wrote src/auth/token.rs",
        "tool_call toolu_bash1 execute echo hi-from-tool > made.txt []",
        "update toolu_bash1 in_progress made.txt|",
        "update toolu_bash1 completed ",
        "tool_call toolu_prop1 - mcp__han__propose []",
        "update toolu_prop1 in_progress ",
        "update toolu_prop1 completed Proposed as effect E-1 (status: waiting-approval): Push the auth fix branch",
        "usage 2007/200000 None",
        "output The push is proposed and waits for approval.",
        "end end_turn",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(golden.unwrap(), expected);
}

#[tokio::test]
async fn auto_turn_allows_and_reads_quota_meta() {
    let script = fixture("claude-session.json");
    let tmp = tempfile::tempdir().unwrap();
    let mut run = Run::new(&["replay", script.to_str().unwrap()], policy(tmp.path())).await;
    let started = run.driver.start(run.cfg(true)).await.unwrap();
    assert_eq!(started.gate_fidelity, GateFidelity::EveryCall);
    assert!(started.mcp_http);
    let end = run
        .driver
        .turn(
            TurnInput {
                text: "send the build notification".into(),
                data_blocks: vec![],
            },
            Mode::Auto,
        )
        .await
        .unwrap();
    let counts = TokenCounts::from_meta(end.meta.as_ref().unwrap()).unwrap();
    assert_eq!(
        (counts.input, counts.cached_input, counts.output),
        (6, 70787, 582)
    );
    let ev = summary(&run.drain());
    assert!(ev.contains(
        &"permission curl -X POST https://hooks.example.com/build allowed=true".to_owned()
    ));
    assert!(ev.contains(&"update toolu_02 completed sent".to_owned()));
    assert!(!ev.iter().any(|e| e.contains("mcp__han__propose")));
    assert!(ev.contains(&"usage 30254/1000000 Some((0.1714134, \"USD\"))".to_owned()));
    // No mode switch for an auto turn that is already in the session's mode.
    assert!(!run.log_lines().iter().any(|v| v["event"] == "set_mode"));
}

#[tokio::test]
async fn strict_turn_rejects_world_calls_and_the_reason_reaches_the_next_prompt() {
    let script = fixture("claude-session.json");
    let tmp = tempfile::tempdir().unwrap();
    let mut run = Run::new(&["replay", script.to_str().unwrap()], policy(tmp.path())).await;
    run.driver
        .start(SessionConfig {
            auto_mode: Some("acceptEdits".into()),
            ..run.cfg(true)
        })
        .await
        .unwrap();
    run.driver
        .turn(
            TurnInput {
                text: "send the build notification".into(),
                data_blocks: vec![],
            },
            Mode::Strict,
        )
        .await
        .unwrap();
    let ev = run.drain();
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::PermissionDecided { allowed: false, reason: Some(r), .. } if r.contains("stricter mode"))));
    let s = summary(&ev);
    assert!(s.contains(&"update toolu_02 failed User refused permission to run tool".to_owned()));
    assert!(
        s.iter()
            .any(|e| e.starts_with("update toolu_03 completed Proposed as effect E-1"))
    );
    run.driver
        .turn(
            TurnInput {
                text: "status?".into(),
                data_blocks: vec![],
            },
            Mode::Auto,
        )
        .await
        .unwrap();
    let prompts = run.prompts();
    assert!(
        prompts[1][0].starts_with(
            "HAN: the tool call 'curl -X POST https://hooks.example.com/build' was not run: this turn runs in HAN's stricter mode"
        ),
        "{:?}",
        prompts[1]
    );
    assert!(prompts[1][0].ends_with("status?"));
    // Strict switched to the ask-before-acting mode, auto switched back.
    let modes: Vec<String> = run
        .log_lines()
        .iter()
        .filter(|v| v["event"] == "set_mode")
        .map(|v| v["request"]["modeId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(modes, vec!["default", "acceptEdits"]);
    // The permission response carried the reason in _meta too (it is dropped by real adapters).
    let perm = run
        .log_lines()
        .into_iter()
        .find(|v| v["event"] == "permission")
        .unwrap();
    assert_eq!(perm["response"]["outcome"]["optionId"], "reject");
    assert!(perm["response"]["_meta"]["han"]["reason"].is_string());
}

#[tokio::test]
async fn a_turn_with_nothing_in_it_is_a_suspect_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let mut run = Run::new(&["edge", "hidden-error"], policy(tmp.path())).await;
    run.driver.start(run.cfg(false)).await.unwrap();
    let probe_dir = run.worktree().join("sessions");
    std::fs::create_dir_all(&probe_dir).unwrap();
    std::fs::write(
        probe_dir.join("s.jsonl"),
        "{\"message\":{\"stopReason\":\"error\",\"errorMessage\":\"400 out of extra usage\"}}\n",
    )
    .unwrap();
    let mut driver = run.driver.with_failure_probe(Arc::new(move || {
        han_host::driver::pi_session_error(&probe_dir)
    }));
    let end = driver
        .turn(
            TurnInput {
                text: "go".into(),
                data_blocks: vec![],
            },
            Mode::Auto,
        )
        .await
        .unwrap();
    assert!(end.suspect_failure);
    assert_eq!(
        end.provider_error.as_deref(),
        Some("400 out of extra usage")
    );
}

#[tokio::test]
async fn a_crash_mid_turn_ends_the_turn_and_reports_the_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut run = Run::new(&["edge", "crash"], policy(tmp.path())).await;
    run.driver.start(run.cfg(false)).await.unwrap();
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        run.driver.turn(
            TurnInput {
                text: "go".into(),
                data_blocks: vec![],
            },
            Mode::Auto,
        ),
    )
    .await
    .expect("no hang");
    assert!(matches!(r, Err(AcpError::Closed)), "{r:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let ev = run.drain();
    assert!(
        ev.iter()
            .any(|e| matches!(e, AgentEvent::Exited { code: Some(1) })),
        "{ev:?}"
    );
    // Later turns fail fast.
    let again = run
        .driver
        .turn(
            TurnInput {
                text: "again".into(),
                data_blocks: vec![],
            },
            Mode::Auto,
        )
        .await;
    assert!(again.is_err());
}

#[tokio::test]
async fn a_stuck_tool_call_times_out_and_cancels() {
    let tmp = tempfile::tempdir().unwrap();
    let run = Run::new(&["edge", "stuck-tool"], policy(tmp.path())).await;
    let cfg = run.cfg(false);
    let log = run.log.clone();
    let Run {
        driver,
        mut events,
        dir: _dir,
        ..
    } = run;
    let mut driver = driver.with_turn_timeout(Duration::from_millis(500));
    driver.start(cfg).await.unwrap();
    let r = driver
        .turn(
            TurnInput {
                text: "go".into(),
                data_blocks: vec![],
            },
            Mode::Auto,
        )
        .await;
    assert!(matches!(r, Err(AcpError::Timeout)), "{r:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = std::fs::read_to_string(log).unwrap();
    assert!(text.contains("\"cancel\""), "{text}");
    let mut saw = false;
    while let Some(Some(e)) = events.next().now_or_never() {
        saw |= matches!(e, AgentEvent::ToolCall { ref id, .. } if id == "stuck");
    }
    assert!(saw);
}

#[tokio::test]
async fn edge_cases_complete_without_hanging() {
    for case in [
        "invalid-json",
        "huge-output",
        "slow",
        "permission-storm",
        "write-outside",
        "network-permission",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let mut run = Run::new(&["edge", case], policy(tmp.path())).await;
        run.driver.start(run.cfg(false)).await.unwrap();
        let r = tokio::time::timeout(
            Duration::from_secs(30),
            run.driver.turn(
                TurnInput {
                    text: "go".into(),
                    data_blocks: vec![],
                },
                Mode::Strict,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{case} hung"));
        let ev = run.drain();
        match case {
            "invalid-json" => {
                // A line that is not JSON-RPC is dropped; the session goes on.
                assert!(r.is_ok(), "{case}: {r:?}");
                assert!(summary(&ev).contains(&"output still here".to_owned()));
            }
            "huge-output" => {
                let bytes: usize = ev
                    .iter()
                    .map(|e| match e {
                        AgentEvent::Output { text } => text.len(),
                        _ => 0,
                    })
                    .sum();
                assert_eq!(bytes, 256 * 4096);
            }
            "slow" => assert!(r.unwrap().produced_output),
            "permission-storm" => {
                let rejected = ev
                    .iter()
                    .filter(|e| matches!(e, AgentEvent::PermissionDecided { allowed: false, .. }))
                    .count();
                assert_eq!(rejected, 50);
            }
            "write-outside" => assert!(
                run.log_lines()
                    .iter()
                    .any(|v| v["event"] == "write_outside")
            ),
            "network-permission" => {
                let s = summary(&ev);
                assert!(s.contains(
                    &"permission curl -X POST https://example.com/hook allowed=false".to_owned()
                ));
                assert!(s.contains(
                    &"update net1 failed User refused permission to run tool".to_owned()
                ));
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn events_are_handed_out_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut run = Run::new(&["edge", "slow"], policy(tmp.path())).await;
    let mut second = run.driver.events();
    assert_eq!(second.next().await, None);
}
