//! Randomized convergence and sync tests with signed envelopes (PLAN.md T1).
//!
//! Several replicas edit maps, registers, texts and worktree trees on several timelines, fork,
//! plan and record merges with random resolutions, and sync pairwise with small answers. At the
//! end every replica holds the same batches and the same state at every timeline head, and a
//! fresh receiver fed every envelope in a random order (with duplicates) reaches the same.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use han_timeline::{
    At, Conflict, Directory, Error, History, HistoryKind, KeyInfo, KeySigner, MemoryStore,
    ObjectId, Op, OpId, Resolution, Scope, Signer as _, Value, Verdict,
};
use sha2::{Digest, Sha256};

const SPACE: &str = "s1:a.example";

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn text(&mut self) -> String {
        const ALPHABET: &[&str] = &["a", "b", "c", "x", "é", "\n", "\n", "\r\n", "🙂", " "];
        (0..1 + self.below(6))
            .map(|_| *self.pick(ALPHABET))
            .collect()
    }
}

struct Dir(HashMap<(String, String), KeyInfo>);

impl Directory for Dir {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        self.0.get(&(actor.to_string(), key.to_string())).copied()
    }
    fn may_write(&self, _: &str, _: Scope<'_>) -> bool {
        true
    }
}

fn signer(actor: &str, key: &str) -> KeySigner {
    let seed: [u8; 32] = Sha256::digest(format!("random-{actor}-{key}")).into();
    KeySigner::from_seed(actor, key, seed)
}

struct Replica {
    signer: KeySigner,
    lane: u32,
    history: History,
}

fn sync_pair(a: &mut Replica, b: &mut Replica, dir: &Dir, max_bytes: Option<u64>) {
    for _ in 0..1000 {
        let hello = a.history.hello(max_bytes).encode();
        let answer = b.history.answer(&hello).unwrap();
        let (reports, more) = a
            .history
            .receive_answer(&answer, max_bytes, dir)
            .unwrap()
            .unwrap();
        for r in &reports {
            assert!(
                !matches!(r.verdict, Verdict::Rejected(_)),
                "sync rejected: {:?}",
                r
            );
        }
        if !more {
            return;
        }
    }
    panic!("sync did not finish");
}

fn snapshot(h: &History) -> BTreeMap<OpId, (han_timeline::Version, [u8; 32])> {
    h.timelines()
        .into_iter()
        .map(|t| {
            let digest = h.state(&At::Timeline(t.id.clone())).unwrap().digest();
            (t.id, (t.head, digest))
        })
        .collect()
}

fn resolutions(rng: &mut Rng, conflicts: &[Conflict]) -> Vec<Resolution> {
    conflicts
        .iter()
        .map(|c| {
            match (
                c,
                if std::env::var_os("ONLY_TARGET").is_some() {
                    0
                } else {
                    rng.below(3)
                },
            ) {
                (_, 0) => Resolution::Target,
                (_, 1) => Resolution::Incoming,
                (Conflict::Text(_), _) => Resolution::Text(rng.text()),
                // A tree's values must be file entries; only plain keys get a value of our own.
                (Conflict::Register { .. }, _) => Resolution::Value(Value::text(rng.text())),
                (Conflict::Map { key, .. }, _) if key.starts_with('k') => {
                    Resolution::Value(Value::text(rng.text()))
                }
                _ => Resolution::Incoming,
            }
        })
        .collect()
}

/// An object's observable state without op ids (a preview's merge op has a stand-in id).
fn content(state: &han_timeline::State, o: &ObjectId) -> Option<String> {
    Some(match state.object(o)? {
        han_timeline::ObjectState::Map { tree, entries } => format!(
            "{tree} {:?}",
            entries
                .iter()
                .map(|(k, v)| (k, v.iter().map(|(_, x)| x).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        ),
        other => format!("{other:?}"),
    })
}

/// Canonical characters keep one relative order in every timeline head (TL-72 step 2). A
/// diagnostic behind `CHECK_ORDER=1`: copies can break it today (SPEC-QUESTIONS.md, Q1).
fn check_order(h: &History) {
    let heads: Vec<OpId> = h.timelines().into_iter().map(|t| t.id).collect();
    let states: Vec<_> = heads
        .iter()
        .map(|t| h.state(&At::Timeline(t.clone())).unwrap())
        .collect();
    for (a, sa) in states.iter().enumerate() {
        for o in sa.object_ids() {
            let Some(ca) = sa.chars(o) else { continue };
            let pos: HashMap<_, usize> = ca
                .iter()
                .enumerate()
                .map(|(i, c)| (c.1.clone(), i))
                .collect();
            for (b, sb) in states.iter().enumerate().skip(a + 1) {
                let Some(cb) = sb.chars(o) else { continue };
                let mut last = None;
                for c in &cb {
                    if let Some(&p) = pos.get(&c.1) {
                        assert!(
                            last.is_none_or(|l| l < p),
                            "order differs between {} and {} in {o}: {:?} vs {:?}",
                            heads[a],
                            heads[b],
                            ca.iter().map(|c| c.2).collect::<String>(),
                            cb.iter().map(|c| c.2).collect::<String>()
                        );
                        last = Some(p);
                    }
                }
            }
        }
    }
}

/// The text objects of a state.
fn texts(h: &History, t: &OpId) -> Vec<ObjectId> {
    let state = h.state(&At::Timeline(t.clone())).unwrap();
    state
        .object_ids()
        .filter(|o| state.text(o).is_some())
        .cloned()
        .collect()
}

fn local_text_edit(rng: &mut Rng, r: &mut Replica, t: &OpId) {
    let objs = texts(&r.history, t);
    if objs.is_empty() {
        return;
    }
    let obj = rng.pick(&objs).clone();
    let at = At::Timeline(t.clone());
    let before = r.history.state(&at).unwrap().text(&obj).unwrap();
    let chars: Vec<char> = before.chars().collect();
    let pos = rng.below(chars.len() + 1);
    let del = rng.below(chars.len() - pos + 1).min(4);
    let ins = if rng.chance(70) {
        rng.text()
    } else {
        String::new()
    };
    let Some(op) = r.history.plan_edit(&at, &obj, pos, del, &ins).unwrap() else {
        return;
    };
    r.history
        .write(&r.signer, r.lane, t, vec![op], None)
        .unwrap();
    let after = r.history.state(&at).unwrap().text(&obj).unwrap();
    let want: String = chars[..pos].iter().collect::<String>()
        + &ins.replace("\r\n", "\n").replace('\r', "\n")
        + &chars[pos + del..].iter().collect::<String>();
    assert_eq!(after, want, "local edit");
}

fn tree_of(h: &History, t: &OpId) -> Option<ObjectId> {
    let state = h.state(&At::Timeline(t.clone())).unwrap();
    state
        .map_value(&ObjectId::Root, "tree")
        .and_then(|v| v.obj_field())
        .map(ObjectId::Op)
}

fn entry(obj: &OpId) -> Value {
    let mut m = BTreeMap::new();
    m.insert(
        "obj".to_string(),
        Value::object_ref(obj).as_map().unwrap()["obj"].clone(),
    );
    m.insert("mode".to_string(), Value::text("100644"));
    m.insert("eol".to_string(), Value::text("lf"));
    Value::Map(m)
}

fn tree_op(rng: &mut Rng, r: &mut Replica, t: &OpId) {
    let Some(tree) = tree_of(&r.history, t) else {
        return;
    };
    let state = r.history.state(&At::Timeline(t.clone())).unwrap();
    let paths: Vec<(String, Value)> = match state.object(&tree) {
        Some(han_timeline::ObjectState::Map { entries, .. }) => entries
            .into_iter()
            .map(|(k, v)| (k, v[0].1.clone()))
            .collect(),
        _ => Vec::new(),
    };
    let names = ["a.txt", "b.txt", "lib", "lib/z.txt", "src/c.rs", "src/d.rs"];
    match rng.below(4) {
        0 | 1 if !paths.is_empty() => {
            // Rename or delete.
            let (p, v) = rng.pick(&paths).clone();
            let mut ops = vec![Op::MapDelete {
                target: tree.clone(),
                key: p,
            }];
            if rng.chance(60) {
                ops.push(Op::MapSet {
                    target: tree.clone(),
                    key: rng.pick(&names).to_string(),
                    value: v,
                });
            }
            if let Err(e) = r.history.write(&r.signer, r.lane, t, ops, None) {
                panic!("tree op: {e}");
            }
        }
        _ => {
            // Create a file.
            let path = rng.pick(&names).to_string();
            let w = r
                .history
                .write(
                    &r.signer,
                    r.lane,
                    t,
                    vec![Op::TextCreate {
                        text: rng.text().replace('\r', ""),
                        transcript: false,
                    }],
                    None,
                )
                .unwrap();
            r.history
                .write(
                    &r.signer,
                    r.lane,
                    t,
                    vec![Op::MapSet {
                        target: tree.clone(),
                        key: path,
                        value: entry(&w.ops[0]),
                    }],
                    None,
                )
                .unwrap();
        }
    }
}

fn step(
    rng: &mut Rng,
    r: &mut Replica,
    kind: HistoryKind,
    stats: &mut BTreeMap<&'static str, usize>,
) {
    let timelines: Vec<OpId> = r.history.timelines().into_iter().map(|t| t.id).collect();
    let t = rng.pick(&timelines).clone();
    match rng.below(10) {
        0 if timelines.len() < 5 => {
            let at = if rng.chance(30) {
                // Fork at a checkpoint or an older version of the source.
                r.history.checkpoints(&t).last().map(|c| c.version.clone())
            } else {
                None
            };
            r.history
                .fork(
                    &r.signer,
                    r.lane,
                    &t,
                    at.as_ref(),
                    &format!("s{}", timelines.len()),
                )
                .unwrap();
            *stats.entry("fork").or_default() += 1;
        }
        1 | 2 if timelines.len() > 1 => {
            let from = rng.pick(&timelines).clone();
            match r.history.plan_merge(&t, &from) {
                Ok(plan) => {
                    let res = resolutions(rng, &plan.conflicts);
                    let draft = plan.resolve(&res).unwrap();
                    let preview = r.history.preview(&draft).unwrap();
                    // Texts the target left alone since the base end up exactly as incoming
                    // has them (TL-72 step 7, TL-77).
                    let canon = |at: At, o: &ObjectId| {
                        r.history
                            .state(&at)
                            .unwrap()
                            .chars(o)
                            .map(|cs| cs.into_iter().map(|c| (c.1, c.2)).collect::<Vec<_>>())
                    };
                    let untouched: Vec<(ObjectId, String)> = texts(&r.history, &t)
                        .into_iter()
                        .filter(|o| {
                            canon(At::Timeline(t.clone()), o)
                                == canon(At::Version(plan.base.clone()), o)
                        })
                        .filter_map(|o| {
                            let s = r.history.state(&At::Timeline(from.clone())).unwrap();
                            s.text(&o).map(|x| (o, x))
                        })
                        .collect();
                    r.history
                        .merge(&r.signer, r.lane, &draft, None)
                        .unwrap_or_else(|e| panic!("recording a planned merge: {e}: {draft:?}"));
                    let after = r.history.state(&At::Timeline(t.clone())).unwrap();
                    for o in preview.object_ids() {
                        assert_eq!(
                            content(&preview, o),
                            content(&after, o),
                            "preview equals result"
                        );
                    }
                    for (o, want) in untouched {
                        assert_eq!(after.text(&o), Some(want), "untouched text takes incoming");
                    }
                    *stats.entry("merge").or_default() += 1;
                    *stats.entry("conflicts").or_default() += plan.conflicts.len();
                }
                Err(Error::UpToDate | Error::MergeSelf) => {}
                Err(Error::MergeOrder) => *stats.entry("merge_order").or_default() += 1,
                Err(e) => panic!("plan: {e}"),
            }
        }
        3 => {
            r.history
                .write(
                    &r.signer,
                    r.lane,
                    &t,
                    vec![Op::Checkpoint { label: "cp".into() }],
                    None,
                )
                .unwrap();
        }
        4 if kind == HistoryKind::Worktree => tree_op(rng, r, &t),
        4 | 5 => {
            let key = format!("k{}", rng.below(3));
            let op = if rng.chance(70) {
                Op::MapSet {
                    target: ObjectId::Root,
                    key,
                    value: Value::Int(rng.below(100) as i64),
                }
            } else {
                Op::MapDelete {
                    target: ObjectId::Root,
                    key,
                }
            };
            if kind == HistoryKind::Worktree {
                tree_op(rng, r, &t);
            } else {
                r.history
                    .write(&r.signer, r.lane, &t, vec![op], None)
                    .unwrap();
            }
        }
        _ => local_text_edit(rng, r, &t),
    }
}

fn scenario(seed: u64, kind: HistoryKind, steps: usize) -> BTreeMap<&'static str, usize> {
    let mut rng = Rng(seed);
    let mut stats = BTreeMap::new();
    let signers = [
        signer("@alice:a.example", "laptop"),
        signer("@bob:b.example", "desk"),
        signer("@alice:a.example", "phone"),
    ];
    let dir = Dir(signers
        .iter()
        .map(|s| {
            (
                (s.actor().to_string(), s.key_id().to_string()),
                KeyInfo {
                    public_key: s.public_key(),
                    valid: true,
                },
            )
        })
        .collect());
    let mut genesis_ops = vec![
        Op::TextCreate {
            text: "one\ntwo\nthree\n".into(),
            transcript: false,
        },
        Op::RegisterCreate {
            value: Value::Int(0),
        },
    ];
    if kind == HistoryKind::Worktree {
        genesis_ops.push(Op::MapCreate { tree: true });
    }
    let (first, genesis) = History::create(
        MemoryStore::default(),
        SPACE,
        &signers[0],
        0,
        kind,
        None,
        genesis_ops,
    )
    .unwrap();
    let id = first.id();
    let mut replicas: Vec<Replica> = Vec::new();
    for (i, s) in signers.into_iter().enumerate() {
        let history = if i == 0 {
            History::open(first.store().clone(), SPACE, id).unwrap()
        } else {
            let mut h = History::open(MemoryStore::default(), SPACE, id).unwrap();
            assert_eq!(
                h.receive(&genesis.envelope, &dir).unwrap().verdict,
                Verdict::Integrated
            );
            h
        };
        replicas.push(Replica {
            signer: s,
            lane: i as u32 % 2,
            history,
        });
    }
    if kind == HistoryKind::Worktree {
        // Point the root at the tree.
        let main = replicas[0].history.main().unwrap();
        let tree = OpId::new(main.counter + 3, main.replica.clone());
        let r = &mut replicas[0];
        r.history
            .write(
                &r.signer,
                r.lane,
                &main,
                vec![Op::MapSet {
                    target: ObjectId::Root,
                    key: "tree".into(),
                    value: Value::object_ref(&tree),
                }],
                None,
            )
            .unwrap();
    }
    for _ in 0..steps {
        let i = rng.below(replicas.len());
        if rng.chance(20) {
            let j = (i + 1 + rng.below(replicas.len() - 1)) % replicas.len();
            let max = if rng.chance(50) {
                Some(64 * 1024)
            } else {
                None
            };
            let (a, b) = if i < j {
                let (l, r) = replicas.split_at_mut(j);
                (&mut l[i], &mut r[0])
            } else {
                let (l, r) = replicas.split_at_mut(i);
                (&mut r[0], &mut l[j])
            };
            sync_pair(a, b, &dir, max);
        } else {
            step(&mut rng, &mut replicas[i], kind, &mut stats);
            if std::env::var_os("CHECK_ORDER").is_some() {
                check_order(&replicas[i].history);
            }
        }
    }
    // Full sync, twice around.
    for _ in 0..2 {
        for i in 0..replicas.len() {
            for j in 0..replicas.len() {
                if i != j {
                    let (a, b) = if i < j {
                        let (l, r) = replicas.split_at_mut(j);
                        (&mut l[i], &mut r[0])
                    } else {
                        let (l, r) = replicas.split_at_mut(i);
                        (&mut r[0], &mut l[j])
                    };
                    sync_pair(a, b, &dir, Some(64 * 1024));
                }
            }
        }
    }
    let reference = snapshot(&replicas[0].history);
    let inventory = replicas[0].history.inventory();
    for r in &replicas[1..] {
        assert_eq!(r.history.inventory(), inventory, "seed {seed}: inventories");
        assert_eq!(snapshot(&r.history), reference, "seed {seed}: states");
    }
    // Any order, with duplicates, into a fresh receiver.
    let mut envelopes: Vec<Arc<[u8]>> = replicas[0].history.envelopes().map(Arc::from).collect();
    for i in (1..envelopes.len()).rev() {
        envelopes.swap(i, rng.below(i + 1));
    }
    let dups: Vec<Arc<[u8]>> = (0..envelopes.len() / 5)
        .map(|_| rng.pick(&envelopes).clone())
        .collect();
    envelopes.extend(dups);
    let mut fresh = History::open(MemoryStore::default(), SPACE, id).unwrap();
    fresh.set_buffer_limits(1 << 20, 1024);
    for e in &envelopes {
        let v = fresh.receive(e, &dir).unwrap().verdict;
        assert!(
            !matches!(v, Verdict::Rejected(_)),
            "seed {seed}: any order: {v:?}"
        );
    }
    assert!(fresh.held_back().is_empty());
    assert_eq!(snapshot(&fresh), reference, "seed {seed}: any order");
    // Every version that a timeline passed through materialises the same from scratch.
    let reopened = History::open(fresh.store().clone(), SPACE, id).unwrap();
    assert_eq!(snapshot(&reopened), reference, "seed {seed}: reopen");
    *stats.entry("batches").or_default() += envelopes.len();
    stats
}

#[test]
fn data_histories_converge() {
    let seeds: u64 = std::env::var("SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let mut total: BTreeMap<&str, usize> = BTreeMap::new();
    for seed in 0..seeds {
        for (k, v) in scenario(
            seed,
            HistoryKind::Data,
            std::env::var("STEPS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(160),
        ) {
            *total.entry(k).or_default() += v;
        }
    }
    eprintln!("data: {total:?}");
    assert!(total.get("merge").copied().unwrap_or(0) > 10);
    assert!(total.get("conflicts").copied().unwrap_or(0) > 0);
}

#[test]
fn worktree_histories_converge() {
    let seeds: u64 = std::env::var("SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let mut total: BTreeMap<&str, usize> = BTreeMap::new();
    for seed in 100..100 + seeds {
        for (k, v) in scenario(seed, HistoryKind::Worktree, 160) {
            *total.entry(k).or_default() += v;
        }
    }
    eprintln!("worktree: {total:?}");
    assert!(total.get("merge").copied().unwrap_or(0) > 10);
}
