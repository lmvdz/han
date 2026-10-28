//! Overlap equals planning (TL-134), as a property of random histories.
//!
//! Random worktree and data histories (edits, renames, copies, deletes, binary changes, map
//! keys, registers, forks, merges with random resolutions, checkpoints). For every ordered pair
//! of timelines:
//! - the overlap's conflicting entries are the plan's conflicts under the TL-134 mapping;
//! - its `region` entries are exactly the clusters with edits of both sides, `converged` as
//!   TL-72 step 6 says, computed here by an independent reading of TL-72 steps 1 – 6;
//! - swapping the timelines swaps `a`/`b` and `deleted_by` only (TL-133);
//! - computing it changes no head, no inventory and adds no batch (TL-135).
//!
//! Touch sets' line ranges are checked against the same reading (TL-131), and every result is
//! the same on fresh receivers fed the envelopes in permuted orders.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use han_timeline::{
    At, CharId, Conflict, Directory, Error, History, HistoryKind, KeyInfo, KeySigner, MemoryStore,
    ObjectId, Op, OpId, OverlapEntry, OverlapSide, Resolution, Scope, Side, Signer as _, State,
    TouchChange, Value, Verdict,
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
        const ALPHABET: &[&str] = &["a", "b", "x", "\n", "\n", "\n", "yz", "é"];
        (0..1 + self.below(5))
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
    let seed: [u8; 32] = Sha256::digest(format!("overlap-{actor}-{key}")).into();
    KeySigner::from_seed(actor, key, seed)
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

fn blob(n: usize) -> Value {
    Value::Map(BTreeMap::from([
        ("hash".to_string(), Value::text(format!("{n:064x}"))),
        ("size".to_string(), Value::Int(n as i64)),
        ("type".to_string(), Value::text("application/octet-stream")),
    ]))
}

// ---------------------------------------------------------------------------------------------
// Random histories

struct World {
    history: History,
    signers: Vec<KeySigner>,
    tree: Option<ObjectId>,
    rng: Rng,
}

impl World {
    fn new(seed: u64, kind: HistoryKind) -> (World, Dir) {
        let signers = vec![
            signer("@alice:a.example", "laptop"),
            signer("@bob:b.example", "desk"),
        ];
        let dir = Dir(signers
            .iter()
            .map(|s| {
                let info = KeyInfo {
                    public_key: s.public_key(),
                    valid: true,
                };
                ((s.actor().to_string(), s.key_id().to_string()), info)
            })
            .collect());
        let text = |s: &str| Op::TextCreate {
            text: s.into(),
            transcript: false,
        };
        let mut ops = vec![
            text("one\ntwo\nthree\nfour\nfive\n"),
            text("p\nq\nr\n"),
            Op::RegisterCreate {
                value: Value::Int(0),
            },
        ];
        if kind == HistoryKind::Worktree {
            ops.push(Op::MapCreate { tree: true });
        }
        let (history, genesis) = History::create(
            MemoryStore::default(),
            SPACE,
            &signers[0],
            0,
            kind,
            None,
            ops,
        )
        .unwrap();
        let mut w = World {
            history,
            signers,
            tree: None,
            rng: Rng(seed),
        };
        if kind == HistoryKind::Worktree {
            // The first text is a file; the second stays unreferenced; the register is a binary.
            let [_, t1, _, reg, tree] = &genesis.ops[..] else {
                unreachable!()
            };
            let tree_id = ObjectId::Op(tree.clone());
            let mut bin = entry(reg);
            if let Value::Map(m) = &mut bin {
                m.remove("eol");
            }
            let main = w.history.main().unwrap();
            w.history
                .write(
                    &w.signers[0],
                    0,
                    &main,
                    vec![
                        Op::MapSet {
                            target: ObjectId::Root,
                            key: "tree".into(),
                            value: Value::object_ref(tree),
                        },
                        Op::RegisterSet {
                            target: ObjectId::Op(reg.clone()),
                            value: blob(1),
                        },
                        Op::MapSet {
                            target: tree_id.clone(),
                            key: "src/a.txt".into(),
                            value: entry(t1),
                        },
                        Op::MapSet {
                            target: tree_id.clone(),
                            key: "img.bin".into(),
                            value: bin,
                        },
                    ],
                    None,
                )
                .unwrap();
            w.tree = Some(tree_id);
        }
        (w, dir)
    }

    fn timelines(&self) -> Vec<OpId> {
        self.history.timelines().into_iter().map(|t| t.id).collect()
    }

    fn write(&mut self, t: &OpId, ops: Vec<Op>) {
        let s = self.rng.below(self.signers.len());
        self.history
            .write(&self.signers[s], 0, t, ops, None)
            .unwrap();
    }

    fn step(&mut self) {
        let timelines = self.timelines();
        let t = self.rng.pick(&timelines).clone();
        let state = self.history.state(&At::Timeline(t.clone())).unwrap();
        match self.rng.below(12) {
            0 if timelines.len() < 5 => {
                let at = if self.rng.chance(30) {
                    self.history
                        .checkpoints(&t)
                        .last()
                        .map(|c| c.version.clone())
                } else {
                    None
                };
                let name = format!("s{}", timelines.len());
                self.history
                    .fork(&self.signers[0], 0, &t, at.as_ref(), &name)
                    .unwrap();
            }
            1 | 2 if timelines.len() > 1 => {
                let from = self.rng.pick(&timelines).clone();
                match self.history.plan_merge(&t, &from) {
                    Ok(plan) => {
                        let res: Vec<Resolution> = plan
                            .conflicts
                            .iter()
                            .map(|c| match (c, self.rng.below(3)) {
                                (_, 0) => Resolution::Target,
                                (Conflict::Text(_), 2) => Resolution::Text(self.rng.text()),
                                _ => Resolution::Incoming,
                            })
                            .collect();
                        let draft = plan.resolve(&res).unwrap();
                        self.history
                            .merge(&self.signers[0], 0, &draft, None)
                            .unwrap();
                    }
                    Err(Error::UpToDate | Error::MergeSelf) => {}
                    Err(e) => panic!("plan: {e}"),
                }
            }
            3 => self.write(&t, vec![Op::Checkpoint { label: "cp".into() }]),
            4 | 5 if self.tree.is_some() => self.tree_op(&t, &state),
            6 => {
                let key = format!("k{}", self.rng.below(2));
                let op = if self.rng.chance(75) {
                    Op::MapSet {
                        target: ObjectId::Root,
                        key,
                        value: Value::Int(self.rng.below(4) as i64),
                    }
                } else {
                    Op::MapDelete {
                        target: ObjectId::Root,
                        key,
                    }
                };
                self.write(&t, vec![op]);
            }
            7 => {
                let regs: Vec<ObjectId> = state
                    .object_ids()
                    .filter(|o| state.register(o).is_some())
                    .cloned()
                    .collect();
                if !regs.is_empty() {
                    let target = self.rng.pick(&regs).clone();
                    let value = blob(self.rng.below(4));
                    self.write(&t, vec![Op::RegisterSet { target, value }]);
                }
            }
            _ => self.text_edit(&t, &state),
        }
    }

    fn text_edit(&mut self, t: &OpId, state: &State) {
        let texts: Vec<ObjectId> = state
            .object_ids()
            .filter(|o| state.text(o).is_some())
            .cloned()
            .collect();
        let obj = self.rng.pick(&texts).clone();
        let len = state.text(&obj).unwrap().chars().count();
        let pos = self.rng.below(len + 1);
        let del = self.rng.below(len - pos + 1).min(3);
        let ins = if self.rng.chance(70) {
            self.rng.text()
        } else {
            String::new()
        };
        let at = At::Timeline(t.clone());
        if let Some(op) = self.history.plan_edit(&at, &obj, pos, del, &ins).unwrap() {
            self.write(t, vec![op]);
        }
    }

    fn tree_op(&mut self, t: &OpId, state: &State) {
        let tree = self.tree.clone().unwrap();
        let paths: Vec<(String, Value)> = match state.object(&tree) {
            Some(han_timeline::ObjectState::Map { entries, .. }) => entries
                .into_iter()
                .map(|(k, v)| (k, v[0].1.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let names = [
            "src/a.txt",
            "src/b.txt",
            "lib",
            "lib/z.txt",
            "img.bin",
            "c.md",
        ];
        match self.rng.below(5) {
            // Delete, rename or copy.
            0..=2 if !paths.is_empty() => {
                let (p, v) = self.rng.pick(&paths).clone();
                let mut ops = Vec::new();
                let kind = self.rng.below(3);
                if kind < 2 {
                    ops.push(Op::MapDelete {
                        target: tree.clone(),
                        key: p.clone(),
                    });
                }
                if kind > 0 {
                    let q = self.rng.pick(&names).to_string();
                    if q != p {
                        ops.push(Op::MapSet {
                            target: tree.clone(),
                            key: q,
                            value: v,
                        });
                    }
                }
                if !ops.is_empty() {
                    self.write(t, ops);
                }
            }
            _ => {
                let s = self.rng.below(self.signers.len());
                let w = self
                    .history
                    .write(
                        &self.signers[s],
                        0,
                        t,
                        vec![Op::TextCreate {
                            text: self.rng.text(),
                            transcript: false,
                        }],
                        None,
                    )
                    .unwrap();
                let key = self.rng.pick(&names).to_string();
                self.write(
                    t,
                    vec![Op::MapSet {
                        target: tree,
                        key,
                        value: entry(&w.ops[0]),
                    }],
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// An independent reading of TL-72 steps 1 – 6

/// Step 1: canonical ids and characters of the visible text, the first of each canonical id.
fn seq(state: &State, o: &ObjectId) -> Vec<(CharId, char)> {
    let mut seen = BTreeSet::new();
    state
        .chars(o)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, canon, _)| seen.insert(canon.clone()))
        .map(|(_, canon, c)| (canon, c))
        .collect()
}

/// Step 2: the kept pairs (base index, side index), by a quadratic longest increasing
/// subsequence with the lexicographically smallest base positions.
fn kept(base: &[(CharId, char)], side: &[(CharId, char)]) -> Vec<(usize, usize)> {
    let at: HashMap<&CharId, usize> = side.iter().enumerate().map(|(i, c)| (&c.0, i)).collect();
    let shared: Vec<(usize, usize)> = base
        .iter()
        .enumerate()
        .filter_map(|(b, c)| at.get(&c.0).map(|&s| (b, s)))
        .collect();
    let n = shared.len();
    let mut best = vec![1usize; n]; // longest increasing run starting at i
    for i in (0..n).rev() {
        for j in i + 1..n {
            if shared[j].1 > shared[i].1 {
                best[i] = best[i].max(best[j] + 1);
            }
        }
    }
    let mut out = Vec::new();
    let mut need = best.iter().copied().max().unwrap_or(0);
    let mut last: Option<usize> = None;
    for i in 0..n {
        if need > 0 && best[i] == need && last.is_none_or(|l| shared[i].1 > l) {
            out.push(shared[i]);
            last = Some(shared[i].1);
            need -= 1;
        }
    }
    out
}

#[derive(Clone, Debug)]
struct OEdit {
    side: u8,
    /// Side indexes inserted.
    ins: std::ops::Range<usize>,
    lines: (usize, usize),
}

/// Steps 2 – 3: the edits of a side, and base index → side index for kept characters.
fn edits(
    side_id: u8,
    base: &[(CharId, char)],
    side: &[(CharId, char)],
) -> (Vec<OEdit>, Vec<Option<usize>>) {
    let line = |i: usize| base[..i].iter().filter(|c| c.1 == '\n').count();
    let k = kept(base, side);
    let mut map = vec![None; base.len()];
    for &(b, s) in &k {
        map[b] = Some(s);
    }
    let mut out = Vec::new();
    let (mut pb, mut ps) = (0, 0);
    for (b, s) in k.into_iter().chain([(base.len(), side.len())]) {
        if pb < b || ps < s {
            let lines = if pb < b {
                (line(pb), line(b - 1) + 1)
            } else if pb == 0 || base[pb - 1].1 == '\n' {
                (line(pb), line(pb))
            } else {
                (line(pb - 1), line(pb - 1) + 1)
            };
            out.push(OEdit {
                side: side_id,
                ins: ps..s,
                lines,
            });
        }
        (pb, ps) = (b + 1, s + 1);
    }
    (out, map)
}

/// Step 4: clusters as (region, member indexes), ascending by region.
fn oclusters(edits: &[OEdit]) -> Vec<((usize, usize), Vec<usize>)> {
    let meet = |x: &OEdit, y: &OEdit| {
        let ((a1, b1), (a2, b2)) = (x.lines, y.lines);
        match (a1 == b1, a2 == b2) {
            (false, false) => a1 < b2 && a2 < b1,
            (true, true) => a1 == a2,
            (true, false) => a2 < a1 && a1 < b2,
            (false, true) => a1 < a2 && a2 < b1,
        }
    };
    let mut group: Vec<usize> = (0..edits.len()).collect();
    loop {
        let mut changed = false;
        for i in 0..edits.len() {
            for j in 0..edits.len() {
                if group[i] != group[j] && meet(&edits[i], &edits[j]) {
                    let g = group[i].min(group[j]);
                    let (gi, gj) = (group[i], group[j]);
                    for x in group.iter_mut() {
                        if *x == gi || *x == gj {
                            *x = g;
                        }
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut by: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, g) in group.into_iter().enumerate() {
        by.entry(g).or_default().push(i);
    }
    let mut out: Vec<((usize, usize), Vec<usize>)> = by
        .into_values()
        .map(|m| {
            let a = m.iter().map(|&i| edits[i].lines.0).min().unwrap();
            let b = m.iter().map(|&i| edits[i].lines.1).max().unwrap();
            ((a, b), m)
        })
        .collect();
    out.sort_by_key(|x| x.0);
    out
}

/// Step 5: a side's region characters, as side indexes in its order.
fn oregion(
    base: &[(CharId, char)],
    map: &[Option<usize>],
    edits: &[OEdit],
    side: u8,
    (a, b): (usize, usize),
    members: &[usize],
) -> Vec<usize> {
    let line = |i: usize| base[..i].iter().filter(|c| c.1 == '\n').count();
    let mut xs: Vec<usize> = (0..base.len())
        .filter(|&i| (a..b).contains(&line(i)))
        .filter_map(|i| map[i])
        .collect();
    for &m in members {
        if edits[m].side == side {
            xs.extend(edits[m].ins.clone());
        }
    }
    xs.sort_unstable();
    xs.dedup();
    xs
}

/// The clusters with edits of both sides, and whether each converged (steps 4 – 6).
fn meets(base: &State, sa: &State, sb: &State, o: &ObjectId) -> BTreeSet<((u64, u64), bool)> {
    let (b, ta, tb) = (seq(base, o), seq(sa, o), seq(sb, o));
    let (ea, ma) = edits(0, &b, &ta);
    let (eb, mb) = edits(1, &b, &tb);
    let all: Vec<OEdit> = ea.into_iter().chain(eb).collect();
    let text = |s: &[(CharId, char)], xs: &[usize]| xs.iter().map(|&x| s[x].1).collect::<String>();
    oclusters(&all)
        .into_iter()
        .filter(|(_, m)| m.iter().any(|&i| all[i].side == 0) && m.iter().any(|&i| all[i].side == 1))
        .map(|(r, m)| {
            let ra = oregion(&b, &ma, &all, 0, r, &m);
            let rb = oregion(&b, &mb, &all, 1, r, &m);
            ((r.0 as u64, r.1 as u64), text(&ta, &ra) == text(&tb, &rb))
        })
        .collect()
}

/// TL-131's ranges of one side's clusters.
#[allow(clippy::type_complexity)]
fn touch_lines(base: &State, head: &State, o: &ObjectId) -> (Vec<(u64, u64)>, Vec<(u64, u64)>) {
    let (b, h) = (seq(base, o), seq(head, o));
    let (e, map) = edits(0, &b, &h);
    let lf = |upto: usize| h[..upto].iter().filter(|c| c.1 == '\n').count() as u64;
    let line = |i: usize| b[..i].iter().filter(|c| c.1 == '\n').count();
    let mut base_lines = Vec::new();
    let mut head_lines = Vec::new();
    for (r, m) in oclusters(&e) {
        base_lines.push((r.0 as u64, r.1 as u64));
        let region = oregion(&b, &map, &e, 0, r, &m);
        let s: String = region.iter().map(|&x| h[x].1).collect();
        let k = s.matches('\n').count() as u64 + u64::from(!s.is_empty() && !s.ends_with('\n'));
        let start = match region.first() {
            Some(&x) => lf(x),
            None => (0..b.len())
                .filter(|&i| line(i) < r.0)
                .filter_map(|i| map[i])
                .max()
                .map_or(0, |x| lf(x + 1)),
        };
        head_lines.push((start, start + k));
    }
    head_lines.sort_unstable();
    (base_lines, head_lines)
}

// ---------------------------------------------------------------------------------------------
// Checks

/// The conflicts the overlap of `(a, b)` may have under TL-134 and TL-134a, as overlap
/// entries: the plan that merges `b` into `a`; when that merge is up to date, the plan that
/// merges `a` into `b` with the sides swapped if `a` holds all of `b` (TL-70), else none (an
/// empty plan). The closure test is not public, so both readings of that case are offered.
fn mapped_conflicts(h: &History, a: &OpId, b: &OpId) -> Vec<Vec<OverlapEntry>> {
    let (plan, swapped) = match (h.plan_merge(a, b), h.plan_merge(b, a)) {
        (Ok(p), _) => (p, false),
        (Err(Error::UpToDate), Ok(p)) => (p, true),
        (Err(Error::UpToDate), Err(Error::UpToDate)) => return vec![Vec::new()],
        (x, y) => panic!("plan {a} {b}: {:?} {:?}", x.err(), y.err()),
    };
    let base = h.state(&At::Version(plan.base.clone())).unwrap();
    let side = |s: &Side| match s {
        Side::Target => OverlapSide::A,
        Side::Incoming => OverlapSide::B,
    };
    let mut out: Vec<OverlapEntry> = plan
        .conflicts
        .iter()
        .map(|c| match c {
            Conflict::Text(hunk) => OverlapEntry::Region {
                object: hunk.object.clone(),
                lines: hunk.lines,
                converged: false,
            },
            Conflict::ModifyDelete {
                object,
                key,
                deleted_by,
            } => OverlapEntry::ModifyDelete {
                object: ObjectId::Op(base.map_value(object, key).unwrap().obj_field().unwrap()),
                path: key.clone(),
                deleted_by: side(deleted_by),
            },
            Conflict::Rename {
                content,
                target,
                incoming,
                ..
            } => OverlapEntry::Rename {
                object: ObjectId::Op(content.clone()),
                a: target.clone(),
                b: incoming.clone(),
            },
            Conflict::Path { object, paths } => OverlapEntry::Path {
                object: object.clone(),
                paths: paths.clone(),
            },
            Conflict::Map { object, key, .. } => OverlapEntry::MapKey {
                object: object.clone(),
                key: key.clone(),
            },
            Conflict::Register { object, .. } => OverlapEntry::Register {
                object: object.clone(),
            },
        })
        .map(|e| if swapped { swap(&e) } else { e })
        .collect();
    out.sort_by_key(|e| format!("{e:?}"));
    if swapped {
        vec![out, Vec::new()]
    } else {
        vec![out]
    }
}

fn swap(e: &OverlapEntry) -> OverlapEntry {
    match e.clone() {
        OverlapEntry::ModifyDelete {
            object,
            path,
            deleted_by,
        } => OverlapEntry::ModifyDelete {
            object,
            path,
            deleted_by: match deleted_by {
                OverlapSide::A => OverlapSide::B,
                OverlapSide::B => OverlapSide::A,
            },
        },
        OverlapEntry::Rename { object, a, b } => OverlapEntry::Rename { object, a: b, b: a },
        other => other,
    }
}

type Digests = BTreeMap<(OpId, Option<OpId>), ([u8; 32], bool)>;

/// Every overlap and touch-set digest of a history.
fn digests(h: &History) -> Digests {
    let ts: Vec<OpId> = h.timelines().into_iter().map(|t| t.id).collect();
    let mut out = BTreeMap::new();
    for a in &ts {
        if let Ok(Some(t)) = h.touch_set(a, None) {
            out.insert((a.clone(), None), (t.digest(), t.truncated));
        }
        for b in ts.iter().filter(|b| *b != a) {
            let o = h.overlap(a, b).unwrap();
            out.insert((a.clone(), Some(b.clone())), (o.digest(), o.truncated));
        }
    }
    out
}

/// Asserts, or with `OVERLAP_DIAG` set counts and prints the mismatch.
fn expect(
    ok: bool,
    stats: &mut BTreeMap<&'static str, usize>,
    what: &'static str,
    msg: impl FnOnce() -> String,
) {
    if ok {
        return;
    }
    if std::env::var_os("OVERLAP_DIAG").is_some() {
        *stats.entry(what).or_default() += 1;
        eprintln!("{what}: {}", msg());
    } else {
        panic!("{}", msg());
    }
}

fn check(h: &History, kind: HistoryKind, stats: &mut BTreeMap<&'static str, usize>) {
    let ts: Vec<OpId> = h.timelines().into_iter().map(|t| t.id).collect();
    let before = (h.timelines(), h.inventory(), h.envelopes().count());
    for a in &ts {
        for b in ts.iter().filter(|b| *b != a) {
            let ov = h.overlap(a, b).unwrap();
            assert!(!ov.truncated);
            // TL-134: the conflicting entries are the plan's conflicts.
            let mut conflicts: Vec<OverlapEntry> = ov
                .entries
                .iter()
                .filter(|e| e.is_conflict())
                .cloned()
                .collect();
            conflicts.sort_by_key(|e| format!("{e:?}"));
            let allowed = mapped_conflicts(h, a, b);
            assert!(
                allowed.contains(&conflicts),
                "TL-134 for {a} {b}: {conflicts:?} not in {allowed:?}"
            );
            // Region entries are the clusters both sides meet in, by an independent reading.
            let base = h.state(&At::Version(ov.base.clone())).unwrap();
            let sa = h.state(&At::Timeline(a.clone())).unwrap();
            let sb = h.state(&At::Timeline(b.clone())).unwrap();
            let mut want: BTreeSet<(ObjectId, (u64, u64), bool)> = BTreeSet::new();
            for o in sa
                .object_ids()
                .filter(|o| sa.text(o).is_some() && sb.text(o).is_some())
            {
                for (lines, converged) in meets(&base, &sa, &sb, o) {
                    want.insert((o.clone(), lines, converged));
                }
            }
            let got: BTreeSet<(ObjectId, (u64, u64), bool)> = ov
                .entries
                .iter()
                .filter_map(|e| match e {
                    OverlapEntry::Region {
                        object,
                        lines,
                        converged,
                    } => Some((object.clone(), *lines, *converged)),
                    _ => None,
                })
                .collect();
            assert_eq!(got, want, "regions of {a} {b}");
            // Symmetry (TL-133): swapping the timelines swaps `a`/`b` and `deleted_by`; only
            // `rename` and `path` entries, judged by TL-87 after the clean changes, may differ.
            // A `file` entry stands for an object with no other entry, so an object whose
            // `rename` shows in one direction only has a `file` entry in the other
            // (SPEC-QUESTIONS Q26); every other entry is symmetric.
            let rev = h.overlap(b, a).unwrap();
            let swapped: Vec<OverlapEntry> = ov.entries.iter().map(swap).collect();
            let renamed: BTreeSet<&ObjectId> = swapped
                .iter()
                .chain(&rev.entries)
                .filter(|e| matches!(e, OverlapEntry::Rename { .. }))
                .map(OverlapEntry::object)
                .collect();
            let strict = |es: &[OverlapEntry]| -> Vec<OverlapEntry> {
                es.iter()
                    .filter(|e| match e {
                        OverlapEntry::Rename { .. } | OverlapEntry::Path { .. } => false,
                        OverlapEntry::File { object } => !renamed.contains(object),
                        _ => true,
                    })
                    .cloned()
                    .collect()
            };
            expect(
                strict(&rev.entries) == strict(&swapped),
                stats,
                "asymmetric",
                || {
                    let (x, y) = (strict(&swapped), strict(&rev.entries));
                    let only = |p: &[OverlapEntry], q: &[OverlapEntry]| -> Vec<OverlapEntry> {
                        p.iter().filter(|e| !q.contains(e)).cloned().collect()
                    };
                    format!(
                        "symmetry of {a} {b}: only in ({a},{b}) swapped {:?}; only in ({b},{a}) {:?}",
                        only(&x, &y),
                        only(&y, &x)
                    )
                },
            );
            if rev.entries != swapped {
                *stats.entry("rename_or_path_asymmetric").or_default() += 1;
            }
            if kind != HistoryKind::Worktree {
                assert!(ov.entries.iter().all(|e| matches!(
                    e,
                    OverlapEntry::Region { .. }
                        | OverlapEntry::MapKey { .. }
                        | OverlapEntry::Register { .. }
                )));
            }
            for e in &ov.entries {
                *stats.entry(e.class()).or_default() += 1;
                if matches!(
                    e,
                    OverlapEntry::Region {
                        converged: true,
                        ..
                    }
                ) {
                    *stats.entry("converged").or_default() += 1;
                }
            }
        }
        // Touch sets: TL-138 scope, and TL-131 ranges by the independent reading.
        match (kind, h.touch_set(a, None)) {
            (HistoryKind::Worktree, Ok(Some(set))) => {
                let base = h.state(&At::Version(set.base.clone())).unwrap();
                let head = h.state(&At::Timeline(a.clone())).unwrap();
                for e in &set.entries {
                    if e.change.contains(TouchChange::EDITED) {
                        let o = ObjectId::Op(e.object.clone());
                        let (bl, hl) = touch_lines(&base, &head, &o);
                        assert_eq!((&e.base_lines, &e.head_lines), (&bl, &hl), "lines of {o}");
                        *stats.entry("edited").or_default() += 1;
                    }
                }
            }
            (HistoryKind::Worktree, Err(Error::NeedsBase)) => {
                assert_eq!(Some(a.clone()), h.main());
            }
            (HistoryKind::Worktree, other) => panic!("touch set of {a}: {other:?}"),
            (_, got) => assert_eq!(got, Ok(None), "no touch set outside worktrees"),
        }
    }
    // TL-135: nothing changed.
    assert!(
        (h.timelines(), h.inventory(), h.envelopes().count()) == before,
        "a read changed the history"
    );
}

fn scenario(seed: u64, kind: HistoryKind, steps: usize, stats: &mut BTreeMap<&'static str, usize>) {
    let (mut w, dir) = World::new(seed, kind);
    for i in 0..steps {
        w.step();
        if i % 25 == 24 {
            check(&w.history, kind, stats);
        }
    }
    check(&w.history, kind, stats);
    // Permuted delivery orders reach the same results (TL-135).
    let reference = digests(&w.history);
    let envelopes: Vec<Vec<u8>> = w.history.envelopes().map(<[u8]>::to_vec).collect();
    for round in 0..3 {
        let mut order = envelopes.clone();
        if round == 1 {
            order.reverse();
        } else {
            for i in (1..order.len()).rev() {
                order.swap(i, w.rng.below(i + 1));
            }
        }
        let mut fresh = History::open(MemoryStore::default(), SPACE, w.history.id()).unwrap();
        fresh.set_buffer_limits(1 << 20, 1024);
        for e in &order {
            let v = fresh.receive(e, &dir).unwrap().verdict;
            assert!(!matches!(v, Verdict::Rejected(_)), "seed {seed}: {v:?}");
        }
        check(&fresh, kind, stats);
        let got = digests(&fresh);
        for (k, v) in &reference {
            if got.get(k) != Some(v) {
                let (a, b) = k;
                let detail = match b {
                    None => format!(
                        "{:?}\nvs\n{:?}",
                        w.history.touch_set(a, None),
                        fresh.touch_set(a, None)
                    ),
                    Some(b) => format!(
                        "{:?}\nvs\n{:?}",
                        w.history.overlap(a, b),
                        fresh.overlap(a, b)
                    ),
                };
                if let Some(b) = b
                    && let Ok(ov) = w.history.overlap(a, b)
                {
                    for at in [
                        At::Version(ov.base.clone()),
                        At::Timeline(a.clone()),
                        At::Timeline(b.clone()),
                    ] {
                        let x = w.history.state(&at).unwrap();
                        let y = fresh.state(&at).unwrap();
                        for o in x.object_ids() {
                            if x.chars(o) != y.chars(o) {
                                eprintln!(
                                    "{at:?} {o}: {:?}\n vs {:?}",
                                    x.chars(o)
                                        .map(|c| c.iter().map(|c| c.2).collect::<String>()),
                                    y.chars(o)
                                        .map(|c| c.iter().map(|c| c.2).collect::<String>())
                                );
                            }
                        }
                    }
                }
                panic!("seed {seed}: order {round}: {a} {b:?}:\n{detail}");
            }
        }
        assert_eq!(got, reference, "seed {seed}: order {round}");
    }
}

fn seeds() -> u64 {
    std::env::var("SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

#[test]
fn worktree_overlap_equals_planning() {
    let mut stats = BTreeMap::new();
    for seed in 0..seeds() {
        scenario(seed, HistoryKind::Worktree, 120, &mut stats);
    }
    eprintln!("worktree: {stats:?}");
    for class in ["region", "converged", "file", "modify_delete", "edited"] {
        assert!(stats.contains_key(class), "no {class} entry was generated");
    }
}

#[test]
fn data_overlap_equals_planning() {
    let mut stats = BTreeMap::new();
    for seed in 1000..1000 + seeds() {
        scenario(seed, HistoryKind::Data, 120, &mut stats);
    }
    eprintln!("data: {stats:?}");
    for class in ["region", "map_key", "register"] {
        assert!(stats.contains_key(class), "no {class} entry was generated");
    }
}
