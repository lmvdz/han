//! A durable `Store` on the file system, and what TL-120 – TL-125 ask of it: histories reopen
//! as they were, a torn append is lost whole, counters survive crashes, newer formats refuse.

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use common::{Open, SPACE, signer};
use han_timeline::{At, Error, History, HistoryKind, ObjectId, Op, Store, Value};
use sha2::{Digest, Sha256};

/// One directory per history: `format`, an append-only `log` of checksummed records, and
/// `high-water`, replaced atomically.
struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// Opens the store, cutting a torn record left by a crash during an append.
    fn new(dir: &Path) -> Self {
        fs::create_dir_all(dir).unwrap();
        let store = FileStore {
            dir: dir.to_path_buf(),
        };
        if let Ok(f) = OpenOptions::new().write(true).open(store.path("log")) {
            let good: usize = store
                .envelopes()
                .unwrap()
                .iter()
                .map(|e| 4 + e.len() + 32)
                .sum();
            f.set_len(good as u64).unwrap();
            f.sync_all().unwrap();
        }
        store
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Writes a whole file and makes it durable before it replaces the old one.
    fn replace(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let tmp = self.path(&format!("{name}.tmp"));
        let mut f = File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.path(name)).map_err(|e| e.to_string())
    }

    fn high_waters(&self) -> Result<Vec<(String, String, u64)>, String> {
        let Ok(text) = fs::read_to_string(self.path("high-water")) else {
            return Ok(Vec::new());
        };
        text.lines()
            .map(|l| {
                let mut p = l.split(' ');
                match (p.next(), p.next(), p.next().and_then(|c| c.parse().ok())) {
                    (Some(a), Some(k), Some(c)) => Ok((a.to_string(), k.to_string(), c)),
                    _ => Err(format!("bad high-water line {l:?}")),
                }
            })
            .collect()
    }
}

impl Store for FileStore {
    fn format_version(&self) -> Result<Option<u32>, String> {
        match fs::read_to_string(self.path("format")) {
            Ok(s) => s.trim().parse().map(Some).map_err(|e| format!("{e}")),
            Err(_) => Ok(None),
        }
    }

    fn set_format_version(&mut self, version: u32) -> Result<(), String> {
        self.replace("format", version.to_string().as_bytes())
    }

    /// Every complete record; a torn record at the end (a crash during append) is ignored.
    fn envelopes(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut bytes = Vec::new();
        if let Ok(mut f) = File::open(self.path("log")) {
            f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        }
        let mut out = Vec::new();
        let mut pos = 0;
        while pos + 4 <= bytes.len() {
            let n = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            let end = pos + 4 + n + 32;
            if end > bytes.len() {
                break;
            }
            let body = &bytes[pos + 4..pos + 4 + n];
            if Sha256::digest(body).as_slice() != &bytes[pos + 4 + n..end] {
                break;
            }
            out.push(body.to_vec());
            pos = end;
        }
        Ok(out)
    }

    fn append(&mut self, envelope: &[u8]) -> Result<(), String> {
        let mut record = (envelope.len() as u32).to_le_bytes().to_vec();
        record.extend_from_slice(envelope);
        record.extend_from_slice(&Sha256::digest(envelope));
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("log"))
            .map_err(|e| e.to_string())?;
        f.write_all(&record).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())
    }

    fn high_water(&self, actor: &str, key: &str) -> Result<u64, String> {
        Ok(self
            .high_waters()?
            .into_iter()
            .filter(|(a, k, _)| a == actor && k == key)
            .map(|(_, _, c)| c)
            .max()
            .unwrap_or(0))
    }

    fn raise_high_water(&mut self, actor: &str, key: &str, counter: u64) -> Result<(), String> {
        let mut all = self.high_waters()?;
        match all.iter_mut().find(|(a, k, _)| a == actor && k == key) {
            Some(e) => e.2 = e.2.max(counter),
            None => all.push((actor.to_string(), key.to_string(), counter)),
        }
        let text: String = all
            .iter()
            .map(|(a, k, c)| format!("{a} {k} {c}\n"))
            .collect();
        self.replace("high-water", text.as_bytes())
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "han-timeline-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&d);
    d
}

fn set(v: i64) -> Op {
    Op::MapSet {
        target: ObjectId::Root,
        key: "k".into(),
        value: Value::Int(v),
    }
}

#[test]
fn a_history_reopens_as_it_was_and_survives_a_torn_append() {
    let dir = temp_dir("reopen");
    let a = signer("@alice:a.example", "laptop");
    let (mut h, _) = History::create(
        FileStore::new(&dir),
        SPACE,
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![Op::TextCreate {
            text: "hello\n".into(),
            transcript: false,
        }],
    )
    .unwrap();
    let id = h.id();
    let main = h.main().unwrap();
    for i in 0..5 {
        h.write(&a, 0, &main, vec![set(i)], None).unwrap();
    }
    let fork = h.fork(&a, 1, &main, None, "s1").unwrap().ops[0].clone();
    h.write(&a, 1, &fork, vec![set(99)], None).unwrap();
    let digest = |h: &History<FileStore>, t| h.state(&At::Timeline(t)).unwrap().digest();
    let (dm, df) = (digest(&h, main.clone()), digest(&h, fork.clone()));
    drop(h);

    let h = History::open(FileStore::new(&dir), SPACE, id).unwrap();
    assert_eq!(
        (digest(&h, main.clone()), digest(&h, fork.clone())),
        (dm, df)
    );
    drop(h);

    // A crash in the middle of an append leaves a partial record: it is lost whole (TL-122).
    let mut log = OpenOptions::new()
        .append(true)
        .open(dir.join("log"))
        .unwrap();
    log.write_all(&[200, 0, 0, 0, 1, 2, 3]).unwrap();
    drop(log);
    let mut h = History::open(FileStore::new(&dir), SPACE, id).unwrap();
    assert_eq!(
        (digest(&h, main.clone()), digest(&h, fork.clone())),
        (dm, df)
    );
    h.write(&a, 0, &main, vec![set(7)], None).unwrap();
    let h = History::open(FileStore::new(&dir), SPACE, id).unwrap();
    let st = h.state(&At::Timeline(main)).unwrap();
    assert_eq!(st.map_value(&ObjectId::Root, "k"), Some(&Value::Int(7)));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn counters_survive_a_crash_between_allocation_and_append() {
    let dir = temp_dir("counters");
    let a = signer("@alice:a.example", "laptop");
    let dir_open = Open::new(&[&a]);
    let (mut h, _) = History::create(
        FileStore::new(&dir),
        SPACE,
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![],
    )
    .unwrap();
    let id = h.id();
    let main = h.main().unwrap();
    let sent = h.write(&a, 0, &main, vec![set(1)], None).unwrap();
    drop(h);
    // The batch left the device, but the crash lost it from the log; the high-water mark,
    // made durable first (TL-120), still holds its counter.
    let kept: Vec<Vec<u8>> = FileStore::new(&dir).envelopes().unwrap();
    fs::remove_file(dir.join("log")).unwrap();
    let mut store = FileStore::new(&dir);
    store.append(&kept[0]).unwrap();
    let mut h = History::open(store, SPACE, id).unwrap();
    let again = h.write(&a, 0, &main, vec![set(2)], None).unwrap();
    assert!(
        again.ops[0].counter > sent.ops[0].counter,
        "no counter is reused"
    );
    // The lost batch, relayed back by a peer, is no equivocation and integrates.
    let r = h.receive(&sent.envelope, &dir_open).unwrap();
    assert_eq!(r.verdict, han_timeline::Verdict::Integrated);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_newer_format_is_refused() {
    let dir = temp_dir("format");
    let a = signer("@alice:a.example", "laptop");
    let (h, _) = History::create(
        FileStore::new(&dir),
        SPACE,
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![],
    )
    .unwrap();
    let id = h.id();
    drop(h);
    let mut store = FileStore::new(&dir);
    store.set_format_version(2).unwrap();
    assert_eq!(
        History::open(store, SPACE, id).map(|_| ()).unwrap_err(),
        Error::Format(2)
    );
    fs::remove_dir_all(&dir).unwrap();
}
