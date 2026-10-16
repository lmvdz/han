//! The executor end to end against a fake node: the gate, plan checks before claiming (SR-70), one claim and
//! one outcome per approval, lost replies, X-04 expiry, restarts (X-06), crash injection (SR-68) and the
//! instance lock (X-07).

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::*;
use han_exec::actions::GitPush;
use han_exec::{Action, LedgerError, Report};
use serde_json::json;

#[tokio::test]
async fn no_approval_no_claim_then_one_claim_and_one_outcome() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone()]);
    let p = rig.node.propose(&count_plan(1));
    let ex = rig.executor();

    let r = ex.step().await.unwrap();
    assert_eq!(r.waiting, vec![p.clone()]);
    assert_eq!(rig.node.claims(), 0, "no approval, no claim");

    rig.node.approve(&p);
    let r = ex.step().await.unwrap();
    assert_eq!(r.claimed, vec![p.clone()]);
    assert_eq!(
        r.reported,
        vec![(p.clone(), Report::new("succeeded", "yes"))]
    );
    for _ in 0..3 {
        ex.step().await.unwrap();
    }
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(rig.node.state().outcomes.len(), 1);
    assert_eq!(counter.world.load(Ordering::SeqCst), 1);
    assert_eq!(rig.node.row(&p).phase, "done");
}

#[tokio::test]
async fn plans_that_fail_their_checks_are_never_claimed() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone(), Arc::new(GitPush)]);
    let sha = "a".repeat(40);
    let digest = "d".repeat(64);
    rig.vcs.0.lock().unwrap().digest = digest.clone();
    let good = json!({"action": "git_push", "target": "repo", "args": {
        "branch": "han/1-agent", "base": sha, "from": null, "to": "b".repeat(40),
        "changeset": digest, "source": "/ws"}});
    // A digest that is not the plan's.
    let bad_digest = rig
        .node
        .propose_raw("count", &count_plan(1).to_string(), &"0".repeat(64));
    // An unknown field.
    let mut unknown = good.clone();
    unknown["args"]["force"] = json!(true);
    let unknown = rig.node.propose(&unknown);
    // A branch outside han/.
    let mut main = good.clone();
    main["args"]["branch"] = json!("main");
    let main = rig.node.propose(&main);
    // The approved changeset is not what `base..to` holds.
    let mut other = good.clone();
    other["args"]["changeset"] = json!("e".repeat(64));
    let other = rig.node.propose(&other);
    // The only good one.
    let ok = rig.node.propose(&good);
    for p in [&bad_digest, &unknown, &main, &other, &ok] {
        rig.node.approve(p);
    }
    let ex = rig.executor();
    let r = ex.step().await.unwrap();
    let refused: Vec<&str> = r.refused.iter().map(|(p, _)| p.as_str()).collect();
    for p in [&bad_digest, &unknown, &main, &other] {
        assert!(
            refused.contains(&p.as_str()),
            "{p} refused: {:?}",
            r.refused
        );
    }
    assert!(r.refused.iter().any(|(_, why)| why.contains("digest")));
    assert!(
        r.refused
            .iter()
            .any(|(_, why)| why.contains("out of scope"))
    );
    assert!(r.refused.iter().any(|(_, why)| why.contains("changeset")));
    assert_eq!(r.claimed, vec![ok.clone()], "only the good plan is claimed");
    assert_eq!(rig.vcs.0.lock().unwrap().lands, 1);
    assert_eq!(rig.node.row(&ok).phase, "done");
    // A plan that can never pass is refused once; a failed preflight is tried again only when the head
    // moves (X-09), and never claimed.
    let r = ex.step().await.unwrap();
    assert!(r.claimed.is_empty());
    assert!(
        r.refused.iter().all(|(p, _)| *p == other),
        "{:?}",
        r.refused
    );
    let r = ex.step().await.unwrap();
    assert!(r.refused.is_empty() && r.claimed.is_empty());
}

#[tokio::test]
async fn a_moved_remote_ref_fails_and_is_never_forced() {
    let rig = Rig::new(vec![Arc::new(GitPush)]);
    let digest = "d".repeat(64);
    {
        let mut v = rig.vcs.0.lock().unwrap();
        v.digest = digest.clone();
        v.remote = Some("c".repeat(40));
    }
    let p = rig
        .node
        .propose(&json!({"action": "git_push", "target": "repo", "args": {
        "branch": "han/1-agent", "base": "a".repeat(40), "from": null, "to": "b".repeat(40),
        "changeset": digest, "source": "/ws"}}));
    rig.node.approve(&p);
    let r = rig.executor().step().await.unwrap();
    assert_eq!(r.reported, vec![(p, Report::new("failed", "yes"))]);
    assert_eq!(rig.vcs.0.lock().unwrap().remote, Some("c".repeat(40)));
}

#[tokio::test]
async fn a_lost_outcome_reply_is_reported_once() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone()]);
    let p = rig.node.propose(&count_plan(1));
    rig.node.approve(&p);
    rig.node.state().outcome_faults.push(Fault::LostReply);
    let ex = rig.executor();
    ex.step().await.unwrap();
    ex.step().await.unwrap();
    ex.step().await.unwrap();
    assert_eq!(rig.node.state().outcomes.len(), 1);
    assert_eq!(counter.runs.load(Ordering::SeqCst), 1);
    let pending = ex.with_ledger(|l| l.pending().unwrap());
    assert!(pending.is_empty(), "{pending:?}");
}

#[tokio::test]
async fn a_claim_whose_reply_is_lost_never_launches() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone()]);
    let p = rig.node.propose(&count_plan(1));
    rig.node.approve(&p);
    rig.node.state().claim_faults.push(Fault::LostReply);
    let ex = rig.executor();
    ex.step().await.unwrap();
    assert_eq!(counter.runs.load(Ordering::SeqCst), 0);
    // The claim landed: it is reported not started, the effect opens again with its approval returned, and
    // the next attempt runs it.
    for _ in 0..4 {
        rig.clock.advance(2_000); // past the X-11 back-off
        ex.step().await.unwrap();
    }
    let outcomes = rig.node.state().outcomes.clone();
    assert_eq!(
        outcomes,
        vec![
            (p.clone(), "failed".into(), "no".into()),
            (p.clone(), "succeeded".into(), "yes".into())
        ]
    );
    assert_eq!(counter.world.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn past_the_launch_deadline_nothing_launches() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone()]);
    let p = rig.node.propose(&count_plan(1));
    rig.node.approve(&p);
    // The claim takes 21 s on the elapsed clock: W − stallBound = 30 − 10 = 20 s has passed.
    rig.node.state().stall_on_claim = Some((rig.clock.clone(), 21_000));
    let ex = rig.executor();
    let r = ex.step().await.unwrap();
    assert_eq!(r.reported, vec![(p.clone(), Report::not_started())]);
    assert_eq!(counter.runs.load(Ordering::SeqCst), 0);
    let key = han_exec::IdempotencyKey {
        space: SPACE.into(),
        proposal: p,
        attempt: 1,
    };
    let states: Vec<String> = ex
        .with_ledger(|l| l.journal(&key).unwrap())
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert_eq!(
        states,
        [
            "claiming",
            "claimed",
            "launching",
            "expired",
            "result",
            "reported"
        ]
    );
}

#[tokio::test]
async fn a_restart_mid_launch_looks_the_result_up_and_never_runs_again() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone()]);
    let p = rig.node.propose(&count_plan(1));
    rig.node.approve(&p);
    *counter.fault.lock().unwrap() = RunFault::After;
    {
        let ex = rig.executor();
        assert!(or_crash(ex.step()).await.is_none(), "crashed mid-launch");
    }
    let ex = rig.executor();
    let r = ex.recover().await.unwrap();
    assert_eq!(
        r.reported,
        vec![(p.clone(), Report::new("succeeded", "yes"))]
    );
    for _ in 0..3 {
        ex.step().await.unwrap();
    }
    assert_eq!(counter.runs.load(Ordering::SeqCst), 1);
    assert_eq!(counter.world.load(Ordering::SeqCst), 1);
    assert_eq!(rig.node.state().outcomes.len(), 1);
}

#[tokio::test]
async fn a_second_instance_is_refused_naming_x07() {
    let rig = Rig::new(vec![Counter::new()]);
    let first = rig.executor();
    let second = han_exec::ledger::InstanceLock::acquire(rig.dir.path());
    match second {
        Err(e @ LedgerError::Held(_)) => assert!(e.to_string().contains("X-07"), "{e}"),
        Err(e) => panic!("wrong error {e}"),
        Ok(_) => panic!("a second holder"),
    }
    drop(first);
    // The holder is gone: a restart is not blocked.
    let _again = rig.executor();
}

/// Crash injection at every transition (SR-68): the executor is dropped mid-step, restarted, recovered and
/// stepped until quiet. Across every crash point the world changes at most once, a success is reported only
/// when the world changed, and no attempt number is claimed twice.
#[tokio::test]
async fn crash_at_every_transition_never_runs_twice() {
    #[derive(Clone, Copy, Debug)]
    enum Crash {
        ClaimBefore,
        ClaimAfter,
        RunBefore,
        RunAfter,
        OutcomeBefore,
        OutcomeAfter,
        None,
    }
    for crash in [
        Crash::ClaimBefore,
        Crash::ClaimAfter,
        Crash::RunBefore,
        Crash::RunAfter,
        Crash::OutcomeBefore,
        Crash::OutcomeAfter,
        Crash::None,
    ] {
        for restarts in 1..=3u32 {
            let counter = Counter::new();
            let rig = Rig::new(vec![counter.clone()]);
            let p = rig.node.propose(&count_plan(u64::from(restarts)));
            rig.node.approve(&p);
            match crash {
                Crash::ClaimBefore => rig.node.state().claim_faults.push(Fault::HangBefore),
                Crash::ClaimAfter => rig.node.state().claim_faults.push(Fault::HangAfter),
                Crash::RunBefore => *counter.fault.lock().unwrap() = RunFault::Before,
                Crash::RunAfter => *counter.fault.lock().unwrap() = RunFault::After,
                Crash::OutcomeBefore => rig.node.state().outcome_faults.push(Fault::HangBefore),
                Crash::OutcomeAfter => rig.node.state().outcome_faults.push(Fault::HangAfter),
                Crash::None => {}
            }
            {
                let ex = rig.executor();
                let _ = or_crash(ex.step()).await;
            }
            for _ in 0..restarts {
                let ex = rig.executor();
                let _ = or_crash(ex.recover()).await;
                for _ in 0..4 {
                    let _ = or_crash(ex.step()).await;
                    // A claim that never landed is given up after its window; let time pass.
                    rig.clock.advance(31_000);
                }
            }
            let world = counter.world.load(Ordering::SeqCst);
            let st = rig.node.state();
            let succeeded = st.outcomes.iter().filter(|o| o.1 == "succeeded").count();
            assert!(world <= 1, "{crash:?}: the world changed {world} times");
            assert!(succeeded <= 1, "{crash:?}: {:?}", st.outcomes);
            if succeeded == 1 {
                assert_eq!(world, 1, "{crash:?}: success reported without the change");
            }
            let mut attempts: Vec<u64> = st
                .commands
                .iter()
                .filter_map(|c| match c {
                    han_node::api::Command::EffectClaim { attempt, .. } => Some(*attempt),
                    _ => None,
                })
                .collect();
            let n = attempts.len();
            attempts.dedup();
            assert_eq!(attempts.len(), n, "{crash:?}: an attempt number was reused");
            // The effect ends settled: done, or open again only when nothing ran.
            let row = st.rows.iter().find(|r| r.proposal == p).unwrap();
            assert!(
                row.phase == "done" || (row.phase == "open" && world == 0),
                "{crash:?}: {} with world {world}",
                row.phase
            );
            match crash {
                // Launched, then lost before touching the world: nothing proves it ran or not.
                Crash::RunBefore => assert!(
                    world == 0 && st.outcomes.iter().any(|o| o.1 == "unknown"),
                    "{crash:?}: {:?}",
                    st.outcomes
                ),
                _ => assert!(
                    world == 1 && succeeded == 1,
                    "{crash:?} x{restarts}: world {world}, {:?}",
                    st.outcomes
                ),
            }
            if world == 1 && row.phase == "done" {
                assert!(
                    st.outcomes
                        .iter()
                        .any(|o| o.1 == "succeeded" || o.1 == "unknown"),
                    "{crash:?}: {:?}",
                    st.outcomes
                );
            }
        }
    }
}

#[test]
fn plan_check_refuses_branches_outside_the_prefixes() {
    let rig = Rig::new(vec![]);
    let plan = json!({"action": "git_push", "target": "repo", "args": {
        "branch": "han/../main", "base": "a".repeat(40), "from": null, "to": "b".repeat(40),
        "changeset": "d".repeat(64), "source": "/ws"}});
    assert!(GitPush.plan_check(&plan, &rig.config).is_err());
    let mut p2 = plan.clone();
    p2["target"] = json!("elsewhere");
    p2["args"]["branch"] = json!("han/ok");
    assert!(matches!(
        GitPush.plan_check(&p2, &rig.config),
        Err(han_exec::PlanError::OutOfScope(_))
    ));
}

/// R15: a push carries its commits as a bundle blob (`argsBlob`) or, for a same-user setup, a `source`
/// path; never both, never neither, and the blob is a git bundle.
#[test]
fn plan_check_takes_a_bundle_blob_or_a_source() {
    let rig = Rig::new(vec![]);
    let blob = json!({"hash": "e".repeat(64), "size": 10, "type": han_vcs::BUNDLE_MEDIA_TYPE});
    let args = json!({"branch": "han/ok", "base": "a".repeat(40), "from": null, "to": "b".repeat(40),
        "changeset": "d".repeat(64)});
    let bundled = json!({"action": "git_push", "target": "repo", "args": args, "argsBlob": blob});
    GitPush.plan_check(&bundled, &rig.config).unwrap();
    let mut both = bundled.clone();
    both["args"]["source"] = json!("/ws");
    assert!(GitPush.plan_check(&both, &rig.config).is_err());
    let neither = json!({"action": "git_push", "target": "repo", "args": args});
    assert!(GitPush.plan_check(&neither, &rig.config).is_err());
    let mut text = bundled.clone();
    text["argsBlob"]["type"] = json!("text/plain");
    assert!(GitPush.plan_check(&text, &rig.config).is_err());
    let mut bad_hash = bundled.clone();
    bad_hash["argsBlob"]["hash"] = json!("xyz");
    assert!(GitPush.plan_check(&bad_hash, &rig.config).is_err());
    // other actions take no argsBlob
    let pr = json!({"action": "open_pr", "target": "repo", "args": {"head": "han/ok", "base": "main",
        "title": "t"}, "argsBlob": blob});
    assert!(
        han_exec::actions::OpenPr
            .plan_check(&pr, &rig.config)
            .is_err()
    );
}

#[tokio::test]
async fn the_gate_waits_for_an_approval_unless_policy_says_the_proposer_suffices() {
    let counter = Counter::new();
    let rig = Rig::new(vec![counter.clone()]);
    let p = rig.node.propose(&count_plan(1));
    let ex = rig.executor();
    let r = ex.step().await.unwrap();
    assert_eq!(r.waiting, vec![p.clone()]);
    assert_eq!(rig.node.claims(), 0);

    // Policy's report (the rule that applies has `proposer: true`, P-17): the gate does not wait, and nothing
    // in the executor names the action.
    rig.node.set_proposer_suffices(&p, true);
    let r = ex.step().await.unwrap();
    assert_eq!(r.claimed, vec![p.clone()]);
    assert_eq!(
        r.reported,
        vec![(p.clone(), Report::new("succeeded", "yes"))]
    );
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(counter.world.load(Ordering::SeqCst), 1);

    // The decision itself, for each combination.
    use han_exec::{Gate, GateDecision, GateInput, NeedsApproval};
    let input = |approvals: &[&str], proposer_suffices| GateInput {
        action: "any.action".into(),
        target: "t".into(),
        changes: None,
        approvals: approvals.iter().map(|a| (*a).to_owned()).collect(),
        proposer_suffices,
    };
    assert!(matches!(
        NeedsApproval.decide(&input(&[], false)),
        GateDecision::NeedsApproval { .. }
    ));
    assert_eq!(NeedsApproval.decide(&input(&[], true)), GateDecision::Run);
    assert_eq!(
        NeedsApproval.decide(&input(&["@a:b.c"], false)),
        GateDecision::Run
    );
}
