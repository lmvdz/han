//! Two-message anti-entropy (§15): hello with an inventory, answer with what the peer lacks.

use std::collections::BTreeMap;

use crate::cbor::Cbor;
use crate::error::{Error, Reason, Report};
use crate::history::History;
use crate::ids::{BatchId, MAX_SAFE, ReplicaId};
use crate::store::{Directory, Store};

/// The default and smallest `maxBytes` of a hello (TL-110).
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;
const MIN_MAX_BYTES: u64 = 64 * 1024;
const MAX_INVENTORY_REPLICAS: usize = 4096;
const MAX_INVENTORY_RANGES: usize = 65_536;

/// Per replica, ascending, non-overlapping, non-adjacent inclusive counter ranges (TL-110).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory(pub BTreeMap<ReplicaId, Vec<(u64, u64)>>);

impl Inventory {
    /// Whether some range of `replica` covers `[from, to]` entirely.
    pub fn covers(&self, replica: &ReplicaId, from: u64, to: u64) -> bool {
        self.0
            .get(replica)
            .is_some_and(|rs| rs.iter().any(|&(a, b)| a <= from && to <= b))
    }

    fn to_cbor(&self) -> Cbor {
        Cbor::Array(
            self.0
                .iter()
                .map(|(r, ranges)| {
                    Cbor::Array(vec![
                        Cbor::Array(vec![
                            Cbor::text(r.actor.clone()),
                            Cbor::text(r.key.clone()),
                            Cbor::u(u64::from(r.lane)),
                        ]),
                        Cbor::Array(
                            ranges
                                .iter()
                                .map(|&(a, b)| Cbor::Array(vec![Cbor::u(a), Cbor::u(b)]))
                                .collect(),
                        ),
                    ])
                })
                .collect(),
        )
    }

    fn from_cbor(c: &Cbor) -> Result<Self, Reason> {
        let m = Reason::Malformed;
        let Cbor::Array(items) = c else { return Err(m) };
        if items.len() > MAX_INVENTORY_REPLICAS {
            return Err(Reason::TooLarge);
        }
        let mut out = BTreeMap::new();
        let mut total = 0usize;
        let mut last: Option<ReplicaId> = None;
        for item in items {
            let Cbor::Array(pair) = item else {
                return Err(m);
            };
            let [Cbor::Array(rep), Cbor::Array(ranges)] = pair.as_slice() else {
                return Err(m);
            };
            let [Cbor::Text(actor), Cbor::Text(key), Cbor::Uint(lane)] = rep.as_slice() else {
                return Err(m);
            };
            let lane = u32::try_from(*lane).map_err(|_| m)?;
            let r = ReplicaId::new(actor.clone(), key.clone(), lane);
            if last.as_ref().is_some_and(|l| *l >= r) {
                return Err(m);
            }
            total += ranges.len();
            if total > MAX_INVENTORY_RANGES {
                return Err(Reason::TooLarge);
            }
            let mut rs: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
            for range in ranges {
                let Cbor::Array(ab) = range else {
                    return Err(m);
                };
                let [Cbor::Uint(a), Cbor::Uint(b)] = ab.as_slice() else {
                    return Err(m);
                };
                // Counter 0, reversed, overlapping, out-of-order or adjacent ranges are malformed.
                if *a == 0
                    || a > b
                    || *b > MAX_SAFE
                    || rs.last().is_some_and(|&(_, pb)| *a <= pb + 1)
                {
                    return Err(m);
                }
                rs.push((*a, *b));
            }
            last = Some(r.clone());
            out.insert(r, rs);
        }
        Ok(Inventory(out))
    }
}

/// A hello (TL-110).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// The space.
    pub space: String,
    /// The history.
    pub history: BatchId,
    /// What the sender holds.
    pub inventory: Inventory,
    /// The largest answer the sender wants; `None` is 4 MiB.
    pub max_bytes: Option<u64>,
}

impl Hello {
    /// The deterministic encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut f = vec![
            (Cbor::u(1), Cbor::u(1)),
            (Cbor::u(2), Cbor::text(self.space.clone())),
            (Cbor::u(3), Cbor::Bytes(self.history.0.to_vec())),
            (Cbor::u(4), self.inventory.to_cbor()),
        ];
        if let Some(m) = self.max_bytes {
            f.push((Cbor::u(5), Cbor::u(m)));
        }
        Cbor::Map(f).encode()
    }

    /// Decodes and checks a hello (TL-112).
    pub fn decode(bytes: &[u8]) -> Result<Hello, Reason> {
        let m = Reason::Malformed;
        let c = Cbor::decode(bytes).map_err(|_| m)?;
        let Cbor::Map(entries) = &c else {
            return Err(m);
        };
        if entries.iter().any(|(k, _)| !matches!(k, Cbor::Uint(1..=5))) {
            return Err(m);
        }
        let (space, history) = header(&c)?;
        let inventory = Inventory::from_cbor(c.get(4).ok_or(m)?)?;
        let max_bytes = match c.get(5) {
            None => None,
            Some(Cbor::Uint(n)) if *n >= MIN_MAX_BYTES => Some(*n),
            Some(_) => return Err(m),
        };
        Ok(Hello {
            space,
            history,
            inventory,
            max_bytes,
        })
    }
}

fn header(c: &Cbor) -> Result<(String, BatchId), Reason> {
    let m = Reason::Malformed;
    if c.get(1) != Some(&Cbor::Uint(1)) {
        return Err(m);
    }
    let Some(Cbor::Text(space)) = c.get(2) else {
        return Err(m);
    };
    let Some(Cbor::Bytes(h)) = c.get(3) else {
        return Err(m);
    };
    let h: [u8; 32] = h.as_slice().try_into().map_err(|_| m)?;
    Ok((space.clone(), BatchId(h)))
}

/// An answer (TL-110).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The space.
    pub space: String,
    /// The history.
    pub history: BatchId,
    /// Encoded envelopes, ascending by start, then replica.
    pub envelopes: Vec<Vec<u8>>,
    /// Whether the answer was cut.
    pub more: bool,
}

impl Answer {
    /// The deterministic encoding.
    pub fn encode(&self) -> Vec<u8> {
        Cbor::Map(vec![
            (Cbor::u(1), Cbor::u(1)),
            (Cbor::u(2), Cbor::text(self.space.clone())),
            (Cbor::u(3), Cbor::Bytes(self.history.0.to_vec())),
            (
                Cbor::u(4),
                Cbor::Array(
                    self.envelopes
                        .iter()
                        .map(|e| Cbor::Bytes(e.clone()))
                        .collect(),
                ),
            ),
            (Cbor::u(5), Cbor::Bool(self.more)),
        ])
        .encode()
    }

    /// Decodes an answer; `max_bytes` is what our hello asked for (TL-112).
    pub fn decode(bytes: &[u8], max_bytes: u64) -> Result<Answer, Reason> {
        let m = Reason::Malformed;
        let c = Cbor::decode(bytes).map_err(|_| m)?;
        let Cbor::Map(entries) = &c else {
            return Err(m);
        };
        if entries.len() != 5 {
            return Err(m);
        }
        let (space, history) = header(&c)?;
        let Some(Cbor::Array(items)) = c.get(4) else {
            return Err(m);
        };
        let Some(Cbor::Bool(more)) = c.get(5) else {
            return Err(m);
        };
        let envelopes = items
            .iter()
            .map(|e| match e {
                Cbor::Bytes(b) => Ok(b.clone()),
                _ => Err(m),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if bytes.len() as u64 > max_bytes && envelopes.len() > 1 {
            return Err(Reason::TooLarge);
        }
        Ok(Answer {
            space,
            history,
            envelopes,
            more: *more,
        })
    }
}

/// The encoded size of a byte string of `n` bytes, or the head of an array of `n` items.
fn head_len(n: u64) -> u64 {
    match n {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

impl<S: Store> History<S> {
    /// The inventory of integrated batches: exact ranges per replica (TL-110).
    pub fn inventory(&self) -> Inventory {
        let mut out = BTreeMap::new();
        for (replica, starts) in self.graph.replicas() {
            let mut ranges: Vec<(u64, u64)> = Vec::new();
            for &idx in starts.values() {
                let b = &self.graph.nodes[idx].batch;
                match ranges.last_mut() {
                    Some(last) if last.1 + 1 == b.start => last.1 = b.end(),
                    _ => ranges.push((b.start, b.end())),
                }
            }
            out.insert((**replica).clone(), ranges);
        }
        Inventory(out)
    }

    /// Our hello (TL-110).
    pub fn hello(&self, max_bytes: Option<u64>) -> Hello {
        Hello {
            space: self.space().to_string(),
            history: self.id(),
            inventory: self.inventory(),
            max_bytes,
        }
    }

    /// The answer to a peer's hello: every integrated batch the hello does not cover, in
    /// ascending start, then replica, order, cut to fit (TL-110). The caller checks that the
    /// peer may read the history (TL-113).
    pub fn answer_to(&self, hello: &Hello) -> Result<Answer, Reason> {
        if hello.space != self.space() || hello.history != self.id() {
            return Err(Reason::Malformed);
        }
        let max = hello.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        let mut lacking: Vec<&crate::graph::Node> = self
            .graph
            .nodes
            .iter()
            .filter(|n| {
                !hello
                    .inventory
                    .covers(&n.batch.replica, n.batch.start, n.batch.end())
            })
            .collect();
        lacking.sort_by(|a, b| {
            (a.batch.start, &a.batch.replica).cmp(&(b.batch.start, &b.batch.replica))
        });
        let mut empty = Answer {
            space: self.space().to_string(),
            history: self.id(),
            envelopes: Vec::new(),
            more: false,
        };
        // The fixed part, with an empty array (one head byte).
        let fixed = empty.encode().len() as u64 - 1;
        let mut body = 0u64;
        let mut envelopes = Vec::new();
        for n in &lacking {
            let len = n.envelope.len() as u64;
            let item = head_len(len) + len;
            let total = fixed + head_len(envelopes.len() as u64 + 1) + body + item;
            if total > max && !envelopes.is_empty() {
                empty.more = true;
                break;
            }
            body += item;
            envelopes.push(n.envelope.to_vec());
        }
        empty.envelopes = envelopes;
        Ok(empty)
    }

    /// Answers an encoded hello with an encoded answer.
    pub fn answer(&self, hello: &[u8]) -> Result<Vec<u8>, Reason> {
        let hello = Hello::decode(hello)?;
        Ok(self.answer_to(&hello)?.encode())
    }

    /// Integrates every envelope of an encoded answer (TL-111). Returns the reports and
    /// whether the peer has more (send a new hello then).
    pub fn receive_answer(
        &mut self,
        answer: &[u8],
        max_bytes: Option<u64>,
        dir: &dyn Directory,
    ) -> Result<Result<(Vec<Report>, bool), Reason>, Error> {
        let answer = match Answer::decode(answer, max_bytes.unwrap_or(DEFAULT_MAX_BYTES)) {
            Ok(a) => a,
            Err(r) => return Ok(Err(r)),
        };
        if answer.space != self.space() || answer.history != self.id() {
            return Ok(Err(Reason::Malformed));
        }
        let mut reports = Vec::new();
        for e in &answer.envelopes {
            reports.push(self.receive(e, dir)?);
        }
        Ok(Ok((reports, answer.more)))
    }
}
