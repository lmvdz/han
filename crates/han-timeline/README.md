# han-timeline

The work-timeline engine of HAN: profile [`han.timeline/1`](../../spec/profiles/han.timeline-1.md).

A **history** holds signed batches of operations about one body of work: a repository's
worktree, a session's transcript, or other data. Inside it, **timelines** are named lines of
work. A timeline starts as a fork of another, and work moves between timelines only by
**merges**, where overlapping changes become conflict hunks to resolve. The **state** at any
version (any set of ops) is the same on every replica that holds those ops, whatever order they
arrived in:
- registers (last writer wins);
- maps that keep concurrent values;
- texts: a sequence CRDT with anchors, seals and merge copies.

The engine does no I/O and no async. Three traits connect it to its surroundings:

| Trait | Provides |
|---|---|
| [`Store`] | durable storage of the integrated envelopes and the per-key counter high-water marks ([`MemoryStore`] for tests) |
| [`Directory`] | the space's view of device keys (in profiles, valid or revoked) and of who may write |
| [`Signer`] | Ed25519 signatures by one device key ([`KeySigner`] holds a key in memory) |

## Using it

```rust
use han_timeline::{
    At, Directory, History, HistoryKind, KeyInfo, KeySigner, MemoryStore, ObjectId, Op,
    Resolution, Scope, Signer,
};

// The space's directory: here, one key, valid, whose actor may write.
struct Space(KeySigner);
impl Directory for Space {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        (actor == self.0.actor() && key == self.0.key_id())
            .then(|| KeyInfo { public_key: self.0.public_key(), valid: true })
    }
    fn may_write(&self, _actor: &str, _scope: Scope<'_>) -> bool {
        true
    }
}

let me = KeySigner::from_seed("@alice:a.example", "laptop", [1; 32]);

// Create a history: its genesis batch creates `main` and, here, a text.
let (mut history, genesis) = History::create(
    MemoryStore::default(),
    "s1:a.example",
    &me,
    0, // lane
    HistoryKind::Data,
    None,
    vec![Op::TextCreate { text: "a\nb\nc\n".into(), transcript: false }],
)?;
let main = history.main().unwrap();
let text = ObjectId::Op(genesis.ops[1].clone());
let first = history.head(&main).unwrap(); // a version to come back to

// Fork a session timeline and edit it. Local edits are planned from visible positions.
let session = history.fork(&me, 0, &main, None, "session")?.ops[0].clone();
let edit = history.plan_edit(&At::Timeline(session.clone()), &text, 2, 1, "B")?.unwrap();
history.write(&me, 0, &session, vec![edit], Some("tool-call-1".into()))?;

// Meanwhile main changes the first line.
let edit = history.plan_edit(&At::Timeline(main.clone()), &text, 0, 1, "A")?.unwrap();
history.write(&me, 0, &main, vec![edit], None)?;

// Plan the merge of the session into main: different lines, so no conflicts.
let plan = history.plan_merge(&main, &session)?;
assert!(plan.conflicts.is_empty());
let resolutions: Vec<Resolution> = Vec::new(); // one per conflict, in plan order
let draft = plan.resolve(&resolutions)?;
let digest = draft.digest(&history.id()); // what an accept effect approves (TL-74)
history.merge(&me, 0, &draft, None)?;
let state = history.state(&At::Timeline(main.clone()))?;
assert_eq!(state.text(&text).as_deref(), Some("A\nB\nc\n"));

// Time travel: any version materialises as it was.
let before = history.state(&At::Version(first))?;
assert_eq!(before.text(&text).as_deref(), Some("a
b
c
"));

// Sync another replica with two messages: our hello, their answer (TL-110).
let dir = Space(me);
let mut peer = History::open(MemoryStore::default(), "s1:a.example", history.id())?;
let answer = history.answer(&peer.hello(None).encode()).unwrap();
let (reports, more) = peer.receive_answer(&answer, None, &dir)?.unwrap();
assert!(!more && reports.iter().all(|r| r.verdict == han_timeline::Verdict::Integrated));
assert_eq!(
    peer.state(&At::Timeline(main.clone()))?.digest(),
    history.state(&At::Timeline(main))?.digest()
);
let _ = digest;
# Ok::<(), han_timeline::Error>(())
```

## The API at a glance

| Call | Does |
|---|---|
| `History::create` / `History::open` | start a history (genesis batch) or open one from a store |
| `History::receive` | integrate an envelope from anywhere: checked, held back, quarantined, or rejected, with a [`Report`] |
| `History::write`, `fork`, `merge` | write local batches; each is validated like a peer's before it is kept |
| `History::plan_edit`, `plan_seal` | turn visible positions into identity-based ops |
| `History::plan_merge` → `MergePlan::resolve` → `MergeDraft` | plan a three-way merge, resolve its conflicts, preview or record it |
| `History::state`, `head`, `version`, `timelines`, `checkpoints` | read any version |
| `History::hello`, `answer`, `receive_answer` | two-message anti-entropy sync |
| `State` | observable objects, anchors, map and tree conflicts, the state digest |

## Tests and benchmarks

- `cargo test -p han-timeline` runs every vector in `conformance/timeline-vectors/`, plus:
  - randomized convergence and sync tests;
  - authorisation tests;
  - time-travel tests;
  - durable-store tests.
- `cargo run --release -p han-timeline --example bench` times the hot paths.

Built by Team B of the clean room (PLAN.md §8) from the spec and vectors alone; open questions
to the spec live in [`SPEC-QUESTIONS.md`](SPEC-QUESTIONS.md).
