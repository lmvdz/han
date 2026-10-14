//! Work timelines on a reading node: a han-timeline [`History`] per watched session, fed by the
//! `han.timeline/1` items of the session's stream, behind the views' [`TimelineSource`] (`Timeline` and
//! `Patches` queries).
//!
//! An item's body is `{history, envelope, actor, key, public_key, timeline?, step?, files}`: one signed batch
//! of the session's worktree history, as the agent's host wrote it.
//!
//! **Key check.** The home delivered the item only after checking that its publisher is the session's actor
//! (T-07), so the envelope's signing key is taken from the item, and accepted only when the envelope's actor
//! is the session's actor. The key is not yet checked against the actor's pinned profile (SR-92): a gap until
//! the node keeps profiles of the actors whose sessions it watches.
//!
//! **Late readers.** Session streams carry no backlog, so a reader that starts watching mid-session holds
//! batches back until their dependencies arrive (the host can republish its envelopes; the batches are
//! idempotent).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use han_core::base64url;
use han_core::json::Value;
use han_timeline::{
    At, BatchId, Directory, Envelope, History, KeyInfo, MemoryStore, ObjectId, ObjectState, Op,
    OpId, Scope, Verdict, Version,
};

use crate::api::{FilePatchRow, NodeError, PatchStatus, TimelinePointRow};
use crate::views::TimelineSource;

/// The profile name of timeline items.
pub const TIMELINE_PROFILE: &str = "han.timeline/1";

/// What one batch was, as the node read it when it arrived.
#[derive(Clone, Debug)]
struct BatchInfo {
    /// The version right after the batch: its last op.
    after: Version,
    /// The timeline it is on, `None` when it creates one.
    timeline: Option<OpId>,
    /// The timeline it creates, if its first op creates one, and that timeline's name.
    creates: Option<(OpId, String)>,
    step: Option<String>,
    files: Vec<String>,
    at: String,
}

#[derive(Clone, Debug)]
struct Point {
    id: String,
    label: String,
    at: String,
    files: u32,
    after: Version,
}

struct SessionTimeline {
    actor: String,
    history: History<MemoryStore>,
    keys: HashMap<String, [u8; 32]>,
    batches: HashMap<BatchId, BatchInfo>,
    session_tl: Option<OpId>,
    fork: Option<Version>,
    points: Vec<Point>,
    /// How many paths the host said it kept out (`excluded` in an item body): the most it ever said.
    excluded: u32,
}

struct Keys<'a> {
    actor: &'a str,
    keys: &'a HashMap<String, [u8; 32]>,
}

impl Directory for Keys<'_> {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        if actor != self.actor {
            return None;
        }
        self.keys.get(key).map(|pk| KeyInfo {
            public_key: *pk,
            valid: true,
        })
    }

    fn may_write(&self, actor: &str, _scope: Scope<'_>) -> bool {
        actor == self.actor
    }
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The work timelines of the sessions this node watches.
#[derive(Default)]
pub struct StreamTimelines {
    sessions: Mutex<BTreeMap<String, SessionTimeline>>,
}

impl StreamTimelines {
    /// No timelines yet.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, SessionTimeline>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Takes one `han.timeline/1` item body of `session`, whose actor is `actor`, delivered at `time`.
    /// Returns whether the session's timeline changed.
    pub fn receive(&self, session: &str, actor: &str, time: &str, body: &Value) -> bool {
        let s = |k: &str| body.get(k).and_then(Value::as_str);
        let (Some(history), Some(env_b64), Some(env_actor), Some(key), Some(pk)) = (
            s("history").and_then(parse_hex32),
            s("envelope"),
            s("actor"),
            s("key"),
            s("public_key"),
        ) else {
            return false;
        };
        if env_actor != actor {
            return false; // only the session's actor writes its timeline
        }
        let Ok(bytes) = base64url::decode(env_b64) else {
            return false;
        };
        let Some(pk) = base64url::decode(pk)
            .ok()
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
        else {
            return false;
        };
        let Ok(env) = Envelope::decode(&bytes) else {
            return false;
        };
        if env.actor != actor || env.key != key {
            return false;
        }
        let Ok((batch, _)) = env.batch() else {
            return false;
        };
        let mut sessions = self.lock();
        let st = match sessions.entry(session.to_owned()) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                let Ok(h) = History::open(MemoryStore::default(), &env.space, BatchId(history))
                else {
                    return false;
                };
                v.insert(SessionTimeline {
                    actor: actor.to_owned(),
                    history: h,
                    keys: HashMap::new(),
                    batches: HashMap::new(),
                    session_tl: None,
                    fork: None,
                    points: Vec::new(),
                    excluded: 0,
                })
            }
        };
        if st.history.id() != BatchId(history) || st.actor != actor {
            return false;
        }
        st.keys.entry(key.to_owned()).or_insert(pk);
        if let Some(n) = body.get("excluded").and_then(Value::as_u64) {
            st.excluded = st.excluded.max(u32::try_from(n).unwrap_or(u32::MAX));
        }
        let creates = match batch.ops.first() {
            Some(Op::TimelineCreate { name, .. }) => Some((batch.op_id(0), name.clone())),
            _ => None,
        };
        let files = body
            .get("files")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        st.batches.insert(
            env.batch_id(),
            BatchInfo {
                after: Version::new(vec![batch.op_id(batch.ops.len().saturating_sub(1))]),
                timeline: batch.timeline.clone(),
                creates,
                step: batch.step.clone(),
                files,
                at: time.to_owned(),
            },
        );
        let dir = Keys {
            actor: &st.actor,
            keys: &st.keys,
        };
        let Ok(report) = st.history.receive(&bytes, &dir) else {
            return false;
        };
        let mut integrated: Vec<BatchId> = Vec::new();
        if report.verdict == Verdict::Integrated
            && let Some(b) = report.batch
        {
            integrated.push(b);
        }
        integrated.extend(
            report
                .others
                .iter()
                .filter(|(_, v)| *v == Verdict::Integrated)
                .map(|(b, _)| *b),
        );
        for b in &integrated {
            let Some(info) = st.batches.get(b).cloned() else {
                continue;
            };
            if let Some((tl, name)) = &info.creates
                && name == "session"
                && info.timeline.is_none()
            {
                st.session_tl = Some(tl.clone());
                st.fork = Some(info.after.clone());
                continue;
            }
            if st.session_tl.is_some() && info.timeline == st.session_tl {
                st.points.push(Point {
                    id: b.to_hex(),
                    label: info.step.clone().unwrap_or_else(|| "edit".into()),
                    at: info.at.clone(),
                    files: info.files.len() as u32,
                    after: info.after.clone(),
                });
            }
        }
        !integrated.is_empty()
    }
}

/// The files of a tree at a version: path → (text, or a blob marker).
fn files_at(h: &History<MemoryStore>, v: &Version) -> Result<BTreeMap<String, String>, NodeError> {
    let state = h
        .state(&At::Version(v.clone()))
        .map_err(|e| NodeError::unavailable(e.to_string()))?;
    let mut out = BTreeMap::new();
    let Some(tree) = state
        .map_value(&ObjectId::Root, "tree")
        .and_then(han_timeline::Value::obj_field)
    else {
        return Ok(out);
    };
    let Some(ObjectState::Map { entries, .. }) = state.object(&ObjectId::Op(tree)) else {
        return Ok(out);
    };
    for (path, values) in entries {
        let Some((_, v)) = values.first() else {
            continue;
        };
        let Some(obj) = v.obj_field() else { continue };
        let content = match state.object(&ObjectId::Op(obj)) {
            Some(ObjectState::Text { text, .. }) => text,
            Some(ObjectState::Register(han_timeline::Value::Text(t))) => format!("-> {t}\n"),
            Some(ObjectState::Register(han_timeline::Value::Map(m))) => format!(
                "(binary {})\n",
                m.get("hash")
                    .and_then(han_timeline::Value::as_text)
                    .unwrap_or("")
            ),
            _ => continue,
        };
        out.insert(path, content);
    }
    Ok(out)
}

/// A unified diff of one file, and its added and removed line counts.
fn unified(path: &str, old: Option<&str>, new: Option<&str>) -> (String, u32, u32) {
    let ol: Vec<&str> = old
        .map(|s| s.split_inclusive('\n').collect())
        .unwrap_or_default();
    let nl: Vec<&str> = new
        .map(|s| s.split_inclusive('\n').collect())
        .unwrap_or_default();
    let ops = lcs_ops(&ol, &nl);
    let mut out = format!(
        "--- {}\n+++ {}\n",
        if old.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".into()
        },
        if new.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".into()
        }
    );
    let (mut added, mut removed) = (0u32, 0u32);
    // Hunks with three lines of context.
    let n = ops.len();
    let changed: Vec<usize> = (0..n).filter(|&i| ops[i].0 != ' ').collect();
    let mut i = 0;
    while i < changed.len() {
        let start = changed[i].saturating_sub(3);
        let mut end = (changed[i] + 4).min(n);
        while i + 1 < changed.len() && changed[i + 1] <= end + 3 {
            i += 1;
            end = (changed[i] + 4).min(n);
        }
        let (mut a, mut c) = (1usize, 1usize);
        for op in &ops[..start] {
            if op.0 != '+' {
                a += 1;
            }
            if op.0 != '-' {
                c += 1;
            }
        }
        let olen = ops[start..end].iter().filter(|o| o.0 != '+').count();
        let nlen = ops[start..end].iter().filter(|o| o.0 != '-').count();
        out.push_str(&format!("@@ -{a},{olen} +{c},{nlen} @@\n"));
        for (k, line) in &ops[start..end] {
            out.push(*k);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
            match k {
                '+' => added += 1,
                '-' => removed += 1,
                _ => {}
            }
        }
        i += 1;
    }
    (out, added, removed)
}

/// A line script: `' '` kept, `'-'` removed, `'+'` added.
fn lcs_ops<'a>(o: &[&'a str], n: &[&'a str]) -> Vec<(char, &'a str)> {
    let mut pre = 0;
    while pre < o.len() && pre < n.len() && o[pre] == n[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < o.len() - pre && suf < n.len() - pre && o[o.len() - 1 - suf] == n[n.len() - 1 - suf]
    {
        suf += 1;
    }
    let (om, nm) = (&o[pre..o.len() - suf], &n[pre..n.len() - suf]);
    let mut out: Vec<(char, &str)> = o[..pre].iter().map(|l| (' ', *l)).collect();
    if om.len().saturating_mul(nm.len()) > 4_000_000 {
        out.extend(om.iter().map(|l| ('-', *l)));
        out.extend(nm.iter().map(|l| ('+', *l)));
    } else {
        let w = nm.len() + 1;
        let mut t = vec![0u32; (om.len() + 1) * w];
        for i in (0..om.len()).rev() {
            for j in (0..nm.len()).rev() {
                t[i * w + j] = if om[i] == nm[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < om.len() || j < nm.len() {
            if i < om.len() && j < nm.len() && om[i] == nm[j] {
                out.push((' ', om[i]));
                i += 1;
                j += 1;
            } else if j < nm.len() && (i == om.len() || t[i * w + j + 1] >= t[(i + 1) * w + j]) {
                out.push(('+', nm[j]));
                j += 1;
            } else {
                out.push(('-', om[i]));
                i += 1;
            }
        }
    }
    out.extend(o[o.len() - suf..].iter().map(|l| (' ', *l)));
    out
}

impl TimelineSource for StreamTimelines {
    fn points(&self, session: &str, at: Option<&str>) -> Result<Vec<TimelinePointRow>, NodeError> {
        let sessions = self.lock();
        // A session whose host has not mirrored anything yet has no points (yet): an empty view that
        // fills in, not an error.
        let Some(st) = sessions.get(session) else {
            return Ok(Vec::new());
        };
        Ok(st
            .points
            .iter()
            .enumerate()
            .filter(|(_, p)| at.is_none_or(|t| p.at.as_str() <= t))
            .map(|(i, p)| TimelinePointRow {
                key: format!("{i:08}"),
                session: session.to_owned(),
                point: p.id.clone(),
                label: p.label.clone(),
                at: p.at.clone(),
                files: p.files,
            })
            .collect())
    }

    fn patches(
        &self,
        session: &str,
        from: Option<&str>,
        to: &str,
    ) -> Result<Vec<FilePatchRow>, NodeError> {
        let sessions = self.lock();
        let st = sessions
            .get(session)
            .ok_or_else(|| NodeError::not_found(format!("no timeline for session {session}")))?;
        let find = |id: &str| {
            st.points
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.after.clone())
                .ok_or_else(|| NodeError::not_found(format!("no point {id}")))
        };
        let to_v = find(to)?;
        let from_v = match from {
            Some(f) => find(f)?,
            None => st
                .fork
                .clone()
                .ok_or_else(|| NodeError::unavailable("the session's base is not here yet"))?,
        };
        let a = files_at(&st.history, &from_v)?;
        let b = files_at(&st.history, &to_v)?;
        let mut paths: Vec<&String> = a.keys().chain(b.keys()).collect();
        paths.sort();
        paths.dedup();
        let mut rows = Vec::new();
        for p in paths {
            let (o, n) = (a.get(p), b.get(p));
            if o == n {
                continue;
            }
            let status = match (o, n) {
                (None, _) => PatchStatus::Added,
                (_, None) => PatchStatus::Deleted,
                _ => PatchStatus::Modified,
            };
            let (patch, added, removed) = unified(p, o.map(String::as_str), n.map(String::as_str));
            rows.push(FilePatchRow {
                key: p.clone(),
                path: p.clone(),
                status,
                patch,
                added,
                removed,
                masked: Vec::new(),
            });
        }
        Ok(rows)
    }

    fn change_set(&self, _space: &str, _proposal: &str) -> Result<Vec<FilePatchRow>, NodeError> {
        Err(NodeError::unavailable(
            "change sets of proposals are read from the executor's plan, not the timeline",
        ))
    }

    fn excluded(&self, session: &str) -> u32 {
        self.lock().get(session).map_or(0, |st| st.excluded)
    }
}

#[cfg(test)]
mod tests {
    use super::unified;

    #[test]
    fn unified_diff_counts_lines() {
        let (p, a, r) = unified("x", Some("1\n2\n3\n"), Some("1\ntwo\n3\n4\n"));
        assert_eq!((a, r), (2, 1), "{p}");
        assert!(p.contains("@@ -1,3 +1,4 @@"), "{p}");
        let (p, a, r) = unified("y", None, Some("new\n"));
        assert_eq!((a, r), (1, 0));
        assert!(p.starts_with("--- /dev/null\n+++ b/y\n"), "{p}");
    }
}
