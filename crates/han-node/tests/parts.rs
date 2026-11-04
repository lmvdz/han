//! The parts one at a time: key stores, `node.db` and its migrations, the supervisor seam, and the TypeScript
//! bindings (checked in at `apps/han-gui/src/gen/node-api.ts`).

use std::path::Path;

use han_node::db::{FORMAT_VERSION, NodeDb};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore, KeyringStore};
use han_node::supervisor::{NoSupervisor, ProcessSpawner, RoleSpec, RoleStatus, Supervisor};
use han_registry::HostSpec;

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "han-node-parts-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_file_key_store_keeps_a_seed_and_refuses_odd_names() {
    let dir = temp("keys");
    let store = FileKeyStore::new(dir.join("keys"));
    assert_eq!(store.load(DEVICE_KEY).unwrap(), None);
    let seed = store.load_or_create(DEVICE_KEY).unwrap();
    assert_eq!(
        store.load_or_create(DEVICE_KEY).unwrap(),
        seed,
        "it is kept"
    );
    assert_ne!(seed, [0u8; 32]);
    for bad in ["", "../x", "a/b", ".hidden", "a\\b"] {
        assert!(store.load(bad).is_err(), "{bad:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The OS keychain: needs a real one (Windows Credential Manager, macOS Keychain, a Secret Service), so it
/// is run by hand with `--ignored`.
#[test]
#[ignore = "needs an OS keychain"]
fn the_keyring_store_round_trips() {
    let store = KeyringStore::new("han-node-test");
    let name = format!("test-{}", std::process::id());
    store.store(&name, [7u8; 32]).unwrap();
    assert_eq!(store.load(&name).unwrap(), Some([7u8; 32]));
}

#[test]
fn node_db_migrates_keeps_its_format_and_survives_a_reopen() {
    let dir = temp("db");
    let path = dir.join("node.db");
    {
        let db = NodeDb::open(&path).unwrap();
        assert_eq!(db.format().unwrap(), FORMAT_VERSION);
        db.set_config("k", "v").unwrap();
        let entry = han_core::json::parse(br#"{"kind":"task"}"#).unwrap();
        let a = db.outbox_insert("s:a.b", "e1", &entry, None).unwrap();
        let again = db.outbox_insert("s:a.b", "e1", &entry, None).unwrap();
        assert_eq!(a, again, "an entry already queued keeps its row");
        assert_eq!(db.outbox_count("s:a.b").unwrap(), 1);
    }
    let db = NodeDb::open(&path).unwrap();
    assert_eq!(db.config("k").unwrap().as_deref(), Some("v"));
    assert_eq!(db.outbox_pending("s:a.b").unwrap().len(), 1);
    // A file written by a newer version of the format is refused, not misread.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(NodeDb::open(&path).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

fn role(name: &str, dir: &Path) -> RoleSpec {
    RoleSpec {
        run_as: None,
        name: name.into(),
        spec: serde_json::from_value::<HostSpec>(serde_json::json!({
            "space": "s1:a",
            "agent": "@coder:a",
            "harness": {"name": "pi"},
            "home": {"server": "a"},
            "dirs": {"state": dir, "keys": dir, "login": dir.join("login")}
        }))
        .unwrap(),
    }
}

#[tokio::test]
async fn the_supervisor_seam_starts_and_watches_a_process() {
    let dir = std::env::temp_dir().join(format!("han-spawn-{}", std::process::id()));
    let none = NoSupervisor;
    assert!(none.start(role("h", &dir)).is_err());
    // The process is this test binary, which the build made (never `cargo` or a `han` found on PATH, which
    // may have no `host` subcommand). It is asked for a test that does not exist, so it starts and ends
    // quietly, which is all the seam has to show. The spec file follows the arguments as one more filter.
    let spawner = ProcessSpawner::new(std::env::current_exe().unwrap()).with_prefix([
        "--exact",
        "no_such_test",
        "--quiet",
    ]);
    spawner.start(role("h1", &dir)).unwrap();
    // the host's only input is left where it can read it
    let left = std::fs::read_dir(dir.join("spawn")).unwrap().count();
    assert_eq!(left, 1);
    let mut ended = false;
    for _ in 0..200 {
        if matches!(spawner.status().as_slice(), [(n, RoleStatus::Exited(Some(_)))] if n == "h1") {
            ended = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(ended, "the child ended and the supervisor noticed");
    assert!(spawner.stop("nobody").is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// Run as a child by `a_role_starts_with_a_cleared_environment`: writes its environment next to the spec file
/// it was given as its last argument.
#[test]
#[ignore]
fn env_dump_child() {
    let Some(spec) = std::env::args()
        .next_back()
        .filter(|a| a.ends_with(".json"))
    else {
        return;
    };
    let all: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    std::fs::write(
        Path::new(&spec).with_extension("env"),
        all.join(
            "
",
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn a_role_starts_with_a_cleared_environment() {
    // SR-08: the parent holds a canary secret; the child sees only the allow-list. `set_var` is unsafe in
    // edition 2024 and the workspace forbids unsafe code, so the canary is a variable cargo already sets for
    // every test process and that is not on the allow-list.
    assert!(
        std::env::var_os("CARGO_PKG_NAME").is_some(),
        "cargo sets it"
    );
    let dir = std::env::temp_dir().join(format!("han-env-{}", std::process::id()));
    let spawner = ProcessSpawner::new(std::env::current_exe().unwrap()).with_prefix([
        "env_dump_child",
        "--exact",
        "--include-ignored",
    ]);
    spawner.start(role("dump", &dir)).unwrap();
    let out = han_registry::spec_file(&dir, "dump").with_extension("env");
    for _ in 0..400 {
        if out.exists() && matches!(spawner.status().as_slice(), [(_, RoleStatus::Exited(_))]) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let env = std::fs::read_to_string(&out).expect("the child wrote its environment");
    for line in env.lines() {
        let key = line.split('=').next().unwrap_or("");
        assert!(
            han_node::supervisor::ENV_ALLOW
                .iter()
                .any(|k| k.eq_ignore_ascii_case(key))
                // set by the test harness inside the child itself
                || key == "RUST_TEST_THREADS"
                // macOS adds it to every process it starts
                || (cfg!(target_os = "macos") && key == "__CF_USER_TEXT_ENCODING")
                || key.is_empty(),
            "the child inherited {key}"
        );
    }
    assert!(!env.contains("CARGO_PKG_NAME="));
    let _ = std::fs::remove_dir_all(dir);
}

fn gen_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/han-gui/src/gen/node-api.ts")
}

#[test]
fn the_checked_in_typescript_matches_the_rust_types() {
    let want = han_node::api::typescript();
    if std::env::var_os("HAN_UPDATE_GEN").is_some() {
        std::fs::create_dir_all(gen_path().parent().unwrap()).unwrap();
        std::fs::write(gen_path(), &want).unwrap();
    }
    let have = std::fs::read_to_string(gen_path()).unwrap_or_default();
    assert_eq!(
        have.replace("\r\n", "\n"),
        want,
        "apps/han-gui/src/gen/node-api.ts is out of date: run `HAN_UPDATE_GEN=1 cargo test -p han-node --test parts`"
    );
}

#[test]
fn a_rejected_outbox_row_keeps_the_code_the_home_answered() {
    let db = NodeDb::in_memory().unwrap();
    let entry = han_core::json::Value::Str("e".into());
    let id = db
        .outbox_insert("s:a.localhost", "id1", &entry, None)
        .unwrap();
    db.outbox_rejected(id, "invalid", "bad shape").unwrap();
    let info = db.outbox_get(id).unwrap().unwrap();
    assert_eq!(
        (
            info.state.as_str(),
            info.code.as_deref(),
            info.error.as_deref()
        ),
        ("rejected", Some("invalid"), Some("bad shape"))
    );
}

#[test]
fn node_db_keeps_host_items_in_first_seen_order_and_replaces_by_id() {
    use han_node::api::{ItemBody, MessageRole};
    let dir = temp("db-items");
    let path = dir.join("node.db");
    let msg = |text: &str, done: bool| ItemBody::Message {
        role: MessageRole::Agent,
        text: text.into(),
        done,
        masked: Vec::new(),
    };
    {
        let db = NodeDb::open(&path).unwrap();
        db.put_item("s:a.b", "s1", "msg:1:1", Some(1), &msg("Which", false), 3)
            .unwrap();
        db.put_item("s:a.b", "s1", "msg:1:2", Some(1), &msg("two", true), 3)
            .unwrap();
        db.put_item(
            "s:a.b",
            "s1",
            "msg:1:1",
            Some(1),
            &msg("Which branch?", true),
            3,
        )
        .unwrap();
    }
    let db = NodeDb::open(&path).unwrap();
    let ids: Vec<_> = db
        .items("s:a.b")
        .unwrap()
        .into_iter()
        .map(|i| (i.id, i.body))
        .collect();
    assert_eq!(
        ids,
        [
            ("msg:1:1".to_owned(), msg("Which branch?", true)),
            ("msg:1:2".to_owned(), msg("two", true))
        ]
    );
    // Only the newest `keep` stay.
    for n in 3..7 {
        db.put_item(
            "s:a.b",
            "s1",
            &format!("msg:1:{n}"),
            None,
            &msg("x", true),
            3,
        )
        .unwrap();
    }
    let kept: Vec<_> = db
        .items("s:a.b")
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(kept, ["msg:1:4", "msg:1:5", "msg:1:6"]);
}
