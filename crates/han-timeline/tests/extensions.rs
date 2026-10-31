//! Touch sets, overlap, turn marks and notes (§21 – 23) on histories too large or too specific
//! for the hand-written vectors: reader limits, the head-range shift, regions outside trees,
//! register content, rename counting, transcripts without or with odd marks, and the writers.

mod common;

use std::collections::BTreeMap;

use common::{SPACE, signer};
use han_timeline::{
    Anchor, At, Bias, CharId, Error, History, HistoryKind, InsertionId, KeySigner,
    MAX_NOTES_PER_OBJECT, MAX_TOUCH_ENTRIES, MAX_TOUCH_RANGES, MemoryStore, NoteDraft, NoteKind,
    NotePlace, ObjectId, Op, OpId, OverlapEntry, OverlapSide, Reason, TouchChange, TurnOf, Value,
};

fn me() -> KeySigner {
    signer("@alice:a.example", "laptop")
}

fn entry(obj: &OpId) -> Value {
    Value::Map(BTreeMap::from([
        (
            "obj".to_string(),
            Value::object_ref(obj).as_map().unwrap()["obj"].clone(),
        ),
        ("mode".to_string(), Value::text("100644")),
        ("eol".to_string(), Value::text("lf")),
    ]))
}

/// A worktree history whose main has these text files; the tree and each file's object.
struct Repo {
    h: History,
    me: KeySigner,
    main: OpId,
    tree: ObjectId,
    files: BTreeMap<String, OpId>,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let me = signer("@alice:a.example", "laptop");
        let mut ops = vec![Op::MapCreate { tree: true }];
        ops.extend(files.iter().map(|(_, text)| Op::TextCreate {
            text: text.to_string(),
            transcript: false,
        }));
        let (mut h, g) = History::create(
            MemoryStore::default(),
            SPACE,
            &me,
            0,
            HistoryKind::Worktree,
            None,
            ops,
        )
        .unwrap();
        let main = h.main().unwrap();
        let tree = ObjectId::Op(g.ops[1].clone());
        let files: BTreeMap<String, OpId> = files
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (p.to_string(), g.ops[i + 2].clone()))
            .collect();
        let mut set = vec![Op::MapSet {
            target: ObjectId::Root,
            key: "tree".into(),
            value: Value::object_ref(&g.ops[1]),
        }];
        set.extend(files.iter().map(|(p, o)| Op::MapSet {
            target: tree.clone(),
            key: p.clone(),
            value: entry(o),
        }));
        h.write(&me, 0, &main, set, None).unwrap();
        Repo {
            h,
            me,
            main,
            tree,
            files,
        }
    }

    fn fork(&mut self, name: &str) -> OpId {
        self.h
            .fork(&self.me, 0, &self.main, None, name)
            .unwrap()
            .ops[0]
            .clone()
    }

    fn write(&mut self, t: &OpId, ops: Vec<Op>) -> Vec<OpId> {
        self.h.write(&self.me, 0, t, ops, None).unwrap().ops
    }

    fn edit(&mut self, t: &OpId, path: &str, pos: usize, delete: usize, text: &str) {
        let obj = ObjectId::Op(self.files[path].clone());
        let at = At::Timeline(t.clone());
        let op = self
            .h
            .plan_edit(&at, &obj, pos, delete, text)
            .unwrap()
            .unwrap();
        self.write(t, vec![op]);
    }

    fn set_path(&self, path: &str, obj: &OpId) -> Op {
        Op::MapSet {
            target: self.tree.clone(),
            key: path.into(),
            value: entry(obj),
        }
    }

    fn delete_path(&self, path: &str) -> Op {
        Op::MapDelete {
            target: self.tree.clone(),
            key: path.into(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Touch sets (TL-130, TL-131, TL-137)

#[test]
fn a_touch_set_is_cut_at_the_reader_limits() {
    let big = "x\n".repeat(2 * MAX_TOUCH_RANGES + 10);
    let mut r = Repo::new(&[("big.txt", &big)]);
    // s1 creates one file more than a touch set holds, in one batch.
    let s1 = r.fork("s1");
    let n = MAX_TOUCH_ENTRIES + 1;
    let created = r.write(
        &s1,
        (0..n)
            .map(|_| Op::TextCreate {
                text: "f\n".into(),
                transcript: false,
            })
            .collect(),
    );
    let sets: Vec<Op> = created
        .iter()
        .enumerate()
        .map(|(i, o)| r.set_path(&format!("gen/{i:05}.txt"), o))
        .collect();
    r.write(&s1, sets);
    let set = r.h.touch_set(&s1, None).unwrap().unwrap();
    assert!(set.truncated);
    assert_eq!(set.entries.len(), MAX_TOUCH_ENTRIES);
    // The first entries in object id order: all but the newest object.
    assert_eq!(
        set.entries.iter().map(|e| &e.object).collect::<Vec<_>>(),
        created[..MAX_TOUCH_ENTRIES].iter().collect::<Vec<_>>()
    );
    assert!(set.entries.iter().all(|e| e.change == TouchChange::CREATED));

    // s2 edits every other line of one file: more clusters than a list holds.
    let s2 = r.fork("s2");
    let big_id = r.files["big.txt"].clone();
    let ins = InsertionId {
        op: big_id.clone(),
        index: 0,
    };
    let lines = MAX_TOUCH_RANGES + 5;
    let char_at = |k: u32| CharId {
        insertion: ins.clone(),
        offset: k,
    };
    let edit = Op::TextEdit {
        target: ObjectId::Op(big_id.clone()),
        deletes: (0..lines as u32)
            .map(|l| han_timeline::IdRange {
                insertion: ins.clone(),
                from: 4 * l,
                to: 4 * l + 1,
            })
            .collect(),
        inserts: (0..lines as u32)
            .map(|l| han_timeline::Insert {
                anchor: (l > 0).then(|| char_at(4 * l - 1)),
                text: "y".into(),
            })
            .collect(),
    };
    r.write(&s2, vec![edit]);
    let set = r.h.touch_set(&s2, None).unwrap().unwrap();
    assert!(set.truncated);
    let e = &set.entries[0];
    assert_eq!(e.change, TouchChange::EDITED);
    assert_eq!(e.base_lines.len(), MAX_TOUCH_RANGES);
    assert_eq!(e.head_lines.len(), MAX_TOUCH_RANGES);
    assert_eq!(e.base_lines[..3], [(0, 1), (2, 3), (4, 5)]);
    assert_eq!(e.head_lines[..3], [(0, 1), (2, 3), (4, 5)]);

    // An overlap built from a truncated touch set is truncated: absence means unknown.
    let ov = r.h.overlap(&s1, &s2).unwrap();
    assert!(ov.truncated);
    assert!(ov.entries.is_empty());
    assert!(!ov.is_disjoint());
}

#[test]
fn head_ranges_shift_with_line_breaks_removed_before_a_later_cluster() {
    // Joining line 0 with line 1 moves the later cluster up one head line.
    let mut r = Repo::new(&[("a.txt", "a\nb\nc\nd\ne\n")]);
    let s1 = r.fork("s1");
    r.edit(&s1, "a.txt", 6, 1, "D");
    r.edit(&s1, "a.txt", 1, 1, "");
    let set = r.h.touch_set(&s1, None).unwrap().unwrap();
    let e = &set.entries[0];
    assert_eq!(e.change, TouchChange::EDITED);
    assert_eq!(e.base_lines, [(0, 1), (3, 4)]);
    assert_eq!(e.head_lines, [(0, 1), (2, 3)]);

    // A deleted line leaves an empty head range at its gap; the next cluster follows it.
    let mut r = Repo::new(&[("a.txt", "a\nb\nc\nd\n")]);
    let s1 = r.fork("s1");
    r.edit(&s1, "a.txt", 6, 1, "D");
    r.edit(&s1, "a.txt", 2, 2, "");
    let e = &r.h.touch_set(&s1, None).unwrap().unwrap().entries[0];
    assert_eq!(e.base_lines, [(1, 2), (3, 4)]);
    assert_eq!(e.head_lines, [(1, 1), (2, 3)]);

    // An insertion between lines is an empty base range.
    let mut r = Repo::new(&[("a.txt", "a\nb\n")]);
    let s1 = r.fork("s1");
    r.edit(&s1, "a.txt", 2, 0, "new\n");
    let e = &r.h.touch_set(&s1, None).unwrap().unwrap().entries[0];
    assert_eq!(e.base_lines, [(1, 1)]);
    assert_eq!(e.head_lines, [(1, 2)]);
}

#[test]
fn main_needs_a_base_and_other_histories_have_no_touch_set() {
    let mut r = Repo::new(&[("a.txt", "a\n")]);
    assert_eq!(r.h.touch_set(&r.main, None), Err(Error::NeedsBase));
    let genesis = r.h.version(&At::Timeline(r.main.clone())).unwrap();
    r.edit(&r.main.clone(), "a.txt", 0, 1, "b");
    let set = r.h.touch_set(&r.main, Some(&genesis)).unwrap().unwrap();
    assert_eq!(set.entries.len(), 1);
    assert_eq!(set.base, genesis);

    let me = signer("@alice:a.example", "laptop");
    let (h, _) = History::create(
        MemoryStore::default(),
        SPACE,
        &me,
        0,
        HistoryKind::Data,
        None,
        vec![Op::MapCreate { tree: false }],
    )
    .unwrap();
    assert_eq!(h.touch_set(&h.main().unwrap(), None), Ok(None));
}

// ---------------------------------------------------------------------------------------------
// Overlap (TL-133, TL-134)

#[test]
fn regions_arise_in_data_histories_and_for_objects_no_path_refers_to() {
    // A data history: a region, no touch set.
    let me = signer("@alice:a.example", "laptop");
    let (mut h, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &me,
        0,
        HistoryKind::Data,
        None,
        vec![Op::TextCreate {
            text: "a\nb\nc\n".into(),
            transcript: false,
        }],
    )
    .unwrap();
    let main = h.main().unwrap();
    let text = ObjectId::Op(g.ops[1].clone());
    let s1 = h.fork(&me, 0, &main, None, "s1").unwrap().ops[0].clone();
    let s2 = h.fork(&me, 0, &main, None, "s2").unwrap().ops[0].clone();
    for (t, s) in [(&s1, "X"), (&s2, "Y")] {
        let op = h
            .plan_edit(&At::Timeline(t.clone()), &text, 2, 1, s)
            .unwrap()
            .unwrap();
        h.write(&me, 0, t, vec![op], None).unwrap();
    }
    let ov = h.overlap(&s1, &s2).unwrap();
    assert_eq!(
        ov.entries,
        [OverlapEntry::Region {
            object: text.clone(),
            lines: (1, 2),
            converged: false
        }]
    );
    assert_eq!(h.touch_set(&s1, None), Ok(None));

    // A worktree text that no path refers to: a region, and no touch-set or file entry.
    let mut r = Repo::new(&[("a.txt", "a\n")]);
    let loose = r.write(
        &r.main.clone(),
        vec![Op::TextCreate {
            text: "p\nq\n".into(),
            transcript: false,
        }],
    )[0]
    .clone();
    let s1 = r.fork("s1");
    let s2 = r.fork("s2");
    let obj = ObjectId::Op(loose);
    for (t, s) in [(&s1, "P"), (&s2, "P")] {
        let op =
            r.h.plan_edit(&At::Timeline(t.clone()), &obj, 0, 1, s)
                .unwrap()
                .unwrap();
        r.write(t, vec![op]);
    }
    let ov = r.h.overlap(&s1, &s2).unwrap();
    assert_eq!(
        ov.entries,
        [OverlapEntry::Region {
            object: obj,
            lines: (0, 1),
            converged: true
        }]
    );
    assert!(
        r.h.touch_set(&s1, None)
            .unwrap()
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn modify_delete_of_a_binary_file() {
    let mut r = Repo::new(&[]);
    let main = r.main.clone();
    let blob = |n: i64| {
        Value::Map(BTreeMap::from([
            ("hash".to_string(), Value::text(format!("{n:064x}"))),
            ("size".to_string(), Value::Int(n)),
            ("type".to_string(), Value::text("application/octet-stream")),
        ]))
    };
    let reg = r.write(&main, vec![Op::RegisterCreate { value: blob(1) }])[0].clone();
    let mut bin = entry(&reg);
    if let Value::Map(m) = &mut bin {
        m.remove("eol");
    }
    r.write(
        &main,
        vec![Op::MapSet {
            target: r.tree.clone(),
            key: "img.bin".into(),
            value: bin,
        }],
    );
    let s1 = r.fork("s1");
    let s2 = r.fork("s2");
    r.write(&s1, vec![r.delete_path("img.bin")]);
    r.write(
        &s2,
        vec![Op::RegisterSet {
            target: ObjectId::Op(reg.clone()),
            value: blob(2),
        }],
    );
    let md = |by| OverlapEntry::ModifyDelete {
        object: ObjectId::Op(reg.clone()),
        path: "img.bin".into(),
        deleted_by: by,
    };
    assert_eq!(r.h.overlap(&s1, &s2).unwrap().entries, [md(OverlapSide::A)]);
    assert_eq!(r.h.overlap(&s2, &s1).unwrap().entries, [md(OverlapSide::B)]);
    let t2 = r.h.touch_set(&s2, None).unwrap().unwrap();
    assert_eq!(t2.entries[0].change, TouchChange::BINARY_CHANGED);
    assert!(t2.entries[0].base_lines.is_empty());
    let plan = r.h.plan_merge(&s1, &s2).unwrap();
    assert!(matches!(
        plan.conflicts[..],
        [han_timeline::Conflict::ModifyDelete { .. }]
    ));
}

#[test]
fn rename_entries_count_paths_as_tl_133_says() {
    let paths = |r: &Repo, t: &OpId| -> Vec<String> {
        match r.h.state(&At::Timeline(t.clone())).unwrap().object(&r.tree) {
            Some(han_timeline::ObjectState::Map { entries, .. }) => entries.into_keys().collect(),
            _ => Vec::new(),
        }
    };
    // Both copy a.txt to new paths: {a, b} ∪ {c} has more paths than either side.
    let mut r = Repo::new(&[("a.txt", "x\n")]);
    let a = r.files["a.txt"].clone();
    let (s1, s2) = (r.fork("s1"), r.fork("s2"));
    let op = r.set_path("b.txt", &a);
    r.write(&s1, vec![op]);
    let op = r.set_path("c.txt", &a);
    r.write(&s2, vec![op]);
    assert_eq!(
        r.h.overlap(&s1, &s2).unwrap().entries,
        [OverlapEntry::Rename {
            object: ObjectId::Op(a.clone()),
            a: vec!["a.txt".into(), "b.txt".into()],
            b: vec!["a.txt".into(), "c.txt".into()],
        }]
    );
    assert_eq!(paths(&r, &s1), ["a.txt", "b.txt"]);

    // One renames, the other copies: {b} ∪ {c} is not more than {a, c}. Both touch the object.
    let mut r = Repo::new(&[("a.txt", "x\n")]);
    let a = r.files["a.txt"].clone();
    let (s1, s2) = (r.fork("s1"), r.fork("s2"));
    let op = r.set_path("b.txt", &a);
    r.write(&s1, vec![r.delete_path("a.txt"), op]);
    let op = r.set_path("c.txt", &a);
    r.write(&s2, vec![op]);
    assert_eq!(
        r.h.overlap(&s1, &s2).unwrap().entries,
        [OverlapEntry::File {
            object: ObjectId::Op(a.clone())
        }]
    );

    // One renames, the other renames and copies: {b, c, d} is more than {b} and {c, d}.
    let mut r = Repo::new(&[("a.txt", "x\n")]);
    let a = r.files["a.txt"].clone();
    let (s1, s2) = (r.fork("s1"), r.fork("s2"));
    let op = r.set_path("b.txt", &a);
    r.write(&s1, vec![r.delete_path("a.txt"), op]);
    let (c, d) = (r.set_path("c.txt", &a), r.set_path("d.txt", &a));
    r.write(&s2, vec![r.delete_path("a.txt"), c, d]);
    assert_eq!(
        r.h.overlap(&s1, &s2).unwrap().entries,
        [OverlapEntry::Rename {
            object: ObjectId::Op(a),
            a: vec!["b.txt".into()],
            b: vec!["c.txt".into(), "d.txt".into()],
        }]
    );
}

// ---------------------------------------------------------------------------------------------
// Turn marks (TL-141 – TL-146)

#[test]
fn worktree_marks_rewind_and_fork_without_fallback() {
    let mut r = Repo::new(&[("a.txt", "a\n")]);
    let s1 = r.fork("s1");
    let me = me();
    r.edit(&s1, "a.txt", 0, 1, "b");
    let m1 =
        r.h.mark_turn(&me, 0, &s1, 1, Some("turn-1".into()))
            .unwrap();
    r.edit(&s1, "a.txt", 0, 1, "c");
    r.h.mark_turn(&me, 0, &s1, 3, None).unwrap(); // turn 2 failed: a gap
    assert_eq!(r.h.mark_turn(&me, 0, &s1, 0, None), Err(Error::OutOfRange));
    let marks = r.h.turn_marks(&s1);
    assert_eq!(marks.keys().copied().collect::<Vec<_>>(), [1, 3]);
    assert_eq!(marks[&1].id, m1.ops[0]);

    // The view is a read: no op, no head moved.
    let head = r.h.head(&s1).unwrap();
    let inventory = r.h.inventory();
    let view = r.h.rewind(&s1, 1).unwrap();
    let a = ObjectId::Op(r.files["a.txt"].clone());
    assert_eq!(view.text(&a).as_deref(), Some("b\n"));
    assert_eq!(
        (r.h.head(&s1).unwrap(), r.h.inventory()),
        (head.clone(), inventory)
    );
    assert_eq!(r.h.rewind(&s1, 2).err(), Some(Error::UnknownMark));

    // The fork is named for the turn and keeps the original as it was.
    let w = r.h.rewind_fork(&me, 0, &s1, 1).unwrap();
    let fork = w.ops[0].clone();
    let info = r.h.timelines().into_iter().find(|t| t.id == fork).unwrap();
    assert_eq!(info.name, "rewind:1:s1");
    assert_eq!(info.source, Some(s1.clone()));
    assert_eq!(r.h.head(&s1).unwrap(), head);
    let st = r.h.state(&At::Timeline(fork.clone())).unwrap();
    assert_eq!(st.text(&a).as_deref(), Some("b\n"));
    // A fork has none of its source's marks.
    assert!(r.h.turn_marks(&fork).is_empty());
    assert_eq!(r.h.rewind(&fork, 1).err(), Some(Error::UnknownMark));

    // A long name is cut at a scalar boundary to fit 256 bytes.
    // "x" makes every `é` start at an odd offset after "rewind:12:x", so 256 is no boundary.
    let long = format!("x{}", "é".repeat(125));
    let t = r.h.fork(&me, 0, &r.main, None, &long).unwrap().ops[0].clone();
    r.h.mark_turn(&me, 0, &t, 12, None).unwrap();
    let f = r.h.rewind_fork(&me, 0, &t, 12).unwrap().ops[0].clone();
    let name =
        r.h.timelines()
            .into_iter()
            .find(|x| x.id == f)
            .unwrap()
            .name;
    assert!(name.starts_with("rewind:12:xé"));
    assert_eq!(name.len(), 255);
}

/// A transcript history with one text and these marks: `(turn, end offset)`.
fn transcript(text: &str, marks: &[(&str, Value)]) -> (History, ObjectId) {
    let me = signer("@alice:a.example", "laptop");
    let (mut h, g) = History::create(
        MemoryStore::default(),
        SPACE,
        &me,
        0,
        HistoryKind::Transcript,
        Some("5e".repeat(32)),
        vec![Op::TextCreate {
            text: text.into(),
            transcript: true,
        }],
    )
    .unwrap();
    let main = h.main().unwrap();
    if !marks.is_empty() {
        let ops = marks
            .iter()
            .map(|(k, v)| Op::MapSet {
                target: ObjectId::Root,
                key: k.to_string(),
                value: v.clone(),
            })
            .collect();
        h.write(&me, 0, &main, ops, None).unwrap();
    }
    (h, ObjectId::Op(g.ops[1].clone()))
}

fn mark(text: &ObjectId, offset: u32) -> Value {
    let ObjectId::Op(op) = text else {
        unreachable!()
    };
    han_timeline::TurnMark {
        turn: 1,
        history: han_timeline::BatchId([0xab; 32]),
        timeline: OpId::new(8, op.replica.clone()),
        version: han_timeline::Version::new(vec![OpId::new(9, op.replica.clone())]),
        end: CharId {
            insertion: InsertionId {
                op: op.clone(),
                index: 0,
            },
            offset,
        },
    }
    .value()
}

#[test]
fn transcript_positions_without_marks_with_gaps_and_with_unresolved_marks() {
    let turn = |turn, in_progress| Some(TurnOf { turn, in_progress });
    // No marks: everything is the turn in progress, turn 1.
    let (h, text) = transcript("q1\na1\n", &[]);
    let st = h.state(&At::Timeline(h.main().unwrap())).unwrap();
    assert!(st.transcript_marks().marks.is_empty());
    assert_eq!(st.turn_of(&text, 0), turn(1, true));
    assert_eq!(st.turn_of(&text, 6), None);

    // Turns 1 and 3 marked (turn 2 failed), and turn 4 whose end this replica does not hold.
    let ObjectId::Op(op) = &text else {
        unreachable!()
    };
    let mut unheld = mark(&text, 0);
    if let Value::Map(m) = &mut unheld {
        let elsewhere = CharId {
            insertion: InsertionId {
                op: OpId::new(999, op.replica.clone()),
                index: 0,
            },
            offset: 0,
        };
        m.insert("end".into(), Value::text(elsewhere.to_string()));
    }
    let (h, text) = transcript(
        "q1\na1\nq3\na3\nq4\n",
        &[
            ("turn:1", mark(&text, 5)),
            ("turn:3", mark(&text, 11)),
            ("turn:4", unheld),
            ("turn:05", mark(&text, 14)),
        ],
    );
    let st = h.state(&At::Timeline(h.main().unwrap())).unwrap();
    let marks = st.transcript_marks();
    assert_eq!(marks.marks.keys().copied().collect::<Vec<_>>(), [1, 3, 4]);
    assert_eq!(marks.unresolved, [4]);
    assert_eq!(marks.ignored, ["turn:05"]);
    assert_eq!(st.turn_of(&text, 5), turn(1, false));
    assert_eq!(st.turn_of(&text, 6), turn(3, false));
    assert_eq!(st.turn_of(&text, 11), turn(3, false));
    // The unresolved mark 4 is skipped: after mark 3 comes the turn in progress.
    assert_eq!(st.turn_of(&text, 12), turn(4, true));
    // Only the history a mark names resolves it; a transcript names no worktree it holds.
    assert!(!h.resolves_mark(&marks.marks[&1]));
}

// ---------------------------------------------------------------------------------------------
// Notes (TL-150 – TL-156)

fn note(object: &OpId, from: Anchor, to: Anchor) -> NoteDraft {
    NoteDraft {
        object: object.clone(),
        from,
        to,
        kind: NoteKind::Note,
        body: "why?".into(),
        reply_to: None,
    }
}

#[test]
fn a_note_with_an_unknown_anchor_is_unresolved() {
    let mut r = Repo::new(&[("a.txt", "a\nb\n")]);
    let a = r.files["a.txt"].clone();
    let main = r.main.clone();
    let nowhere = CharId {
        insertion: InsertionId {
            op: OpId::new(999, a.replica.clone()),
            index: 0,
        },
        offset: 0,
    };
    let draft = note(&a, Anchor::Char(nowhere, Bias::Right), Anchor::End);
    // The writer refuses it; a peer's note is shown unresolved.
    let me = me();
    assert_eq!(
        r.h.write_note(&me, 0, &main, None, &draft, None),
        Err(Error::UnknownAnchor)
    );
    r.write(
        &main,
        vec![Op::MapSet {
            target: ObjectId::Root,
            key: "note:x".into(),
            value: draft.value(None),
        }],
    );
    let notes = r.h.state(&At::Timeline(main)).unwrap().notes();
    assert_eq!(notes.notes[0].place, NotePlace::Unresolved);
}

#[test]
fn the_note_writer_names_and_checks_notes() {
    let mut r = Repo::new(&[("a.txt", "a\nb\n")]);
    let a = r.files["a.txt"].clone();
    let main = r.main.clone();
    let me = me();
    let ch = |k| CharId {
        insertion: InsertionId {
            op: a.clone(),
            index: 0,
        },
        offset: k,
    };
    let draft = note(
        &a,
        Anchor::Char(ch(2), Bias::Right),
        Anchor::Char(ch(3), Bias::Left),
    );
    let w =
        r.h.write_note(&me, 0, &main, None, &draft, Some("step-7".into()))
            .unwrap();
    let notes = r.h.state(&At::Timeline(main.clone())).unwrap().notes();
    let n = &notes.notes[0];
    // The default key is the text form of the writing op's id.
    assert_eq!(n.key, w.ops[0].to_string());
    assert_eq!(n.place, NotePlace::Placed { from: 2, to: 4 });
    assert_eq!(n.step.as_deref(), Some("step-7"));
    assert!(!n.resolved && !n.parent_missing);

    let bad = |f: &dyn Fn(&mut NoteDraft)| {
        let mut d = draft.clone();
        f(&mut d);
        d
    };
    let write = |r: &mut Repo, d: &NoteDraft| r.h.write_note(&me, 0, &main, Some("k"), d, None);
    assert_eq!(
        write(&mut r, &bad(&|d| d.body = String::new())),
        Err(Error::Rejected(Reason::Malformed))
    );
    assert_eq!(
        write(&mut r, &bad(&|d| d.body = "a\u{7}b".into())),
        Err(Error::Rejected(Reason::Malformed))
    );
    assert_eq!(
        write(&mut r, &bad(&|d| d.body = "x".repeat(16_385))),
        Err(Error::Rejected(Reason::TooLarge))
    );
    assert_eq!(
        write(&mut r, &bad(&|d| d.from = Anchor::End)),
        Err(Error::Rejected(Reason::Malformed))
    );
    assert_eq!(
        write(&mut r, &bad(&|d| std::mem::swap(&mut d.from, &mut d.to))),
        Err(Error::OutOfRange)
    );
    assert_eq!(
        write(
            &mut r,
            &bad(&|d| d.object = OpId::new(999, a.replica.clone()))
        ),
        Err(Error::UnknownObject)
    );
    assert_eq!(
        r.h.write_note(&me, 0, &main, Some(&"k".repeat(129)), &draft, None),
        Err(Error::Rejected(Reason::TooLarge))
    );
}

#[test]
fn the_note_writer_refuses_too_many_notes_on_one_object() {
    let mut r = Repo::new(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let (a, b) = (r.files["a.txt"].clone(), r.files["b.txt"].clone());
    let main = r.main.clone();
    let me = me();
    let draft = note(&a, Anchor::Start, Anchor::End);
    // Peers wrote the limit's worth of notes on a.txt.
    let ops: Vec<Op> = (0..MAX_NOTES_PER_OBJECT)
        .map(|i| Op::MapSet {
            target: ObjectId::Root,
            key: format!("note:n{i}"),
            value: draft.value(None),
        })
        .collect();
    r.write(&main, ops);
    assert_eq!(
        r.h.write_note(&me, 0, &main, None, &draft, None),
        Err(Error::TooMany)
    );
    // Rewriting an existing note adds none; a note on another object is within its limit.
    r.h.write_note(&me, 0, &main, Some("n0"), &draft, None)
        .unwrap();
    r.h.write_note(
        &me,
        0,
        &main,
        None,
        &note(&b, Anchor::Start, Anchor::End),
        None,
    )
    .unwrap();
    assert_eq!(
        r.h.state(&At::Timeline(main)).unwrap().notes().notes.len(),
        MAX_NOTES_PER_OBJECT + 1
    );
}
