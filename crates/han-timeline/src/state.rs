//! States: the objects at a version (TL-33 – TL-37, TL-60 – TL-62, TL-86).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::cbor::Cbor;
use crate::codec::full_id;
use crate::ids::{CharId, InsertionId, ObjectId, OpId};
use crate::op::{Change, MergeOp, Op};
use crate::text::{Anchor, Bias, TextObj};
use crate::value::Value;

/// A register: the value of its greatest write (TL-33).
#[derive(Clone, Debug)]
pub(crate) struct RegisterObj {
    pub value: Value,
    pub writer: OpId,
}

/// A map: per key, its values, greatest first (TL-35).
#[derive(Clone, Debug, Default)]
pub(crate) struct MapObj {
    pub tree: bool,
    pub keys: BTreeMap<String, Vec<(OpId, Value)>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Obj {
    Register(RegisterObj),
    Map(Arc<MapObj>),
    Text(Arc<TextObj>),
}

/// Decides which current values an observer of a map key had seen.
pub(crate) enum Seen<'a> {
    /// The state is exactly the observer's parent state: it saw every current value.
    All,
    /// The observer saw exactly the values this predicate accepts.
    Some(&'a dyn Fn(&OpId) -> bool),
}

/// The state at a version: every object present, by object id.
#[derive(Clone, Debug)]
pub struct State {
    pub(crate) objects: BTreeMap<ObjectId, Obj>,
}

impl Default for State {
    fn default() -> Self {
        let mut objects = BTreeMap::new();
        objects.insert(ObjectId::Root, Obj::Map(Arc::new(MapObj::default())));
        State { objects }
    }
}

/// The observable state of one object (TL-61).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectState {
    /// A register's value.
    Register(Value),
    /// A map: its tree flag and, per present key, its values greatest first.
    Map {
        /// The tree flag.
        tree: bool,
        /// Per present key, its values (op id, value), greatest first.
        entries: BTreeMap<String, Vec<(OpId, Value)>>,
    },
    /// A text: its transcript flag and visible text.
    Text {
        /// The transcript flag.
        transcript: bool,
        /// The visible text.
        text: String,
    },
}

/// Tree conflicts in a state (TL-86).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeConflicts {
    /// Paths with several concurrent entries.
    pub keys: Vec<String>,
    /// Content objects referenced by several paths, with those paths.
    pub duplicates: Vec<(OpId, Vec<String>)>,
    /// Paths `p` present together with a path under `p/`.
    pub clashes: Vec<(String, String)>,
}

impl State {
    /// The object ids present, ascending (`root` first).
    pub fn object_ids(&self) -> impl Iterator<Item = &ObjectId> {
        self.objects.keys()
    }

    /// Whether the object exists in this state.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.objects.contains_key(id)
    }

    /// The observable state of an object (TL-61).
    pub fn object(&self, id: &ObjectId) -> Option<ObjectState> {
        Some(match self.objects.get(id)? {
            Obj::Register(r) => ObjectState::Register(r.value.clone()),
            Obj::Map(m) => ObjectState::Map {
                tree: m.tree,
                entries: m.keys.clone(),
            },
            Obj::Text(t) => ObjectState::Text {
                transcript: t.transcript,
                text: t.text(),
            },
        })
    }

    /// The visible text of a text object.
    pub fn text(&self, id: &ObjectId) -> Option<String> {
        match self.objects.get(id)? {
            Obj::Text(t) => Some(t.text()),
            _ => None,
        }
    }

    /// The visible characters of a text in order: identity, canonical id (TL-72) and character.
    pub fn chars(&self, id: &ObjectId) -> Option<Vec<(CharId, CharId, char)>> {
        let t = self.text_obj(id)?;
        let lay = t.layout();
        Some(
            lay.visible_index
                .iter()
                .map(|&i| {
                    let l = lay.order[i];
                    (t.char_id(l), t.canonical(l), t.char_at(l))
                })
                .collect(),
        )
    }

    /// The value of a register.
    pub fn register(&self, id: &ObjectId) -> Option<&Value> {
        match self.objects.get(id)? {
            Obj::Register(r) => Some(&r.value),
            _ => None,
        }
    }

    /// The visible value of a map key: the value of its greatest write (TL-36).
    pub fn map_value(&self, id: &ObjectId, key: &str) -> Option<&Value> {
        self.map(id)?.keys.get(key)?.first().map(|(_, v)| v)
    }

    pub(crate) fn map(&self, id: &ObjectId) -> Option<&MapObj> {
        match self.objects.get(id)? {
            Obj::Map(m) => Some(m),
            _ => None,
        }
    }

    pub(crate) fn text_obj(&self, id: &ObjectId) -> Option<&TextObj> {
        match self.objects.get(id)? {
            Obj::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Map keys in conflict: per key with several values, every value greatest first (TL-36).
    pub fn map_conflicts(&self, id: &ObjectId) -> Vec<(String, Vec<(OpId, Value)>)> {
        self.map(id)
            .map(|m| {
                m.keys
                    .iter()
                    .filter(|(_, v)| v.len() > 1)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The tree conflicts of a tree map (TL-86).
    pub fn tree_conflicts(&self, id: &ObjectId) -> TreeConflicts {
        let Some(m) = self.map(id) else {
            return TreeConflicts::default();
        };
        let keys = m
            .keys
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(k, _)| k.clone())
            .collect();
        let mut by_object: BTreeMap<OpId, Vec<String>> = BTreeMap::new();
        for (path, values) in &m.keys {
            if let Some(obj) = values.first().and_then(|(_, v)| v.obj_field()) {
                by_object.entry(obj).or_default().push(path.clone());
            }
        }
        let duplicates = by_object.into_iter().filter(|(_, p)| p.len() > 1).collect();
        TreeConflicts {
            keys,
            duplicates,
            clashes: path_clashes(m.keys.keys()),
        }
    }

    /// Makes an anchor at a visible position of a text (TL-46).
    pub fn make_anchor(&self, text: &ObjectId, pos: usize, bias: Bias) -> Option<Anchor> {
        self.text_obj(text)?.make_anchor(pos, bias)
    }

    /// Resolves an anchor in a text (TL-46); `None` is `unknown_anchor`.
    pub fn resolve_anchor(&self, text: &ObjectId, anchor: &Anchor) -> Option<usize> {
        self.text_obj(text)?.resolve_anchor(anchor)
    }

    /// The state digest (TL-61).
    pub fn digest(&self) -> [u8; 32] {
        let entries: Vec<Cbor> = self
            .objects
            .iter()
            .map(|(id, obj)| {
                let id = match id {
                    ObjectId::Root => Cbor::Null,
                    ObjectId::Op(op) => full_id(op),
                };
                let (kind, content) = match obj {
                    Obj::Register(r) => (1, r.value.to_cbor()),
                    Obj::Map(m) => (
                        2,
                        Cbor::Array(vec![
                            Cbor::Bool(m.tree),
                            Cbor::Array(
                                m.keys
                                    .iter()
                                    .map(|(k, vals)| {
                                        Cbor::Array(vec![
                                            Cbor::text(k.clone()),
                                            Cbor::Array(
                                                vals.iter()
                                                    .map(|(op, v)| {
                                                        Cbor::Array(vec![full_id(op), v.to_cbor()])
                                                    })
                                                    .collect(),
                                            ),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ]),
                    ),
                    Obj::Text(t) => (
                        3,
                        Cbor::Array(vec![Cbor::Bool(t.transcript), Cbor::text(t.text())]),
                    ),
                };
                Cbor::Array(vec![id, Cbor::u(kind), content])
            })
            .collect();
        let mut h = Sha256::new();
        h.update(b"han-timeline-state-v1\n");
        h.update(Cbor::Array(entries).encode());
        h.finalize().into()
    }

    // -----------------------------------------------------------------------------------------
    // Applying ops. Validation happens before (history.rs); these only change the state.

    fn map_mut(&mut self, id: &ObjectId) -> Option<&mut MapObj> {
        match self.objects.get_mut(id)? {
            Obj::Map(m) => Some(Arc::make_mut(m)),
            _ => None,
        }
    }

    pub(crate) fn text_mut(&mut self, id: &ObjectId) -> Option<&mut TextObj> {
        match self.objects.get_mut(id)? {
            Obj::Text(t) => Some(Arc::make_mut(t)),
            _ => None,
        }
    }

    fn write_register(&mut self, id: &ObjectId, writer: &OpId, value: &Value) {
        if let Some(Obj::Register(r)) = self.objects.get_mut(id)
            && *writer > r.writer
        {
            r.writer = writer.clone();
            r.value = value.clone();
        }
    }

    fn observe_key(
        &mut self,
        id: &ObjectId,
        key: &str,
        op: &OpId,
        value: Option<&Value>,
        seen: &Seen<'_>,
    ) {
        let Some(m) = self.map_mut(id) else { return };
        let values = m.keys.entry(key.to_string()).or_default();
        match seen {
            Seen::All => values.clear(),
            Seen::Some(saw) => values.retain(|(w, _)| !saw(w)),
        }
        if let Some(v) = value {
            let at = values.partition_point(|(w, _)| w > op);
            values.insert(at, (op.clone(), v.clone()));
        }
        if values.is_empty() {
            m.keys.remove(key);
        }
    }

    /// Applies op `id`. `seen` says which current map values the op had seen.
    pub(crate) fn apply(&mut self, id: &OpId, op: &Op, seen: &Seen<'_>) {
        let obj = ObjectId::Op(id.clone());
        match op {
            Op::TimelineCreate { .. } | Op::Checkpoint { .. } => {}
            Op::RegisterCreate { value } => {
                self.objects.insert(
                    obj,
                    Obj::Register(RegisterObj {
                        value: value.clone(),
                        writer: id.clone(),
                    }),
                );
            }
            Op::RegisterSet { target, value } => self.write_register(target, id, value),
            Op::MapCreate { tree } => {
                self.objects.insert(
                    obj,
                    Obj::Map(Arc::new(MapObj {
                        tree: *tree,
                        keys: BTreeMap::new(),
                    })),
                );
            }
            Op::MapSet { target, key, value } => {
                self.observe_key(target, key, id, Some(value), seen)
            }
            Op::MapDelete { target, key } => self.observe_key(target, key, id, None, seen),
            Op::TextCreate { text, transcript } => {
                let mut t = TextObj::new(*transcript);
                if !text.is_empty() {
                    t.insert(
                        InsertionId {
                            op: id.clone(),
                            index: 0,
                        },
                        None,
                        text,
                        None,
                    );
                }
                self.objects.insert(obj, Obj::Text(Arc::new(t)));
            }
            Op::TextEdit {
                target,
                deletes,
                inserts,
            } => {
                if let Some(t) = self.text_mut(target) {
                    for r in deletes {
                        t.delete(r);
                    }
                    for (j, ins) in inserts.iter().enumerate() {
                        t.insert(
                            InsertionId {
                                op: id.clone(),
                                index: j as u32,
                            },
                            ins.anchor.as_ref(),
                            &ins.text,
                            None,
                        );
                    }
                }
            }
            Op::TextSeal { target, ranges } => {
                if let Some(t) = self.text_mut(target) {
                    t.seal(ranges.clone());
                }
            }
            Op::Merge(m) => self.apply_merge(id, m, seen),
        }
    }

    fn apply_merge(&mut self, id: &OpId, m: &MergeOp, seen: &Seen<'_>) {
        let mut j = 0u32;
        for c in &m.changes {
            match c {
                Change::Register {
                    object,
                    create,
                    value,
                } => {
                    if *create && !self.contains(object) {
                        self.objects.insert(
                            object.clone(),
                            Obj::Register(RegisterObj {
                                value: value.clone(),
                                writer: id.clone(),
                            }),
                        );
                    } else {
                        self.write_register(object, id, value);
                    }
                }
                Change::Map {
                    object,
                    create,
                    tree,
                    sets,
                    deletes,
                } => {
                    if *create && !self.contains(object) {
                        self.objects.insert(
                            object.clone(),
                            Obj::Map(Arc::new(MapObj {
                                tree: *tree,
                                keys: BTreeMap::new(),
                            })),
                        );
                    }
                    for (k, v) in sets {
                        self.observe_key(object, k, id, Some(v), seen);
                    }
                    for k in deletes {
                        self.observe_key(object, k, id, None, seen);
                    }
                }
                Change::Text {
                    object,
                    create,
                    transcript,
                    deletes,
                    inserts,
                } => {
                    if *create && !self.contains(object) {
                        self.objects.insert(
                            object.clone(),
                            Obj::Text(Arc::new(TextObj::new(*transcript))),
                        );
                    }
                    let first = j;
                    j += inserts.len() as u32;
                    if let Some(t) = self.text_mut(object) {
                        for r in deletes {
                            t.delete(r);
                        }
                        for (n, ins) in inserts.iter().enumerate() {
                            t.insert(
                                InsertionId {
                                    op: id.clone(),
                                    index: first + n as u32,
                                },
                                ins.anchor.as_ref(),
                                &ins.text,
                                ins.origin.clone(),
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Path clashes among `paths`: `(p, q)` with `q` under `p/` (TL-86), sorted.
pub(crate) fn path_clashes<'a>(paths: impl Iterator<Item = &'a String>) -> Vec<(String, String)> {
    let all: BTreeSet<&String> = paths.collect();
    let mut out = Vec::new();
    for p in &all {
        let prefix = format!("{p}/");
        for q in all.range::<String, _>(prefix.clone()..) {
            if !q.starts_with(&prefix) {
                break;
            }
            out.push(((*p).clone(), (*q).clone()));
        }
    }
    out
}
