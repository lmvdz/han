//! The calls of section 3 and G-09 on a [`Home`]: `hello`, `read`, `state`, `subscribe` and `signal`.

use std::sync::Arc;

use han_core::json::{self, Value};
use han_core::policy::{SpaceState, can_read};
use han_core::{PublicKey, grammar, limits};

use crate::auth;
use crate::error::{Code, Result, WireError};
use crate::fanout::{Live, SubId};
use crate::home::{Home, Inner, Space, readable_any};
use crate::identity::PinView;
use crate::jv;
use crate::paging::{self, MapOrder, Offsets, PAGED};

/// `maxFollows`: open streaming calls per connection, and the bound on `knownHeads`.
pub const MAX_FOLLOWS: usize = 256;
/// `maxStateItems`.
pub const MAX_STATE_ITEMS: usize = 1000;
/// `maxBlobPartBytes`.
pub const MAX_BLOB_PART_BYTES: usize = 786_432;
/// `maxBlobRangeBytes`.
pub const MAX_BLOB_RANGE_BYTES: usize = 786_432;
/// `maxStreamItemBytes`.
pub const MAX_STREAM_ITEM_BYTES: usize = 65_536;
/// `maxClockSkewSeconds`.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 60;
/// A signal body, as JCS.
pub const MAX_SIGNAL_BYTES: usize = 65_536;

/// A successful `hello`.
pub(crate) struct Authed {
    /// The `ok` of the reply.
    pub ok: Value,
    /// The node ids that vouch for the actor (N-01).
    pub nodes: Vec<String>,
}

/// The arguments of `read` besides who asks and what space.
#[derive(Clone, Copy)]
pub(crate) struct ReadReq<'a> {
    pub after: u64,
    pub follow: bool,
    pub limit: Option<u64>,
    pub known_heads: Option<&'a Value>,
}

/// What `read`, `subscribe` and `watch` hand back: the backlog and, for a live call, the queue.
pub(crate) struct Feed {
    pub items: Vec<Value>,
    pub live: Option<(SubId, Live)>,
}

/// The selectors of a `state` call that are present, whatever their values (section 3).
#[derive(Default)]
pub(crate) struct StateSel<'a> {
    pub at: Option<&'a Value>,
    pub cursor: Option<&'a Value>,
    pub effect: Option<&'a Value>,
    pub agent: Option<&'a Value>,
}

impl Home {
    pub(crate) fn limits_value(&self, g: &Inner) -> Value {
        jv::obj([
            ("maxFrameBytes", jv::int(g.max_frame_bytes as u64)),
            ("maxHeaderBytes", jv::int(limits::MAX_HEADER_BYTES as u64)),
            ("maxContentBytes", jv::int(limits::MAX_CONTENT_BYTES as u64)),
            ("maxBlobBytes", jv::int(limits::MAX_BLOB_BYTES)),
            ("maxBlobPartBytes", jv::int(MAX_BLOB_PART_BYTES as u64)),
            ("maxBlobRangeBytes", jv::int(MAX_BLOB_RANGE_BYTES as u64)),
            ("maxStreamItemBytes", jv::int(MAX_STREAM_ITEM_BYTES as u64)),
            ("maxFollows", jv::int(MAX_FOLLOWS as u64)),
            ("maxClockSkewSeconds", jv::int(MAX_CLOCK_SKEW_SECONDS)),
            ("maxStateItems", jv::int(MAX_STATE_ITEMS as u64)),
        ])
    }

    /// G-08: authenticates `who` from its profile. `challenge` is the one this connection was issued, `node`
    /// this end's node id on it (`han/1`; `None` on the legacy carrier).
    pub(crate) fn hello(
        &self,
        who: &str,
        key_id: &str,
        proof: &str,
        challenge: &str,
        node: Option<&str>,
        known_heads: Option<&Value>,
    ) -> Result<Authed> {
        let g = self.lock();
        let now = self.now_str();
        let view = self.profile_view(&g, who, &now).ok_or_else(|| {
            if grammar::server_of(who) == Some(self.server.as_str()) {
                WireError::not_found("no such actor")
            } else {
                WireError::unavailable("the profile cannot be fetched now").retry_after(1000)
            }
        })?;
        let denied = || WireError::denied("authentication failed");
        let key = view
            .keys
            .get(key_id)
            .filter(|k| k.status == han_core::policy::KeyStatus::Valid)
            .ok_or_else(denied)?;
        let public = PublicKey::from_base64url(&key.public_key).map_err(|_| denied())?;
        auth::verify_hello(&public, proof, challenge, &self.server, node).map_err(|_| denied())?;
        let heads = self.heads_for(&g, who, known_heads.unwrap_or(&Value::Array(vec![])))?;
        let ok = jv::obj([
            ("limits", self.limits_value(&g)),
            (
                "durability",
                jv::s(match self.durability {
                    han_core::types::Durability::Local => "local",
                    han_core::types::Durability::Replicated => "replicated",
                }),
            ),
            ("time", jv::s(&now)),
            ("knownHeads", Value::Array(heads)),
        ]);
        Ok(Authed {
            ok,
            nodes: self.nodes_for_locked(&g, who),
        })
    }

    /// R-15: the latest receipt of each space the asker named and may read.
    fn heads_for(&self, g: &Inner, who: &str, known: &Value) -> Result<Vec<Value>> {
        let list = known
            .as_array()
            .filter(|l| l.len() <= MAX_FOLLOWS)
            .ok_or_else(|| WireError::invalid("knownHeads"))?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for h in list {
            let Some(sp) = h.get("space").and_then(Value::as_str) else {
                continue;
            };
            let Some(s) = g.spaces.get(sp) else {
                continue;
            };
            if seen.insert(sp) && can_read(&s.state, who, s.state.head) {
                let last = s.records.last().map(|r| &r.receipt);
                if let Some(r) = last {
                    out.push(json::to_value(r).map_err(WireError::from_json)?);
                }
            }
        }
        Ok(out)
    }

    fn held<'a>(&self, g: &'a Inner, space: &str) -> Result<&'a Space> {
        self.route(g, space)?;
        g.spaces
            .get(space)
            .ok_or_else(|| WireError::not_found("no such space"))
    }

    /// `read`: the records after `after` that the reader may read, at most `limit`; with `follow`, the live
    /// records from the head at registration on. Entries only (G-09).
    pub(crate) fn read(
        &self,
        who: &str,
        conn: u64,
        space: &str,
        req: &ReadReq<'_>,
    ) -> Result<Feed> {
        let ReadReq {
            after,
            follow,
            limit,
            known_heads,
        } = *req;
        let limit = limit.unwrap_or(1000);
        if !(1..=1000).contains(&limit) {
            return Err(WireError::invalid("limit"));
        }
        let mut g = self.lock();
        let s = self.held(&g, space)?;
        if !readable_any(&s.state, who) {
            return Err(WireError::not_found("no such space"));
        }
        let mut items = Vec::new();
        if let Some(known) = known_heads {
            let heads = self.heads_for(&g, who, known)?;
            items.push(jv::obj([
                ("type", jv::s("heads")),
                ("heads", Value::Array(heads)),
            ]));
        }
        let state = &s.state;
        let start = usize::try_from(after)
            .unwrap_or(usize::MAX)
            .min(s.records.len());
        for rec in s.records[start..]
            .iter()
            .filter(|r| can_read(state, who, r.receipt.seq))
            .take(usize::try_from(limit).unwrap_or(1000))
        {
            items.push(record_item(rec)?);
        }
        let head = state.head;
        let live = if follow {
            let (sub, live) = g.subs.make(who, head, false, conn);
            let id = SubId(sub.id);
            g.subs
                .by_space
                .entry(space.to_owned())
                .or_default()
                .push(sub);
            Some((id, live))
        } else {
            None
        };
        Ok(Feed { items, live })
    }

    /// `subscribe`: records and signals; with `after`, the records after it replayed atomically with
    /// registration. A profile space gives anyone its records, and its notices only to the actor itself.
    pub(crate) fn subscribe(
        &self,
        who: &str,
        conn: u64,
        space: &str,
        after: Option<u64>,
    ) -> Result<Feed> {
        let mut g = self.lock();
        self.route(&g, space)?;
        let Some(s) = g.spaces.get(space) else {
            return Err(if space.starts_with("profile:") {
                WireError::denied("cannot follow this space")
            } else {
                WireError::not_found("no such space")
            });
        };
        if !can_read(&s.state, who, s.state.head) {
            return Err(WireError::denied("cannot follow this space"));
        }
        let state = &s.state;
        let mut items = Vec::new();
        if let Some(after) = after {
            let start = usize::try_from(after)
                .unwrap_or(usize::MAX)
                .min(s.records.len());
            for rec in s.records[start..]
                .iter()
                .filter(|r| can_read(state, who, r.receipt.seq))
            {
                items.push(record_item(rec)?);
            }
        }
        let head = state.head;
        let own_profile = space == format!("profile:{who}");
        let signals = !space.starts_with("profile:") || own_profile;
        let (sub, live) = g.subs.make(who, head, signals, conn);
        let id = SubId(sub.id);
        if own_profile {
            // G-03: the notices of the actor's own profile go to this subscription, and only to it.
            g.subs
                .notices
                .entry(who.to_owned())
                .or_default()
                .push(sub.duplicate());
        }
        g.subs
            .by_space
            .entry(space.to_owned())
            .or_default()
            .push(sub);
        Ok(Feed {
            items,
            live: Some((id, live)),
        })
    }

    /// Ends a subscription (cancel, or the connection closing).
    pub(crate) fn unsubscribe(&self, id: SubId) {
        self.lock().subs.remove(id.0);
    }

    /// `signal` (T-05, G-04). `actor` is the sender (the relayed `from` when there is one), `relayed_by` the
    /// server actor that relayed it. Signals go to every subscription that may read the space at its head,
    /// except those on the sender's own connection.
    pub(crate) fn signal(
        &self,
        actor: &str,
        conn: u64,
        relayed_by: Option<&str>,
        space: &str,
        body: &Value,
    ) -> Result<()> {
        let mut g = self.lock();
        self.route(&g, space)?;
        if let Some(target) = space.strip_prefix("profile:") {
            let notice = actor.starts_with("@:")
                && body.get("type").and_then(Value::as_str) == Some("notice");
            if !notice {
                return Err(WireError::denied("only server notices on profile spaces"));
            }
            if !g.spaces.contains_key(space) {
                return Err(WireError::not_found("no such profile"));
            }
            let item = signal_item(actor, body);
            if let Some(subs) = g.subs.notices.get_mut(target) {
                subs.retain(|sub| sub.push(item.clone()));
            }
            return Ok(());
        }
        let Some(s) = g.spaces.get(space) else {
            return Err(WireError::not_found(space));
        };
        if let Some(relay) = relayed_by
            && grammar::server_of(actor) != relay.strip_prefix("@:")
        {
            return Err(WireError::denied(
                "a server may relay signals only from its own actors",
            ));
        }
        if !can_read(&s.state, actor, s.state.head) {
            return Err(WireError::denied("cannot read this space"));
        }
        let state = Arc::clone(&s.state);
        let item = signal_item(actor, body);
        if let Some(subs) = g.subs.by_space.get_mut(space) {
            subs.retain(|sub| {
                if sub.signals && sub.conn != conn && can_read(&state, &sub.actor, state.head) {
                    sub.push(item.clone())
                } else {
                    true
                }
            });
        }
        Ok(())
    }

    // ───────────────────────── state ─────────────────────────

    /// The state after record `at`: the chain folded through `at` with the views its receipts pinned.
    fn state_at(&self, s: &Space, at: u64) -> Result<(SpaceState, MapOrder)> {
        let n = usize::try_from(at).unwrap_or(0).min(s.records.len());
        let mut state: Option<SpaceState> = None;
        let mut order = MapOrder::default();
        for (rec, pins) in s.records[..n].iter().zip(&s.pins) {
            let ctx = han_core::policy::Ctx::new(
                &rec.receipt.time,
                pins.iter()
                    .map(|(a, v)| (a.clone(), v.for_policy()))
                    .collect(),
            )?;
            let new = han_core::policy::fold(
                state.as_ref(),
                &han_core::policy::Record::from_log_record(rec)?,
                &ctx,
            )?;
            order.advance(state.as_ref(), &new);
            state = Some(new);
        }
        state
            .map(|st| (st, order))
            .ok_or_else(|| WireError::unavailable("no stored profile pins for this seq"))
    }

    /// `state` (section 3): one page of the `StateView`, or the effect or agent view.
    pub(crate) fn state(&self, who: &str, space: &str, sel: &StateSel<'_>) -> Result<Value> {
        let g = self.lock();
        let s = self.held(&g, space)?;
        let head = s.state.head;
        let n_exclusive = [sel.cursor, sel.effect, sel.agent].iter().flatten().count();
        if n_exclusive > 1 || (sel.at.is_some() && (sel.cursor.is_some() || sel.effect.is_some())) {
            return Err(WireError::invalid(
                "at, cursor, effect and agent exclude each other (at may join agent)",
            ));
        }
        let mut offsets: Offsets = [0; 7];
        let mut at = head;
        if let Some(v) = sel.at {
            at = match v.as_u64() {
                Some(n) if (1..=head).contains(&n) => n,
                _ => return Err(WireError::invalid("at must be 1 <= at <= head")),
            };
        }
        if let Some(v) = sel.cursor {
            let (h, offs) = v
                .as_str()
                .and_then(paging::parse_cursor)
                .filter(|(h, _)| (1..=head).contains(h))
                .ok_or_else(|| WireError::invalid("cursor"))?;
            at = h;
            offsets = offs;
        }
        if sel.effect.is_some_and(|v| v.as_str().is_none()) {
            return Err(WireError::invalid("effect must be an EntryId"));
        }
        if sel
            .agent
            .is_some_and(|v| !v.as_str().is_some_and(grammar::is_actor_id))
        {
            return Err(WireError::invalid("agent must be an ActorId"));
        }
        if !can_read(&s.state, who, at) {
            return Err(if readable_any(&s.state, who) {
                WireError::denied("state needs read rights at that seq")
            } else {
                WireError::not_found("no such space")
            });
        }
        let (state, order) = if at == head {
            (SpaceState::clone(&s.state), s.order.clone())
        } else {
            self.state_at(s, at)?
        };
        let mut view = json::to_value(&state).map_err(WireError::from_json)?;
        if let Value::Object(m) = &mut view {
            m.remove("entries");
        }
        let scalar = |view: &Value| {
            [
                ("space", "space"),
                ("head", "head"),
                ("home", "home"),
                ("policy", "policy"),
            ]
            .into_iter()
            .map(|(k, _)| (k.to_owned(), view[k].clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
        };
        if let Some(Value::Str(id)) = sel.effect {
            let eff = view["effects"]
                .get(id)
                .cloned()
                .ok_or_else(|| WireError::not_found("no such effect"))?;
            let mut one = scalar(&view);
            one.insert("effects".into(), jv::obj([(id.as_str(), eff)]));
            return Ok(jv::obj([("state", Value::Object(one))]));
        }
        if let Some(Value::Str(agent)) = sel.agent {
            let mut one = scalar(&view);
            let guarded = if state.guarded.contains(agent) {
                jv::obj([(agent.as_str(), Value::Bool(true))])
            } else {
                Value::Object(Default::default())
            };
            one.insert("guarded".into(), guarded);
            // Z1: the tasks whose `accepted` lists the agent and whose `guardedFor` isn't empty, which is what
            // `carried` reads, so the reply is a GuardInput as it stands
            let carried = |t: &Value| {
                t["accepted"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(agent)))
                    && t["guardedFor"].as_array().is_some_and(|g| !g.is_empty())
            };
            let tasks = match &view["tasks"] {
                Value::Object(all) => all
                    .iter()
                    .filter(|(_, t)| carried(t))
                    .map(|(id, t)| (id.clone(), t.clone()))
                    .collect(),
                _ => Default::default(),
            };
            one.insert("tasks".into(), Value::Object(tasks));
            let reply = jv::obj([("state", Value::Object(one))]);
            let framed = jv::obj([
                ("id", jv::int(json::MAX_SAFE_INT as u64)),
                ("ok", reply.clone()),
            ]);
            if json::canonicalize(&framed).map_or(usize::MAX, |b| b.len()) > g.max_frame_bytes {
                // an agent in more guarded tasks than a frame holds: the host reads the paged state
                return Err(WireError::new(
                    Code::Limit,
                    "the agent view does not fit a frame: read the paged state",
                ));
            }
            return Ok(reply);
        }
        if sel.cursor.is_some() && (0..PAGED.len()).any(|m| offsets[m] > paging::len_of(&view, m)) {
            return Err(WireError::invalid("cursor offset beyond the map"));
        }
        let page = paging::page(&view, &order, offsets, MAX_STATE_ITEMS, g.max_frame_bytes);
        let mut out = vec![("state", page.state)];
        if page.more {
            out.push(("next", jv::s(&paging::format_cursor(at, &page.offsets))));
        }
        Ok(Value::Object(
            out.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        ))
    }

    /// The profile of a local actor as it stands, for tools and tests.
    pub fn profile_of(&self, actor: &str) -> Option<PinView> {
        let g = self.lock();
        self.profile_view(&g, actor, &self.now_str())
    }
}

fn record_item(rec: &han_core::LogRecord) -> Result<Value> {
    Ok(jv::obj([
        ("type", jv::s("record")),
        ("record", json::to_value(rec).map_err(WireError::from_json)?),
    ]))
}

fn signal_item(from: &str, body: &Value) -> Value {
    jv::obj([
        ("type", jv::s("signal")),
        ("from", jv::s(from)),
        ("body", body.clone()),
    ])
}
