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
