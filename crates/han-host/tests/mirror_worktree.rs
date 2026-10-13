//! Worktree mirroring: what is on disk is what the session timeline records (TL-81…TL-88), attributed to
//! steps, in batches that fit a stream item, with secrets masked and excluded paths never recorded.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_host::mask::Masker;
use han_host::mirror::{
    BaseFile, Content, Mirror, MirrorBatch, MirrorConfig, MirrorReport, materialise,
};
use han_timeline::{Envelope, KeySigner};

const AGENT: &str = "@coder:a.localhost";

struct Fixture {
    _dir: tempfile::TempDir,
    wt: std::path::PathBuf,
    store: std::path::PathBuf,
    batches: Arc<Mutex<Vec<MirrorBatch>>>,
}

fn fixture(base: &[(&str, &str)]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    for (p, c) in base {
        let f = wt.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, c).unwrap();
    }
    std::fs::create_dir_all(wt.join(".git")).unwrap();
    std::fs::write(wt.join(".git/config"), "[core]\n").unwrap();
    Fixture {
        store: dir.path().join("store"),
        wt,
        _dir: dir,
        batches: Arc::new(Mutex::new(Vec::new())),
    }
}

fn open(f: &Fixture, base: &[(&str, &str)], known: Vec<String>) -> Mirror {
    let sink = Arc::clone(&f.batches);
    Mirror::open(
        MirrorConfig {
            worktree: f.wt.clone(),
            store_dir: f.store.clone(),
            space: "s1:a.localhost".into(),
            session: "a".repeat(64),
            signer: KeySigner::from_seed(AGENT, "k1", [7; 32]),
            base_commit: "c".repeat(40),
            base_files: base
                .iter()
                .map(|(p, c)| BaseFile {
                    path: (*p).into(),
                    mode: "100644".into(),
                    bytes: c.as_bytes().to_vec(),
                })
                .collect(),
            masker: Arc::new(Masker::new(known)),
            ignore: None,
            quiet: Duration::from_millis(150),
        },
        Box::new(move |b| sink.lock().unwrap().push(b)),
    )
    .unwrap()
}

/// What the mirror recorded, as bytes (blobs as their hash).
fn recorded(m: &Mirror) -> BTreeMap<String, Vec<u8>> {
    m.files()
        .unwrap()
        .into_iter()
        .map(|(k, v)| {
            let bytes = materialise(&v).unwrap_or_else(|| match &v.content {
                Content::Blob { hash, .. } => format!("blob:{hash}").into_bytes(),
                _ => unreachable!(),
            });
            (k, bytes)
        })
        .collect()
}

fn write(f: &Fixture, p: &str, c: &[u8]) {
    let full = f.wt.join(p);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, c).unwrap();
}

#[test]
fn creates_edits_deletes_and_renames_follow_the_disk() {
    let base = [
        ("README.md", "hello\n"),
        ("src/lib.rs", "fn a() {}\nfn b() {}\n"),
    ];
    let f = fixture(&base);
    let m = open(&f, &base, vec![]);
    assert_eq!(recorded(&m).len(), 2);

    write(&f, "src/new.rs", b"pub fn n() {}\n");
    write(&f, "src/lib.rs", b"fn a() {}\nfn b2() {}\nfn c() {}\n");
    write(&f, "crlf.txt", b"one\r\ntwo\r\n");
    std::fs::remove_file(f.wt.join("README.md")).unwrap();
    std::fs::create_dir_all(f.wt.join("docs")).unwrap();
    std::fs::write(f.wt.join("docs/README.md"), "hello\n").unwrap();
    write(&f, ".git/HEAD", b"ref: refs/heads/x\n");
    write(&f, ".env", b"TOKEN=abc\n");
    write(&f, "keys/server.pem", b"pem");
    write(&f, "bin.dat", &[0u8, 1, 2, 3, 255]);
    m.rescan().unwrap();

    let rec = recorded(&m);
    assert_eq!(rec["src/new.rs"], b"pub fn n() {}\n");
    assert_eq!(rec["src/lib.rs"], b"fn a() {}\nfn b2() {}\nfn c() {}\n");
    assert_eq!(rec["crlf.txt"], b"one\r\ntwo\r\n");
    assert_eq!(rec["docs/README.md"], b"hello\n");
    assert!(!rec.contains_key("README.md"));
    assert!(!rec.contains_key(".env") && !rec.contains_key("keys/server.pem"));
    assert!(rec.keys().all(|k| !k.starts_with(".git")));
    assert!(String::from_utf8_lossy(&rec["bin.dat"]).starts_with("blob:"));
    assert!(
        m.take_reports()
            .iter()
            .any(|r| matches!(r, MirrorReport::BlobNotUploaded { path, .. } if path == "bin.dat"))
    );

    // The rename kept the content object: one delete and one set, no new text.
    let batches = f.batches.lock().unwrap();
    let last = batches.last().unwrap();
    assert!(last.files.contains(&"README.md".to_string()));
    let (b, _) = Envelope::decode(&last.envelope().unwrap())
        .unwrap()
        .batch()
        .unwrap();
    let creates = b
        .ops
        .iter()
        .filter(|o| matches!(o, han_timeline::Op::TextCreate { text, .. } if text == "hello\n"))
        .count();
    assert_eq!(creates, 0, "a rename moves the object, it does not copy it");

    // Deleting a directory deletes what was under it.
    drop(batches);
    std::fs::remove_dir_all(f.wt.join("docs")).unwrap();
    m.rescan().unwrap();
    assert!(!recorded(&m).contains_key("docs/README.md"));
    // Nothing changed: nothing written.
    assert_eq!(m.rescan().unwrap(), 0);
}

#[test]
fn every_disk_change_matches_after_reopen() {
    let base = [("a.txt", "1\n2\n3\n")];
    let f = fixture(&base);
    {
        let m = open(&f, &base, vec![]);
        write(&f, "a.txt", b"1\ntwo\n3\n4\n");
        m.rescan().unwrap();
    }
    // Reopen from the store: same history, same files, and it keeps writing.
    let m = open(&f, &base, vec![]);
    assert_eq!(recorded(&m)["a.txt"], b"1\ntwo\n3\n4\n");
    write(&f, "b.txt", b"b\n");
    m.rescan().unwrap();
    let rec = recorded(&m);
    assert_eq!(rec["b.txt"], b"b\n");
    assert_eq!(rec["a.txt"], b"1\ntwo\n3\n4\n");
}

#[test]
fn batches_carry_the_step() {
    let f = fixture(&[]);
    let m = open(&f, &[], vec![]);
    m.set_step(Some("toolu_1".into()));
    write(&f, "x.rs", b"x\n");
    m.rescan().unwrap();
    m.set_step(None);
    write(&f, "y.rs", b"y\n");
    m.rescan().unwrap();
    let batches = f.batches.lock().unwrap();
    let n = batches.len();
    assert_eq!(batches[n - 2].step.as_deref(), Some("toolu_1"));
    assert_eq!(batches[n - 2].files, vec!["x.rs".to_string()]);
    assert_eq!(batches[n - 1].step, None);
    let (b, _) = Envelope::decode(&batches[n - 2].envelope().unwrap())
        .unwrap()
        .batch()
        .unwrap();
    assert_eq!(b.step.as_deref(), Some("toolu_1"));
}

#[test]
fn large_files_are_chunked_under_the_stream_item_limit() {
    // Hard to compress: a pseudo-random text of 300 KiB, then an edit replacing most of it.
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut big = String::new();
    while big.len() < 300 * 1024 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        big.push_str(&format!("{x:016x}{}\n", (x % 977) as u32));
    }
    let base = [("big.txt", big.as_str())];
    let f = fixture(&base);
    let m = open(&f, &base, vec![]);
    let mut changed = big.replace('a', "Z");
    changed.push_str("tail\n");
    write(&f, "big.txt", changed.as_bytes());
    write(&f, "new_big.txt", big.as_bytes());
    m.rescan().unwrap();
    let rec = recorded(&m);
    assert_eq!(rec["big.txt"], changed.as_bytes());
    assert_eq!(rec["new_big.txt"], big.as_bytes());
    let batches = f.batches.lock().unwrap();
    for b in batches.iter() {
        let item = serde_json::to_string(&b.body()).unwrap();
        assert!(
            item.len() < 60 * 1024,
            "a stream item body of {} bytes",
            item.len()
        );
    }
}

#[test]
fn secrets_never_leave_unmasked() {
    let canary_model = &"sk-~ant-api03-CANARYmodelKEY0123456789abcdef".replace('~', "");
    let canary_gh = &"gh~p_CANARY0123456789abcdefghijklmnopqrstuv".replace('~', "");
    let session_token = "han-session-CANARY-7f3a9";
    let f = fixture(&[]);
    let m = open(&f, &[], vec![session_token.into()]);
    write(
        &f,
        "config.py",
        format!(
            "MODEL_KEY = \"{canary_model}\"\nGH = '{canary_gh}'\nproxy = 'http://x:{session_token}@127.0.0.1'\n"
        )
        .as_bytes(),
    );
    write(
        &f,
        ".env",
        format!("ANTHROPIC_API_KEY={canary_model}\n").as_bytes(),
    );
    m.rescan().unwrap();
    let rec = recorded(&m);
    let text = String::from_utf8_lossy(&rec["config.py"]).into_owned();
    assert!(text.contains("[masked:"), "{text}");
    for b in f.batches.lock().unwrap().iter() {
        let env = b.envelope().unwrap();
        let (batch, raw) = Envelope::decode(&env).unwrap().batch().unwrap();
        let all = format!("{:?}{}", batch.ops, String::from_utf8_lossy(&raw));
        for c in [canary_model, canary_gh, session_token, "CANARY"] {
            assert!(!all.contains(c), "{c} leaked in a batch");
        }
        assert!(!b.files.iter().any(|p| p == ".env"));
    }
}

#[test]
fn the_watcher_records_changes_after_a_quiet_period() {
    let f = fixture(&[]);
    let m = open(&f, &[], vec![]);
    m.watch().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    write(&f, "watched.txt", b"seen\n");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if recorded(&m).get("watched.txt").map(Vec::as_slice) == Some(b"seen\n".as_slice()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the watcher never recorded it"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Ignored and .git changes are not recorded.
    write(&f, ".gitignore", b"target/\n");
    std::thread::sleep(Duration::from_millis(600));
    write(&f, "target/out.o", b"obj");
    write(&f, ".git/index", b"idx");
    std::thread::sleep(Duration::from_millis(800));
    let rec = recorded(&m);
    assert!(rec.contains_key(".gitignore"));
    assert!(
        !rec.keys()
            .any(|k| k.starts_with("target/") || k.starts_with(".git/"))
    );
    m.unwatch();
    let _ = Path::new("");
}

#[test]
fn a_reading_node_rebuilds_the_mirrored_timeline() {
    use han_node::timeline::StreamTimelines;
    use han_node::views::TimelineSource;
    let base = [("README.md", "hi\n")];
    let f = fixture(&base);
    let m = open(&f, &base, vec![]);
    m.set_step(Some("toolu_9".into()));
    write(&f, "src/a.rs", b"fn a() {}\n");
    write(&f, "README.md", b"hi\nthere\n");
    m.rescan().unwrap();
    let t = StreamTimelines::new();
    let session = "a".repeat(64);
    for b in f.batches.lock().unwrap().iter() {
        let body = han_core::json::parse(b.body().to_string().as_bytes()).unwrap();
        t.receive(&session, AGENT, "2026-01-01T00:00:00Z", &body);
    }
    let points = t.points(&session, None).unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].label, "toolu_9");
    let patches = t.patches(&session, None, &points[0].point).unwrap();
    let paths: Vec<&str> = patches.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, vec!["README.md", "src/a.rs"]);
    assert!(patches[0].patch.contains("+there"), "{}", patches[0].patch);
}
