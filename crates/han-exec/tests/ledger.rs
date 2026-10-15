//! The launch journal: its format, its transitions, durable reopening and attempt numbers (SR-68).

use han_exec::IdempotencyKey;
use han_exec::ledger::{Ledger, LedgerError, Report};

fn key(n: u64) -> IdempotencyKey {
    IdempotencyKey {
        space: "s1:a.localhost".into(),
        proposal: "p1".into(),
        attempt: n,
    }
}

#[test]
fn transitions_follow_the_journal_and_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.db");
    {
        let l = Ledger::open(&path).unwrap();
        assert_eq!(l.format().unwrap(), han_exec::ledger::FORMAT_VERSION);
        l.claiming(&key(1), "count", "{}", "test", 5, 30).unwrap();
        // An attempt number is used once.
        assert!(matches!(
            l.claiming(&key(1), "count", "{}", "test", 6, 30),
            Err(LedgerError::Exists(_))
        ));
        // `reported` needs a result first; `expired` needs `launching`.
        assert!(l.reported(&key(1), "").is_err());
        assert!(l.expired(&key(1), "").is_err());
        l.claimed(&key(1), "c1").unwrap();
        l.launching(&key(1)).unwrap();
        assert!(l.launching(&key(1)).is_err());
    }
    // Reopened ("after a crash"): the state is as journalled.
    let l = Ledger::open(&path).unwrap();
    let a = l.get(&key(1)).unwrap().unwrap();
    assert_eq!(a.state, "launching");
    assert_eq!(a.claim_id.as_deref(), Some("c1"));
    assert_eq!((a.clock.as_str(), a.sent_ms, a.window), ("test", 5, 30));
    l.result(&key(1), &Report::unknown(), "lost").unwrap();
    l.reported(&key(1), "ok").unwrap();
    assert!(l.pending().unwrap().is_empty());
    let states: Vec<String> = l
        .journal(&key(1))
        .unwrap()
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert_eq!(
        states,
        ["claiming", "claimed", "launching", "result", "reported"]
    );
    let a = l.get(&key(1)).unwrap().unwrap();
    assert_eq!(a.report, Some(Report::new("unknown", "unknown")));
}

#[test]
fn a_claim_that_never_landed_may_be_claimed_again_under_its_number() {
    let dir = tempfile::tempdir().unwrap();
    let l = Ledger::open(&dir.path().join("ledger.db")).unwrap();
    l.claiming(&key(1), "count", "{}", "test", 1, 30).unwrap();
    l.refused(&key(1), "denied").unwrap();
    l.claiming(&key(1), "count", "{}", "test", 2, 30).unwrap();
    l.claimed(&key(1), "c1").unwrap();
    l.launching(&key(1)).unwrap();
    // Once its claim landed, the number is spent.
    l.result(&key(1), &Report::new("succeeded", "yes"), "")
        .unwrap();
    l.reported(&key(1), "").unwrap();
    assert!(matches!(
        l.claiming(&key(1), "count", "{}", "test", 3, 30),
        Err(LedgerError::Exists(_))
    ));
}

#[test]
fn a_lanes_instruction_is_journalled_with_the_claim_and_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.db");
    let lane = han_exec::LaneInstruction {
        onto: "a".repeat(40),
        commit: "b".repeat(40),
        repository: dir.path().join("landing.git"),
        evidence: None,
    };
    {
        let l = Ledger::open(&path).unwrap();
        l.claiming_with(&key(1), "vcs.land", "{}", "test", 5, 30, Some(&lane))
            .unwrap();
        l.claiming(&key(2), "count", "{}", "test", 6, 30).unwrap();
        l.lane_put("s1:a.localhost", "p1", r#"{"state":"testing"}"#)
            .unwrap();
        l.lane_put("s1:a.localhost", "p1", r#"{"state":"landing"}"#)
            .unwrap();
        l.meta_put("lane-session|s1:a.localhost", "e1").unwrap();
    }
    let l = Ledger::open(&path).unwrap();
    assert_eq!(l.get(&key(1)).unwrap().unwrap().lane, Some(lane.clone()));
    assert_eq!(l.get(&key(2)).unwrap().unwrap().lane, None);
    assert_eq!(l.pending().unwrap()[0].lane, Some(lane));
    assert_eq!(
        l.lane_all().unwrap(),
        vec![(
            "s1:a.localhost".to_owned(),
            "p1".to_owned(),
            r#"{"state":"landing"}"#.to_owned()
        )]
    );
    assert_eq!(
        l.meta_get("lane-session|s1:a.localhost")
            .unwrap()
            .as_deref(),
        Some("e1")
    );
    assert_eq!(l.meta_get("nope").unwrap(), None);
    assert_eq!(l.format().unwrap(), han_exec::ledger::FORMAT_VERSION);
}

#[test]
fn a_journal_from_before_the_lane_opens_and_gains_its_tables() {
    // The first migration alone is what the executor shipped before the lane.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             INSERT INTO meta VALUES ('format', '1');
             CREATE TABLE attempts (key TEXT PRIMARY KEY, space TEXT NOT NULL, proposal TEXT NOT NULL,
               attempt INTEGER NOT NULL, action TEXT NOT NULL, plan TEXT NOT NULL, state TEXT NOT NULL,
               claim_id TEXT, clock TEXT NOT NULL, sent_ms INTEGER NOT NULL, window INTEGER NOT NULL,
               report TEXT, detail TEXT NOT NULL DEFAULT '');
             CREATE INDEX attempts_proposal ON attempts (space, proposal);
             CREATE TABLE journal (id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL,
               state TEXT NOT NULL, detail TEXT NOT NULL);
             INSERT INTO attempts (key, space, proposal, attempt, action, plan, state, clock, sent_ms, window)
               VALUES ('s|p|1', 's', 'p', 1, 'git_push', '{}', 'launching', 'test', 1, 30);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let l = Ledger::open(&path).unwrap();
    let a = l.pending().unwrap();
    assert_eq!(
        (a.len(), a[0].state.as_str(), a[0].lane.clone()),
        (1, "launching", None)
    );
    l.lane_put("s", "p", "{}").unwrap();
    assert_eq!(l.lane_all().unwrap().len(), 1);
}
