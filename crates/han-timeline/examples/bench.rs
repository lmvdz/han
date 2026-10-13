//! Timings of the engine's hot paths. Run with `cargo run --release --example bench [n]`.
//!
//! 1. Applying `n` local ops (text edits and map writes, one op per batch), which leaves a
//!    text made of thousands of small insertions; then reopening that history from its store,
//!    the first read of the fragmented text, planning edits in it, and answering a full sync.
//! 2. Integrating `n / 10` batches from three writers, in random and in causal order.
//! 3. Materialising a 1 MiB text: cold, at a past version, cached, and shared.
//! 4. A worktree of `n / 10` files: writing 3-file changes (some renames), reading every file of
//!    a cold-opened worktree, and planning and recording a merge of a session into main.

use std::time::Instant;

use std::collections::BTreeMap;

use han_timeline::{
    At, Directory, Hello, History, HistoryKind, KeyInfo, KeySigner, MemoryStore, ObjectId, Op,
    OpId, Resolution, Scope, Signer as _, Value, Version,
};

const SPACE: &str = "s1:a.example";

struct Open(Vec<KeyInfo>, Vec<(String, String)>);

impl Directory for Open {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        self.1
            .iter()
            .position(|(a, k)| a == actor && k == key)
            .map(|i| self.0[i])
    }
    fn may_write(&self, _: &str, _: Scope<'_>) -> bool {
        true
    }
}

fn signer(name: &str) -> KeySigner {
    let mut seed = [7u8; 32];
    seed[..name.len()].copy_from_slice(name.as_bytes());
    KeySigner::from_seed(format!("@{name}:x.example"), "dev", seed)
}

fn dir(signers: &[&KeySigner]) -> Open {
    Open(
        signers
            .iter()
            .map(|s| KeyInfo {
                public_key: s.public_key(),
                valid: true,
            })
            .collect(),
        signers
            .iter()
            .map(|s| (s.actor().to_string(), s.key_id().to_string()))
            .collect(),
    )
}

/// splitmix64.
fn rng(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Varies every workload: set `BENCH_SEED` to get a different but repeatable run.
fn seed() -> u64 {
    std::env::var("BENCH_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn report(what: &str, n: usize, start: Instant) {
    let total = start.elapsed();
    println!(
        "{what:<52} {:>9.1} ms total {:>9.1} µs each",
        total.as_secs_f64() * 1e3,
        total.as_secs_f64() * 1e6 / n as f64
    );
}

fn new_history(a: &KeySigner, text: &str) -> (History, OpId, ObjectId) {
    let (h, g) = History::create(
        MemoryStore::default(),
        SPACE,
        a,
        0,
        HistoryKind::Data,
        None,
        vec![Op::TextCreate {
            text: text.into(),
            transcript: false,
        }],
    )
    .unwrap();
    let main = h.main().unwrap();
    (h, main, ObjectId::Op(g.ops[1].clone()))
}

fn local_ops(n: usize) -> (History, OpId, ObjectId) {
    let a = signer("alice");
    let (mut h, main, text) = new_history(&a, "hello\n");
    let at = At::Timeline(main.clone());
    let mut s = 1u64 ^ seed().wrapping_mul(0x9e37_79b9);
    let start = Instant::now();
    for i in 0..n {
        let op = if i % 4 == 3 {
            Op::MapSet {
                target: ObjectId::Root,
                key: format!("k{}", i % 16),
                value: Value::Int(i as i64),
            }
        } else {
            let len = h.state(&at).unwrap().text(&text).unwrap().chars().count();
            let pos = (rng(&mut s) as usize) % (len + 1);
            let del = if i % 5 == 0 && pos < len { 1 } else { 0 };
            h.plan_edit(&at, &text, pos, del, "x").unwrap().unwrap()
        };
        h.write(&a, 0, &main, vec![op], None).unwrap();
    }
    report(
        &format!("apply {n} local ops (plan + sign + validate)"),
        n,
        start,
    );
    (h, main, text)
}

/// The history `local_ops` left: `n` batches and a text of thousands of small insertions.
fn fragmented(h: &History, main: &OpId, text: &ObjectId) {
    let batches = h.envelopes().count();
    let start = Instant::now();
    let reopened = History::open(h.store().clone(), SPACE, h.id()).unwrap();
    report(
        &format!("open a history of {batches} batches from its store"),
        batches,
        start,
    );
    let at = At::Timeline(main.clone());
    let start = Instant::now();
    let len = reopened
        .state(&at)
        .unwrap()
        .text_shared(text)
        .unwrap()
        .chars()
        .count();
    report("first read of a fragmented text after opening", 1, start);
    let mut s = 5u64 ^ seed().wrapping_mul(0x9e37_79b9);
    let start = Instant::now();
    for _ in 0..100 {
        let pos = (rng(&mut s) as usize) % (len + 1);
        let del = usize::from(pos < len);
        let _ = reopened.plan_edit(&at, text, pos, del, "q").unwrap();
    }
    report("plan an edit in a fragmented text", 100, start);
    let hello = Hello {
        space: SPACE.into(),
        history: h.id(),
        inventory: Default::default(),
        max_bytes: Some(u64::MAX),
    }
    .encode();
    let start = Instant::now();
    let answer = h.answer(&hello).unwrap();
    report(
        &format!("answer a full sync of {batches} batches"),
        batches,
        start,
    );
    assert!(answer.len() > batches);
}

fn out_of_order(n: usize) {
    let signers = [signer("alice"), signer("bob"), signer("carol")];
    let d = dir(&signers.iter().collect::<Vec<_>>());
    let (first, main, text) = new_history(&signers[0], "base\n");
    let id = first.id();
    let genesis: Vec<u8> = first.envelopes().next().unwrap().to_vec();
    let mut copies = vec![first];
    for _ in 1..3 {
        let mut c = History::open(MemoryStore::default(), SPACE, id).unwrap();
        c.receive(&genesis, &d).unwrap();
        copies.push(c);
    }
    let at = At::Timeline(main.clone());
    let mut s = 3u64 ^ seed().wrapping_mul(0x9e37_79b9);
    for i in 0..n {
        let w = (rng(&mut s) % 3) as usize;
        if i % 10 == 9 {
            // Exchange so the writers see each other's work now and then.
            let from = (w + 1) % 3;
            let envs: Vec<Vec<u8>> = copies[from].envelopes().map(<[u8]>::to_vec).collect();
            for e in envs {
                copies[w].receive(&e, &d).unwrap();
            }
        }
        let c = &mut copies[w];
        let len = c.state(&at).unwrap().text(&text).unwrap().chars().count();
        let pos = (rng(&mut s) as usize) % (len + 1);
        let op = c.plan_edit(&at, &text, pos, 0, "y").unwrap().unwrap();
        c.write(&signers[w], 0, &main, vec![op], None).unwrap();
    }
    let mut all: Vec<Vec<u8>> = Vec::new();
    for c in &copies {
        for e in c.envelopes() {
            if !all.iter().any(|x| x == e) {
                all.push(e.to_vec());
            }
        }
    }
    for i in (1..all.len()).rev() {
        all.swap(i, (rng(&mut s) as usize) % (i + 1));
    }
    let mut r = History::open(MemoryStore::default(), SPACE, id).unwrap();
    r.set_buffer_limits(usize::MAX, 1024);
    let start = Instant::now();
    for e in &all {
        r.receive(e, &d).unwrap();
    }
    report(
        &format!("integrate {} batches in random order", all.len()),
        all.len(),
        start,
    );
    assert!(r.held_back().is_empty());
    let mut sorted = all.clone();
    sorted.sort_by_cached_key(|e| {
        let (b, _) = han_timeline::Envelope::decode(e).unwrap().batch().unwrap();
        b.start
    });
    let start = Instant::now();
    let mut r2 = History::open(MemoryStore::default(), SPACE, id).unwrap();
    for e in &sorted {
        r2.receive(e, &d).unwrap();
    }
    report(
        &format!("integrate {} batches in causal order", all.len()),
        all.len(),
        start,
    );
}

fn big_text() {
    let a = signer("alice");
    let line = "the quick brown fox jumps over the lazy dog, again and again!!\n";
    let text: String = line.repeat((1 << 20) / line.len());
    let (mut h, main, obj) = new_history(&a, &text);
    let at = At::Timeline(main.clone());
    let mut versions: Vec<Version> = Vec::new();
    let start = Instant::now();
    for i in 0..100 {
        let op = h
            .plan_edit(
                &at,
                &obj,
                (i * 9973 + seed() as usize * 7919) % text.len(),
                1,
                "Z",
            )
            .unwrap()
            .unwrap();
        h.write(&a, 0, &main, vec![op], None).unwrap();
        versions.push(h.head(&main).unwrap());
    }
    report("edit a 1 MiB text (100 edits)", 100, start);
    // Cold: a reopened history has no cached state.
    let reopened = History::open(h.store().clone(), SPACE, h.id()).unwrap();
    let start = Instant::now();
    let st = reopened.state(&at).unwrap();
    let len = st.text(&obj).unwrap().len();
    report(
        &format!("materialise 1 MiB text at head, warm ({len} bytes)"),
        1,
        start,
    );
    let fresh = History::open(MemoryStore::default(), SPACE, h.id()).unwrap();
    let d = dir(&[&a]);
    let mut fresh = fresh;
    for e in h.envelopes() {
        fresh.receive(e, &d).unwrap();
    }
    let start = Instant::now();
    let past = fresh.state(&At::Version(versions[10].clone())).unwrap();
    let _ = past.text(&obj).unwrap();
    report("materialise 1 MiB text at a past version, cold", 1, start);
    let start = Instant::now();
    let _ = fresh
        .state(&At::Version(versions[10].clone()))
        .unwrap()
        .text(&obj);
    report("materialise the same version again (cached)", 1, start);
    let at10 = At::Version(versions[10].clone());
    let _ = fresh.state(&at10).unwrap().text_shared(&obj); // fills the shared cache
    let start = Instant::now();
    let shared = fresh.state(&at10).unwrap().text_shared(&obj).unwrap();
    report("read the same version's text again, shared", 1, start);
    assert_eq!(
        shared.len(),
        fresh.state(&at10).unwrap().text(&obj).unwrap().len()
    );
}

/// A tree map entry for a text file (TL-83).
fn file_entry(obj: &OpId) -> Value {
    let Value::Map(mut m) = Value::object_ref(obj) else {
        unreachable!()
    };
    m.insert("mode".into(), Value::text("100644"));
    m.insert("eol".into(), Value::text("lf"));
    Value::Map(m)
}

/// Some lines of source-like text.
fn file_text(s: &mut u64, lines: usize) -> String {
    (0..lines)
        .map(|i| format!("    let value_{i} = compute({});\n", rng(s) % 1000))
        .collect()
}

/// Edits `count` random files of `paths` (and renames one now and then) in one batch.
fn change(
    h: &mut History,
    who: &KeySigner,
    t: &OpId,
    tree: &ObjectId,
    paths: &mut Vec<String>,
    s: &mut u64,
    count: usize,
) {
    let at = At::Timeline(t.clone());
    let state = h.state(&at).unwrap();
    let mut ops = Vec::new();
    for _ in 0..count {
        let path = paths[(rng(s) as usize) % paths.len()].clone();
        let entry = state.map_value(tree, &path).unwrap().clone();
        let obj = ObjectId::Op(entry.obj_field().unwrap());
        if rng(s).is_multiple_of(10) {
            // A rename: the content object moves with the path (TL-85).
            let to = format!("{path}.moved{}", rng(s) % 1000);
            if state.map_value(tree, &to).is_none() {
                ops.push(Op::MapDelete {
                    target: tree.clone(),
                    key: path.clone(),
                });
                ops.push(Op::MapSet {
                    target: tree.clone(),
                    key: to.clone(),
                    value: entry,
                });
                paths.retain(|p| *p != path);
                paths.push(to);
            }
            continue;
        }
        let len = state.text_shared(&obj).unwrap().chars().count();
        let pos = (rng(s) as usize) % (len + 1);
        let del = (rng(s) as usize % 20).min(len - pos);
        if let Some(op) = h
            .plan_edit(&at, &obj, pos, del, "    edited(); // by a session\n")
            .unwrap()
        {
            // One edit per file per batch: later ones are planned against the same state.
            if !ops
                .iter()
                .any(|o| matches!(o, Op::TextEdit { target, .. } if *target == obj))
            {
                ops.push(op);
            }
        }
    }
    if !ops.is_empty() {
        h.write(who, 0, t, ops, None).unwrap();
    }
}

fn worktree(n: usize) {
    let a = signer("alice");
    let b = signer("bob");
    let (mut h, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &a,
        0,
        HistoryKind::Worktree,
        None,
        vec![Op::MapCreate { tree: true }],
    )
    .unwrap();
    let main = h.main().unwrap();
    let tree_id = g.ops[1].clone();
    let tree = ObjectId::Op(tree_id.clone());
    h.write(
        &a,
        0,
        &main,
        vec![Op::MapSet {
            target: ObjectId::Root,
            key: "tree".into(),
            value: Value::object_ref(&tree_id),
        }],
        None,
    )
    .unwrap();
    let mut s = 7u64 ^ seed().wrapping_mul(0x9e37_79b9);
    // The files, twenty per batch, as a mirror of a repository would record them.
    let files = (n / 10).max(40);
    let mut paths: Vec<String> = Vec::new();
    for chunk in 0..files.div_ceil(20) {
        let mut ops = Vec::new();
        for _ in 0..20.min(files - chunk * 20) {
            let lines = 10 + (rng(&mut s) % 40) as usize;
            ops.push(Op::TextCreate {
                text: file_text(&mut s, lines),
                transcript: false,
            });
        }
        let w = h.write(&a, 0, &main, ops, None).unwrap();
        let mut sets = Vec::new();
        for (k, obj) in w.ops.iter().enumerate() {
            let path = format!("src/m{}/file_{}.rs", chunk % 7, chunk * 20 + k);
            sets.push(Op::MapSet {
                target: tree.clone(),
                key: path.clone(),
                value: file_entry(obj),
            });
            paths.push(path);
        }
        h.write(&a, 0, &main, sets, None).unwrap();
    }
    let changes = (n / 20).max(20);
    let start = Instant::now();
    for _ in 0..changes {
        change(&mut h, &a, &main, &tree, &mut paths, &mut s, 3);
    }
    report("write a 3-file change to a worktree", changes, start);

    // A session forked from main; both sides work on, partly on the same files.
    let session = h.fork(&b, 0, &main, None, "session").unwrap().ops[0].clone();
    let mut session_paths = paths.clone();
    for _ in 0..(n / 40).max(10) {
        change(&mut h, &b, &session, &tree, &mut session_paths, &mut s, 3);
        change(&mut h, &a, &main, &tree, &mut paths, &mut s, 2);
    }
    let start = Instant::now();
    let plan = h.plan_merge(&main, &session).unwrap();
    report("plan a merge of a session into main", 1, start);
    let resolutions = vec![Resolution::Incoming; plan.conflicts.len()];
    let start = Instant::now();
    let draft = plan.resolve(&resolutions).unwrap();
    h.merge(&a, 0, &draft, None).unwrap();
    report("resolve and record that merge", 1, start);

    // Every file of the merged worktree, from a cold open.
    let reopened = History::open(h.store().clone(), SPACE, h.id()).unwrap();
    let start = Instant::now();
    let state = reopened.state(&At::Timeline(main.clone())).unwrap();
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(han_timeline::ObjectState::Map { entries: e, .. }) = state.object(&tree) {
        for (k, v) in e {
            entries.insert(k, v[0].1.clone());
        }
    }
    let mut bytes = 0;
    for v in entries.values() {
        let obj = ObjectId::Op(v.obj_field().unwrap());
        bytes += state.text_shared(&obj).unwrap().len();
    }
    report(
        &format!(
            "read every file of a cold-opened worktree ({} files)",
            entries.len()
        ),
        entries.len(),
        start,
    );
    assert!(bytes > 0);
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let (h, main, text) = local_ops(n);
    fragmented(&h, &main, &text);
    out_of_order(n / 10);
    big_text();
    worktree(n);
}
