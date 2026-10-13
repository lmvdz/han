//! Merge planning and recording (§11, TL-87): the base over merge edges, three-way plans of
//! registers, maps, trees and texts, conflict hunks, resolutions, and the merge digest.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::cbor::Cbor;
use crate::codec::{full_id, merge_digest_parts};
use crate::envelope::Signer;
use crate::error::Error;
use crate::history::{History, Written};
use crate::ids::{CharId, HistoryId, IdRange, InsertionId, ObjectId, OpId, ReplicaId, Version};
use crate::op::{Change, ConflictKind, ConflictRecord, CopyInsert, MergeOp, Op, ResolutionKind};
use crate::state::{Obj, Seen, State, path_clashes};
use crate::store::Store;
use crate::text::{Local, TextObj};
use crate::value::Value;

/// A text conflict hunk (TL-72 step 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextHunk {
    /// The text object.
    pub object: ObjectId,
    /// The path, when the text is a tree's file.
    pub path: Option<String>,
    /// The base line range `[a, b)`, 0-based.
    pub lines: (u64, u64),
    /// The base's region text.
    pub base: String,
    /// The target's region text.
    pub target: String,
    /// The incoming region text.
    pub incoming: String,
}

/// A conflict of a merge plan (TL-71, TL-72, TL-87).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// A register both sides set differently.
    Register {
        /// The register.
        object: ObjectId,
        /// Its base value, if the base has it.
        base: Option<Value>,
        /// The target's value.
        target: Value,
        /// The incoming value.
        incoming: Value,
    },
    /// A map key both sides changed differently.
    Map {
        /// The map.
        object: ObjectId,
        /// The key.
        key: String,
        /// The base's visible value, `None` for absent.
        base: Option<Value>,
        /// The target's visible value.
        target: Option<Value>,
        /// The incoming visible value.
        incoming: Option<Value>,
    },
    /// A text hunk.
    Text(TextHunk),
    /// One side removed a path whose content the other side changed.
    ModifyDelete {
        /// The tree map.
        object: ObjectId,
        /// The path.
        key: String,
        /// Which side removed it.
        deleted_by: Side,
    },
    /// A content object would be referenced by more paths than on either side.
    Rename {
        /// The tree map.
        object: ObjectId,
        /// The content object.
        content: OpId,
        /// The target's paths of it.
        target: Vec<String>,
        /// The incoming paths of it.
        incoming: Vec<String>,
    },
    /// A file and a directory of one name that neither side had.
    Path {
        /// The tree map.
        object: ObjectId,
        /// The clashing paths: `p` and a path under `p/`.
        paths: (String, String),
    },
}

impl Conflict {
    fn record(&self, resolution: ResolutionKind) -> ConflictRecord {
        let (kind, object, key, lines) = match self {
            Conflict::Register { object, .. } => (ConflictKind::Register, object, None, None),
            Conflict::Map { object, key, .. } => {
                (ConflictKind::Map, object, Some(key.clone()), None)
            }
            Conflict::Text(h) => (ConflictKind::Text, &h.object, None, Some(h.lines)),
            Conflict::ModifyDelete { object, key, .. } => {
                (ConflictKind::ModifyDelete, object, Some(key.clone()), None)
            }
            Conflict::Rename { object, .. } => (ConflictKind::Rename, object, None, None),
            Conflict::Path { object, paths } => {
                (ConflictKind::Path, object, Some(paths.0.clone()), None)
            }
        };
        ConflictRecord {
            kind,
            object: object.clone(),
            key,
            lines,
            resolution,
        }
    }
}

/// A side of a merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The timeline merged into.
    Target,
    /// The timeline merged from.
    Incoming,
}

/// How a conflict is resolved (TL-75).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the target's side.
    Target,
    /// Take the incoming side.
    Incoming,
    /// Set a register or map key to this value.
    Value(Value),
    /// Replace a text hunk's region with this text (empty deletes the region).
    Text(String),
}

// ---------------------------------------------------------------------------------------------
// Internal plan shapes

/// An anchor inside a draft: `start`, an existing character, or a character of an earlier
/// insert of the same region edit (local index), which becomes the merge op's own insert.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum DraftAnchor {
    Start,
    Char(CharId),
    Prev(usize, u32),
}

#[derive(Clone, Debug)]
struct DraftInsert {
    anchor: DraftAnchor,
    text: String,
    origin: Option<CharId>,
}

/// Deletes and inserts for one region of one text.
#[derive(Clone, Debug, Default)]
struct RegionEdit {
    deletes: Vec<CharId>,
    inserts: Vec<DraftInsert>,
}

#[derive(Clone, Debug)]
enum Cluster {
    Clean(RegionEdit),
    Conflict {
        index: usize,
        incoming: RegionEdit,
        replace_deletes: Vec<CharId>,
        replace_anchor: DraftAnchor,
    },
}

/// What a map key conflict does when resolved `incoming`.
#[derive(Clone, Debug)]
enum KeyAction {
    /// A map or modify/delete conflict: set (or delete) the key as incoming has it.
    Key {
        key: String,
        incoming: Option<Value>,
    },
    /// A rename conflict: target and incoming paths of the content object.
    Rename {
        target: Vec<String>,
        incoming: Vec<String>,
    },
    /// A path conflict: the target's and the incoming path.
    Path { target: String, incoming: String },
}

#[derive(Clone, Debug)]
enum ObjectPlan {
    Register {
        object: ObjectId,
        create: bool,
        clean: Option<Value>,
        conflict: Option<(usize, Value)>,
    },
    Map {
        object: ObjectId,
        create: bool,
        tree: bool,
        /// Clean changes: key → value, `None` deletes.
        clean: BTreeMap<String, Option<Value>>,
        conflicts: Vec<(usize, KeyAction)>,
    },
    Text {
        object: ObjectId,
        create: bool,
        transcript: bool,
        clusters: Vec<Cluster>,
    },
}

/// A merge plan (TL-71): the base, the clean changes and the conflicts, which the caller
/// resolves with [`MergePlan::resolve`].
#[derive(Clone, Debug)]
pub struct MergePlan {
    /// The target timeline.
    pub target: OpId,
    /// The incoming timeline.
    pub source: OpId,
    /// The incoming version planned (the incoming head).
    pub incoming: Version,
    /// The merge base (TL-70).
    pub base: Version,
    /// The conflicts in plan order.
    pub conflicts: Vec<Conflict>,
    objects: Vec<ObjectPlan>,
}

/// A merge ready to record: the op with the this-op marker where it refers to itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeDraft {
    /// The target timeline.
    pub target: OpId,
    /// The merge op; references to its own inserts use [`MergeDraft::this_op`].
    pub op: MergeOp,
}

fn this_op() -> OpId {
    OpId::new(0, Arc::new(ReplicaId::new("", "", 0)))
}

impl MergeDraft {
    /// The placeholder op id standing for the merge op itself until it is written.
    pub fn this_op() -> OpId {
        this_op()
    }

    /// The merge digest an accept effect's plan binds (TL-74).
    pub fn digest(&self, history: &HistoryId) -> [u8; 32] {
        Sha256::digest(self.digest_preimage(history)).into()
    }

    /// `"han-timeline-merge-v1\n" ‖ dCBOR([history, target, incoming, base, changes,
    /// conflicts])`, with full op ids and the this-op marker as null (TL-74).
    pub fn digest_preimage(&self, history: &HistoryId) -> Vec<u8> {
        let marker = this_op();
        let (changes, conflicts) = merge_digest_parts(&self.op, Some(&marker));
        let full_version = |v: &Version| Cbor::Array(v.heads().iter().map(full_id).collect());
        let body = Cbor::Array(vec![
            Cbor::Bytes(history.0.to_vec()),
            full_id(&self.target),
            full_version(&self.op.incoming),
            full_version(&self.op.base),
            changes,
            conflicts,
        ]);
        [b"han-timeline-merge-v1\n".as_slice(), &body.encode()].concat()
    }

    /// The op with the this-op marker replaced by the op's real id.
    pub fn op_with_id(&self, id: &OpId) -> MergeOp {
        let marker = this_op();
        let fix = |c: &mut CharId| {
            if c.insertion.op == marker {
                c.insertion.op = id.clone();
            }
        };
        let mut op = self.op.clone();
        for change in &mut op.changes {
            if let Change::Text { inserts, .. } = change {
                for i in inserts {
                    if let Some(a) = &mut i.anchor {
                        fix(a);
                    }
                }
            }
        }
        op
    }
}

// ---------------------------------------------------------------------------------------------
// Text planning (TL-72)

#[derive(Clone, Debug)]
struct Elem {
    canon: CharId,
    ch: char,
    local: Local,
}

/// Canonical ids of the visible characters in order, the first of each canonical id.
fn sequence(t: Option<&TextObj>) -> Vec<Elem> {
    let Some(t) = t else { return Vec::new() };
    let lay = t.layout();
    let mut seen = HashSet::new();
    lay.visible_index
        .iter()
        .filter_map(|&i| {
            let l = lay.order[i];
            let canon = t.canonical(l);
            seen.insert(canon.clone()).then(|| Elem {
                canon,
                ch: t.char_at(l),
                local: l,
            })
        })
        .collect()
}

#[derive(Clone, Debug)]
struct Edit {
    side: Side,
    /// The first base position the edit covers (TL-72 step 2).
    p: usize,
    /// Side positions inserted.
    x: (usize, usize),
    /// Lines touched [a, b).
    lines: (usize, usize),
}

struct Side3<'a> {
    seq: &'a [Elem],
    /// base index → side index, for kept base characters.
    bx: Vec<Option<usize>>,
}

fn edits_of(
    side: Side,
    base: &[Elem],
    base_index: &HashMap<&CharId, usize>,
    seq: &[Elem],
    line: &dyn Fn(usize) -> usize,
    base_char: &dyn Fn(usize) -> char,
) -> Result<(Vec<Edit>, Vec<Option<usize>>), Error> {
    let mut kept: Vec<(usize, usize)> = Vec::new();
    for (xi, e) in seq.iter().enumerate() {
        if let Some(&bi) = base_index.get(&e.canon) {
            if kept.last().is_some_and(|(pb, _)| *pb >= bi) {
                return Err(Error::MergeOrder);
            }
            kept.push((bi, xi));
        }
    }
    let mut bx = vec![None; base.len()];
    for &(bi, xi) in &kept {
        bx[bi] = Some(xi);
    }
    let mut out = Vec::new();
    let mut prev: (usize, usize) = (0, 0); // next base index, next side index
    for &(bi, xi) in kept.iter().chain(std::iter::once(&(base.len(), seq.len()))) {
        let (p, q) = (prev.0, bi);
        let x = (prev.1, xi);
        if p < q || x.0 < x.1 {
            let lines = if p < q {
                (line(p), line(q - 1) + 1)
            } else if p == 0 || base_char(p - 1) == '\n' {
                (line(p), line(p))
            } else {
                (line(p - 1), line(p - 1) + 1)
            };
            out.push(Edit { side, p, x, lines });
        }
        prev = (bi + 1, xi + 1);
    }
    Ok((out, bx))
}

fn meet(a: &Edit, b: &Edit) -> bool {
    let (a1, b1) = a.lines;
    let (a2, b2) = b.lines;
    let e1 = a1 == b1;
    let e2 = a2 == b2;
    match (e1, e2) {
        (false, false) => a1 < b2 && a2 < b1,
        (true, true) => a1 == a2,
        (true, false) => a2 < a1 && a1 < b2,
        (false, true) => a1 < a2 && a2 < b1,
    }
}

fn find(parent: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while parent[r] != r {
        r = parent[r];
    }
    let mut i = i;
    while parent[i] != r {
        let next = parent[i];
        parent[i] = r;
        i = next;
    }
    r
}

/// The target text with the lookups step 7 needs.
struct TargetText<'a> {
    t: &'a TextObj,
    /// canonical → the target character holding it (a visible one when there is one).
    holds: HashMap<CharId, Local>,
    /// canonical → characters hidden only by TL-45 (b).
    hidden: HashMap<CharId, Vec<Local>>,
    /// canonical → visible target character.
    shows: HashMap<CharId, Local>,
}

impl<'a> TargetText<'a> {
    fn new(t: &'a TextObj) -> Self {
        let lay = t.layout();
        let mut holds: HashMap<CharId, Local> = HashMap::new();
        let mut hidden: HashMap<CharId, Vec<Local>> = HashMap::new();
        let mut shows = HashMap::new();
        for (i, &l) in lay.order.iter().enumerate() {
            let c = t.canonical(l);
            if lay.visible[i] {
                shows.insert(c.clone(), l);
                holds.insert(c, l);
            } else {
                if lay.vis_a[i] {
                    hidden.entry(c.clone()).or_default().push(l);
                }
                holds.entry(c).or_insert(l);
            }
        }
        TargetText {
            t,
            holds,
            hidden,
            shows,
        }
    }

    /// The character and its hidden duplicates, for a delete that must not let it reappear.
    fn delete_with_duplicates(&self, l: Local, out: &mut Vec<CharId>) {
        out.push(self.t.char_id(l));
        if let Some(dups) = self.hidden.get(&self.t.canonical(l)) {
            out.extend(dups.iter().map(|d| self.t.char_id(*d)));
        }
    }

    /// The anchor immediately before character `l` in the target's order.
    fn before(&self, l: Local) -> DraftAnchor {
        let lay = self.t.layout();
        let i = lay.pos[l.0 as usize][l.1 as usize];
        if i == 0 {
            DraftAnchor::Start
        } else {
            DraftAnchor::Char(self.t.char_id(lay.order[i - 1]))
        }
    }
}

struct TextPlanner<'a> {
    base: Vec<Elem>,
    tseq: Vec<Elem>,
    iseq: Vec<Elem>,
    target: Option<TargetText<'a>>,
}

impl TextPlanner<'_> {
    /// Where the first run of a region goes (TL-72 step 7).
    fn first_anchor(&self, t_region: &[usize], i_start: usize) -> DraftAnchor {
        let Some(tt) = &self.target else {
            return DraftAnchor::Start;
        };
        if let Some(&first) = t_region.first() {
            return tt.before(self.tseq[first].local);
        }
        for xi in (0..i_start).rev() {
            if let Some(&l) = tt.holds.get(&self.iseq[xi].canon) {
                return DraftAnchor::Char(tt.t.char_id(l));
            }
        }
        DraftAnchor::Start
    }

    /// Makes the target region read the incoming region text (TL-72 step 7).
    fn make_equal(&self, t_region: &[usize], i_region: &[usize]) -> RegionEdit {
        let mut edit = RegionEdit::default();
        let incoming: HashSet<&CharId> = i_region.iter().map(|&x| &self.iseq[x].canon).collect();
        if let Some(tt) = &self.target {
            for &xt in t_region {
                let e = &self.tseq[xt];
                if !incoming.contains(&e.canon) {
                    tt.delete_with_duplicates(e.local, &mut edit.deletes);
                }
            }
        }
        let shows = |c: &CharId| self.target.as_ref().and_then(|tt| tt.shows.get(c));
        let mut prev_shown: Option<Local> = None;
        let mut prev_was_run = false;
        for (r, &xi) in i_region.iter().enumerate() {
            let e = &self.iseq[xi];
            if let Some(&l) = shows(&e.canon) {
                prev_shown = Some(l);
                prev_was_run = false;
                continue;
            }
            // Extend the current run when this character continues its origin insertion.
            if prev_was_run
                && let Some(run) = edit.inserts.last_mut()
                && let Some(o) = &run.origin
                && o.insertion == e.canon.insertion
                && o.offset + run.text.chars().count() as u32 == e.canon.offset
            {
                run.text.push(e.ch);
                continue;
            }
            let anchor = if r == 0 {
                self.first_anchor(t_region, xi)
            } else if prev_was_run {
                let k = edit.inserts.len() - 1;
                DraftAnchor::Prev(k, edit.inserts[k].text.chars().count() as u32 - 1)
            } else {
                let tt = self.target.as_ref();
                match (prev_shown, tt) {
                    (Some(l), Some(tt)) => DraftAnchor::Char(tt.t.char_id(l)),
                    _ => self.first_anchor(t_region, xi),
                }
            };
            edit.inserts.push(DraftInsert {
                anchor,
                text: e.ch.to_string(),
                origin: Some(e.canon.clone()),
            });
            prev_was_run = true;
        }
        edit
    }

    fn text_of(seq: &[Elem], idx: &[usize]) -> String {
        idx.iter().map(|&i| seq[i].ch).collect()
    }
}

/// Plans one text present in both target and incoming (TL-72). Pushes conflicts.
fn plan_text(
    object: &ObjectId,
    path: Option<String>,
    sb: Option<&TextObj>,
    st: &TextObj,
    si: &TextObj,
    conflicts: &mut Vec<Conflict>,
) -> Result<Vec<Cluster>, Error> {
    let planner = TextPlanner {
        base: sequence(sb),
        tseq: sequence(Some(st)),
        iseq: sequence(Some(si)),
        target: Some(TargetText::new(st)),
    };
    let base = &planner.base;
    let base_index: HashMap<&CharId, usize> = base
        .iter()
        .enumerate()
        .map(|(i, e)| (&e.canon, i))
        .collect();
    // line(i): LFs before base position i.
    let mut lf_before = Vec::with_capacity(base.len() + 1);
    let mut n = 0;
    lf_before.push(0);
    for e in base {
        if e.ch == '\n' {
            n += 1;
        }
        lf_before.push(n);
    }
    let line = |i: usize| lf_before[i];
    let base_char = |i: usize| base[i].ch;
    // Line starts, for base positions of a line range.
    let mut line_start = vec![0usize];
    for (i, e) in base.iter().enumerate() {
        if e.ch == '\n' {
            line_start.push(i + 1);
        }
    }
    let line_pos = |l: usize| line_start.get(l).copied().unwrap_or(base.len());

    let (t_edits, t_bx) = edits_of(
        Side::Target,
        base,
        &base_index,
        &planner.tseq,
        &line,
        &base_char,
    )?;
    let (i_edits, i_bx) = edits_of(
        Side::Incoming,
        base,
        &base_index,
        &planner.iseq,
        &line,
        &base_char,
    )?;
    let sides = [
        Side3 {
            seq: &planner.tseq,
            bx: t_bx,
        },
        Side3 {
            seq: &planner.iseq,
            bx: i_bx,
        },
    ];
    let edits: Vec<Edit> = t_edits.into_iter().chain(i_edits).collect();
    let mut parent: Vec<usize> = (0..edits.len()).collect();
    for a in 0..edits.len() {
        for b in a + 1..edits.len() {
            if meet(&edits[a], &edits[b]) {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent[ra] = rb;
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..edits.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut clusters: Vec<(usize, usize, Vec<usize>)> = groups
        .into_values()
        .map(|members| {
            let a = members.iter().map(|&m| edits[m].lines.0).min().unwrap_or(0);
            let b = members.iter().map(|&m| edits[m].lines.1).max().unwrap_or(0);
            (a, b, members)
        })
        .collect();
    clusters.sort_by_key(|(a, b, members)| (*a, *b, members.first().map(|m| edits[*m].p)));

    let region = |side: usize, a: usize, b: usize, members: &[usize]| -> Vec<usize> {
        let s = &sides[side];
        let want = if side == 0 {
            Side::Target
        } else {
            Side::Incoming
        };
        let mut xs: Vec<usize> = (line_pos(a)..line_pos(b).max(line_pos(a)))
            .filter_map(|bi| s.bx[bi])
            .collect();
        for &m in members {
            if edits[m].side == want {
                xs.extend(edits[m].x.0..edits[m].x.1);
            }
        }
        xs.sort_unstable();
        xs.dedup();
        let _ = s.seq;
        xs
    };

    let mut out = Vec::new();
    for (a, b, members) in clusters {
        let has_t = members.iter().any(|&m| edits[m].side == Side::Target);
        let has_i = members.iter().any(|&m| edits[m].side == Side::Incoming);
        if !has_i {
            continue;
        }
        let t_region = region(0, a, b, &members);
        let i_region = region(1, a, b, &members);
        if !has_t {
            out.push(Cluster::Clean(planner.make_equal(&t_region, &i_region)));
            continue;
        }
        let t_text = TextPlanner::text_of(&planner.tseq, &t_region);
        let i_text = TextPlanner::text_of(&planner.iseq, &i_region);
        if t_text == i_text {
            continue;
        }
        let base_text: String = (line_pos(a)..line_pos(b).max(line_pos(a)))
            .map(|bi| base[bi].ch)
            .collect();
        let index = conflicts.len();
        conflicts.push(Conflict::Text(TextHunk {
            object: object.clone(),
            path: path.clone(),
            lines: (a as u64, b as u64),
            base: base_text,
            target: t_text,
            incoming: i_text,
        }));
        let mut replace_deletes = Vec::new();
        if let Some(tt) = &planner.target {
            for &xt in &t_region {
                tt.delete_with_duplicates(planner.tseq[xt].local, &mut replace_deletes);
            }
        }
        let i_start = i_region.first().copied().unwrap_or(0);
        out.push(Cluster::Conflict {
            index,
            incoming: planner.make_equal(&t_region, &i_region),
            replace_deletes,
            replace_anchor: planner.first_anchor(&t_region, i_start),
        });
    }
    Ok(out)
}

/// An imported text: all visible characters as copies (TL-71).
fn import_text(si: &TextObj) -> RegionEdit {
    let planner = TextPlanner {
        base: Vec::new(),
        tseq: Vec::new(),
        iseq: sequence(Some(si)),
        target: None,
    };
    let all: Vec<usize> = (0..planner.iseq.len()).collect();
    planner.make_equal(&[], &all)
}

// ---------------------------------------------------------------------------------------------
// Maps and trees (TL-71, TL-87)

fn visible(state: &State, object: &ObjectId, key: &str) -> Option<Value> {
    state.map_value(object, key).cloned()
}

/// The content of a file entry's object, for modify/delete (TL-87).
fn content(state: &State, entry: Option<&Value>) -> Option<String> {
    let obj = ObjectId::Op(entry?.obj_field()?);
    match state.objects.get(&obj)? {
        Obj::Text(t) => Some(format!("t:{}", t.text())),
        Obj::Register(r) => Some(format!("r:{:?}", r.value.encode())),
        Obj::Map(_) => None,
    }
}

fn paths_of(state: &State, tree: &ObjectId) -> BTreeMap<String, Value> {
    state
        .map(tree)
        .map(|m| {
            m.keys
                .iter()
                .filter_map(|(k, v)| v.first().map(|(_, v)| (k.clone(), v.clone())))
                .collect()
        })
        .unwrap_or_default()
}

fn by_content(paths: &BTreeMap<String, Value>) -> BTreeMap<OpId, Vec<String>> {
    let mut out: BTreeMap<OpId, Vec<String>> = BTreeMap::new();
    for (p, v) in paths {
        if let Some(o) = v.obj_field() {
            out.entry(o).or_default().push(p.clone());
        }
    }
    out
}

/// Clean map changes: key → value, `None` deletes.
type CleanKeys = BTreeMap<String, Option<Value>>;

fn plan_map(
    object: &ObjectId,
    tree: bool,
    sb: &State,
    st: &State,
    si: &State,
    conflicts: &mut Vec<Conflict>,
) -> (CleanKeys, Vec<(usize, KeyAction)>) {
    let keys: BTreeSet<String> = [sb, si]
        .iter()
        .filter_map(|s| s.map(object))
        .flat_map(|m| m.keys.keys().cloned())
        .collect();
    let mut clean = BTreeMap::new();
    let mut key_conflicts: Vec<(String, Conflict, KeyAction)> = Vec::new();
    for key in keys {
        let b = visible(sb, object, &key);
        let t = visible(st, object, &key);
        let s = visible(si, object, &key);
        if tree && b.is_some() {
            let deleted_by = match (&t, &s) {
                (None, Some(_)) if content(si, s.as_ref()) != content(sb, b.as_ref()) => {
                    Some(Side::Target)
                }
                (Some(_), None) if content(st, t.as_ref()) != content(sb, b.as_ref()) => {
                    Some(Side::Incoming)
                }
                _ => None,
            };
            if let Some(deleted_by) = deleted_by {
                key_conflicts.push((
                    key.clone(),
                    Conflict::ModifyDelete {
                        object: object.clone(),
                        key: key.clone(),
                        deleted_by,
                    },
                    KeyAction::Key {
                        key: key.clone(),
                        incoming: s.clone(),
                    },
                ));
                continue;
            }
        }
        if s == b || s == t {
            continue;
        }
        if t == b {
            clean.insert(key, s);
        } else {
            key_conflicts.push((
                key.clone(),
                Conflict::Map {
                    object: object.clone(),
                    key: key.clone(),
                    base: b,
                    target: t,
                    incoming: s.clone(),
                },
                KeyAction::Key { key, incoming: s },
            ));
        }
    }
    let mut actions = Vec::new();
    for (_, c, a) in key_conflicts {
        actions.push((conflicts.len(), a));
        conflicts.push(c);
    }
    if !tree {
        return (clean, actions);
    }
    // The tree after the clean changes.
    let t_paths = paths_of(st, object);
    let i_paths = paths_of(si, object);
    let mut after = t_paths.clone();
    for (k, v) in &clean {
        match v {
            Some(v) => after.insert(k.clone(), v.clone()),
            None => after.remove(k),
        };
    }
    let t_by = by_content(&t_paths);
    let i_by = by_content(&i_paths);
    for (content, paths) in by_content(&after) {
        let nt = t_by.get(&content).map_or(0, Vec::len);
        let ni = i_by.get(&content).map_or(0, Vec::len);
        if paths.len() > nt && paths.len() > ni {
            let target = t_by.get(&content).cloned().unwrap_or_default();
            let incoming = i_by.get(&content).cloned().unwrap_or_default();
            actions.push((
                conflicts.len(),
                KeyAction::Rename {
                    target: target.clone(),
                    incoming: incoming.clone(),
                },
            ));
            conflicts.push(Conflict::Rename {
                object: object.clone(),
                content,
                target,
                incoming,
            });
        }
    }
    let had: HashSet<(String, String)> = path_clashes(t_paths.keys())
        .into_iter()
        .chain(path_clashes(i_paths.keys()))
        .collect();
    for (p, q) in path_clashes(after.keys()) {
        if had.contains(&(p.clone(), q.clone())) {
            continue;
        }
        let p_new = clean.get(&p).is_some_and(Option::is_some) && !t_paths.contains_key(&p);
        let (target, incoming) = if p_new {
            (q.clone(), p.clone())
        } else {
            (p.clone(), q.clone())
        };
        actions.push((conflicts.len(), KeyAction::Path { target, incoming }));
        conflicts.push(Conflict::Path {
            object: object.clone(),
            paths: (p, q),
        });
    }
    (clean, actions)
}

/// The first path of a tree in `state` whose entry names `object`.
fn path_of(state: &State, object: &ObjectId) -> Option<String> {
    let ObjectId::Op(id) = object else {
        return None;
    };
    state.objects.iter().find_map(|(_, o)| match o {
        Obj::Map(m) if m.tree => m
            .keys
            .iter()
            .find_map(|(k, v)| (v.first()?.1.obj_field().as_ref() == Some(id)).then(|| k.clone())),
        _ => None,
    })
}

// ---------------------------------------------------------------------------------------------
// Planning and resolving

impl<S: Store> History<S> {
    /// Plans merging timeline `from` into timeline `into` (TL-70 – TL-72, TL-87). An empty
    /// plan is `up_to_date`.
    pub fn plan_merge(&self, into: &OpId, from: &OpId) -> Result<MergePlan, Error> {
        if into == from {
            return Err(Error::MergeSelf);
        }
        let ht = self
            .graph
            .head(into)
            .ok_or(Error::UnknownTimeline)?
            .to_vec();
        let hi = self
            .graph
            .head(from)
            .ok_or(Error::UnknownTimeline)?
            .to_vec();
        let ct = self.graph.closure(&ht, true);
        if self.graph.closure(&hi, false).is_subset(&ct) {
            return Err(Error::UpToDate);
        }
        let ci = self.graph.closure(&hi, true);
        let base_heads = self.graph.frontier(&ct.intersect(&ci));
        let st = self.materialise(&ht).1;
        let si = self.materialise(&hi).1;
        let sb = self.materialise(&base_heads).1;

        let mut conflicts = Vec::new();
        let mut objects = Vec::new();
        for (id, obj) in &si.objects {
            let Some(target_obj) = st.objects.get(id) else {
                objects.push(match obj {
                    Obj::Register(r) => ObjectPlan::Register {
                        object: id.clone(),
                        create: true,
                        clean: Some(r.value.clone()),
                        conflict: None,
                    },
                    Obj::Map(m) => ObjectPlan::Map {
                        object: id.clone(),
                        create: true,
                        tree: m.tree,
                        clean: paths_of(&si, id)
                            .into_iter()
                            .map(|(k, v)| (k, Some(v)))
                            .collect(),
                        conflicts: Vec::new(),
                    },
                    Obj::Text(t) => ObjectPlan::Text {
                        object: id.clone(),
                        create: true,
                        transcript: t.transcript,
                        clusters: vec![Cluster::Clean(import_text(t))],
                    },
                });
                continue;
            };
            match (target_obj, obj) {
                (Obj::Register(t), Obj::Register(s)) => {
                    let b = sb.register(id);
                    let (t, s) = (&t.value, &s.value);
                    let (mut clean, mut conflict) = (None, None);
                    if Some(s) == b || s == t {
                    } else if Some(t) == b {
                        clean = Some(s.clone());
                    } else {
                        conflict = Some((conflicts.len(), s.clone()));
                        conflicts.push(Conflict::Register {
                            object: id.clone(),
                            base: b.cloned(),
                            target: t.clone(),
                            incoming: s.clone(),
                        });
                    }
                    objects.push(ObjectPlan::Register {
                        object: id.clone(),
                        create: false,
                        clean,
                        conflict,
                    });
                }
                (Obj::Map(m), Obj::Map(_)) => {
                    let (clean, actions) = plan_map(id, m.tree, &sb, &st, &si, &mut conflicts);
                    objects.push(ObjectPlan::Map {
                        object: id.clone(),
                        create: false,
                        tree: m.tree,
                        clean,
                        conflicts: actions,
                    });
                }
                (Obj::Text(t), Obj::Text(s)) => {
                    let path = path_of(&st, id).or_else(|| path_of(&si, id));
                    let clusters = plan_text(id, path, sb.text_obj(id), t, s, &mut conflicts)?;
                    objects.push(ObjectPlan::Text {
                        object: id.clone(),
                        create: false,
                        transcript: t.transcript,
                        clusters,
                    });
                }
                _ => {}
            }
        }
        let plan = MergePlan {
            target: into.clone(),
            source: from.clone(),
            incoming: self.graph.version_of(&hi),
            base: self.graph.version_of(&base_heads),
            conflicts,
            objects,
        };
        if plan.conflicts.is_empty() && plan.resolve(&[])?.op.changes.is_empty() {
            return Err(Error::UpToDate);
        }
        Ok(plan)
    }

    /// The state the target would have after recording `draft` (for review).
    pub fn preview(&self, draft: &MergeDraft) -> Result<State, Error> {
        let ht = self
            .graph
            .head(&draft.target)
            .ok_or(Error::UnknownTimeline)?;
        let mut state = (*self.materialise(ht).1).clone();
        let id = OpId::new(
            u64::MAX,
            Arc::new(ReplicaId::new("@preview:invalid", "preview", 0)),
        );
        let op = Op::Merge(Box::new(draft.op_with_id(&id)));
        state.apply(&id, &op, &Seen::All);
        Ok(state)
    }

    /// Records a resolved merge as one `merge` op on its target (TL-73). `effect` is the EntryId
    /// of the approved accept effect, when there is one (TL-74).
    pub fn merge(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        draft: &MergeDraft,
        effect: Option<String>,
    ) -> Result<Written, Error> {
        let head = self.head(&draft.target).ok_or(Error::UnknownTimeline)?;
        let target = draft.target.clone();
        self.write_with(signer, lane, head, Some(target), |id| {
            let mut op = draft.op_with_id(id);
            op.effect = effect;
            vec![Op::Merge(Box::new(op))]
        })
    }
}

/// Coalesces character ids into identity ranges, sorted.
fn ranges(mut chars: Vec<CharId>) -> Vec<IdRange> {
    chars.sort();
    chars.dedup();
    let mut out: Vec<IdRange> = Vec::new();
    for c in chars {
        if let Some(last) = out.last_mut()
            && last.insertion == c.insertion
            && last.to == c.offset
        {
            last.to += 1;
            continue;
        }
        out.push(IdRange {
            insertion: c.insertion,
            from: c.offset,
            to: c.offset + 1,
        });
    }
    out
}

impl MergePlan {
    /// Applies one resolution per conflict, in plan order (TL-75), giving the merge to record.
    pub fn resolve(&self, resolutions: &[Resolution]) -> Result<MergeDraft, Error> {
        if resolutions.len() != self.conflicts.len() {
            return Err(Error::Unresolved);
        }
        let kind_of = |r: &Resolution| match r {
            Resolution::Target => ResolutionKind::Target,
            Resolution::Incoming => ResolutionKind::Incoming,
            Resolution::Value(_) | Resolution::Text(_) => ResolutionKind::Given,
        };
        let mut records = Vec::new();
        for (c, r) in self.conflicts.iter().zip(resolutions) {
            let ok = match (c, r) {
                (_, Resolution::Target | Resolution::Incoming) => true,
                (Conflict::Register { .. } | Conflict::Map { .. }, Resolution::Value(v)) => {
                    v.check().is_ok()
                }
                (Conflict::Text(_), Resolution::Text(_)) => true,
                _ => false,
            };
            if !ok {
                return Err(Error::Unresolved);
            }
            records.push(c.record(kind_of(r)));
        }
        let marker = this_op();
        let mut changes = Vec::new();
        let mut j = 0u32;
        for plan in &self.objects {
            match plan {
                ObjectPlan::Register {
                    object,
                    create,
                    clean,
                    conflict,
                } => {
                    let mut value = clean.clone();
                    if let Some((i, incoming)) = conflict {
                        match &resolutions[*i] {
                            Resolution::Incoming => value = Some(incoming.clone()),
                            Resolution::Value(v) => value = Some(v.clone()),
                            _ => {}
                        }
                    }
                    if let Some(value) = value {
                        changes.push(Change::Register {
                            object: object.clone(),
                            create: *create,
                            value,
                        });
                    }
                }
                ObjectPlan::Map {
                    object,
                    create,
                    tree,
                    clean,
                    conflicts,
                } => {
                    let mut result = clean.clone();
                    for (i, action) in conflicts {
                        let r = &resolutions[*i];
                        match (action, r) {
                            (KeyAction::Key { key, incoming }, Resolution::Incoming) => {
                                result.insert(key.clone(), incoming.clone());
                            }
                            (KeyAction::Key { key, .. }, Resolution::Value(v)) => {
                                result.insert(key.clone(), Some(v.clone()));
                            }
                            (KeyAction::Rename { target, incoming }, Resolution::Target) => {
                                for p in incoming.iter().filter(|p| !target.contains(p)) {
                                    result.remove(p);
                                }
                                for p in target {
                                    if result.get(p).is_some_and(Option::is_none) {
                                        result.remove(p);
                                    }
                                }
                            }
                            (KeyAction::Rename { target, incoming }, Resolution::Incoming) => {
                                for p in target.iter().filter(|p| !incoming.contains(p)) {
                                    result.insert(p.clone(), None);
                                }
                            }
                            (KeyAction::Path { incoming, .. }, Resolution::Target) => {
                                result.remove(incoming);
                            }
                            (KeyAction::Path { target, .. }, Resolution::Incoming) => {
                                result.insert(target.clone(), None);
                            }
                            _ => {}
                        }
                    }
                    if *create || !result.is_empty() {
                        let mut sets = Vec::new();
                        let mut deletes = Vec::new();
                        for (k, v) in result {
                            match v {
                                Some(v) => sets.push((k, v)),
                                None => deletes.push(k),
                            }
                        }
                        changes.push(Change::Map {
                            object: object.clone(),
                            create: *create,
                            tree: *tree,
                            sets,
                            deletes,
                        });
                    }
                }
                ObjectPlan::Text {
                    object,
                    create,
                    transcript,
                    clusters,
                } => {
                    let mut deletes = Vec::new();
                    let mut inserts: Vec<DraftInsert> = Vec::new();
                    let mut add = |edit: &RegionEdit, deletes: &mut Vec<CharId>| {
                        deletes.extend(edit.deletes.iter().cloned());
                        let base = inserts.len();
                        for ins in &edit.inserts {
                            let anchor = match &ins.anchor {
                                DraftAnchor::Prev(k, off) => DraftAnchor::Prev(base + k, *off),
                                a => a.clone(),
                            };
                            inserts.push(DraftInsert {
                                anchor,
                                text: ins.text.clone(),
                                origin: ins.origin.clone(),
                            });
                        }
                    };
                    for cluster in clusters {
                        match cluster {
                            Cluster::Clean(edit) => add(edit, &mut deletes),
                            Cluster::Conflict {
                                index,
                                incoming,
                                replace_deletes,
                                replace_anchor,
                            } => match &resolutions[*index] {
                                Resolution::Incoming => add(incoming, &mut deletes),
                                Resolution::Text(s) => {
                                    let s = crate::local::normalise(s);
                                    let mut edit = RegionEdit {
                                        deletes: replace_deletes.clone(),
                                        inserts: Vec::new(),
                                    };
                                    if !s.is_empty() {
                                        edit.inserts.push(DraftInsert {
                                            anchor: replace_anchor.clone(),
                                            text: s,
                                            origin: None,
                                        });
                                    }
                                    add(&edit, &mut deletes);
                                }
                                _ => {}
                            },
                        }
                    }
                    if !*create && deletes.is_empty() && inserts.is_empty() {
                        continue;
                    }
                    // No two inserts may name one anchor (TL-48): a later one goes after the
                    // earlier one, which keeps their order.
                    let mut taken: HashMap<DraftAnchor, usize> = HashMap::new();
                    let mut out = Vec::new();
                    for (k, ins) in inserts.iter().enumerate() {
                        let mut anchor = ins.anchor.clone();
                        while let Some(&prev) = taken.get(&anchor) {
                            let n = inserts[prev].text.chars().count() as u32;
                            anchor = DraftAnchor::Prev(prev, n - 1);
                        }
                        taken.insert(anchor.clone(), k);
                        let anchor = match anchor {
                            DraftAnchor::Start => None,
                            DraftAnchor::Char(c) => Some(c),
                            DraftAnchor::Prev(i, off) => Some(CharId {
                                insertion: InsertionId {
                                    op: marker.clone(),
                                    index: j + i as u32,
                                },
                                offset: off,
                            }),
                        };
                        out.push(CopyInsert {
                            anchor,
                            text: ins.text.clone(),
                            origin: ins.origin.clone(),
                        });
                    }
                    j += out.len() as u32;
                    changes.push(Change::Text {
                        object: object.clone(),
                        create: *create,
                        transcript: *transcript,
                        deletes: ranges(deletes),
                        inserts: out,
                    });
                }
            }
        }
        Ok(MergeDraft {
            target: self.target.clone(),
            op: MergeOp {
                source: self.source.clone(),
                incoming: self.incoming.clone(),
                base: self.base.clone(),
                changes,
                conflicts: records,
                effect: None,
            },
        })
    }
}
