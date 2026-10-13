//! Blobs (G-01, G-02), session streams (G-05, T-07) and invite tickets (N-03) on a [`Home`].

use std::collections::{HashMap, HashSet};

use han_core::json::{self, Value};
use han_core::policy::{Role, can_read, session_access};
use han_core::types::BlobRef;
use han_core::{base64url, grammar, ids, limits};

use crate::calls::{Feed, MAX_BLOB_PART_BYTES, MAX_BLOB_RANGE_BYTES};
use crate::error::{Result, WireError};
use crate::fanout::SubId;
use crate::home::{Home, Inner};
use crate::jv;
use crate::store::{Store, StoreResult};
use crate::stream_item;
use crate::time::parse_time;

/// Unfinished uploads one actor may hold at a time.
const MAX_OPEN_UPLOADS: usize = 16;
/// Bytes of unfinished uploads one actor may hold.
const MAX_UPLOAD_BYTES: usize = 256 << 20;
/// An uploader may read its own unreferenced blob this long (D3).
const OWN_BLOB_MS: u64 = 86_400_000;
/// An invite lasts at most this long (N-03).
const MAX_INVITE_SECS: i64 = 7 * 86_400;

const NS_BLOB: &str = "blob";
const NS_UPLOAD: &str = "upload";
const NS_SUPPLIED: &str = "supplied";
const NS_STREAM: &str = "stream";
const NS_CONTEXT: &str = "ctxblob";
const NS_INVITE: &str = "invite";

/// What a home keeps about blobs.
#[derive(Default)]
pub(crate) struct BlobState {
    /// Hashes whose bytes are in the store.
    held: HashSet<String>,
    /// hash -> uploader -> when its own `blobPut` calls completed it (ms).
    uploads: HashMap<String, HashMap<String, u64>>,
    /// `(space, hash)`: bytes this home may serve for that space (supply, G-01).
    supplied: HashSet<(String, String)>,
    /// `(hash, uploader)` -> the contiguous bytes held so far.
    parts: HashMap<(String, String), Vec<u8>>,
    /// `(hash, uploader)` -> the `size` and `type` the first part named.
    part_refs: HashMap<(String, String), (u64, String)>,
}

impl BlobState {
    pub(crate) fn load(&mut self, store: &dyn Store) -> StoreResult<()> {
        for (key, _) in store.list(NS_BLOB)? {
            self.held.insert(key);
        }
        for (key, value) in store.list(NS_UPLOAD)? {
            if let (Some((hash, actor)), Ok(at)) = (
                key.split_once('|'),
                String::from_utf8_lossy(&value).parse::<u64>(),
            ) {
                self.uploads
                    .entry(hash.to_owned())
                    .or_default()
                    .insert(actor.to_owned(), at);
            }
        }
        for (key, _) in store.list(NS_SUPPLIED)? {
            if let Some((space, hash)) = key.split_once('|') {
                self.supplied.insert((space.to_owned(), hash.to_owned()));
            }
        }
        Ok(())
    }

    fn uploaded_by(&self, hash: &str, actor: &str) -> bool {
        self.uploads
            .get(hash)
            .is_some_and(|m| m.contains_key(actor))
    }
}

/// What a home keeps about session streams.
#[derive(Default)]
pub(crate) struct StreamState {
    /// `(space, session)` -> the last accepted item and its `n`.
    last: HashMap<(String, String), (u64, Value)>,
    /// `(space, hash)` -> the sessions that streamed a `context` item naming that blob.
    context: HashMap<(String, String), Vec<String>>,
}

impl StreamState {
    pub(crate) fn load(&mut self, store: &dyn Store) -> StoreResult<()> {
        for (key, value) in store.list(NS_STREAM)? {
            let Some((space, session)) = key.split_once('|') else {
                continue;
            };
            if let Ok(v) = json::parse(&value)
                && let (Some(n), item) = (v["n"].as_u64(), v["item"].clone())
            {
                self.last
                    .insert((space.to_owned(), session.to_owned()), (n, item));
            }
        }
        for (key, _) in store.list(NS_CONTEXT)? {
            let mut parts = key.splitn(3, '|');
            if let (Some(space), Some(hash), Some(session)) =
                (parts.next(), parts.next(), parts.next())
            {
                self.context
                    .entry((space.to_owned(), hash.to_owned()))
                    .or_default()
                    .push(session.to_owned());
            }
        }
        Ok(())
    }
}

/// Parses a `BlobRef` (section 2): `hash`, `size` and `type`, and optionally a `uri` of at most 1,024 bytes.
pub(crate) fn parse_blobref(v: &Value) -> Option<BlobRef> {
    let r: BlobRef = json::from_value(v).ok()?;
    (grammar::is_hex(&r.hash)
        && r.size <= limits::MAX_BLOB_BYTES
        && grammar::is_media_type(&r.media_type)
        && r.uri.as_ref().is_none_or(|u| u.len() <= 1024))
    .then_some(r)
}

/// Every `BlobRef` a record references in its unredacted content (G-01).
pub(crate) fn blob_refs(kind: &str, content: Option<&Value>) -> Vec<BlobRef> {
    let Some(content) = content else {
        return Vec::new();
    };
    let mut out: Vec<BlobRef> = content
        .get("blobs")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(parse_blobref).collect())
        .unwrap_or_default();
    let extra = match kind {
        "effect.propose" => content.get("plan").and_then(|p| p.get("argsBlob")),
        "effect.outcome" | "effect.resolve" => content.get("evidence"),
        _ => None,
    };
    out.extend(extra.and_then(parse_blobref));
    out
}

impl Home {
    /// G-01: the blobs an ordered record's author supplied are supplied for its space.
    pub(crate) fn supply_from_record(&self, g: &mut Inner, record: &han_core::LogRecord) {
        for r in blob_refs(&record.entry.kind, record.content.as_ref()) {
            if g.blobs.uploaded_by(&r.hash, &record.entry.author) {
                self.mark_supplied(g, &record.entry.space, &r.hash);
            }
        }
    }

    fn mark_supplied(&self, g: &mut Inner, space: &str, hash: &str) {
        if g.blobs.supplied.insert((space.to_owned(), hash.to_owned())) {
            let _ = self
                .store
                .put(NS_SUPPLIED, &format!("{space}|{hash}"), b"1");
        }
    }

    /// `blobPut` (G-01): one part of an upload. Completion counts only this uploader's own bytes, and they
    /// must hash-match.
    pub(crate) fn blob_put(
        &self,
        who: &str,
        r: &BlobRef,
        offset: u64,
        data: &[u8],
    ) -> Result<Value> {
        if data.len() > MAX_BLOB_PART_BYTES {
            return Err(WireError::invalid("blobPut part over maxBlobPartBytes"));
        }
        if offset + data.len() as u64 > r.size {
            return Err(WireError::invalid("bytes beyond ref.size"));
        }
        let mut g = self.lock();
        if g.blobs.uploaded_by(&r.hash, who) {
            return Ok(have(r.size, true));
        }
        let key = (r.hash.clone(), who.to_owned());
        let b = &mut g.blobs;
        if !b.parts.contains_key(&key) {
            let mine = b.parts.keys().filter(|(_, a)| a == who).count();
            let bytes: usize = b
                .parts
                .iter()
                .filter(|((_, a), _)| a == who)
                .map(|(_, v)| v.len())
                .sum();
            if mine >= MAX_OPEN_UPLOADS || bytes >= MAX_UPLOAD_BYTES {
                return Err(WireError::limit("too many uploads in progress", 1000));
            }
        }
        let first = (r.size, r.media_type.clone());
        if *b.part_refs.entry(key.clone()).or_insert(first.clone()) != first {
            return Err(WireError::invalid("ref.size and ref.type must repeat"));
        }
        let buf = b.parts.entry(key.clone()).or_default();
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset > buf.len() {
            return Err(WireError::invalid(format!(
                "gap: part starts after the bytes held ({})",
                buf.len()
            )));
        }
        let overlap = (buf.len() - offset).min(data.len());
        if buf[offset..offset + overlap] != data[..overlap] {
            return Err(WireError::invalid("part differs from bytes already held"));
        }
        buf.extend_from_slice(&data[overlap..]);
        let held = buf.len() as u64;
        if held < r.size {
            return Ok(have(held, false));
        }
        let bytes = b.parts.remove(&key).unwrap_or_default();
        b.part_refs.remove(&key);
        if ids::sha256_hex(&bytes) != r.hash {
            return Err(WireError::invalid("bytes do not hash to ref.hash")); // the parts are discarded
        }
        let at = self.clock.now_ms();
        if self.store.put(NS_BLOB, &r.hash, &bytes).is_err()
            || self
                .store
                .put(
                    NS_UPLOAD,
                    &format!("{}|{who}", r.hash),
                    at.to_string().as_bytes(),
                )
                .is_err()
        {
            return Err(WireError::unavailable("storage failure"));
        }
        g.blobs.held.insert(r.hash.clone());
        g.blobs
            .uploads
            .entry(r.hash.clone())
            .or_default()
            .insert(who.to_owned(), at);
        Ok(have(r.size, true))
    }

    /// `blobGet` (G-01): access follows references, supply follows authors; anything else is `not_found`.
    pub(crate) fn blob_get(
        &self,
        who: &str,
        r: &BlobRef,
        space: &str,
        offset: u64,
        length: Option<u64>,
    ) -> Result<Value> {
        if length == Some(0) {
            return Err(WireError::invalid("length is at least 1"));
        }
        let g = self.lock();
        self.route(&g, space)?;
        let not_found = || WireError::not_found("blob not available"); // never confirms that a blob exists
        let sp = g
            .spaces
            .get(space)
            .ok_or_else(|| WireError::not_found("no such space"))?;
        let state = &sp.state;
        // access (1): a record the caller may read references it, or it is a context blob of a live session
        // the caller may read, or it is the caller's own recent upload
        let referenced = sp.records.iter().any(|rec| {
            !rec.redacted
                && blob_refs(&rec.entry.kind, rec.content.as_ref())
                    .iter()
                    .any(|b| b.hash == r.hash && b.size == r.size)
                && can_read(state, who, rec.receipt.seq)
        });
        let context = g
            .streams
            .context
            .get(&(space.to_owned(), r.hash.clone()))
            .into_iter()
            .flatten()
            .any(|sid| {
                state
                    .sessions
                    .get(sid)
                    .is_some_and(|s| g.blobs.uploaded_by(&r.hash, &s.actor))
                    && session_access(state, who, sid).read
            });
        let own = g
            .blobs
            .uploads
            .get(&r.hash)
            .and_then(|m| m.get(who))
            .is_some_and(|at| self.clock.now_ms().saturating_sub(*at) < OWN_BLOB_MS)
            && can_read(state, who, state.head);
        // supply (2): this home holds the bytes for that space
        let supplied = g
            .blobs
            .supplied
            .contains(&(space.to_owned(), r.hash.clone()));
        if !((referenced && supplied) || context || own) {
            return Err(not_found());
        }
        if !g.blobs.held.contains(&r.hash) {
            return Err(WireError::unavailable("the bytes are missing"));
        }
        let data = self
            .store
            .get(NS_BLOB, &r.hash)
            .map_err(|_| WireError::unavailable("storage failure"))?
            .ok_or_else(|| WireError::unavailable("the bytes are missing"))?;
        let size = data.len() as u64;
        if offset > size {
            return Err(WireError::invalid("offset past the end"));
        }
        let want = length
            .unwrap_or(MAX_BLOB_RANGE_BYTES as u64)
            .min(MAX_BLOB_RANGE_BYTES as u64);
        let end = (offset + want).min(size);
        let slice = &data[usize::try_from(offset).unwrap_or(0)..usize::try_from(end).unwrap_or(0)];
        Ok(jv::obj([
            ("data", jv::s(&base64url::encode(slice))),
            ("size", jv::int(size)),
            ("offset", jv::int(offset)),
        ]))
    }

    /// `stream` (G-05): only the session's actor, on a live session, with `n` one more than its last item.
    pub(crate) fn stream(&self, who: &str, item: &Value) -> Result<()> {
        if let Some(why) = stream_item::verdict(item) {
            return Err(WireError::invalid(format!("StreamItem: {why}")));
        }
        let (space, session) = (
            item["space"].as_str().unwrap_or_default(),
            item["session"].as_str().unwrap_or_default(),
        );
        let mut g = self.lock();
        self.route(&g, space)?;
        let sp = g
            .spaces
            .get(space)
            .ok_or_else(|| WireError::not_found("no such session"))?;
        let sess = sp
            .state
            .sessions
            .get(session)
            .ok_or_else(|| WireError::not_found("no such session"))?;
        if sess.actor != who {
            return Err(WireError::denied("only the session's actor publishes"));
        }
        if !session_access(&sp.state, who, session).watch {
            return Err(WireError::denied("session not live"));
        }
        let state = std::sync::Arc::clone(&sp.state);
        let key = (space.to_owned(), session.to_owned());
        let n = item["n"].as_u64().unwrap_or(0);
        let last = g.streams.last.get(&key).map_or(0, |(n, _)| *n);
        if n == last && g.streams.last.get(&key).is_some_and(|(_, i)| i == item) {
            return Ok(()); // a resend of the last item
        }
        if n != last + 1 {
            return Err(WireError::invalid(format!("n must be {}", last + 1)));
        }
        let record = jv::obj([("n", jv::int(n)), ("item", item.clone())]);
        self.store
            .put(
                NS_STREAM,
                &format!("{space}|{session}"),
                jv::text(&record).as_bytes(),
            )
            .map_err(|_| WireError::unavailable("storage failure"))?;
        g.streams.last.insert(key.clone(), (n, item.clone()));
        let is_context = item["type"].as_str() == Some("context");
        if is_context && let Some(hash) = item["blob"]["hash"].as_str() {
            let sessions = g
                .streams
                .context
                .entry((space.to_owned(), hash.to_owned()))
                .or_default();
            if !sessions.iter().any(|s| s == session) {
                sessions.push(session.to_owned());
                let _ = self
                    .store
                    .put(NS_CONTEXT, &format!("{space}|{hash}|{session}"), b"1");
            }
            if g.blobs.uploaded_by(hash, who) {
                self.mark_supplied(&mut g, space, hash); // supply: the session's actor's own upload
            }
        }
        if let Some(subs) = g.subs.watchers.get_mut(&key) {
            subs.retain(|sub| {
                let acc = session_access(&state, &sub.actor, session);
                if acc.watch && (!is_context || acc.read) {
                    sub.push(item.clone())
                } else {
                    true
                }
            });
        }
        Ok(())
    }

    /// `watch` (G-05): the session's items from now on, while `sessionAccess` grants watch. There is no
    /// initial reply and no replay.
    pub(crate) fn watch(&self, who: &str, conn: u64, space: &str, session: &str) -> Result<Feed> {
        let mut g = self.lock();
        self.route(&g, space)?;
        let sp = g
            .spaces
            .get(space)
            .ok_or_else(|| WireError::not_found("no such session"))?;
        if !sp.state.sessions.contains_key(session) {
            return Err(WireError::not_found("no such session"));
        }
        if !session_access(&sp.state, who, session).watch {
            return Err(WireError::denied("no watch right on this session"));
        }
        let head = sp.state.head;
        let (sub, live) = g.subs.make(who, head, false, conn);
        let id = SubId(sub.id);
        g.subs
            .watchers
            .entry((space.to_owned(), session.to_owned()))
            .or_default()
            .push(sub);
        Ok(Feed {
            items: Vec::new(),
            live: Some((id, live)),
        })
    }

    /// `invite` (N-03): registers the hash of a ticket's secret. Never in the log.
    pub(crate) fn invite(
        &self,
        who: &str,
        space: &str,
        hash: &str,
        expires_at: &str,
    ) -> Result<()> {
        let g = self.lock();
        self.route(&g, space)?;
        let sp = g
            .spaces
            .get(space)
            .ok_or_else(|| WireError::not_found("no such space"))?;
        let may = match sp.state.roles.get(who) {
            Some(Role::Owner) => true,
            Some(Role::Member) => sp.state.policy.members_may_invite,
            _ => false,
        };
        if !may {
            return Err(WireError::denied(
                "only an owner (or a member, if allowed) invites",
            ));
        }
        let (now, until) = (self.now_secs(), parse_time(expires_at));
        let until = until
            .filter(|u| *u > now && *u <= now + MAX_INVITE_SECS)
            .ok_or_else(|| WireError::invalid("expiresAt is at most 7 days ahead"))?;
        self.store
            .put(
                NS_INVITE,
                &format!("{space}|{hash}"),
                until.to_string().as_bytes(),
            )
            .map_err(|_| WireError::unavailable("storage failure"))
    }

    /// `join` (N-03): uses up a known, unused, unexpired secret, then signals the space's owners. Anything
    /// else is `denied` (the caller closes the connection).
    pub(crate) fn join(&self, who: &str, space: &str, secret: &[u8]) -> Result<()> {
        let mut g = self.lock();
        self.route(&g, space)?;
        let sp = g
            .spaces
            .get(space)
            .ok_or_else(|| WireError::not_found("no such space"))?;
        let key = format!("{space}|{}", ids::sha256_hex(secret));
        let live = self
            .store
            .get(NS_INVITE, &key)
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8_lossy(&v).parse::<i64>().ok())
            .is_some_and(|until| until > self.now_secs());
        if !live {
            return Err(WireError::denied("unknown, used or expired ticket"));
        }
        // Used up at once; a failed delete means the secret might be replayed, so refuse.
        match self.store.delete(NS_INVITE, &key) {
            Ok(true) => {}
            _ => return Err(WireError::denied("unknown, used or expired ticket")),
        }
        let state = std::sync::Arc::clone(&sp.state);
        let item = jv::obj([
            ("type", jv::s("signal")),
            ("from", jv::s(&format!("@:{}", self.server))),
            (
                "body",
                jv::obj([("type", jv::s("join")), ("actor", jv::s(who))]),
            ),
        ]);
        if let Some(subs) = g.subs.by_space.get_mut(space) {
            subs.retain(|sub| {
                if sub.signals && state.roles.get(&sub.actor) == Some(&Role::Owner) {
                    sub.push(item.clone())
                } else {
                    true
                }
            });
        }
        Ok(())
    }
}

fn have(n: u64, complete: bool) -> Value {
    jv::obj([("have", jv::int(n)), ("complete", Value::Bool(complete))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_ref_positions() {
        let hash = "ab".repeat(32);
        let r =
            json::parse(format!(r#"{{"hash":"{hash}","size":3,"type":"text/plain"}}"#).as_bytes())
                .unwrap();
        let content = jv::obj([
            ("blobs", Value::Array(vec![r.clone()])),
            ("plan", jv::obj([("argsBlob", r)])),
        ]);
        assert_eq!(blob_refs("message", Some(&content)).len(), 1);
        assert_eq!(blob_refs("effect.propose", Some(&content)).len(), 2);
    }
}
