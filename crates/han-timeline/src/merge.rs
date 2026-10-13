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

/// One region of a text where the incoming side has edits (TL-72 steps 4 – 6).
#[derive(Clone, Debug)]
struct Region {
    /// The target's region characters (indexes into the target sequence), in order.
    t: Vec<usize>,
    /// The incoming region characters (indexes into the incoming sequence), in order.
    i: Vec<usize>,
    /// Where the region sits in the target sequence: its first character, or for an empty
    /// target region the index right after the last character of its prefix (step 7).
    gap: usize,
    /// The conflict this region raised, or `None` for a clean incoming change.
    conflict: Option<usize>,
}

/// A text present on both sides, planned: the sequences, what each side kept, and the regions.
#[derive(Debug)]
struct TextPlan {
    target: Arc<TextObj>,
    tseq: Vec<Elem>,
    iseq: Vec<Elem>,
    /// target sequence index → base index, for characters the target kept.
    t_kept: Vec<Option<usize>>,
    /// incoming sequence index → base index, for characters the incoming side kept.
    i_kept: Vec<Option<usize>>,
    /// base index → target sequence index, for characters the target kept.
    t_bx: Vec<Option<usize>>,
    /// base index → incoming sequence index, for characters the incoming side kept.
    i_bx: Vec<Option<usize>>,
    regions: Vec<Region>,
}

#[derive(Clone, Debug)]
enum TextSource {
    /// An imported text: its inserts.
    Import(Vec<DraftInsert>),
    /// A text on both sides.
    Both(Arc<TextPlan>),
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
        source: TextSource,
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
    /// The first base position the edit covers.
    p: usize,
    /// Side positions inserted.
    x: (usize, usize),
    /// Lines touched [a, b).
    lines: (usize, usize),
}

/// The kept characters of a side (TL-72 step 2): of the shared characters `(base, side)` in
/// base order, the longest subsequence whose side positions increase; among equally long ones,
/// the one whose base positions are lexicographically smallest.
fn kept_subsequence(shared: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let n = shared.len();
    // longest[i]: the longest increasing subsequence that starts at shared[i].
    let mut longest = vec![0usize; n];
    // tails[k]: the greatest side position that starts an increasing run of length k + 1 seen
    // so far (scanning from the right); decreasing in k.
    let mut tails: Vec<usize> = Vec::new();
    for i in (0..n).rev() {
        let x = shared[i].1;
        let k = tails.partition_point(|&t| t > x);
        longest[i] = k + 1;
        if k == tails.len() {
            tails.push(x);
        } else if tails[k] < x {
            tails[k] = x;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut need = tails.len();
    let mut last: Option<usize> = None;
    for (i, &(b, x)) in shared.iter().enumerate() {
        if need == 0 {
            break;
        }
        if longest[i] == need && last.is_none_or(|l| x > l) {
            out.push((b, x));
            last = Some(x);
            need -= 1;
        }
    }
    out
}

/// The edits of one side against the base (TL-72 steps 2 – 3), and base index → side index
/// for the characters it kept.
fn edits_of(
    side: Side,
    base: &[Elem],
    base_index: &HashMap<&CharId, usize>,
    seq: &[Elem],
    line: &dyn Fn(usize) -> usize,
    base_char: &dyn Fn(usize) -> char,
) -> (Vec<Edit>, Vec<Option<usize>>) {
    let mut shared: Vec<(usize, usize)> = seq
        .iter()
        .enumerate()
        .filter_map(|(xi, e)| base_index.get(&e.canon).map(|&bi| (bi, xi)))
        .collect();
    shared.sort_unstable();
    let kept = kept_subsequence(&shared);
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
    (out, bx)
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
    /// canonical → characters hidden only by TL-45 (b).
    hidden: HashMap<CharId, Vec<Local>>,
}

impl<'a> TargetText<'a> {
    fn new(t: &'a TextObj) -> Self {
        let lay = t.layout();
        let mut hidden: HashMap<CharId, Vec<Local>> = HashMap::new();
        for (i, &l) in lay.order.iter().enumerate() {
            if lay.vis_a[i] && !lay.visible[i] {
                hidden.entry(t.canonical(l)).or_default().push(l);
            }
        }
        TargetText { t, hidden }
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

/// Plans one text present in both target and incoming (TL-72 steps 1 – 6). Pushes conflicts.
fn plan_text(
    object: &ObjectId,
    path: Option<String>,
    sb: Option<&TextObj>,
    st: &Arc<TextObj>,
    si: &TextObj,
    conflicts: &mut Vec<Conflict>,
) -> Option<TextPlan> {
    let base = sequence(sb);
    let tseq = sequence(Some(st));
    let iseq = sequence(Some(si));
    let base_index: HashMap<&CharId, usize> = base
        .iter()
        .enumerate()
        .map(|(i, e)| (&e.canon, i))
        .collect();
    // line(i): LFs before base position i.
    let mut lf_before = Vec::with_capacity(base.len() + 1);
    let mut n = 0;
    lf_before.push(0);
    for e in &base {
        if e.ch == '\n' {
            n += 1;
        }
        lf_before.push(n);
    }
    let line = |i: usize| lf_before[i];
    let base_char = |i: usize| base[i].ch;
    let mut line_start = vec![0usize];
    for (i, e) in base.iter().enumerate() {
        if e.ch == '\n' {
            line_start.push(i + 1);
        }
    }
    let line_pos = |l: usize| line_start.get(l).copied().unwrap_or(base.len());

    let (t_edits, t_bx) = edits_of(Side::Target, &base, &base_index, &tseq, &line, &base_char);
    // Where each target character's line range ends: a kept character's base line + 1, an
    // edit's character the end of the edit's line range (step 7: the prefix before a region
    // is what ends at or before the region's first line).
    let mut t_end = vec![0usize; tseq.len()];
    for (bi, x) in t_bx.iter().enumerate() {
        if let Some(x) = x {
            t_end[*x] = line(bi) + 1;
        }
    }
    for e in &t_edits {
        t_end[e.x.0..e.x.1].fill(e.lines.1);
    }
    let (i_edits, i_bx) = edits_of(Side::Incoming, &base, &base_index, &iseq, &line, &base_char);
    let mut t_kept = vec![None; tseq.len()];
    for (bi, x) in t_bx.iter().enumerate() {
        if let Some(x) = x {
            t_kept[*x] = Some(bi);
        }
    }
    let mut i_kept = vec![None; iseq.len()];
    for (bi, x) in i_bx.iter().enumerate() {
        if let Some(x) = x {
            i_kept[*x] = Some(bi);
        }
    }
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
    clusters.sort_by_key(|(a, b, members)| (*a, *b, members.iter().map(|m| edits[*m].p).min()));

    // A side's region characters (step 5): its kept base characters in the region's lines and
    // what its edits in the cluster inserted, in its order.
    let region = |bx: &[Option<usize>], want: Side, a: usize, b: usize, members: &[usize]| {
        let mut xs: Vec<usize> = (line_pos(a)..line_pos(b).max(line_pos(a)))
            .filter_map(|bi| bx[bi])
            .collect();
        for &m in members {
            if edits[m].side == want {
                xs.extend(edits[m].x.0..edits[m].x.1);
            }
        }
        xs.sort_unstable();
        xs.dedup();
        xs
    };
    let text_of = |seq: &[Elem], idx: &[usize]| idx.iter().map(|&i| seq[i].ch).collect::<String>();

    let mut regions = Vec::new();
    for (a, b, members) in clusters {
        let has_t = members.iter().any(|&m| edits[m].side == Side::Target);
        let has_i = members.iter().any(|&m| edits[m].side == Side::Incoming);
        if !has_i {
            continue;
        }
        let t = region(&t_bx, Side::Target, a, b, &members);
        let i = region(&i_bx, Side::Incoming, a, b, &members);
        let base_range = (line_pos(a), line_pos(b).max(line_pos(a)));
        let mut conflict = None;
        if has_t {
            let (t_text, i_text) = (text_of(&tseq, &t), text_of(&iseq, &i));
            if t_text == i_text {
                continue;
            }
            conflict = Some(conflicts.len());
            conflicts.push(Conflict::Text(TextHunk {
                object: object.clone(),
                path: path.clone(),
                lines: (a as u64, b as u64),
                base: base[base_range.0..base_range.1]
                    .iter()
                    .map(|e| e.ch)
                    .collect(),
                target: t_text,
                incoming: i_text,
            }));
        }
        let gap = match t.first() {
            Some(&first) => first,
            None => t_end.iter().rposition(|&end| end <= a).map_or(0, |x| x + 1),
        };
        regions.push(Region {
            t,
            i,
            gap,
            conflict,
        });
    }
    (!regions.is_empty()).then(|| TextPlan {
        target: st.clone(),
        tseq,
        iseq,
        t_kept,
        i_kept,
        t_bx,
        i_bx,
        regions,
    })
}

/// An imported text: all its visible characters as copies, in runs (TL-71).
fn import_text(si: &TextObj) -> Vec<DraftInsert> {
    let mut out: Vec<DraftInsert> = Vec::new();
    for e in sequence(Some(si)) {
        if let Some(run) = out.last_mut()
            && let Some(o) = &run.origin
            && o.insertion == e.canon.insertion
            && o.offset + run.text.chars().count() as u32 == e.canon.offset
        {
            run.text.push(e.ch);
            continue;
        }
        let anchor = match out.len() {
            0 => DraftAnchor::Start,
            k => DraftAnchor::Prev(k - 1, out[k - 1].text.chars().count() as u32 - 1),
        };
        out.push(DraftInsert {
            anchor,
            text: e.ch.to_string(),
            origin: Some(e.canon.clone()),
        });
    }
    out
}

/// What a region becomes in a resolved merge.
enum RegionAction<'a> {
    /// Untouched (the `target` resolution).
    Skip,
    /// Made to read its incoming region text (step 7).
    Rewrite,
    /// Replaced by a given string (step 8).
    Replace(&'a str),
}

impl TextPlan {
    /// Where the first run of a region goes when no earlier run can take it (step 7): before
    /// the target's first region character; for an empty target region, after the last target
    /// character that survives the merge before the region's gap, else `start`. (The
    /// RECOMMENDED "character holding the nearest kept character" can be a copy that sits
    /// elsewhere in the target's order, which would break the region's reading.)
    fn first_anchor(
        &self,
        tt: &TargetText<'_>,
        r: &Region,
        deleted: &HashSet<usize>,
    ) -> DraftAnchor {
        if let Some(&first) = r.t.first() {
            return tt.before(self.tseq[first].local);
        }
        (0..r.gap)
            .rev()
            .find(|xt| !deleted.contains(xt))
            .map_or(DraftAnchor::Start, |xt| {
                DraftAnchor::Char(tt.t.char_id(self.tseq[xt].local))
            })
    }

    /// The deletes and inserts (object-level indexes) of the resolved regions (steps 7 – 8).
    fn resolve(&self, actions: &[RegionAction<'_>]) -> (Vec<CharId>, Vec<DraftInsert>) {
        let tt = TargetText::new(&self.target);
        // Step 7, first two points: what each rewritten region keeps, and what goes.
        let mut kept: Vec<HashSet<usize>> = Vec::with_capacity(self.regions.len());
        let mut deleted: HashSet<usize> = HashSet::new();
        for (r, action) in self.regions.iter().zip(actions) {
            let keep: HashSet<usize> = match action {
                RegionAction::Rewrite => {
                    let i_set: HashSet<usize> = r.i.iter().copied().collect();
                    r.t.iter()
                        .copied()
                        .filter(|&xt| {
                            self.t_kept[xt]
                                .and_then(|bi| self.i_bx[bi])
                                .is_some_and(|xi| i_set.contains(&xi))
                        })
                        .collect()
                }
                _ => HashSet::new(),
            };
            if !matches!(action, RegionAction::Skip) {
                deleted.extend(r.t.iter().copied().filter(|xt| !keep.contains(xt)));
            }
            kept.push(keep);
        }
        let mut deletes = Vec::new();
        let mut sorted: Vec<usize> = deleted.iter().copied().collect();
        sorted.sort_unstable();
        for xt in sorted {
            tt.delete_with_duplicates(self.tseq[xt].local, &mut deletes);
        }
        let t_index: HashMap<&CharId, usize> = self
            .tseq
            .iter()
            .enumerate()
            .map(|(i, e)| (&e.canon, i))
            .collect();

        let mut inserts: Vec<DraftInsert> = Vec::new();
        // The last thing placed in document order when it is an insert run, with the target
        // position right after it: a later region with no surviving target character in
        // between continues after that run (the chaining of step 7).
        let mut tail: Option<(DraftAnchor, usize)> = None;
        let survivor_between = |from: usize, to: usize| (from..to).any(|xt| !deleted.contains(&xt));
        let lead = |r: &Region, tail: &Option<(DraftAnchor, usize)>| match tail {
            Some((anchor, pos)) if *pos <= r.gap && !survivor_between(*pos, r.gap) => {
                anchor.clone()
            }
            _ => self.first_anchor(&tt, r, &deleted),
        };
        for ((r, action), keep) in self.regions.iter().zip(actions).zip(&kept) {
            match action {
                RegionAction::Skip => {
                    if !r.t.is_empty() {
                        tail = None;
                    }
                }
                RegionAction::Replace(s) => {
                    if !s.is_empty() {
                        inserts.push(DraftInsert {
                            anchor: lead(r, &tail),
                            text: (*s).to_string(),
                            origin: None,
                        });
                        let k = inserts.len() - 1;
                        let n = inserts[k].text.chars().count() as u32;
                        tail = Some((DraftAnchor::Prev(k, n - 1), r.gap));
                    }
                }
                RegionAction::Rewrite => {
                    let in_region: HashSet<usize> = r.t.iter().copied().collect();
                    let mut cursor: Option<DraftAnchor> = None;
                    let mut cursor_pos = r.gap;
                    let mut open = false;
                    for &xi in &r.i {
                        let e = &self.iseq[xi];
                        if let Some(xt) = self.i_kept[xi].and_then(|bi| self.t_bx[bi])
                            && keep.contains(&xt)
                        {
                            cursor = Some(DraftAnchor::Char(tt.t.char_id(self.tseq[xt].local)));
                            cursor_pos = xt + 1;
                            tail = None;
                            open = false;
                            continue;
                        }
                        // A character the target still shows outside this region after the
                        // merge's other changes goes in as text, or TL-45 (b) would hide it.
                        let plain = t_index
                            .get(&e.canon)
                            .is_some_and(|xt| !in_region.contains(xt) && !deleted.contains(xt));
                        let origin = (!plain).then(|| e.canon.clone());
                        if open
                            && let Some(run) = inserts.last_mut()
                            && match (&run.origin, &origin) {
                                (None, None) => true,
                                (Some(o), Some(c)) => {
                                    o.insertion == c.insertion
                                        && o.offset + run.text.chars().count() as u32 == c.offset
                                }
                                _ => false,
                            }
                        {
                            run.text.push(e.ch);
                            let n = run.text.chars().count() as u32;
                            let end = DraftAnchor::Prev(inserts.len() - 1, n - 1);
                            cursor = Some(end.clone());
                            tail = Some((end, cursor_pos));
                            continue;
                        }
                        let anchor = cursor.clone().unwrap_or_else(|| lead(r, &tail));
                        inserts.push(DraftInsert {
                            anchor,
                            text: e.ch.to_string(),
                            origin,
                        });
                        let end = DraftAnchor::Prev(inserts.len() - 1, 0);
                        cursor = Some(end.clone());
                        tail = Some((end, cursor_pos));
                        open = true;
                    }
                }
            }
        }
        (deletes, inserts)
    }
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
                        source: TextSource::Import(import_text(t)),
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
                    if let Some(plan) = plan_text(id, path, sb.text_obj(id), t, s, &mut conflicts) {
                        objects.push(ObjectPlan::Text {
                            object: id.clone(),
                            create: false,
                            transcript: t.transcript,
                            source: TextSource::Both(Arc::new(plan)),
                        });
                    }
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
    /// What each text the merge changes must read once `resolutions` are applied, worked out
    /// from strings alone: the target text with every resolved region replaced by its region
    /// text (TL-72 steps 7 – 8). An independent check of the identity-based result.
    #[doc(hidden)]
    pub fn expected_texts(
        &self,
        resolutions: &[Resolution],
    ) -> Result<BTreeMap<ObjectId, String>, Error> {
        if resolutions.len() != self.conflicts.len() {
            return Err(Error::Unresolved);
        }
        let mut out = BTreeMap::new();
        for plan in &self.objects {
            let ObjectPlan::Text { object, source, .. } = plan else {
                continue;
            };
            let text = match source {
                TextSource::Import(inserts) => inserts.iter().map(|i| i.text.as_str()).collect(),
                TextSource::Both(p) => {
                    // Per target position: the region text to put before it, and whether the
                    // character itself goes.
                    let n = p.tseq.len();
                    let mut before: Vec<String> = vec![String::new(); n + 1];
                    let mut gone = vec![false; n];
                    for r in &p.regions {
                        let replacement = match r.conflict.map(|i| &resolutions[i]) {
                            None | Some(Resolution::Incoming) => {
                                r.i.iter().map(|&x| p.iseq[x].ch).collect::<String>()
                            }
                            Some(Resolution::Text(s)) => crate::local::normalise(s),
                            _ => continue,
                        };
                        let at = r.gap;
                        for &xt in &r.t {
                            gone[xt] = true;
                        }
                        before[at].push_str(&replacement);
                    }
                    let mut s = String::new();
                    for (k, e) in p.tseq.iter().enumerate() {
                        s.push_str(&before[k]);
                        if !gone[k] {
                            s.push(e.ch);
                        }
                    }
                    s.push_str(&before[n]);
                    s
                }
            };
            out.insert(object.clone(), text);
        }
        Ok(out)
    }

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
                    source,
                } => {
                    let replacements: Vec<String> = resolutions
                        .iter()
                        .map(|r| match r {
                            Resolution::Text(t) => crate::local::normalise(t),
                            _ => String::new(),
                        })
                        .collect();
                    let (deletes, inserts) = match source {
                        TextSource::Import(inserts) => (Vec::new(), inserts.clone()),
                        TextSource::Both(plan) => {
                            let actions: Vec<RegionAction<'_>> = plan
                                .regions
                                .iter()
                                .map(|r| match r.conflict {
                                    None => RegionAction::Rewrite,
                                    Some(i) => match &resolutions[i] {
                                        Resolution::Incoming => RegionAction::Rewrite,
                                        Resolution::Text(_) => {
                                            RegionAction::Replace(&replacements[i])
                                        }
                                        _ => RegionAction::Skip,
                                    },
                                })
                                .collect();
                            plan.resolve(&actions)
                        }
                    };
                    if !*create && deletes.is_empty() && inserts.is_empty() {
                        continue;
                    }
                    // Chaining (TL-72 step 7, TL-48): a run whose anchor an earlier run took
                    // goes after the last character of the run just before it.
                    let mut taken: HashSet<DraftAnchor> = HashSet::new();
                    let mut out = Vec::new();
                    for (k, ins) in inserts.iter().enumerate() {
                        let mut anchor = ins.anchor.clone();
                        if k > 0 && taken.contains(&anchor) {
                            let n = inserts[k - 1].text.chars().count() as u32;
                            anchor = DraftAnchor::Prev(k - 1, n - 1);
                        }
                        taken.insert(anchor.clone());
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
