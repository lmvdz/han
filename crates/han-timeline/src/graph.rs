//! The causal graph of integrated batches: op lookup, closures, happens-before, frontiers,
//! timelines and heads (TL-12 – TL-14, TL-21 – TL-24, TL-70).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::ids::{OpId, Replica, Version};
use crate::op::{Batch, Op};

/// A position in the graph: batch index and op offset.
pub(crate) type Pos = (usize, u32);

/// One integrated batch.
#[derive(Debug)]
pub(crate) struct Node {
    pub batch: Batch,
    /// The parents of the first op, resolved.
    pub parents: Vec<Pos>,
    /// The timeline every op of the batch is on (TL-21).
    pub timeline: OpId,
    /// Merge ops of the batch: offset and resolved `incoming` heads (for extended closures).
    pub merges: Vec<(u32, Vec<Pos>)>,
    /// The envelope bytes as received (TL-121).
    pub envelope: Arc<[u8]>,
    /// The decompressed batch bytes, to tell duplicates from equivocations (TL-104).
    pub raw: Arc<[u8]>,
}

/// A set of ops closed under parents, as an op-count prefix per batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Closure(pub Vec<u32>);

impl Closure {
    pub fn contains(&self, (b, o): Pos) -> bool {
        self.0.get(b).is_some_and(|&n| o < n)
    }

    pub fn intersect(&self, other: &Closure) -> Closure {
        Closure(
            self.0
                .iter()
                .zip(other.0.iter())
                .map(|(a, b)| (*a).min(*b))
                .collect(),
        )
    }

    pub fn is_subset(&self, other: &Closure) -> bool {
        self.0
            .iter()
            .enumerate()
            .all(|(i, n)| *n <= other.0.get(i).copied().unwrap_or(0))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub nodes: Vec<Node>,
    by_replica: HashMap<Replica, BTreeMap<u64, usize>>,
    /// Timeline id → derived head (TL-24).
    heads: BTreeMap<OpId, Vec<Pos>>,
}

impl Graph {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The position of an integrated op.
    pub fn locate(&self, id: &OpId) -> Option<Pos> {
        let starts = self.by_replica.get(&id.replica)?;
        let (&start, &idx) = starts.range(..=id.counter).next_back()?;
        let n = self.nodes[idx].batch.ops.len() as u64;
        (id.counter < start + n).then(|| (idx, (id.counter - start) as u32))
    }

    pub fn op_id(&self, (b, o): Pos) -> OpId {
        self.nodes[b].batch.op_id(o as usize)
    }

    pub fn op(&self, (b, o): Pos) -> &Op {
        &self.nodes[b].batch.ops[o as usize]
    }

    fn counter(&self, (b, o): Pos) -> u64 {
        self.nodes[b].batch.start + u64::from(o)
    }

    /// The integrated batches of `replica` whose counter range intersects `[from, to]`.
    pub fn batches_in_range(&self, replica: &Replica, from: u64, to: u64) -> Vec<usize> {
        let Some(starts) = self.by_replica.get(replica) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some((_, &i)) = starts.range(..from).next_back()
            && self.nodes[i].batch.end() >= from
        {
            out.push(i);
        }
        out.extend(starts.range(from..=to).map(|(_, &i)| i));
        out
    }

    /// Every replica with integrated batches, and their starts.
    pub fn replicas(&self) -> impl Iterator<Item = (&Replica, &BTreeMap<u64, usize>)> {
        self.by_replica.iter()
    }

    /// The parents of an op: the batch's parents for its first op, else the previous op.
    pub fn op_parents(&self, (b, o): Pos) -> Vec<Pos> {
        if o == 0 {
            self.nodes[b].parents.clone()
        } else {
            vec![(b, o - 1)]
        }
    }

    pub fn push(&mut self, node: Node) -> usize {
        let idx = self.nodes.len();
        let last = (idx, node.batch.ops.len() as u32 - 1);
        self.by_replica
            .entry(node.batch.replica.clone())
            .or_default()
            .insert(node.batch.start, idx);
        let timeline = node.timeline.clone();
        let creates = node.batch.timeline.is_none();
        self.nodes.push(node);
        // The head of the timeline: the new last op replaces whatever it has in its past.
        let mut head = if creates {
            Vec::new()
        } else {
            self.heads.get(&timeline).cloned().unwrap_or_default()
        };
        head.retain(|h| !self.happens_before_eq(*h, last));
        head.push(last);
        self.heads.insert(timeline, head);
        idx
    }

    /// The head of a timeline as positions.
    pub fn head(&self, timeline: &OpId) -> Option<&[Pos]> {
        self.heads.get(timeline).map(Vec::as_slice)
    }

    pub fn timelines(&self) -> impl Iterator<Item = &OpId> {
        self.heads.keys()
    }

    pub fn version_of(&self, positions: &[Pos]) -> Version {
        Version::new(positions.iter().map(|p| self.op_id(*p)).collect())
    }

    /// Whether `a` is in the causal past of `b` (or equal).
    pub fn happens_before_eq(&self, a: Pos, b: Pos) -> bool {
        if a.0 == b.0 {
            return a.1 <= b.1;
        }
        let target = self.counter(a);
        let mut seen: HashSet<usize> = HashSet::from([b.0]);
        let mut stack: Vec<usize> = vec![b.0];
        while let Some(n) = stack.pop() {
            for &(pb, po) in &self.nodes[n].parents {
                if pb == a.0 && po >= a.1 {
                    return true;
                }
                // Counters fall along parent edges, so nothing below `a`'s counter can reach it.
                if self.counter((pb, po)) > target && seen.insert(pb) {
                    stack.push(pb);
                }
            }
        }
        false
    }

    /// The closure of `heads`; with `extended`, the extended closure of TL-70.
    pub fn closure(&self, heads: &[Pos], extended: bool) -> Closure {
        let mut prefix = vec![0u32; self.nodes.len()];
        let mut stack: Vec<Pos> = heads.to_vec();
        while let Some((b, o)) = stack.pop() {
            let old = prefix[b];
            if o < old {
                continue;
            }
            prefix[b] = o + 1;
            let node = &self.nodes[b];
            if old == 0 {
                stack.extend(node.parents.iter().copied());
            }
            if extended {
                for (off, incoming) in &node.merges {
                    if *off >= old && *off <= o {
                        stack.extend(incoming.iter().copied());
                    }
                }
            }
        }
        Closure(prefix)
    }

    /// The frontier of a closed set (TL-13).
    pub fn frontier(&self, c: &Closure) -> Vec<Pos> {
        let mut dominated: Vec<bool> = vec![false; self.nodes.len()];
        for (b, &n) in c.0.iter().enumerate() {
            if n == 0 {
                continue;
            }
            for &(pb, po) in &self.nodes[b].parents {
                if c.0[pb] == po + 1 {
                    dominated[pb] = true;
                }
            }
        }
        c.0.iter()
            .enumerate()
            .filter(|(b, n)| **n > 0 && !dominated[*b])
            .map(|(b, n)| (b, n - 1))
            .collect()
    }

    /// The ops of a closure not in `below`, in ascending op id order (a causal order, TL-63).
    pub fn ops_between(&self, c: &Closure, below: Option<&Closure>) -> Vec<Pos> {
        let mut out = Vec::new();
        for (b, &n) in c.0.iter().enumerate() {
            let from = below.map_or(0, |x| x.0.get(b).copied().unwrap_or(0));
            out.extend((from..n).map(|o| (b, o)));
        }
        out.sort_by_cached_key(|p| self.op_id(*p));
        out
    }

    /// Whether `op` is on timeline `t` (TL-21).
    pub fn is_on(&self, (b, _): Pos, t: &OpId) -> bool {
        self.nodes[b].timeline == *t
    }
}
