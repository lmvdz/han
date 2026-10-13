//! The text CRDT (TL-40 – TL-48) with seals (TL-50 – TL-53) and merge copies (TL-45, TL-72).
//!
//! Characters live in insertions. The order of all characters is the pre-order of the tree in
//! which a character's parent is the previous character of its insertion, or the insertion's
//! anchor for its first character, with children newest first (TL-43). Deletes are tombstones.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::ids::{CharId, IdRange, InsertionId};

/// A character inside one text: local insertion index and offset.
pub(crate) type Local = (u32, u32);

#[derive(Clone, Debug)]
struct InsRec {
    id: InsertionId,
    chars: Vec<char>,
    /// For a copy, the canonical character its first character came from.
    origin: Option<CharId>,
    deleted: Vec<bool>,
    /// The offsets of this insertion's characters that have children, ascending.
    kids: Vec<u32>,
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
#[derive(Debug)]
pub(crate) struct Layout {
    /// Every character, visible or not, in order.
    pub order: Vec<Local>,
    /// Order index of each character, per local insertion.
    pub pos: Vec<Vec<usize>>,
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

/// A text object.
#[derive(Clone, Debug, Default)]
pub(crate) struct TextObj {
    pub transcript: bool,
    ins: Vec<InsRec>,
    index: HashMap<InsertionId, u32>,
    /// Children of a character (or of `start`): local insertions, newest first.
    children: HashMap<Option<Local>, Vec<u32>>,
    seals: Vec<Vec<IdRange>>,
    has_copies: bool,
    layout: OnceLock<Arc<Layout>>,
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
        let anchor = anchor.and_then(|a| self.local(a));
        let chars: Vec<char> = text.chars().collect();
        let l = self.ins.len() as u32;
        self.has_copies |= origin.is_some();
        let n = chars.len();
        self.ins.push(InsRec {
            id: id.clone(),
            chars,
            origin,
            deleted: vec![false; n],
            kids: Vec::new(),
        });
        self.index.insert(id, l);
        if let Some((al, ak)) = anchor {
            let kids = &mut self.ins[al as usize].kids;
            if let Err(at) = kids.binary_search(&ak) {
                kids.insert(at, ak);
            }
        }
        let ins = &self.ins;
        let siblings = self.children.entry(anchor).or_default();
        // Newest first: keep the list sorted by descending insertion id.
        let at = siblings.partition_point(|s| ins[*s as usize].id > ins[l as usize].id);
        siblings.insert(at, l);
        self.layout = OnceLock::new();
    }

    pub fn delete(&mut self, r: &IdRange) {
        if let Some(&l) = self.index.get(&r.insertion) {
            let rec = &mut self.ins[l as usize];
            let to = (r.to as usize).min(rec.deleted.len());
            for d in &mut rec.deleted[(r.from as usize).min(to)..to] {
                *d = true;
            }
            self.layout = OnceLock::new();
        }
    }

    pub fn seal(&mut self, ranges: Vec<IdRange>) {
        self.seals.push(ranges);
        self.layout = OnceLock::new();
    }

    pub fn layout(&self) -> Arc<Layout> {
        self.layout.get_or_init(|| Arc::new(self.compute())).clone()
    }

    fn compute(&self) -> Layout {
        let total: usize = self.ins.iter().map(|r| r.chars.len()).sum();
        let mut order = Vec::with_capacity(total);
        let mut pos: Vec<Vec<usize>> = self.ins.iter().map(|r| vec![0; r.chars.len()]).collect();
        // Pre-order with an explicit stack: a character, then its children newest first, then
        // the next character of its insertion. Runs of characters without children are
        // emitted in one go.
        let mut stack: Vec<Local> = Vec::new();
        let push_children = |stack: &mut Vec<Local>, slot: Option<Local>| {
            if let Some(kids) = self.children.get(&slot) {
                for &k in kids.iter().rev() {
                    if !self.ins[k as usize].chars.is_empty() {
                        stack.push((k, 0));
                    }
                }
            }
        };
        push_children(&mut stack, None);
        while let Some((l, k)) = stack.pop() {
            let rec = &self.ins[l as usize];
            let n = rec.chars.len() as u32;
            let next_kid = rec.kids.get(rec.kids.partition_point(|&o| o < k)).copied();
            let end = next_kid.unwrap_or(n - 1);
            for o in k..=end {
                pos[l as usize][o as usize] = order.len();
                order.push((l, o));
            }
            if end + 1 < n {
                stack.push((l, end + 1));
            }
            if next_kid.is_some() {
                push_children(&mut stack, Some((l, end)));
            }
        }

        let n = order.len();
        let mut sealed = vec![false; n];
        let mut in_region = vec![0i32; n + 1];
        let mut seal_spans = Vec::new();
        for ranges in &self.seals {
            let mut first = usize::MAX;
            let mut last = 0usize;
            for r in ranges {
                let Some(&l) = self.index.get(&r.insertion) else {
                    continue;
                };
                for k in r.from..r.to.min(self.ins[l as usize].chars.len() as u32) {
                    let i = pos[l as usize][k as usize];
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
        let mut vis_a = vec![false; n];
        let mut depth = 0;
        for (i, &(l, k)) in order.iter().enumerate() {
            depth += in_region[i];
            vis_a[i] = sealed[i] || (depth == 0 && !self.ins[l as usize].deleted[k as usize]);
        }
        let visible = if self.has_copies {
            let mut seen: HashSet<CharId> = HashSet::new();
            order
                .iter()
                .enumerate()
                .map(|(i, &c)| vis_a[i] && seen.insert(self.canonical(c)))
                .collect()
        } else {
            vis_a.clone()
        };
        let visible_index = (0..n).filter(|&i| visible[i]).collect();
        Layout {
            order,
            pos,
            vis_a,
            visible,
            visible_index,
            seal_spans,
            sealed,
        }
    }

    /// The visible text (TL-45).
    pub fn text(&self) -> String {
        let lay = self.layout();
        lay.visible_index
            .iter()
            .map(|&i| self.char_at(lay.order[i]))
            .collect()
    }

    /// The number of visible characters.
    pub fn len(&self) -> usize {
        self.layout().visible_index.len()
    }

    /// The visible character at position `p`.
    pub fn visible_char(&self, p: usize) -> Option<Local> {
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
                    Some((l, k)) => lay.pos[l as usize][k as usize],
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
