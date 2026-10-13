//! TL-103 and TL-104 with a directory that denies writes: lost or missing roles, revoked keys,
//! the session's actor in transcripts, quarantine, vouching and drops.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use han_timeline::{
    At, Directory, History, HistoryKind, KeyInfo, KeySigner, MemoryStore, ObjectId, Op, Scope,
    Signer as _, Value, Verdict,
};
use sha2::{Digest, Sha256};

const SPACE: &str = "s1:a.example";
const SESSION: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

fn signer(actor: &str, key: &str) -> KeySigner {
    let seed: [u8; 32] = Sha256::digest(format!("auth-{actor}-{key}")).into();
    KeySigner::from_seed(actor, key, seed)
}

/// Profiles and roles the test changes as it goes; records every scope it is asked about.
struct Dir {
    keys: HashMap<(String, String), KeyInfo>,
    members: HashSet<String>,
    session_actor: String,
    asked: RefCell<Vec<String>>,
}

impl Dir {
    fn new(signers: &[&KeySigner]) -> Self {
        Dir {
            keys: signers
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
                .collect(),
            members: signers.iter().map(|s| s.actor().to_string()).collect(),
            session_actor: "@alice:a.example".into(),
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl Directory for Dir {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        self.keys
            .get(&(actor.to_string(), key.to_string()))
            .copied()
    }
    fn may_write(&self, actor: &str, scope: Scope<'_>) -> bool {
        self.asked.borrow_mut().push(format!("{actor} {scope:?}"));
        match scope {
            Scope::Space => self.members.contains(actor),
            Scope::Session(s) => s == SESSION && actor == self.session_actor,
        }
    }
}

fn set(key: &str, v: i64) -> Op {
    Op::MapSet {
        target: ObjectId::Root,
        key: key.into(),
        value: Value::Int(v),
    }
}

fn root_keys(h: &History) -> Vec<String> {
    let main = h.main().unwrap();
    match h
        .state(&At::Timeline(main))
        .unwrap()
        .object(&ObjectId::Root)
    {
        Some(han_timeline::ObjectState::Map { entries, .. }) => entries.into_keys().collect(),
        _ => Vec::new(),
    }
}

/// A data history by alice's laptop `a`; carol (`c`) writes `x` on main, then `x2` on top;
/// alice writes `y` on top of `x` and `y2` on top of `x2`; carol also writes `z` concurrently.
struct World {
    a: KeySigner,
    c: KeySigner,
    id: han_timeline::HistoryId,
    genesis: Vec<u8>,
    x: Vec<u8>,
    x2: Vec<u8>,
    y: Vec<u8>,
    y2: Vec<u8>,
    z: Vec<u8>,
}

fn world() -> World {
    let a = signer("@alice:a.example", "laptop");
    let c = signer("@carol:c.example", "desk");
    let everyone = Dir::new(&[&a, &c]);
    let (mut ha, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &a,
        0,
        HistoryKind::Data,
        None,
        vec![],
    )
    .unwrap();
    let main = ha.main().unwrap();
    let mut hc = History::open(MemoryStore::default(), SPACE, ha.id()).unwrap();
    hc.receive(&g.envelope, &everyone).unwrap();
    let x = hc.write(&c, 0, &main, vec![set("k", 1)], None).unwrap();
    let z_base = hc.head(&main).unwrap();
    let x2 = hc.write(&c, 0, &main, vec![set("k", 2)], None).unwrap();
    // y builds on x only: alice's copy has not seen x2 yet.
    ha.receive(&x.envelope, &everyone).unwrap();
    let y = ha.write(&a, 0, &main, vec![set("y", 1)], None).unwrap();
    ha.receive(&x2.envelope, &everyone).unwrap();
    let y2 = ha.write(&a, 0, &main, vec![set("y2", 1)], None).unwrap();
    // z: carol, concurrent with x2, on top of x.
    let mut hz = History::open(MemoryStore::default(), SPACE, ha.id()).unwrap();
    hz.receive(&g.envelope, &everyone).unwrap();
    hz.receive(&x.envelope, &everyone).unwrap();
    assert_eq!(hz.head(&main).unwrap(), z_base);
    let z = hz.write(&c, 1, &main, vec![set("z", 1)], None).unwrap();
    World {
        id: ha.id(),
        genesis: g.envelope,
        x: x.envelope,
        x2: x2.envelope,
        y: y.envelope,
        y2: y2.envelope,
        z: z.envelope,
        a,
        c,
    }
}

fn receiver(w: &World, dir: &Dir) -> History {
    let mut h = History::open(MemoryStore::default(), SPACE, w.id).unwrap();
    assert_eq!(
        h.receive(&w.genesis, dir).unwrap().verdict,
        Verdict::Integrated
    );
    h
}

fn deliver(h: &mut History, e: &[u8], dir: &Dir) -> Verdict {
    h.receive(e, dir).unwrap().verdict
}

#[test]
fn a_writer_without_a_role_is_quarantined_until_vouched() {
    let w = world();
    let mut dir = Dir::new(&[&w.a, &w.c]);
    dir.members.remove(w.c.actor());

    // Quarantined, out of the state and the inventory; delivering it again changes nothing.
    let mut h = receiver(&w, &dir);
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Quarantined);
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Quarantined);
    assert_eq!(h.quarantined().len(), 1);
    assert_eq!(root_keys(&h), Vec::<String>::new());
    assert_eq!(h.inventory().0.len(), 1);
    // An authorised batch that depends on it vouches for it.
    assert_eq!(deliver(&mut h, &w.y, &dir), Verdict::Integrated);
    assert!(h.quarantined().is_empty());
    assert_eq!(root_keys(&h), vec!["k", "y"]);

    // The other order: the authorised batch waits, and the quarantined one is integrated at once.
    let mut h = receiver(&w, &dir);
    assert_eq!(deliver(&mut h, &w.y, &dir), Verdict::Held);
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Integrated);
    assert!(h.held_back().is_empty() && h.quarantined().is_empty());
    assert_eq!(root_keys(&h), vec!["k", "y"]);
}

#[test]
fn vouching_is_transitive_and_leaves_unrelated_batches_quarantined() {
    let w = world();
    let mut dir = Dir::new(&[&w.a, &w.c]);
    dir.members.remove(w.c.actor());
    for order in [
        [&w.x, &w.x2, &w.z, &w.y, &w.y2],
        [&w.y2, &w.y, &w.z, &w.x2, &w.x],
    ] {
        let mut h = receiver(&w, &dir);
        for e in order {
            deliver(&mut h, e, &dir);
        }
        // y2 needs x2, which needs x; z is carol's alone and nothing authorised builds on it.
        assert_eq!(h.quarantined().len(), 1, "only z stays");
        assert!(h.held_back().is_empty());
        assert_eq!(root_keys(&h), vec!["k", "y", "y2"]);
    }
}

#[test]
fn a_role_lost_later_removes_nothing() {
    let w = world();
    let mut dir = Dir::new(&[&w.a, &w.c]);
    let mut h = receiver(&w, &dir);
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Integrated);
    dir.members.remove(w.c.actor());
    // Integrated ops stay; the same envelope is a duplicate; carol's new work is quarantined.
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Duplicate);
    assert_eq!(root_keys(&h), vec!["k"]);
    assert_eq!(deliver(&mut h, &w.x2, &dir), Verdict::Quarantined);
    assert_eq!(root_keys(&h), vec!["k"]);
    // Reopening from the store keeps what was integrated, whatever the roles are now.
    let reopened = History::open(h.store().clone(), SPACE, w.id).unwrap();
    assert_eq!(root_keys(&reopened), vec!["k"]);
}

#[test]
fn a_revoked_key_is_quarantined_even_for_a_member() {
    let w = world();
    let mut dir = Dir::new(&[&w.a, &w.c]);
    dir.keys
        .get_mut(&(w.c.actor().to_string(), w.c.key_id().to_string()))
        .unwrap()
        .valid = false;
    let mut h = receiver(&w, &dir);
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Quarantined);
    // A key no profile lists is not quarantined but rejected.
    dir.keys
        .remove(&(w.c.actor().to_string(), w.c.key_id().to_string()));
    assert_eq!(
        deliver(&mut h, &w.x2, &dir),
        Verdict::Rejected(han_timeline::Reason::UnknownKey)
    );
}

#[test]
fn a_full_quarantine_drops_the_greatest_start_as_unauthorized() {
    let w = world();
    let mut dir = Dir::new(&[&w.a, &w.c]);
    dir.members.remove(w.c.actor());
    // x (start 2) then x2 (start 3): x2 has the greatest start and is dropped on arrival.
    let mut h = receiver(&w, &dir);
    h.set_buffer_limits(1024, 1);
    assert_eq!(deliver(&mut h, &w.x, &dir), Verdict::Quarantined);
    assert_eq!(deliver(&mut h, &w.x2, &dir), Verdict::Unauthorized);
    assert_eq!(h.quarantined().len(), 1);
    // The other order: x2 first, then x pushes it out and the report says so.
    let mut h = receiver(&w, &dir);
    h.set_buffer_limits(1024, 1);
    assert_eq!(deliver(&mut h, &w.x2, &dir), Verdict::Quarantined);
    let r = h.receive(&w.x, &dir).unwrap();
    assert_eq!(r.verdict, Verdict::Quarantined);
    assert_eq!(r.others.len(), 1);
    assert_eq!(r.others[0].1, Verdict::Unauthorized);
    // A dropped batch comes back later and is vouched like any other.
    assert_eq!(deliver(&mut h, &w.y, &dir), Verdict::Integrated);
    assert_eq!(deliver(&mut h, &w.x2, &dir), Verdict::Quarantined);
    assert_eq!(deliver(&mut h, &w.y2, &dir), Verdict::Integrated);
    assert!(h.quarantined().is_empty());
}

#[test]
fn transcripts_are_written_only_by_the_sessions_actor() {
    let alice = signer("@alice:a.example", "laptop");
    let bob = signer("@bob:b.example", "desk");
    let (mut ha, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &alice,
        0,
        HistoryKind::Transcript,
        Some(SESSION.into()),
        vec![Op::TextCreate {
            text: "hi".into(),
            transcript: true,
        }],
    )
    .unwrap();
    let main = ha.main().unwrap();
    let text = ObjectId::Op(g.ops[1].clone());
    // Bob is a member of the space but not the session's actor.
    let dir = Dir::new(&[&alice, &bob]);
    let mut hb = History::open(MemoryStore::default(), SPACE, ha.id()).unwrap();
    // Bob's own copy writes the edit (a local write is not checked against roles).
    hb.receive(&g.envelope, &dir).unwrap();
    let edit = hb
        .plan_edit(&At::Timeline(main.clone()), &text, 2, 0, "!")
        .unwrap()
        .unwrap();
    let steer = hb.write(&bob, 0, &main, vec![edit], None).unwrap();
    let seal = ha
        .plan_seal(&At::Timeline(main.clone()), &text, 0, 2)
        .unwrap();
    let sealed = ha.write(&alice, 0, &main, vec![seal], None).unwrap();

    let mut h = History::open(MemoryStore::default(), SPACE, ha.id()).unwrap();
    assert_eq!(deliver(&mut h, &g.envelope, &dir), Verdict::Integrated);
    assert_eq!(deliver(&mut h, &steer.envelope, &dir), Verdict::Quarantined);
    assert_eq!(deliver(&mut h, &sealed.envelope, &dir), Verdict::Integrated);
    assert_eq!(
        h.state(&At::Timeline(main)).unwrap().text(&text).as_deref(),
        Some("hi")
    );
    // The directory was asked about the session named by the genesis, never about the space.
    let asked = dir.asked.borrow();
    assert!(asked.iter().all(|a| a.contains(SESSION)), "{asked:?}");
    assert!(asked.iter().any(|a| a.starts_with("@bob:b.example")));
}
