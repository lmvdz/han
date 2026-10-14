//! The parts one at a time: key stores, `node.db` and its migrations, the supervisor seam, and the TypeScript
//! bindings (checked in at `apps/han-gui/src/gen/node-api.ts`).

use std::path::Path;

use han_node::db::{FORMAT_VERSION, NodeDb};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore, KeyringStore};
use han_node::supervisor::{NoSupervisor, ProcessSpawner, Role, RoleSpec, RoleStatus, Supervisor};

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

#[tokio::test]
async fn the_supervisor_seam_starts_and_watches_a_process() {
    let none = NoSupervisor;
    let spec = |name: &str| RoleSpec {
        env: Vec::new(),
        run_as: None,
        role: Role::Host,
        name: name.into(),
        args: vec![],
    };
    assert!(none.start(spec("h")).is_err());
    // `cargo host` is not a command: the process starts and ends, which is all the seam has to show.
    let spawner = ProcessSpawner::new(env!("CARGO"));
    spawner.start(spec("h1")).unwrap();
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
}

/// Run as a child by `a_role_starts_with_a_cleared_environment`: writes its environment to `HAN_DUMP_TO`.
#[test]
#[ignore]
fn env_dump_child() {
    if let Some(to) = std::env::var_os("HAN_DUMP_TO") {
        let all: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
        std::fs::write(
            to,
            all.join(
                "
",
            ),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn a_role_starts_with_a_cleared_environment() {
    // SR-08: the parent holds a canary secret; the child sees only the allow-list and what the spec passes.
    // `set_var` is unsafe in edition 2024 and the workspace forbids unsafe code, so the canary is a
    // variable cargo already sets for every test process and that is not on the allow-list.
    assert!(
        std::env::var_os("CARGO_PKG_NAME").is_some(),
        "cargo sets it"
    );
    let dir = std::env::temp_dir().join(format!("han-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("env.txt");
    let spawner = ProcessSpawner::new(std::env::current_exe().unwrap());
    spawner
        .start(RoleSpec {
            role: Role::Host,
            name: "dump".into(),
            args: vec![
                "env_dump_child".into(),
                "--exact".into(),
                "--include-ignored".into(),
            ],
            env: vec![("HAN_DUMP_TO".into(), out.to_string_lossy().into_owned())],
            run_as: None,
        })
        .unwrap();
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
                || key == "HAN_DUMP_TO"
                // set by the test harness inside the child itself
                || key == "RUST_TEST_THREADS"
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
