//! Checkpoints and time travel (TL-25, TL-26, TL-60): every version a timeline passed through
//! materialises as it was, from any receiver, in any delivery order.

mod common;

use std::collections::BTreeMap;

use common::{Open, Rng, SPACE, signer};
use han_timeline::{At, History, HistoryKind, MemoryStore, ObjectId, Op, Value, Version};

#[test]
fn every_past_version_and_checkpoint_materialises_as_it_was() {
    for seed in 0..10 {
        let mut rng = Rng(seed);
        let a = signer("@alice:a.example", "laptop");
        let b = signer("@bob:b.example", "desk");
        let dir = Open::new(&[&a, &b]);
        let (mut h, g) = History::create(
            MemoryStore::default(),
            SPACE,
            &a,
            0,
            HistoryKind::Data,
            None,
            vec![
                Op::TextCreate {
                    text: String::new(),
                    transcript: false,
                },
                Op::RegisterCreate {
                    value: Value::Int(0),
                },
            ],
        )
        .unwrap();
        let main = h.main().unwrap();
        let text = ObjectId::Op(g.ops[1].clone());
        let reg = ObjectId::Op(g.ops[2].clone());
        // Version → (text, register) as observed right after writing it.
        let mut seen: BTreeMap<Version, (String, Value)> = BTreeMap::new();
        let mut labels: Vec<(String, String)> = Vec::new();
        let snap = |h: &History| {
            let st = h.state(&At::Timeline(main.clone())).unwrap();
            (st.text(&text).unwrap(), st.register(&reg).unwrap().clone())
        };
        seen.insert(h.head(&main).unwrap(), snap(&h));
        for i in 0..60 {
            let at = At::Timeline(main.clone());
            match rng.below(6) {
                0 => {
                    let label = format!("cp{i}");
                    labels.push((label.clone(), snap(&h).0));
                    h.write(&a, 0, &main, vec![Op::Checkpoint { label }], None)
                        .unwrap();
                }
                1 => {
                    h.write(
                        &a,
                        0,
                        &main,
                        vec![Op::RegisterSet {
                            target: reg.clone(),
                            value: Value::Int(i),
                        }],
                        None,
                    )
                    .unwrap();
                }
                _ => {
                    let len = snap(&h).0.chars().count();
                    let pos = rng.below(len + 1);
                    let del = rng.below(len - pos + 1).min(3);
                    if let Some(op) = h.plan_edit(&at, &text, pos, del, &rng.text()).unwrap() {
                        h.write(&a, 0, &main, vec![op], None).unwrap();
                    }
                }
            }
            seen.insert(h.head(&main).unwrap(), snap(&h));
        }
        // Checkpoints: in op id order, each naming the version it was written at.
        let cps = h.checkpoints(&main);
        assert_eq!(cps.len(), labels.len());
        for (cp, (label, text_then)) in cps.iter().zip(&labels) {
            assert_eq!(&cp.label, label);
            let st = h.state(&At::Version(cp.version.clone())).unwrap();
            assert_eq!(&st.text(&text).unwrap(), text_then, "seed {seed}: {label}");
        }
        // Time travel, on this history and on a receiver fed the batches in reverse order.
        let mut r = History::open(MemoryStore::default(), SPACE, h.id()).unwrap();
        let envs: Vec<Vec<u8>> = h.envelopes().map(<[u8]>::to_vec).collect();
        for e in envs.iter().rev() {
            r.receive(e, &dir).unwrap();
        }
        for (v, (t, val)) in &seen {
            for hist in [&h, &r] {
                let st = hist.state(&At::Version(v.clone())).unwrap();
                assert_eq!(st.text(&text).as_ref(), Some(t), "seed {seed} at {v}");
                assert_eq!(st.register(&reg), Some(val), "seed {seed} at {v}");
            }
        }
        // The root version holds only the root map.
        let root = h.state(&At::Version(Version::root())).unwrap();
        assert_eq!(root.object_ids().count(), 1);
        // Forking at a checkpoint starts from exactly its version (TL-26).
        if let Some(cp) = cps.first() {
            let f = h.fork(&b, 0, &main, Some(&cp.version), "from-cp").unwrap();
            let fork = f.ops[0].clone();
            let st = h.state(&At::Timeline(fork)).unwrap();
            assert_eq!(st.text(&text).unwrap(), labels[0].1);
        }
    }
}

#[test]
fn concurrent_versions_and_their_join() {
    let a = signer("@alice:a.example", "laptop");
    let b = signer("@bob:b.example", "desk");
    let dir = Open::new(&[&a, &b]);
    let (mut ha, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![Op::TextCreate {
            text: "ab".into(),
            transcript: false,
        }],
    )
    .unwrap();
    let main = ha.main().unwrap();
    let text = ObjectId::Op(g.ops[1].clone());
    let mut hb = History::open(MemoryStore::default(), SPACE, ha.id()).unwrap();
    hb.receive(&g.envelope, &dir).unwrap();
    let at = At::Timeline(main.clone());
    let ea = ha.plan_edit(&at, &text, 1, 0, "X").unwrap().unwrap();
    let wa = ha.write(&a, 0, &main, vec![ea], None).unwrap();
    let eb = hb.plan_edit(&at, &text, 2, 0, "Y").unwrap().unwrap();
    let wb = hb.write(&b, 0, &main, vec![eb], None).unwrap();
    ha.receive(&wb.envelope, &dir).unwrap();
    let va = Version::new(vec![wa.ops[0].clone()]);
    let vb = Version::new(vec![wb.ops[0].clone()]);
    let both = Version::new(vec![wa.ops[0].clone(), wb.ops[0].clone()]);
    let text_at = |v: &Version| {
        ha.state(&At::Version(v.clone()))
            .unwrap()
            .text(&text)
            .unwrap()
    };
    assert_eq!(text_at(&va), "aXb");
    assert_eq!(text_at(&vb), "abY");
    assert_eq!(text_at(&both), "aXbY");
    // The head has both ops; any set of held ops is accepted and normalised (TL-13).
    assert_eq!(ha.head(&main).unwrap(), both);
    let with_past = Version::new(vec![g.ops[1].clone(), wa.ops[0].clone(), wb.ops[0].clone()]);
    assert_eq!(ha.version(&At::Version(with_past)).unwrap(), both);
}
