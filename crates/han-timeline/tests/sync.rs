//! Property tests for integration and sync (TL-100 – TL-104, TL-110 – TL-112): any delivery
//! order, duplicates, bounded hold-back with drops, cut answers, and message validation.

mod common;

use std::collections::BTreeMap;

use common::{Open, Rng, SPACE, signer};
use han_timeline::{
    Answer, At, Hello, History, HistoryId, HistoryKind, Inventory, KeySigner, MemoryStore,
    ObjectId, Op, OpId, Reason, ReplicaId, Value, Verdict, Version,
};

/// Three writers on their own copies, with forks and occasional exchanges, so the batches form
/// a concurrent graph over several timelines. Returns the history id and every envelope.
fn build(seed: u64, steps: usize) -> (HistoryId, Vec<Vec<u8>>, Open) {
    let mut rng = Rng(seed);
    let signers: Vec<KeySigner> = vec![
        signer("@alice:a.example", "laptop"),
        signer("@bob:b.example", "desk"),
        signer("@carol:c.example", "pad"),
    ];
    let dir = Open::new(&signers.iter().collect::<Vec<_>>());
    let (first, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &signers[0],
        0,
        HistoryKind::Data,
        None,
        vec![Op::TextCreate {
            text: "x\n".into(),
            transcript: false,
        }],
    )
    .unwrap();
    let id = first.id();
    let text = ObjectId::Op(g.ops[1].clone());
    let mut copies: Vec<History> = vec![first];
    for _ in 1..signers.len() {
        let mut h = History::open(MemoryStore::default(), SPACE, id).unwrap();
        h.receive(&g.envelope, &dir).unwrap();
        copies.push(h);
    }
    for _ in 0..steps {
        let i = rng.below(copies.len());
        if rng.chance(15) {
            let j = rng.below(copies.len());
            let envs: Vec<Vec<u8>> = copies[j].envelopes().map(<[u8]>::to_vec).collect();
            for e in envs {
                copies[i].receive(&e, &dir).unwrap();
            }
            continue;
        }
        let h = &mut copies[i];
        let timelines: Vec<OpId> = h.timelines().into_iter().map(|t| t.id).collect();
        let t = timelines[rng.below(timelines.len())].clone();
        if rng.chance(10) && timelines.len() < 4 {
            h.fork(&signers[i], 0, &t, None, "s").unwrap();
        } else if rng.chance(50) {
            let at = At::Timeline(t.clone());
            let len = h.state(&at).unwrap().text(&text).unwrap().chars().count();
            let pos = rng.below(len + 1);
            let del = rng.below(len - pos + 1).min(2);
            let ins = rng.text();
            if let Some(op) = h.plan_edit(&at, &text, pos, del, &ins).unwrap() {
                h.write(&signers[i], 0, &t, vec![op], None).unwrap();
            }
        } else {
            let ops: Vec<Op> = (0..1 + rng.below(3))
                .map(|_| Op::MapSet {
                    target: ObjectId::Root,
                    key: format!("k{}", rng.below(4)),
                    value: Value::Int(rng.below(9) as i64),
                })
                .collect();
            h.write(&signers[i], 0, &t, ops, None).unwrap();
        }
    }
    // Everything every copy wrote, each envelope once.
    let mut all: BTreeMap<Vec<u8>, ()> = BTreeMap::new();
    for h in &copies {
        for e in h.envelopes() {
            all.insert(e.to_vec(), ());
        }
    }
    (id, all.into_keys().collect(), dir)
}

fn digests(h: &History) -> BTreeMap<OpId, (Version, [u8; 32])> {
    h.timelines()
        .into_iter()
        .map(|t| {
            let d = h.state(&At::Timeline(t.id.clone())).unwrap().digest();
            (t.id, (t.head, d))
        })
        .collect()
}

/// The reference: everything delivered with an unbounded buffer.
fn reference(id: HistoryId, envelopes: &[Vec<u8>], dir: &Open) -> History {
    let mut h = History::open(MemoryStore::default(), SPACE, id).unwrap();
    h.set_buffer_limits(usize::MAX, usize::MAX);
    for _ in 0..2 {
        for e in envelopes {
            h.receive(e, dir).unwrap();
        }
    }
    assert!(h.held_back().is_empty());
    assert_eq!(h.envelopes().count(), envelopes.len());
    h
}

#[test]
fn any_order_with_duplicates_reaches_the_same_state() {
    for seed in 0..5 {
        let (id, envelopes, dir) = build(seed, 100);
        let want = digests(&reference(id, &envelopes, &dir));
        let mut rng = Rng(seed ^ 0xabc);
        for _ in 0..2 {
            let mut order: Vec<&Vec<u8>> = envelopes.iter().collect();
            let dups: Vec<&Vec<u8>> = (0..order.len() / 3)
                .map(|_| &envelopes[rng.below(envelopes.len())])
                .collect();
            order.extend(dups);
            rng.shuffle(&mut order);
            let mut h = History::open(MemoryStore::default(), SPACE, id).unwrap();
            h.set_buffer_limits(usize::MAX, 1024);
            for e in order {
                let v = h.receive(e, &dir).unwrap().verdict;
                assert!(
                    matches!(v, Verdict::Integrated | Verdict::Held | Verdict::Duplicate),
                    "seed {seed}: {v:?}"
                );
            }
            assert!(h.held_back().is_empty());
            assert_eq!(digests(&h), want, "seed {seed}");
        }
    }
}

#[test]
fn a_bounded_hold_back_buffer_drops_and_sync_brings_batches_back() {
    for seed in 0..4 {
        let (id, envelopes, dir) = build(seed + 50, 120);
        let source = reference(id, &envelopes, &dir);
        let want = digests(&source);
        let mut rng = Rng(seed);
        let mut order = envelopes.clone();
        rng.shuffle(&mut order);
        let mut h = History::open(MemoryStore::default(), SPACE, id).unwrap();
        h.set_buffer_limits(4, 1024);
        let mut missing = 0;
        for e in &order {
            let r = h.receive(e, &dir).unwrap();
            missing += usize::from(r.verdict == Verdict::Missing);
            missing += r
                .others
                .iter()
                .filter(|(_, v)| *v == Verdict::Missing)
                .count();
            assert!(h.held_back().len() <= 4);
        }
        // Sync rounds bring back whatever was dropped (TL-102).
        for round in 0.. {
            assert!(round < 1000, "seed {seed}: sync does not finish");
            let hello = h.hello(None);
            let answer = source.answer_to(&hello).unwrap();
            if answer.envelopes.is_empty() {
                break;
            }
            for e in &answer.envelopes {
                h.receive(e, &dir).unwrap();
            }
        }
        assert!(
            missing > 0 || seed > 0,
            "the buffer should overflow at least once"
        );
        assert_eq!(digests(&h), want, "seed {seed}");
        assert_eq!(h.inventory(), source.inventory());
    }
}

/// `x` syncs from `y` with answers cut at `max`, checking each answer.
fn pull(x: &mut History, y: &History, max: u64, dir: &Open) {
    for round in 0.. {
        assert!(round < 1000);
        let hello = x.hello(Some(max)).encode();
        let bytes = y.answer(&hello).unwrap();
        let answer = Answer::decode(&bytes, max).unwrap();
        assert!(bytes.len() as u64 <= max || answer.envelopes.len() == 1);
        // Ascending start, then replica (a causal order, TL-110).
        let keys: Vec<(u64, ReplicaId)> = answer
            .envelopes
            .iter()
            .map(|e| {
                let (b, _) = han_timeline::Envelope::decode(e).unwrap().batch().unwrap();
                (b.start, (*b.replica).clone())
            })
            .collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]));
        let (reports, more) = x.receive_answer(&bytes, Some(max), dir).unwrap().unwrap();
        for r in reports {
            assert!(!matches!(r.verdict, Verdict::Rejected(_)), "{r:?}");
        }
        if !more {
            break;
        }
    }
}

#[test]
fn cut_answers_fit_and_two_way_sync_converges() {
    for seed in 0..4 {
        let (id, envelopes, dir) = build(seed + 100, 150);
        let (half_a, half_b): (Vec<_>, Vec<_>) =
            envelopes.iter().enumerate().partition(|(i, _)| i % 3 != 0);
        let mut a = History::open(MemoryStore::default(), SPACE, id).unwrap();
        let mut b = History::open(MemoryStore::default(), SPACE, id).unwrap();
        for h in [&mut a, &mut b] {
            h.set_buffer_limits(usize::MAX, 1024);
        }
        for (_, e) in half_a {
            a.receive(e, &dir).unwrap();
        }
        for (_, e) in half_b {
            b.receive(e, &dir).unwrap();
        }
        let max = 64 * 1024u64;
        // Held-back batches are never relayed, and one that integrates during a pull is news
        // for the other side: rounds go on until a whole round changes nothing.
        for round in 0.. {
            assert!(round < 100);
            let before = (a.inventory(), b.inventory());
            pull(&mut a, &b, max, &dir);
            pull(&mut b, &a, max, &dir);
            if (a.inventory(), b.inventory()) == before {
                break;
            }
        }
        assert!(a.held_back().is_empty() && b.held_back().is_empty());
        assert_eq!(a.inventory(), b.inventory(), "seed {seed}");
        assert_eq!(digests(&a), digests(&b), "seed {seed}");
        assert_eq!(digests(&a), digests(&reference(id, &envelopes, &dir)));
    }
}

#[test]
fn sync_messages_are_validated() {
    let (id, envelopes, dir) = build(7, 20);
    let h = reference(id, &envelopes, &dir);
    let r = ReplicaId::new("@alice:a.example", "laptop", 0);
    let hello = |ranges: Vec<(u64, u64)>, max: Option<u64>| {
        let mut inv = BTreeMap::new();
        inv.insert(r.clone(), ranges);
        Hello {
            space: SPACE.into(),
            history: id,
            inventory: Inventory(inv),
            max_bytes: max,
        }
        .encode()
    };
    assert!(h.answer(&hello(vec![(1, 2), (5, 9)], None)).is_ok());
    for bad in [
        vec![(0, 2)],         // counter 0
        vec![(3, 2)],         // reversed
        vec![(1, 4), (3, 9)], // overlapping
        vec![(5, 9), (1, 2)], // out of order
        vec![(1, 2), (3, 4)], // adjacent
    ] {
        assert_eq!(
            h.answer(&hello(bad.clone(), None)),
            Err(Reason::Malformed),
            "{bad:?}"
        );
    }
    // maxBytes below 64 KiB, another history, non-deterministic CBOR.
    assert_eq!(h.answer(&hello(vec![], Some(1000))), Err(Reason::Malformed));
    let mut other = Hello::decode(&hello(vec![], None)).unwrap();
    other.history = han_timeline::BatchId([7; 32]);
    assert_eq!(h.answer(&other.encode()), Err(Reason::Malformed));
    let mut bytes = hello(vec![], None);
    bytes[2] = 0x18; // the value 1 of key 1 in a longer form
    bytes.insert(3, 0x01);
    assert_eq!(h.answer(&bytes), Err(Reason::Malformed));
    // A held-back batch is not in the inventory; an equivocation is refused and proven.
    let mut fresh = History::open(MemoryStore::default(), SPACE, id).unwrap();
    let late = envelopes
        .iter()
        .find(|e| {
            let (b, _) = han_timeline::Envelope::decode(e).unwrap().batch().unwrap();
            !b.parents.is_root()
        })
        .unwrap();
    assert_eq!(fresh.receive(late, &dir).unwrap().verdict, Verdict::Held);
    assert!(fresh.inventory().0.is_empty());
}

#[test]
fn an_equivocating_writer_leaves_a_fault_proof() {
    let a = signer("@alice:a.example", "laptop");
    let dir = Open::new(&[&a]);
    let (mut h1, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![],
    )
    .unwrap();
    let main = h1.main().unwrap();
    // A second copy that lost its high-water mark reuses counter 2 for different content.
    let mut h2 = History::open(MemoryStore::default(), SPACE, h1.id()).unwrap();
    h2.receive(&g.envelope, &dir).unwrap();
    let set = |v| Op::MapSet {
        target: ObjectId::Root,
        key: "k".into(),
        value: Value::Int(v),
    };
    let one = h1.write(&a, 0, &main, vec![set(1)], None).unwrap();
    let two = h2.write(&a, 0, &main, vec![set(2)], None).unwrap();
    assert_eq!(one.ops, two.ops);
    let r = h1.receive(&two.envelope, &dir).unwrap();
    assert_eq!(r.verdict, Verdict::Rejected(Reason::Equivocation));
    assert_eq!(h1.faults().len(), 1);
    assert_eq!(&*h1.faults()[0].0, one.envelope.as_slice());
    assert_eq!(&*h1.faults()[0].1, two.envelope.as_slice());
}
