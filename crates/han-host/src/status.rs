//! Status derivation (PLAN §5 `han.status/1`; spec/profiles/han.status-1.md; SR-34, SR-59, SR-99).
//!
//! The host folds what it sees into one [`StatusBody`]: the step and tool from ACP tool calls, the plan from
//! ACP plan updates, files and hunks from the workspace diff, test counts parsed from tool output, tokens and
//! cost from the proxy (API-key mode) or ACP `_meta` / `usage_update` (subscription mode), the layer-1 summary
//! from HAN tool calls, and the sandbox. Free text is length-bounded (SR-59) and masked before it is published
//! (SR-52, by the caller's [`crate::mask`]).

use han_node::api::{FileTouch, PlanItem, Sandbox, SessionState, StatusBody, Tests, Tokens};

/// Bounds on free-text fields (SR-59).
pub const MAX_STEP: usize = 200;
/// Bound on `tool`.
pub const MAX_TOOL: usize = 120;
/// Bound on `summary`.
pub const MAX_SUMMARY: usize = 1000;
/// Bound on each plan item's text, and on the number of items.
pub const MAX_PLAN_TEXT: usize = 200;
/// At most this many plan items.
pub const MAX_PLAN_ITEMS: usize = 50;
/// At most this many files.
pub const MAX_FILES: usize = 500;
/// At most this many hunks per file.
pub const MAX_HUNKS: usize = 200;

/// Cuts `s` to at most `max` characters, on a character boundary, with an ellipsis when cut.
pub fn bound(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// How much of the agent's last message the idle step shows.
pub const WAITING_PREVIEW: usize = 80;

/// The step of a session that waits for input after a turn: "waiting for input: <start of the last message>".
/// Whitespace runs collapse to one space.
pub fn waiting_step(last_message: &str) -> String {
    let flat = last_message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.is_empty() {
        return "waiting for input".into();
    }
    let mut start: String = flat.chars().take(WAITING_PREVIEW).collect();
    if flat.chars().count() > WAITING_PREVIEW {
        start.push('…');
    }
    format!("waiting for input: {start}")
}

/// What the host saw, as the deriver consumes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// A turn started; `step` names what drives it.
    TurnStarted {
        /// For example "working on github:o/r#12".
        step: String,
    },
    /// The start of the agent's latest message (masked), for the step shown while it waits for input.
    LastMessage(String),
    /// A tool call started or changed.
    Tool {
        /// Its title.
        title: String,
        /// Its ACP kind, if any.
        kind: Option<String>,
    },
    /// A tool call ended; its output text (for test counts).
    ToolDone {
        /// Output text, if any.
        output: Option<String>,
    },
    /// The agent's plan.
    Plan(Vec<PlanItem>),
    /// Files and hunks from the workspace diff.
    Files(Vec<FileTouch>),
    /// A HAN tool call's `summary` (layer 1).
    Summary(String),
    /// Token totals (replace, not add).
    Tokens(Tokens),
    /// A proposal waits for approval.
    WaitingApproval,
    /// A proposal is signed but the home has not accepted it yet (it sits in the node's outbox); nobody can
    /// approve it until it does.
    ProposalQueued,
    /// The home refused a proposal, or delivery gave up; the reason is shown.
    ProposalRefused(String),
    /// The proposal was decided or ran; back to work.
    Resumed,
    /// The turn ended normally.
    TurnEnded,
    /// The turn or the agent failed.
    Failed(String),
    /// The work is finished.
    Done,
}

/// The step shown while a proposal waits in the node's outbox.
pub const QUEUED_STEP: &str = "queued: waiting for the home to accept the proposal";

/// Folds signals into the status of one session.
#[derive(Clone, Debug)]
pub struct StatusDeriver {
    body: StatusBody,
    tests: Option<Tests>,
    /// The start of the agent's latest message this turn.
    last_message: Option<String>,
}

impl StatusDeriver {
    /// A deriver for a session; `base` is the workspace's base commit.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: &str,
        agent: &str,
        harness: &str,
        delegate_of: Option<String>,
        issue: Option<String>,
        repo: Option<String>,
        base: Option<String>,
        sandbox: Sandbox,
    ) -> StatusDeriver {
        StatusDeriver {
            body: StatusBody {
                session: session.to_owned(),
                agent: agent.to_owned(),
                delegate_of,
                harness: harness.to_owned(),
                issue,
                repo,
                base,
                state: SessionState::Idle,
                step: "starting".into(),
                tool: None,
                plan: Vec::new(),
                files: Vec::new(),
                tests: None,
                tokens: None,
                summary: None,
                sandbox: Some(sandbox),
                seq: 0,
                at: String::new(),
            },
            tests: None,
            last_message: None,
        }
    }

    /// Applies one signal. Returns whether the status changed.
    pub fn apply(&mut self, signal: Signal) -> bool {
        let before = self.body.clone();
        let b = &mut self.body;
        match signal {
            Signal::TurnStarted { step } => {
                b.state = SessionState::Working;
                b.step = bound(&step, MAX_STEP);
                b.tool = None;
                self.last_message = None;
            }
            Signal::LastMessage(m) => self.last_message = Some(m),
            Signal::Tool { title, kind } => {
                b.state = SessionState::Working;
                b.step = bound(&title, MAX_STEP);
                b.tool = Some(bound(kind.as_deref().unwrap_or(&title), MAX_TOOL));
            }
            Signal::ToolDone { output } => {
                if let Some(t) = output.as_deref().and_then(parse_tests) {
                    self.tests = Some(t);
                    b.tests = Some(t);
                }
            }
            Signal::Plan(items) => {
                b.plan = items
                    .into_iter()
                    .take(MAX_PLAN_ITEMS)
                    .map(|p| PlanItem {
                        text: bound(&p.text, MAX_PLAN_TEXT),
                        done: p.done,
                    })
                    .collect();
            }
            Signal::Files(files) => {
                b.files = files
                    .into_iter()
                    .take(MAX_FILES)
                    .map(|mut f| {
                        f.hunks.truncate(MAX_HUNKS);
                        f
                    })
                    .collect();
            }
            Signal::Summary(s) => b.summary = Some(bound(&s, MAX_SUMMARY)),
            Signal::Tokens(t) => b.tokens = Some(t),
            Signal::WaitingApproval => b.state = SessionState::WaitingApproval,
            Signal::ProposalQueued => {
                b.state = SessionState::Blocked;
                b.step = QUEUED_STEP.into();
            }
            Signal::ProposalRefused(why) => {
                b.state = SessionState::Blocked;
                b.step = bound(&format!("proposal refused: {why}"), MAX_STEP);
            }
            Signal::Resumed => b.state = SessionState::Working,
            Signal::TurnEnded => {
                // A proposal that waits (for the home, or for an approver) or was refused keeps the state and,
                // when it is not an approval wait, its step: they say more than "waiting for input".
                if !matches!(
                    b.state,
                    SessionState::WaitingApproval | SessionState::Blocked
                ) {
                    b.state = SessionState::Idle;
                }
                b.tool = None;
                let step = match self.last_message.take() {
                    Some(m) => waiting_step(&m),
                    None => "waiting for input".into(),
                };
                if b.state != SessionState::Blocked {
                    b.step = step;
                }
            }
            Signal::Failed(why) => {
                b.state = SessionState::Failed;
                b.step = bound(&format!("failed: {why}"), MAX_STEP);
                b.tool = None;
            }
            Signal::Done => {
                b.state = SessionState::Done;
                b.tool = None;
                b.step = "done".into();
            }
        }
        *b != before
    }

    /// The current status, `seq` 0 so the node numbers it.
    pub fn snapshot(&self, at: &str) -> StatusBody {
        let mut b = self.body.clone();
        b.seq = 0;
        b.at = at.to_owned();
        b
    }

    /// The current state.
    pub fn state(&self) -> SessionState {
        self.body.state
    }
}

/// Test counts from common runners' summaries: cargo (`test result: ok. 3 passed; 1 failed`), pytest
/// (`3 passed, 1 failed`), jest/vitest (`Tests: 1 failed, 3 passed`), go (`ok`/`FAIL` lines). Sums several
/// cargo `test result` lines.
pub fn parse_tests(output: &str) -> Option<Tests> {
    use std::sync::LazyLock;
    static CARGO: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed").expect("regex")
    });
    static PASSED: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(\d+) passed").expect("regex"));
    static FAILED: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(\d+) failed").expect("regex"));
    let mut found = false;
    let (mut pass, mut fail) = (0u32, 0u32);
    for c in CARGO.captures_iter(output) {
        found = true;
        pass += c[1].parse::<u32>().unwrap_or(0);
        fail += c[2].parse::<u32>().unwrap_or(0);
    }
    if found {
        return Some(Tests { pass, fail });
    }
    // pytest / jest / vitest: the last summary line that mentions passed or failed.
    let line = output
        .lines()
        .rev()
        .find(|l| PASSED.is_match(l) || FAILED.is_match(l))?;
    let n = |re: &regex::Regex| {
        re.captures(line)
            .and_then(|c| c[1].parse::<u32>().ok())
            .unwrap_or(0)
    };
    Some(Tests {
        pass: n(&PASSED),
        fail: n(&FAILED),
    })
}

/// Token totals from claude-agent-acp's `_meta.quota.token_count` (spike Q6), summed over models.
pub fn tokens_from_meta(meta: &serde_json::Value) -> Option<Tokens> {
    let q = meta.get("quota")?;
    let mut t = Tokens::default();
    let mut add = |tc: &serde_json::Value| {
        let g = |k: &str| tc.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
        t.input += g("inputTokens") + g("cachedInputTokens") + g("cachedWriteTokens");
        t.out += g("outputTokens");
    };
    match q.get("token_count") {
        Some(tc) if tc.is_object() && tc.get("inputTokens").is_some() => add(tc),
        Some(serde_json::Value::Object(per_model)) => per_model.values().for_each(&mut add),
        _ => {}
    }
    Some(t)
}

/// The sandbox line the board shows (SR-99), and the advisory text §8.4 asks for.
pub fn sandbox_note(s: &Sandbox, gaps: &[String]) -> String {
    use han_node::api::{Fidelity, SandboxBackend};
    let backend = match s.backend {
        SandboxBackend::Mxc => "mxc",
        SandboxBackend::Bwrap => "bwrap",
        SandboxBackend::None => "none",
    };
    match s.fidelity {
        Fidelity::Advisory => format!("guarded: advisory (sandbox {backend})"),
        Fidelity::Enforced if gaps.is_empty() => format!("sandbox {backend}: enforced"),
        Fidelity::Enforced => format!("sandbox {backend}: enforced; gaps: {}", gaps.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use han_node::api::{Fidelity, SandboxBackend};

    fn d() -> StatusDeriver {
        StatusDeriver::new(
            "e1",
            "@coder:a.example",
            "pi",
            None,
            Some("github:o/r#1".into()),
            Some("github:o/r".into()),
            Some("abc".into()),
            Sandbox {
                backend: SandboxBackend::Mxc,
                fidelity: Fidelity::Enforced,
            },
        )
    }

    #[test]
    fn a_turn_moves_through_working_tool_and_idle() {
        let mut s = d();
        assert!(s.apply(Signal::TurnStarted {
            step: "working on github:o/r#1".into()
        }));
        assert_eq!(s.state(), SessionState::Working);
        s.apply(Signal::Tool {
            title: "cargo test".into(),
            kind: Some("execute".into()),
        });
        s.apply(Signal::ToolDone {
            output: Some("running 4 tests\ntest result: ok. 3 passed; 1 failed; 0 ignored".into()),
        });
        let snap = s.snapshot("2026-10-13T00:00:00Z");
        assert_eq!(snap.step, "cargo test");
        assert_eq!(snap.tool.as_deref(), Some("execute"));
        assert_eq!(snap.tests, Some(Tests { pass: 3, fail: 1 }));
        assert_eq!(snap.seq, 0);
        s.apply(Signal::TurnEnded);
        assert_eq!(s.state(), SessionState::Idle);
        assert!(!s.apply(Signal::TurnEnded), "no change, no item");
    }

    #[test]
    fn the_idle_step_shows_the_start_of_the_last_message() {
        let mut s = d();
        s.apply(Signal::TurnStarted { step: "w".into() });
        s.apply(Signal::LastMessage(format!(
            "Which
branch should I

 target? {}",
            "x".repeat(200)
        )));
        s.apply(Signal::TurnEnded);
        let step = s.snapshot("t").step;
        assert!(step.starts_with("waiting for input: Which branch should I target? xxx"));
        assert!(step.ends_with('…'));
        assert_eq!(step.chars().count(), "waiting for input: ".len() + 81);
        // The next turn that says nothing falls back to the plain step.
        s.apply(Signal::TurnStarted { step: "w".into() });
        s.apply(Signal::TurnEnded);
        assert_eq!(s.snapshot("t").step, "waiting for input");
    }

    #[test]
    fn free_text_is_bounded() {
        let mut s = d();
        s.apply(Signal::Summary("x".repeat(5000)));
        s.apply(Signal::Tool {
            title: "y".repeat(5000),
            kind: None,
        });
        let snap = s.snapshot("t");
        assert!(snap.summary.unwrap().chars().count() <= MAX_SUMMARY);
        assert!(snap.step.chars().count() <= MAX_STEP);
        assert!(snap.tool.unwrap().chars().count() <= MAX_TOOL);
    }

    #[test]
    fn waiting_approval_survives_the_end_of_the_turn() {
        let mut s = d();
        s.apply(Signal::TurnStarted { step: "w".into() });
        s.apply(Signal::WaitingApproval);
        s.apply(Signal::TurnEnded);
        assert_eq!(s.state(), SessionState::WaitingApproval);
        s.apply(Signal::Resumed);
        assert_eq!(s.state(), SessionState::Working);
    }

    #[test]
    fn a_queued_proposal_is_not_waiting_approval_and_a_refusal_shows_its_reason() {
        let mut s = d();
        s.apply(Signal::TurnStarted { step: "w".into() });
        s.apply(Signal::ProposalQueued);
        s.apply(Signal::TurnEnded);
        assert_ne!(s.state(), SessionState::WaitingApproval);
        assert_eq!(s.snapshot("t").step, QUEUED_STEP);
        s.apply(Signal::WaitingApproval);
        assert_eq!(s.state(), SessionState::WaitingApproval);
        s.apply(Signal::ProposalRefused("@exec:h has no profile".into()));
        s.apply(Signal::TurnEnded);
        assert_eq!(s.state(), SessionState::Blocked);
        assert!(s.snapshot("t").step.contains("has no profile"));
    }

    #[test]
    fn test_summaries_of_common_runners() {
        assert_eq!(
            parse_tests("==== 12 passed, 2 failed in 1.2s ===="),
            Some(Tests { pass: 12, fail: 2 })
        );
        assert_eq!(
            parse_tests("Tests:       1 failed, 30 passed, 31 total"),
            Some(Tests { pass: 30, fail: 1 })
        );
        assert_eq!(
            parse_tests(
                "test result: ok. 2 passed; 0 failed;\n…\ntest result: FAILED. 1 passed; 3 failed;"
            ),
            Some(Tests { pass: 3, fail: 3 })
        );
        assert_eq!(parse_tests("compiling…"), None);
    }

    #[test]
    fn tokens_from_claude_meta() {
        let one = serde_json::json!({"quota": {"token_count": {"inputTokens": 6, "cachedInputTokens": 70, "cachedWriteTokens": 18, "outputTokens": 582}}});
        assert_eq!(
            tokens_from_meta(&one),
            Some(Tokens {
                input: 94,
                out: 582,
                cost: 0
            })
        );
        let per = serde_json::json!({"quota": {"token_count": {"opus": {"inputTokens": 2, "outputTokens": 39}, "haiku": {"inputTokens": 1434, "outputTokens": 99}}}});
        assert_eq!(
            tokens_from_meta(&per),
            Some(Tokens {
                input: 1436,
                out: 138,
                cost: 0
            })
        );
    }

    #[test]
    fn advisory_sessions_say_so() {
        let s = Sandbox {
            backend: SandboxBackend::None,
            fidelity: Fidelity::Advisory,
        };
        assert_eq!(sandbox_note(&s, &[]), "guarded: advisory (sandbox none)");
    }
}
