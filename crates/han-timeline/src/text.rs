//! The text CRDT (TL-40 – TL-48) with seals (TL-50 – TL-53) and merge copies (TL-45, TL-72).
//!
//! Characters live in insertions. The order of all characters is the pre-order of the tree in
//! which a character's parent is the previous character of its insertion, or the insertion's
//! anchor for its first character, with children newest first (TL-43). Deletes are tombstones.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::ids::{CharId, IdRange, InsertionId};

/// A character inside one text: local insertion index and offset.
pub(crate) type Local = (u32, u32);

#[derive(Clone, Debug)]
struct InsRec {
    id: InsertionId,
    chars: Arc<Vec<char>>,
    /// ASCII runs can be copied directly into the visible string.
    ascii: Option<Arc<str>>,
    /// Sorted, coalesced tombstone intervals for the compact layout.
    hidden: Vec<(u32, u32)>,
    /// For a copy, the canonical character its first character came from.
    origin: Option<CharId>,
    deleted: Vec<bool>,
    /// The children of this insertion's characters: per offset that has any (ascending), the
    /// local insertions anchored there, newest first.
    kids: Vec<(u32, Vec<u32>)>,
}

/// An anchor (TL-46).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Anchor {
    /// Position 0.
    Start,
    /// The end of the text.
    End,
    /// A character with a bias.
    Char(CharId, Bias),
}

/// The side an anchor keeps (TL-46).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bias {
    /// Text inserted at the position goes after the anchor.
    Left,
    /// Text inserted at the position goes before the anchor.
    Right,
}

/// The derived layout of a text: order and visibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// Every character, visible or not, in order.
    pub order: Vec<Local>,
    /// Order index of each character, flat: insertion `l`'s characters start at `first[l]`.
    pos: Vec<usize>,
    first: Vec<usize>,
    /// Visible by TL-45 (a) with seals (TL-52), per order index.
    pub vis_a: Vec<bool>,
    /// Visible, per order index.
    pub visible: Vec<bool>,
    /// The order indexes of the visible characters.
    pub visible_index: Vec<usize>,
    /// For each seal, the order indexes of its first and last sealed character.
    pub seal_spans: Vec<(usize, usize)>,
    /// Sealed by some seal, per order index.
    pub sealed: Vec<bool>,
}

impl Layout {
    /// The order index of a character.
    pub fn pos(&self, (l, k): Local) -> usize {
        self.pos[self.first[l as usize] + k as usize]
    }

    /// Adds the `n` characters of new local insertion `l` at order index `at`, all visible
    /// (a text without seals or copies, where visibility is "not deleted").
    fn insert_run(&mut self, at: usize, l: u32, n: usize) {
        self.order.splice(at..at, (0..n as u32).map(|k| (l, k)));
        for x in &mut self.pos {
            if *x >= at {
                *x += n;
            }
        }
        self.first.push(self.pos.len());
        self.pos.extend(at..at + n);
        self.vis_a.splice(at..at, std::iter::repeat_n(true, n));
        self.visible.splice(at..at, std::iter::repeat_n(true, n));
        self.sealed.splice(at..at, std::iter::repeat_n(false, n));
        let q = self.visible_index.partition_point(|&x| x < at);
        for x in &mut self.visible_index[q..] {
            *x += n;
        }
        self.visible_index.splice(q..q, at..at + n);
    }

    /// Hides characters at these order indexes (a text without seals or copies).
    fn hide(&mut self, mut idx: Vec<usize>) {
        idx.retain(|&i| self.visible[i]);
        if idx.is_empty() {
            return;
        }
        idx.sort_unstable();
        for &i in &idx {
            self.vis_a[i] = false;
            self.visible[i] = false;
        }
        // Both lists are sorted: move the stretches between removed entries down.
        let vi = &mut self.visible_index;
        let len = vi.len();
        let mut w = vi.partition_point(|&x| x < idx[0]);
        let mut r = w;
        for &i in &idx {
            let p = r + vi[r..].partition_point(|&x| x < i);
            if vi.get(p) != Some(&i) {
                continue;
            }
            vi.copy_within(r..p, w);
            w += p - r;
            r = p + 1;
        }
        vi.copy_within(r..len, w);
        vi.truncate(w + len - r);
    }
}

/// A text object.
#[derive(Clone, Debug, Default)]
pub(crate) struct TextObj {
    pub transcript: bool,
    ins: Vec<InsRec>,
    index: HashMap<InsertionId, u32>,
    /// Insertions anchored at `start`, newest first.
    roots: Vec<u32>,
    seals: Vec<Vec<IdRange>>,
    has_copies: bool,
    large: bool,
    layout: OnceLock<Arc<Layout>>,
    /// The visible text, built on first use.
    text: OnceLock<Arc<String>>,
    /// Visible runs for ordinary text; dense indexes stay lazy until requested.
    runs: OnceLock<Vec<(u32, u32, u32)>>,
}

impl TextObj {
    pub fn new(transcript: bool) -> Self {
        TextObj {
            transcript,
            ..TextObj::default()
        }
    }

    pub fn local(&self, c: &CharId) -> Option<Local> {
        let l = *self.index.get(&c.insertion)?;
        ((c.offset as usize) < self.ins[l as usize].chars.len()).then_some((l, c.offset))
    }

    pub fn has_char(&self, c: &CharId) -> bool {
        self.local(c).is_some()
    }

    /// Whether some insertion is a merge copy.
    pub fn has_copies(&self) -> bool {
        self.has_copies
    }

    /// The length of an insertion of this text.
    pub fn insertion_len(&self, id: &InsertionId) -> Option<u32> {
        self.index
            .get(id)
            .map(|l| self.ins[*l as usize].chars.len() as u32)
    }

    /// Whether `r` names existing characters (TL-44).
    pub fn range_valid(&self, r: &IdRange) -> bool {
        self.insertion_len(&r.insertion)
            .is_some_and(|n| r.from < r.to && r.to <= n)
    }

    pub fn char_id(&self, (l, k): Local) -> CharId {
        CharId {
            insertion: self.ins[l as usize].id.clone(),
            offset: k,
        }
    }

    pub fn char_at(&self, (l, k): Local) -> char {
        self.ins[l as usize].chars[k as usize]
    }

    /// The canonical id of a character (TL-72).
    pub fn canonical(&self, (l, k): Local) -> CharId {
        let rec = &self.ins[l as usize];
        match &rec.origin {
            Some(o) => CharId {
                insertion: o.insertion.clone(),
                offset: o.offset + k,
            },
            None => CharId {
                insertion: rec.id.clone(),
                offset: k,
            },
        }
    }

    /// Whether some character of this text has canonical id `c` (TL-72).
    pub fn has_canonical(&self, c: &CharId) -> bool {
        self.ins.iter().any(|rec| {
            let (base, off) = match &rec.origin {
                Some(o) => (&o.insertion, o.offset),
                None => (&rec.id, 0),
            };
            *base == c.insertion && c.offset >= off && c.offset < off + rec.chars.len() as u32
        })
    }

    /// Adds an insertion. The anchor must exist (or be `None`).
    pub fn insert(
        &mut self,
        id: InsertionId,
        anchor: Option<&CharId>,
        text: &str,
        origin: Option<CharId>,
    ) {
        self.text = OnceLock::new();
        self.runs = OnceLock::new();
        let anchor = anchor.and_then(|a| self.local(a));
        let chars: Vec<char> = text.chars().collect();
        let l = self.ins.len() as u32;
        self.has_copies |= origin.is_some();
        let n = chars.len();
        self.large |= n >= 4096;
        self.ins.push(InsRec {
            id: id.clone(),
            chars: Arc::new(chars),
            ascii: text.is_ascii().then(|| Arc::from(text)),
            hidden: Vec::new(),
            origin,
            deleted: vec![false; n],
            kids: Vec::new(),
        });
        self.index.insert(id, l);
        // Newest first: keep each list of siblings sorted by descending insertion id.
        let ins = &self.ins;
        let newer = |s: &u32| ins[*s as usize].id > ins[l as usize].id;
        let place = match anchor {
            None => Err(self.roots.partition_point(newer)),
            Some((al, ak)) => {
                let kids = &ins[al as usize].kids;
                Ok(match kids.binary_search_by_key(&ak, |(o, _)| *o) {
                    Ok(i) => (i, true, kids[i].1.partition_point(newer)),
                    Err(i) => (i, false, 0),
                })
            }
        };
        // A new newest child goes right after its anchor in the order (TL-43): in a text
        // without seals or copies the layout is patched instead of rebuilt.
        let newest = match place {
            Err(at) => at == 0,
            Ok((_, found, at)) => !found || at == 0,
        };
        match (place, anchor) {
            (Err(at), _) => self.roots.insert(at, l),
            (Ok((i, found, at)), Some((al, ak))) => {
                let kids = &mut self.ins[al as usize].kids;
                if found {
                    kids[i].1.insert(at, l);
                } else {
                    kids.insert(i, (ak, vec![l]));
                }
            }
            (Ok(_), None) => {}
        }
        if newest
            && !self.has_copies
            && self.seals.is_empty()
            && n > 0
            && let Some(lay) = self.layout.get_mut()
        {
            let at = anchor.map_or(0, |a| lay.pos(a) + 1);
            Arc::make_mut(lay).insert_run(at, l, n);
            return;
        }
        self.layout = OnceLock::new();
    }

    pub fn delete(&mut self, r: &IdRange) {
        self.text = OnceLock::new();
        self.runs = OnceLock::new();
        if let Some(&l) = self.index.get(&r.insertion) {
            let rec = &mut self.ins[l as usize];
            let to = (r.to as usize).min(rec.deleted.len());
            let from = (r.from as usize).min(to);
            if from < to {
                let mut range = (from as u32, to as u32);
                let start = rec.hidden.partition_point(|&(_, end)| end < range.0);
                let mut end = start;
                while end < rec.hidden.len() && rec.hidden[end].0 <= range.1 {
                    range.0 = range.0.min(rec.hidden[end].0);
                    range.1 = range.1.max(rec.hidden[end].1);
                    end += 1;
                }
                rec.hidden.splice(start..end, [range]);
            }
            for d in &mut rec.deleted[from..to] {
                *d = true;
            }
            if !self.has_copies
                && self.seals.is_empty()
                && let Some(lay) = self.layout.get_mut()
            {
                let lay = Arc::make_mut(lay);
                let idx = (from..to).map(|k| lay.pos((l, k as u32))).collect();
                lay.hide(idx);
                return;
            }
            self.layout = OnceLock::new();
        }
    }

    pub fn seal(&mut self, ranges: Vec<IdRange>) {
        self.text = OnceLock::new();
        self.runs = OnceLock::new();
        self.seals.push(ranges);
        self.layout = OnceLock::new();
    }

    pub fn layout(&self) -> Arc<Layout> {
        self.layout.get_or_init(|| Arc::new(self.compute())).clone()
    }

    fn compute(&self) -> Layout {
        let mut starts = Vec::with_capacity(self.ins.len());
        let mut total = 0;
        for r in &self.ins {
            starts.push(total);
            total += r.chars.len();
        }
        let mut order = Vec::with_capacity(total);
        let mut vis_a: Vec<bool> = Vec::with_capacity(total);
        let mut pos = vec![0usize; total];
        // Pre-order with an explicit stack: a character, then its children newest first, then
        // the next character of its insertion. Runs between children are emitted with
        // their visibility in one go.
        let mut stack: Vec<Local> = Vec::new();
        let push_children = |stack: &mut Vec<Local>, kids: &[u32]| {
            for &k in kids.iter().rev() {
                if !self.ins[k as usize].chars.is_empty() {
                    stack.push((k, 0));
                }
            }
        };
        push_children(&mut stack, &self.roots);
        while let Some((l, k)) = stack.pop() {
            let rec = &self.ins[l as usize];
            let n = rec.chars.len() as u32;
            let kid = rec.kids.get(rec.kids.partition_point(|(o, _)| *o < k));
            let end = kid.map_or(n - 1, |(o, _)| *o);
            let base = starts[l as usize];
            let from = order.len();
            for (p, i) in pos[base + k as usize..=base + end as usize]
                .iter_mut()
                .zip(from..)
            {
                *p = i;
            }
            order.extend((k..=end).map(|o| (l, o)));
            vis_a.extend(rec.deleted[k as usize..=end as usize].iter().map(|d| !d));
            if end + 1 < n {
                stack.push((l, end + 1));
            }
            if let Some((_, kids)) = kid {
                push_children(&mut stack, kids);
            }
        }

        let n = order.len();
        let mut sealed = vec![false; n];
        let mut in_region = if self.seals.is_empty() {
            Vec::new()
        } else {
            vec![0i32; n + 1]
        };
        let mut seal_spans = Vec::new();
        for ranges in &self.seals {
            let mut first = usize::MAX;
            let mut last = 0usize;
            for r in ranges {
                let Some(&l) = self.index.get(&r.insertion) else {
                    continue;
                };
                for k in r.from..r.to.min(self.ins[l as usize].chars.len() as u32) {
                    let i = pos[starts[l as usize] + k as usize];
                    sealed[i] = true;
                    first = first.min(i);
                    last = last.max(i);
                }
            }
            if first <= last {
                seal_spans.push((first, last));
                if last > first + 1 {
                    in_region[first + 1] += 1;
                    in_region[last] -= 1;
                }
            }
        }
        if !self.seals.is_empty() {
            let mut depth = 0;
            for (i, v) in vis_a.iter_mut().enumerate() {
                depth += in_region[i];
                *v = sealed[i] || (depth == 0 && *v);
            }
        }
        let visible = if self.has_copies {
            // TL-45 (b): the first character visible by (a) of each canonical id. Canonical
            // insertions get small numbers once per insertion, not a hash per character.
            let mut canon: HashMap<&InsertionId, u32> = HashMap::new();
            let mut extent: Vec<u32> = Vec::new();
            let key: Vec<(u32, u32)> = self
                .ins
                .iter()
                .map(|r| {
                    let (id, off) = match &r.origin {
                        Some(o) => (&o.insertion, o.offset),
                        None => (&r.id, 0),
                    };
                    let next = canon.len() as u32;
                    let c = *canon.entry(id).or_insert(next);
                    if c as usize == extent.len() {
                        extent.push(0);
                    }
                    extent[c as usize] = extent[c as usize].max(off + r.chars.len() as u32);
                    (c, off)
                })
                .collect();
            let mut seen: Vec<Vec<bool>> =
                extent.iter().map(|&n| vec![false; n as usize]).collect();
            order
                .iter()
                .enumerate()
                .map(|(i, &(l, k))| {
                    if !vis_a[i] {
                        return false;
                    }
                    let (c, off) = key[l as usize];
                    let slot = &mut seen[c as usize][(off + k) as usize];
                    !std::mem::replace(slot, true)
                })
                .collect()
        } else {
            vis_a.clone()
        };
        let mut visible_index = Vec::with_capacity(n);
        visible_index.extend(
            visible
                .iter()
                .enumerate()
                .filter(|(_, v)| **v)
                .map(|(i, _)| i),
        );
        Layout {
            order,
            pos,
            first: starts,
            vis_a,
            visible,
            visible_index,
            seal_spans,
            sealed,
        }
    }

    /// Large ordinary texts need only runs for rendering and edit planning.
    pub(crate) fn compact_layout(&self) -> bool {
        self.large && !self.has_copies && self.seals.is_empty()
    }

    /// Identity ranges at visible positions, without expanding runs into character indexes.
    pub(crate) fn visible_ranges(&self, from: usize, to: usize) -> Vec<IdRange> {
        let mut out: Vec<IdRange> = Vec::new();
        let mut pos = 0;
        for &(l, a, b) in self.visible_runs() {
            let end = pos + (b - a) as usize;
            let start = from.max(pos);
            let stop = to.min(end);
            if start < stop {
                let insertion = self.ins[l as usize].id.clone();
                let lo = a + (start - pos) as u32;
                let hi = a + (stop - pos) as u32;
                if let Some(last) = out.last_mut()
                    && last.insertion == insertion
                    && last.to == lo
                {
                    last.to = hi;
                } else {
                    out.push(IdRange {
                        insertion,
                        from: lo,
                        to: hi,
                    });
                }
            }
            pos = end;
            if pos >= to {
                break;
            }
        }
        out
    }

    /// Compact TL-43 pre-order, split at children and tombstone intervals. Seals and
    /// canonical-copy visibility use the dense layout instead.
    fn visible_runs(&self) -> &[(u32, u32, u32)] {
        self.runs.get_or_init(|| {
            let mut out = Vec::new();
            let mut stack: Vec<Local> = self.roots.iter().rev().map(|&l| (l, 0)).collect();
            while let Some((l, k)) = stack.pop() {
                let rec = &self.ins[l as usize];
                let n = rec.chars.len() as u32;
                if k == n {
                    continue;
                }
                let kid = rec.kids.get(rec.kids.partition_point(|(o, _)| *o < k));
                let end = kid.map_or(n, |(o, _)| o + 1);
                let mut from = k;
                let first = rec.hidden.partition_point(|&(_, to)| to <= k);
                for &(a, b) in &rec.hidden[first..] {
                    if a >= end {
                        break;
                    }
                    if from < a {
                        out.push((l, from, a));
                    }
                    from = from.max(b);
                    if from >= end {
                        break;
                    }
                }
                if from < end {
                    out.push((l, from, end));
                }
                if end < n {
                    stack.push((l, end));
                }
                if let Some((_, kids)) = kid {
                    stack.extend(kids.iter().rev().map(|&l| (l, 0)));
                }
            }
            out
        })
    }

    /// The visible text (TL-45).
    pub fn text(&self) -> String {
        self.text
            .get_or_init(|| {
                if self.compact_layout() {
                    let runs = self.visible_runs();
                    let capacity = runs
                        .iter()
                        .map(|&(l, from, to)| {
                            let rec = &self.ins[l as usize];
                            if rec.ascii.is_some() {
                                (to - from) as usize
                            } else {
                                rec.chars[from as usize..to as usize]
                                    .iter()
                                    .map(|c| c.len_utf8())
                                    .sum()
                            }
                        })
                        .sum();
                    let mut out = String::with_capacity(capacity);
                    for &(l, from, to) in runs {
                        let rec = &self.ins[l as usize];
                        if let Some(ascii) = &rec.ascii {
                            out.push_str(&ascii[from as usize..to as usize]);
                        } else {
                            out.extend(rec.chars[from as usize..to as usize].iter());
                        }
                    }
                    return Arc::new(out);
                }
                let lay = self.layout();
                let mut out = String::with_capacity(lay.visible_index.len());
                out.extend(
                    lay.visible_index
                        .iter()
                        .map(|&i| self.char_at(lay.order[i])),
                );
                Arc::new(out)
            })
            .as_ref()
            .clone()
    }

    /// The number of visible characters.
    pub fn len(&self) -> usize {
        if self.compact_layout() {
            self.visible_runs()
                .iter()
                .map(|&(_, from, to)| (to - from) as usize)
                .sum()
        } else {
            self.layout().visible_index.len()
        }
    }

    /// The visible character at position `p`.
    pub fn visible_char(&self, mut p: usize) -> Option<Local> {
        if self.compact_layout() {
            for &(l, from, to) in self.visible_runs() {
                let n = (to - from) as usize;
                if p < n {
                    return Some((l, from + p as u32));
                }
                p -= n;
            }
            return None;
        }
        let lay = self.layout();
        lay.visible_index.get(p).map(|&i| lay.order[i])
    }

    /// Makes an anchor at visible position `p` (TL-46).
    pub fn make_anchor(&self, p: usize, bias: Bias) -> Option<Anchor> {
        let len = self.len();
        if p > len {
            return None;
        }
        Some(match bias {
            Bias::Left if p == 0 => Anchor::Start,
            Bias::Left => Anchor::Char(self.char_id(self.visible_char(p - 1)?), Bias::Left),
            Bias::Right if p == len => Anchor::End,
            Bias::Right => Anchor::Char(self.char_id(self.visible_char(p)?), Bias::Right),
        })
    }

    /// Resolves an anchor to a visible position (TL-46); `None` if it names no character here.
    pub fn resolve_anchor(&self, a: &Anchor) -> Option<usize> {
        let lay = self.layout();
        match a {
            Anchor::Start => Some(0),
            Anchor::End => Some(lay.visible_index.len()),
            Anchor::Char(c, bias) => {
                let i = match self.local(c) {
                    Some(l) => lay.pos(l),
                    None => {
                        // Through merge copies of `c`: the first visible copy, else the first.
                        let copies: Vec<usize> = (0..lay.order.len())
                            .filter(|&i| self.canonical(lay.order[i]) == *c)
                            .collect();
                        *copies
                            .iter()
                            .find(|&&i| lay.visible[i])
                            .or(copies.first())?
                    }
                };
                let at_or_before = lay.visible_index.partition_point(|&x| x <= i);
                Some(match bias {
                    Bias::Left => at_or_before,
                    Bias::Right => lay.visible_index.partition_point(|&x| x < i),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{OpId, ReplicaId};

    /// Patched layouts equal rebuilt ones, through inserts (newest or not) and deletes.
    #[test]
    fn patched_layout_equals_a_rebuild() {
        let reps: Vec<Arc<ReplicaId>> = (0..3)
            .map(|i| Arc::new(ReplicaId::new(format!("@r{i}:x.example"), "k", 0)))
            .collect();
        let mut seed = 42u64;
        let mut rnd = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n.max(1)
        };
        for round in 0..20 {
            let mut t = TextObj::new(false);
            let mut ids: Vec<(InsertionId, u32)> = Vec::new();
            let mut counter = 10u64;
            for step in 0..300 {
                let _ = t.layout();
                if !ids.is_empty() && rnd(4) == 0 {
                    let (id, len) = ids[rnd(ids.len())].clone();
                    let from = rnd(len as usize) as u32;
                    let to = from + 1 + rnd((len - from) as usize) as u32;
                    t.delete(&IdRange {
                        insertion: id,
                        from,
                        to,
                    });
                } else {
                    // Mostly newest; sometimes an older, concurrent insertion.
                    counter += 1;
                    let c = if rnd(5) == 0 { counter / 2 } else { counter };
                    let id = InsertionId {
                        op: OpId::new(c, reps[rnd(3)].clone()),
                        index: step,
                    };
                    let anchor = (!ids.is_empty() && rnd(6) != 0).then(|| {
                        let (aid, alen) = ids[rnd(ids.len())].clone();
                        CharId {
                            insertion: aid,
                            offset: rnd(alen as usize) as u32,
                        }
                    });
                    let n = 1 + rnd(4);
                    t.insert(id.clone(), anchor.as_ref(), &"xyzw"[..n], None);
                    ids.push((id, n as u32));
                }
                let patched = t.layout();
                let rebuilt = t.compute();
                assert_eq!(*patched, rebuilt, "round {round} step {step}");
            }
        }
    }

    /// The run-based path for large texts (no seals, no copies) gives exactly what the full
    /// layout gives: text, length, the character at each position, and identity ranges.
    #[test]
    fn compact_runs_equal_the_dense_layout() {
        let reps: Vec<Arc<ReplicaId>> = (0..3)
            .map(|i| Arc::new(ReplicaId::new(format!("@r{i}:x.example"), "k", 0)))
            .collect();
        let mut seed = 7u64;
        let mut rnd = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n.max(1)
        };
        const PIECES: [&str; 6] = [
            "a", "bc", "
", "é", "🙂x", "line
",
        ];
        for round in 0..12 {
            let mut t = TextObj::new(false);
            let mut counter = 10u64;
            let big: String = (0..5000)
                .map(|i| {
                    [
                        "x", "é", "
", "🙂",
                    ][i % 4 * (round % 2)]
                })
                .collect();
            let first = InsertionId {
                op: OpId::new(counter, reps[0].clone()),
                index: 0,
            };
            t.insert(first.clone(), None, &big, None);
            let mut ids: Vec<(InsertionId, u32)> = vec![(first, big.chars().count() as u32)];
            for step in 0..200u32 {
                if rnd(3) == 0 {
                    let (id, len) = ids[rnd(ids.len())].clone();
                    let from = rnd(len as usize) as u32;
                    let to = (from + 1 + rnd(40) as u32).min(len);
                    t.delete(&IdRange {
                        insertion: id,
                        from,
                        to,
                    });
                } else {
                    counter += 1;
                    let c = if rnd(5) == 0 { counter / 2 } else { counter };
                    let id = InsertionId {
                        op: OpId::new(c, reps[rnd(3)].clone()),
                        index: step,
                    };
                    let anchor = (rnd(8) != 0).then(|| {
                        let (aid, alen) = ids[rnd(ids.len())].clone();
                        CharId {
                            insertion: aid,
                            offset: rnd(alen as usize) as u32,
                        }
                    });
                    let text: String = (0..1 + rnd(3)).map(|_| PIECES[rnd(PIECES.len())]).collect();
                    t.insert(id.clone(), anchor.as_ref(), &text, None);
                    ids.push((id, text.chars().count() as u32));
                }
                assert!(t.compact_layout(), "round {round} step {step}");
                let dense = t.compute();
                let at = |p: usize| dense.order[dense.visible_index[p]];
                let want: String = (0..dense.visible_index.len())
                    .map(|p| t.char_at(at(p)))
                    .collect();
                assert_eq!(t.text(), want, "round {round} step {step}: text");
                assert_eq!(
                    t.len(),
                    dense.visible_index.len(),
                    "round {round} step {step}: len"
                );
                for _ in 0..5 {
                    let n = dense.visible_index.len();
                    let p = rnd(n + 1);
                    assert_eq!(
                        t.visible_char(p),
                        (p < n).then(|| at(p)),
                        "round {round} step {step}: char {p}"
                    );
                    let q = (p + rnd(30)).min(n);
                    let mut ranges: Vec<IdRange> = Vec::new();
                    for x in p..q {
                        let c = t.char_id(at(x));
                        match ranges.last_mut() {
                            Some(last) if last.insertion == c.insertion && last.to == c.offset => {
                                last.to += 1
                            }
                            _ => ranges.push(IdRange {
                                insertion: c.insertion,
                                from: c.offset,
                                to: c.offset + 1,
                            }),
                        }
                    }
                    assert_eq!(
                        t.visible_ranges(p, q),
                        ranges,
                        "round {round} step {step}: ranges {p}..{q}"
                    );
                }
            }
        }
    }
}
