//! Restart rules: a home reopened from its files serves the same chain and state (R-03), and after a restart
//! an owner's abandon waits a full `A` of the new steady clock (R-16).

use std::path::PathBuf;
use std::sync::Arc;

use han_core::json::{self, Numbers, Value};
use han_core::types::Durability;
use han_core::{SigningKey, grammar};
use han_seq::{Code, Home, HomeConfig, MemStore, SqliteStore, Store, TestClock};
use sha2::{Digest, Sha256};

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/wire-vectors")
}

fn load(name: &str) -> Value {
    json::parse_with(&std::fs::read(vectors().join(name)).unwrap(), Numbers::Any).unwrap()
}

fn open(store: Arc<dyn Store>, clock: &Arc<TestClock>) -> Home {
    let config = load("config.json");
    let seed = config["serverKey"]["seed"].as_str().unwrap();
    let home = Home::open_test(
        HomeConfig {
            server: "a.localhost".into(),
            key: SigningKey::from_seed(Sha256::digest(seed.as_bytes()).into()),
            key_id: "h1".into(),
            durability: Durability::Local,
        },
        Arc::clone(clock),
        store,
    )
    .unwrap();
    for a in config["actors"].as_array().unwrap() {
        home.register(
            a["actor"].as_str().unwrap(),
            a["kind"].as_str().unwrap(),
            a["keyId"].as_str().unwrap(),
            a["publicKey"].as_str().unwrap(),
            a["operator"].as_str(),
        )
        .unwrap();
    }
    home
}

/// The entries R-16's vector appends, in order, with the expectation each step has.
fn r16_steps() -> Vec<(Value, Option<Value>)> {
    let probes = load("probes-r23.json");
    let r16 = probes["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == Value::Str("R16".into()))
        .unwrap()
        .clone();
    r16["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["send"]["call"].as_str() == Some("append"))
        .map(|s| {
            (
                s["send"]["args"]["entry"].clone(),
                s["send"]["args"].get("content").cloned(),
            )
        })
        .collect()
}

#[test]
fn abandon_waits_a_full_window_after_a_restart() {
    let steps = r16_steps();
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let clock = Arc::new(TestClock::new(
        u64::try_from(grammar::epoch_seconds("2026-01-01T00:00:00Z").unwrap()).unwrap(),
    ));
    let home = open(Arc::clone(&store), &clock);
    // space, two members, a proposal and its claim, a second proposal and its claim, the executor's own outcome (seq 8)
    for (entry, content) in &steps[..8] {
        home.append(entry, content.as_ref()).unwrap();
    }
    let (abandon, abandon_content) = &steps[8];
    let early = home.append(abandon, abandon_content.as_ref()).unwrap_err();
    assert_eq!(
        (early.code, early.retry_after_ms),
        (Code::Unavailable, Some(18_000))
    );

    // the home restarts: its steady clock restarts, and so does the wait for every claim ordered before
    drop(home);
    clock.advance_steady(3600); // an hour passed while it was down: it cannot be counted
    let home = open(Arc::clone(&store), &clock);
    clock.set_wall(u64::try_from(grammar::epoch_seconds("2026-01-01T00:00:18Z").unwrap()).unwrap());
    let after = home.append(abandon, abandon_content.as_ref()).unwrap_err();
    assert_eq!(
        (after.code, after.retry_after_ms),
        (Code::Unavailable, Some(18_000)),
        "a full A of the new steady clock"
    );
    clock.advance_steady(18);
    let receipt = home.append(abandon, abandon_content.as_ref()).unwrap();
    assert_eq!(receipt.seq, 9);
}

#[test]
fn a_reopened_home_has_the_same_chain_and_state() {
    let dir = std::env::temp_dir().join(format!("han-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let clock = Arc::new(TestClock::new(
        u64::try_from(grammar::epoch_seconds("2026-01-01T00:00:00Z").unwrap()).unwrap(),
    ));
    let steps = r16_steps();
    let before = {
        let home = open(Arc::new(SqliteStore::open(&dir).unwrap()), &clock);
        for (entry, content) in &steps[..7] {
            home.append(entry, content.as_ref()).unwrap();
        }
        // an append sent again returns its receipt
        let again = home.append(&steps[0].0, steps[0].1.as_ref()).unwrap();
        assert_eq!(again.seq, 1);
        home.spaces()
            .into_iter()
            .map(|s| (home.state_of(&s).unwrap(), s))
            .collect::<Vec<_>>()
    };
    let home = open(Arc::new(SqliteStore::open(&dir).unwrap()), &clock);
    assert!(!before.is_empty());
    for (state, space) in &before {
        assert_eq!(home.state_of(space).as_deref(), Some(&**state), "{space}");
    }
    // and it keeps ordering where it left off
    let again = home.append(&steps[0].0, steps[0].1.as_ref()).unwrap();
    assert_eq!(again.seq, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_from_a_newer_format_is_refused() {
    let dir = std::env::temp_dir().join(format!("han-format-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    drop(SqliteStore::open(&dir).unwrap());
    let db = rusqlite_open(&dir.join("home.db"));
    drop(db);
    assert!(
        SqliteStore::open(&dir).is_err(),
        "user_version 99 is newer than this build"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn rusqlite_open(path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    conn
}
