//! Runs every vector of `conformance/timeline-vectors/` (profile `han.timeline/1`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use han_timeline::envelope::{self, Compression, KeySigner, Signer as _};
use han_timeline::{
    Anchor, At, Batch, BatchId, Bias, Change, CharId, Conflict, ConflictKind, ConflictRecord,
    CopyInsert, Directory, History, HistoryKind, IdRange, Insert, InsertionId, KeyInfo,
    MemoryStore, MergeDraft, MergeOp, ObjectId, Op, OpId, ReplicaId, Resolution, ResolutionKind,
    Scope, Value, Verdict, Version,
};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

const SPACE: &str = "s1:a.example";

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/timeline-vectors")
}

fn seed(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn signer_for(r: &ReplicaId) -> KeySigner {
    let name = match (r.actor.as_str(), r.key.as_str()) {
        ("@alice:a.example", "laptop") => "han-timeline-test-alice-laptop",
        ("@bob:b.example", "desk") => "han-timeline-test-bob-desk",
        ("@alice:a.example", "phone") => "han-timeline-test-alice-phone",
        ("@alice:a.example", "tablet") => "han-timeline-test-alice-tablet",
        _ => panic!("no test key for {r}"),
    };
    KeySigner::from_seed(r.actor.clone(), r.key.clone(), seed(name))
}

/// Profiles: every test key; `valid` keys write.
struct TestDir {
    keys: HashMap<(String, String), KeyInfo>,
}

impl Directory for TestDir {
    fn key(&self, actor: &str, key: &str) -> Option<KeyInfo> {
        self.keys
            .get(&(actor.to_string(), key.to_string()))
            .copied()
    }
    fn may_write(&self, _actor: &str, _scope: Scope<'_>) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Vector notation

struct Labels {
    replicas: BTreeMap<String, Arc<ReplicaId>>,
}

impl Labels {
    fn new(v: &Json) -> Self {
        let replicas = v["replicas"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, r)| {
                let r = r.as_array().unwrap();
                (
                    k.clone(),
                    Arc::new(ReplicaId::new(
                        r[0].as_str().unwrap(),
                        r[1].as_str().unwrap(),
                        r[2].as_u64().unwrap() as u32,
                    )),
                )
            })
            .collect();
        Labels { replicas }
    }

    fn label_of(&self, r: &ReplicaId) -> String {
        self.replicas
            .iter()
            .find(|(_, x)| ***x == *r)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| r.to_string())
    }

    fn op(&self, s: &str) -> OpId {
        let (c, r) = s.split_once('@').unwrap();
        OpId::new(c.parse().unwrap(), self.replicas[r].clone())
    }

    fn op_label(&self, id: &OpId) -> String {
        format!("{}@{}", id.counter, self.label_of(&id.replica))
    }

    fn object(&self, j: &Json) -> ObjectId {
        match j.as_str().unwrap() {
            "root" => ObjectId::Root,
            s => ObjectId::Op(self.op(s)),
        }
    }

    fn object_label(&self, o: &ObjectId) -> String {
        match o {
            ObjectId::Root => "root".into(),
            ObjectId::Op(id) => self.op_label(id),
        }
    }

    fn insertion(&self, s: &str) -> InsertionId {
        let (op, i) = s.split_once('#').unwrap();
        InsertionId {
            op: self.op(op),
            index: i.parse().unwrap(),
        }
    }

    fn char_id(&self, s: &str) -> CharId {
        let (ins, k) = s.rsplit_once(':').unwrap();
        CharId {
            insertion: self.insertion(ins),
            offset: k.parse().unwrap(),
        }
    }

    fn anchor(&self, j: &Json) -> Option<CharId> {
        j.as_str().map(|s| self.char_id(s))
    }

    fn range(&self, j: &Json) -> IdRange {
        let a = j.as_array().unwrap();
        IdRange {
            insertion: self.insertion(a[0].as_str().unwrap()),
            from: a[1].as_u64().unwrap() as u32,
            to: a[2].as_u64().unwrap() as u32,
        }
    }

    fn ranges(&self, j: &Json) -> Vec<IdRange> {
        j.as_array()
            .unwrap()
            .iter()
            .map(|r| self.range(r))
            .collect()
    }

    fn version(&self, j: &Json) -> Version {
        Version::new(
            j.as_array()
                .unwrap()
                .iter()
                .map(|s| self.op(s.as_str().unwrap()))
                .collect(),
        )
    }

    fn value(&self, j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => Value::Int(n.as_i64().unwrap()),
            Json::String(s) => Value::Text(s.clone()),
            Json::Array(a) => Value::Array(a.iter().map(|x| self.value(x)).collect()),
            Json::Object(m) => Value::Map(
                m.iter()
                    .map(|(k, v)| {
                        let v = match (k.as_str(), v) {
                            ("obj", Json::String(s)) => {
                                Value::object_ref(&self.op(s)).as_map().unwrap()["obj"].clone()
                            }
                            _ => self.value(v),
                        };
                        (k.clone(), v)
                    })
                    .collect::<BTreeMap<_, _>>(),
            ),
        }
    }

    fn value_json(&self, v: &Value) -> Json {
        match v {
            Value::Null => Json::Null,
            Value::Bool(b) => json!(b),
            Value::Int(i) => json!(i),
            Value::Text(s) => json!(s),
            Value::Bytes(b) => json!(b),
            Value::Array(a) => Json::Array(a.iter().map(|x| self.value_json(x)).collect()),
            Value::Map(m) => Json::Object(
                m.iter()
                    .map(|(k, x)| {
                        let j = match (k.as_str(), Value::Map(m.clone()).obj_field()) {
                            ("obj", Some(id)) => json!(self.op_label(&id)),
                            _ => self.value_json(x),
                        };
                        (k.clone(), j)
                    })
                    .collect(),
            ),
        }
    }

    fn op_json(&self, j: &Json) -> Op {
        let s = |k: &str| j[k].as_str().unwrap().to_string();
        match j["kind"].as_str().unwrap() {
            "timeline.create" => Op::TimelineCreate {
                name: s("name"),
                source: j["source"].as_str().map(|x| self.op(x)),
                history_kind: j
                    .get("historyKind")
                    .map(|k| HistoryKind::parse(k.as_str().unwrap()).unwrap()),
                session: j.get("session").map(|x| x.as_str().unwrap().to_string()),
            },
            "checkpoint" => Op::Checkpoint { label: s("label") },
            "register.create" => Op::RegisterCreate {
                value: self.value(&j["value"]),
            },
            "register.set" => Op::RegisterSet {
                target: self.object(&j["target"]),
                value: self.value(&j["value"]),
            },
            "map.create" => Op::MapCreate {
                tree: j["tree"].as_bool().unwrap(),
            },
            "map.set" => Op::MapSet {
                target: self.object(&j["target"]),
                key: s("key"),
                value: self.value(&j["value"]),
            },
            "map.delete" => Op::MapDelete {
                target: self.object(&j["target"]),
                key: s("key"),
            },
            "text.create" => Op::TextCreate {
                text: s("text"),
                transcript: j["transcript"].as_bool().unwrap(),
            },
            "text.edit" => Op::TextEdit {
                target: self.object(&j["target"]),
                deletes: self.ranges(&j["deletes"]),
                inserts: j["inserts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| Insert {
                        anchor: self.anchor(&i[0]),
                        text: i[1].as_str().unwrap().to_string(),
                    })
                    .collect(),
            },
            "text.seal" => Op::TextSeal {
                target: self.object(&j["target"]),
                ranges: self.ranges(&j["ranges"]),
            },
            "merge" => Op::Merge(Box::new(MergeOp {
                source: self.op(j["source"].as_str().unwrap()),
                incoming: self.version(&j["incoming"]),
                base: self.version(&j["base"]),
                changes: j["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| self.change(c))
                    .collect(),
                conflicts: j["conflicts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| self.conflict_record(c))
                    .collect(),
                effect: j.get("effect").map(|e| e.as_str().unwrap().to_string()),
            })),
            k => panic!("unknown kind {k}"),
        }
    }

    fn change(&self, c: &Json) -> Change {
        let object = self.object(&c["object"]);
        let create = c["create"].as_bool().unwrap();
        match c["type"].as_str().unwrap() {
            "register" => Change::Register {
                object,
                create,
                value: self.value(&c["value"]),
            },
            "map" => Change::Map {
                object,
                create,
                tree: c["tree"].as_bool().unwrap(),
                sets: c["sets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|kv| (kv[0].as_str().unwrap().to_string(), self.value(&kv[1])))
                    .collect(),
                deletes: c["deletes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| k.as_str().unwrap().to_string())
                    .collect(),
            },
            "text" => Change::Text {
                object,
                create,
                transcript: c["transcript"].as_bool().unwrap(),
                deletes: self.ranges(&c["deletes"]),
                inserts: c["inserts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| CopyInsert {
                        anchor: self.anchor(&i[0]),
                        text: i[1].as_str().unwrap().to_string(),
                        origin: i.get(2).and_then(|o| self.anchor(o)),
                    })
                    .collect(),
            },
            t => panic!("unknown change {t}"),
        }
    }

    fn conflict_record(&self, c: &Json) -> ConflictRecord {
        let kind = match c["kind"].as_str().unwrap() {
            "register" => ConflictKind::Register,
            "map" => ConflictKind::Map,
            "text" => ConflictKind::Text,
            "modify_delete" => ConflictKind::ModifyDelete,
            "rename" => ConflictKind::Rename,
            "path" => ConflictKind::Path,
            k => panic!("unknown conflict {k}"),
        };
        let resolution = match &c["resolution"] {
            Json::String(s) if s == "target" => ResolutionKind::Target,
            Json::String(s) if s == "incoming" => ResolutionKind::Incoming,
            _ => ResolutionKind::Given,
        };
        ConflictRecord {
            kind,
            object: self.object(&c["object"]),
            key: c.get("key").and_then(|k| k.as_str()).map(String::from),
            lines: c
                .get("lines")
                .map(|l| (l[0].as_u64().unwrap(), l[1].as_u64().unwrap())),
            resolution,
        }
    }

    fn batch(&self, b: &Json) -> Batch {
        Batch {
            replica: self.replicas[b["replica"].as_str().unwrap()].clone(),
            start: b["start"].as_u64().unwrap(),
            parents: self.version(&b["parents"]),
            timeline: b["timeline"].as_str().map(|t| self.op(t)),
            ops: b["ops"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| self.op_json(o))
                .collect(),
            time: None,
            step: None,
        }
    }

    fn conflict_json(&self, c: &Conflict) -> Json {
        let opt = |v: &Option<Value>| {
            v.as_ref()
                .map_or(Json::Null, |v| json!({"value": self.value_json(v)}))
        };
        match c {
            Conflict::Register {
                object,
                base,
                target,
                incoming,
            } => json!({
                "kind": "register", "object": self.object_label(object),
                "base": base.as_ref().map_or(Json::Null, |v| self.value_json(v)),
                "target": self.value_json(target), "incoming": self.value_json(incoming),
            }),
            Conflict::Map {
                object,
                key,
                base,
                target,
                incoming,
            } => json!({
                "kind": "map", "object": self.object_label(object), "key": key,
                "base": opt(base), "target": opt(target), "incoming": opt(incoming),
            }),
            Conflict::Text(h) => json!({
                "kind": "text", "object": self.object_label(&h.object),
                "lines": [h.lines.0, h.lines.1],
                "base": h.base, "target": h.target, "incoming": h.incoming,
            }),
            Conflict::ModifyDelete {
                object,
                key,
                deleted_by,
            } => json!({
                "kind": "modify_delete", "object": self.object_label(object), "key": key,
                "deleted_by": match deleted_by {
                    han_timeline::Side::Target => "target",
                    han_timeline::Side::Incoming => "incoming",
                },
            }),
            Conflict::Rename {
                object,
                content,
                target,
                incoming,
            } => json!({
                "kind": "rename", "object": self.object_label(object),
                "content": self.op_label(content), "target": target, "incoming": incoming,
            }),
            Conflict::Path { object, paths } => json!({
                "kind": "path", "object": self.object_label(object), "paths": [paths.0, paths.1],
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Running behaviour vectors

struct Run<'a> {
    labels: &'a Labels,
    v: &'a Json,
    name: String,
    history: History,
    envelopes: &'a BTreeMap<String, Vec<u8>>,
    names: &'a HashMap<BatchId, String>,
    dir: &'a TestDir,
    dropped: BTreeSet<String>,
}

impl Run<'_> {
    fn at(&self, j: &Json) -> At {
        match j {
            Json::String(s) => At::Timeline(self.labels.op(s)),
            _ => At::Version(self.labels.version(j)),
        }
    }

    fn names(&self, ids: Vec<BatchId>) -> BTreeSet<String> {
        ids.iter().map(|i| self.names[i].clone()).collect()
    }

    fn set(j: &Json) -> BTreeSet<String> {
        j.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect()
    }

    fn deliver(&mut self, step: &Json) {
        let name = step["deliver"].as_str().unwrap();
        let report = self
            .history
            .receive(&self.envelopes[name], self.dir)
            .unwrap();
        for (id, verdict) in &report.others {
            if *verdict == Verdict::Missing {
                self.dropped.insert(self.names[id].clone());
            }
        }
        if report.verdict == Verdict::Missing {
            self.dropped.insert(name.to_string());
        }
        let got = report.verdict.code();
        match step.get("result").and_then(Json::as_str) {
            Some(want) => assert_eq!(got, want, "{}: deliver {name}", self.name),
            None => assert!(
                !got.starts_with("rejected"),
                "{}: deliver {name}: {got}",
                self.name
            ),
        }
    }

    fn expect(&mut self, e: &Json) {
        let ctx = &self.name;
        let l = self.labels;
        for (key, want) in e.as_object().unwrap() {
            match key.as_str() {
                "pending" => assert_eq!(
                    self.names(self.history.held_back()),
                    Self::set(want),
                    "{ctx}: pending"
                ),
                "quarantined" => assert_eq!(
                    self.names(self.history.quarantined()),
                    Self::set(want),
                    "{ctx}: quarantined"
                ),
                "dropped" => assert_eq!(self.dropped, Self::set(want), "{ctx}: dropped"),
                "heads" => {
                    for (t, v) in want.as_object().unwrap() {
                        assert_eq!(
                            self.history.head(&l.op(t)),
                            Some(l.version(v)),
                            "{ctx}: head of {t}"
                        );
                    }
                }
                "states" => {
                    for s in want.as_array().unwrap() {
                        self.check_state(s);
                    }
                }
                "plan_edits" => {
                    for p in want.as_array().unwrap() {
                        let got = self.history.plan_edit(
                            &self.at(&p["at"]),
                            &l.object(&p["object"]),
                            p["pos"].as_u64().unwrap() as usize,
                            p["delete"].as_u64().unwrap() as usize,
                            p["text"].as_str().unwrap(),
                        );
                        match p.get("error") {
                            Some(err) => assert_eq!(
                                got.map_err(|e| e.to_string()),
                                Err(err.as_str().unwrap().to_string()),
                                "{ctx}: plan_edit {p}"
                            ),
                            None => assert_eq!(
                                got.unwrap(),
                                Some(l.op_json(&p["op"])),
                                "{ctx}: plan_edit {p}"
                            ),
                        }
                    }
                }
                "plans" => {
                    for p in want.as_array().unwrap() {
                        self.check_plan(p);
                    }
                }
                "checkpoints" => {
                    for c in want.as_array().unwrap() {
                        let got: Vec<Json> = self
                            .history
                            .checkpoints(&l.op(c["timeline"].as_str().unwrap()))
                            .iter()
                            .map(|cp| {
                                json!({
                                    "id": l.op_label(&cp.id),
                                    "label": cp.label,
                                    "version": cp.version.heads().iter().map(|h| l.op_label(h)).collect::<Vec<_>>(),
                                })
                            })
                            .collect();
                        assert_eq!(Json::Array(got), c["list"], "{ctx}: checkpoints");
                    }
                }
                "tree_conflicts" => {
                    for t in want.as_array().unwrap() {
                        let state = self.history.state(&self.at(&t["at"])).unwrap();
                        let tc = state.tree_conflicts(&l.object(&t["tree"]));
                        let got = json!({
                            "keys": tc.keys,
                            "duplicates": tc.duplicates.iter().map(|(o, p)| json!({"object": l.op_label(o), "paths": p})).collect::<Vec<_>>(),
                            "clashes": tc.clashes.iter().map(|(p, q)| json!([p, q])).collect::<Vec<_>>(),
                        });
                        let want = json!({"keys": t["keys"], "duplicates": t["duplicates"], "clashes": t["clashes"]});
                        assert_eq!(got, want, "{ctx}: tree_conflicts");
                    }
                }
                "inventory" => {
                    let got: BTreeMap<String, Vec<(u64, u64)>> = self
                        .history
                        .inventory()
                        .0
                        .iter()
                        .map(|(r, rs)| (l.label_of(r), rs.clone()))
                        .collect();
                    assert_eq!(got, ranges_of(want), "{ctx}: inventory");
                }
                "answer" => {
                    let inventory = ranges_of(&want["hello"])
                        .into_iter()
                        .map(|(k, rs)| ((*l.replicas[&k]).clone(), rs))
                        .collect();
                    let hello = han_timeline::Hello {
                        space: SPACE.into(),
                        history: self.history.id(),
                        inventory: han_timeline::Inventory(inventory),
                        max_bytes: None,
                    };
                    let answer = self.history.answer_to(&hello).unwrap();
                    let by_bytes: HashMap<&[u8], &String> = self
                        .envelopes
                        .iter()
                        .map(|(k, v)| (v.as_slice(), k))
                        .collect();
                    let got: Vec<&String> = answer
                        .envelopes
                        .iter()
                        .map(|e| by_bytes[e.as_slice()])
                        .collect();
                    let want: Vec<String> = want["envelopes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect();
                    assert_eq!(got, want.iter().collect::<Vec<_>>(), "{ctx}: answer");
                    assert!(!answer.more);
                }
                k => panic!("{ctx}: unknown expectation {k}"),
            }
        }
    }

    fn check_state(&self, s: &Json) {
        let ctx = &self.name;
        let l = self.labels;
        let at = self.at(&s["at"]);
        let state = self.history.state(&at).unwrap();
        if let Some(objects) = s.get("objects") {
            for (id, want) in objects.as_object().unwrap() {
                let obj = l.object(&json!(id));
                if let Some(v) = want.get("value") {
                    assert_eq!(
                        state.register(&obj),
                        Some(&l.value(v)),
                        "{ctx}: {id} at {at:?}"
                    );
                } else if let Some(t) = want.get("text") {
                    assert_eq!(
                        state.text(&obj).as_deref(),
                        t.as_str(),
                        "{ctx}: {id} at {at:?}"
                    );
                } else if let Some(entries) = want.get("entries") {
                    let got = match state.object(&obj) {
                        Some(han_timeline::ObjectState::Map { entries, .. }) => entries,
                        other => panic!("{ctx}: {id} is not a map: {other:?}"),
                    };
                    let got: BTreeMap<String, Json> = got
                        .iter()
                        .map(|(k, vals)| {
                            (
                                k.clone(),
                                Json::Array(
                                    vals.iter()
                                        .map(|(op, v)| json!([l.op_label(op), l.value_json(v)]))
                                        .collect(),
                                ),
                            )
                        })
                        .collect();
                    let want: BTreeMap<String, Json> = entries
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    assert_eq!(got, want, "{ctx}: {id} at {at:?}");
                }
            }
        }
        if let Some(absent) = s.get("absent") {
            for id in absent.as_array().unwrap() {
                assert!(
                    !state.contains(&l.object(id)),
                    "{ctx}: {id} should be absent"
                );
            }
        }
        if let Some(d) = s.get("digest") {
            let got: String = state.digest().iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(got, d.as_str().unwrap(), "{ctx}: digest at {at:?}");
        }
        if let Some(anchors) = s.get("anchors") {
            for a in anchors.as_array().unwrap() {
                let obj = l.object(&a["object"]);
                let want_anchor = match &a["anchor"] {
                    Json::String(s) if s == "start" => Anchor::Start,
                    Json::String(s) if s == "end" => Anchor::End,
                    j => Anchor::Char(
                        l.char_id(j[0].as_str().unwrap()),
                        if j[1] == "left" {
                            Bias::Left
                        } else {
                            Bias::Right
                        },
                    ),
                };
                if let Some(make) = a.get("make") {
                    let bias = if make["bias"] == "left" {
                        Bias::Left
                    } else {
                        Bias::Right
                    };
                    let got = state.make_anchor(&obj, make["pos"].as_u64().unwrap() as usize, bias);
                    assert_eq!(got, Some(want_anchor.clone()), "{ctx}: make anchor {a}");
                }
                assert_eq!(
                    state.resolve_anchor(&obj, &want_anchor),
                    Some(a["pos"].as_u64().unwrap() as usize),
                    "{ctx}: resolve anchor {a}"
                );
            }
        }
    }

    fn check_plan(&self, p: &Json) {
        let ctx = &self.name;
        let l = self.labels;
        let into = l.op(p["into"].as_str().unwrap());
        let from = l.op(p["from"].as_str().unwrap());
        let kind = p["kind"].as_str().unwrap();
        let plan = self.history.plan_merge(&into, &from);
        match kind {
            "up_to_date" => {
                assert_eq!(
                    plan.err(),
                    Some(han_timeline::Error::UpToDate),
                    "{ctx}: {p}"
                );
                return;
            }
            k if k.starts_with("error:") => {
                assert_eq!(
                    plan.map(|_| ()).map_err(|e| e.to_string()),
                    Err(k["error:".len()..].to_string()),
                    "{ctx}: {p}"
                );
                return;
            }
            _ => {}
        }
        let plan = plan.unwrap_or_else(|e| panic!("{ctx}: plan {p}: {e}"));
        if let Some(base) = p.get("base") {
            assert_eq!(plan.base, l.version(base), "{ctx}: base of {p}");
        }
        if let Some(conflicts) = p.get("conflicts") {
            let got: Vec<Json> = plan.conflicts.iter().map(|c| l.conflict_json(c)).collect();
            assert_eq!(&Json::Array(got), conflicts, "{ctx}: conflicts");
        }
        for r in p
            .get("results")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let resolutions: Vec<Resolution> = r["resolutions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| match x {
                    Json::String(s) if s == "target" => Resolution::Target,
                    Json::String(s) if s == "incoming" => Resolution::Incoming,
                    j if j.get("text").is_some() => {
                        Resolution::Text(j["text"].as_str().unwrap().to_string())
                    }
                    j => Resolution::Value(l.value(&j["value"])),
                })
                .collect();
            let draft = plan.resolve(&resolutions).unwrap();
            let state = self.history.preview(&draft).unwrap();
            for (obj, text) in r["texts"].as_object().unwrap() {
                assert_eq!(
                    state.text(&l.object(&json!(obj))).as_deref(),
                    text.as_str(),
                    "{ctx}: result {r} of {p}: {draft:#?}"
                );
            }
            // Recording the draft must be accepted by the history itself.
            let mut copy = History::open(MemoryStore::default(), SPACE, self.history.id()).unwrap();
            for e in self.history.envelopes() {
                copy.receive(e, self.dir).unwrap();
            }
            let a = signer_for(&l.replicas["A"]);
            copy.merge(&a, 7, &draft, None)
                .unwrap_or_else(|e| panic!("{ctx}: recording {r}: {e}: {draft:#?}"));
        }
    }
}

fn ranges_of(j: &Json) -> BTreeMap<String, Vec<(u64, u64)>> {
    j.as_object()
        .unwrap()
        .iter()
        .map(|(k, rs)| {
            (
                k.clone(),
                rs.as_array()
                    .unwrap()
                    .iter()
                    .map(|r| (r[0].as_u64().unwrap(), r[1].as_u64().unwrap()))
                    .collect(),
            )
        })
        .collect()
}

fn run_vector(path: &std::path::Path) {
    let v: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let name = v["name"].as_str().unwrap().to_string();
    let labels = Labels::new(&v);
    let writers: Option<BTreeSet<String>> = v.get("writers").map(Run::set);
    let mut keys = HashMap::new();
    for (label, r) in &labels.replicas {
        keys.insert(
            (r.actor.clone(), r.key.clone()),
            KeyInfo {
                public_key: signer_for(r).public_key(),
                valid: writers.as_ref().is_none_or(|w| w.contains(label)),
            },
        );
    }
    let dir = TestDir { keys };
    // Seal every batch; the genesis b0 names the history.
    let batches: BTreeMap<String, Batch> = v["batches"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, b)| (k.clone(), labels.batch(b)))
        .collect();
    let seal = |b: &Batch, history: Option<BatchId>| {
        envelope::seal(
            SPACE,
            history,
            b,
            &signer_for(&b.replica),
            Compression::Zstd,
        )
    };
    let genesis = seal(&batches["b0"], None);
    let history_id = han_timeline::Envelope::decode(&genesis).unwrap().batch_id();
    let mut envelopes = BTreeMap::new();
    let mut names = HashMap::new();
    for (k, b) in &batches {
        let e = seal(b, (!b.parents.is_root()).then_some(history_id));
        names.insert(
            han_timeline::Envelope::decode(&e).unwrap().batch_id(),
            k.clone(),
        );
        envelopes.insert(k.clone(), e);
    }
    for (i, run) in v["runs"].as_array().unwrap().iter().enumerate() {
        let mut history = History::open(MemoryStore::default(), SPACE, history_id).unwrap();
        if let Some(limit) = v.get("hold_limit") {
            history.set_buffer_limits(limit.as_u64().unwrap() as usize, 1024);
        }
        let mut r = Run {
            labels: &labels,
            v: &v,
            name: format!("{name} run {i}"),
            history,
            envelopes: &envelopes,
            names: &names,
            dir: &dir,
            dropped: BTreeSet::new(),
        };
        let _ = r.v;
        for step in run["steps"].as_array().unwrap() {
            if step.get("deliver").is_some() {
                r.deliver(step);
            } else {
                r.expect(&step["expect"]);
            }
        }
        // Reopening from the store reproduces every state (TL-123).
        let reopened = History::open(r.history.store().clone(), SPACE, history_id).unwrap();
        for t in r.history.timelines() {
            assert_eq!(
                reopened.head(&t.id),
                Some(t.head.clone()),
                "{} reopen",
                r.name
            );
            assert_eq!(
                reopened
                    .state(&At::Timeline(t.id.clone()))
                    .unwrap()
                    .digest(),
                r.history
                    .state(&At::Timeline(t.id.clone()))
                    .unwrap()
                    .digest(),
                "{} reopen",
                r.name
            );
        }
    }
}

#[test]
fn behaviour_vectors() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(vectors_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n.ends_with(".json") && n.as_bytes()[0].is_ascii_digit()
        })
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 54,
        "expected every vector, found {}",
        paths.len()
    );
    let only = std::env::var("VECTOR").ok();
    let mut failed = Vec::new();
    for p in &paths {
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        if only.as_ref().is_some_and(|o| !n.contains(o.as_str())) {
            continue;
        }
        if let Err(e) = std::panic::catch_unwind(|| run_vector(p)) {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            failed.push(format!("{n}: {msg}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} vectors failed:\n{}",
        failed.len(),
        failed.join("\n\n")
    );
}

// ---------------------------------------------------------------------------------------------
// Envelope vectors

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn envelope_vectors() {
    let v: Json = serde_json::from_str(
        &std::fs::read_to_string(vectors_dir().join("envelopes.json")).unwrap(),
    )
    .unwrap();
    let space = v["space"].as_str().unwrap();
    let history = BatchId(unhex(v["history"].as_str().unwrap()).try_into().unwrap());
    let mut signers = HashMap::new();
    let mut keys = HashMap::new();
    for (label, k) in v["keys"].as_object().unwrap() {
        let s = KeySigner::from_seed(
            k["actor"].as_str().unwrap(),
            k["key"].as_str().unwrap(),
            seed(k["seed"].as_str().unwrap()),
        );
        assert_eq!(
            han_core::base64url::encode(&s.public_key()),
            k["publicKey"].as_str().unwrap()
        );
        if k["inProfile"].as_bool().unwrap() {
            keys.insert(
                (s.actor().to_string(), s.key_id().to_string()),
                KeyInfo {
                    public_key: s.public_key(),
                    valid: true,
                },
            );
        }
        signers.insert(label.clone(), s);
    }
    let dir = TestDir { keys };
    let vectors: BTreeMap<String, &Json> = v["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["name"].as_str().unwrap().to_string(), x))
        .collect();

    // Byte-exact construction.
    for name in ["E1-genesis", "E2-replica-table"] {
        let x = vectors[name];
        let signer = &signers[x["from"].as_str().unwrap()];
        let raw = unhex(x["batch_cbor"].as_str().unwrap());
        let batch = han_timeline::codec::decode_batch(&raw, signer.actor(), signer.key_id())
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(
            han_timeline::codec::encode_batch(&batch),
            raw,
            "{name}: batch"
        );
        let payload = envelope::compress(&raw, Compression::Raw);
        assert_eq!(
            hex(&payload),
            x["payload"].as_str().unwrap(),
            "{name}: payload"
        );
        let hist = (!batch.parents.is_root()).then_some(history);
        let bytes = envelope::seal(space, hist, &batch, signer, Compression::Raw);
        let env = han_timeline::Envelope::decode(&bytes).unwrap();
        assert_eq!(
            hex(&env.batch_id_preimage()),
            x["batch_id_preimage"].as_str().unwrap()
        );
        assert_eq!(env.batch_id().to_hex(), x["batch_id"].as_str().unwrap());
        assert_eq!(
            String::from_utf8(envelope::sig_message(&env.batch_id())).unwrap(),
            x["sig_input"].as_str().unwrap()
        );
        assert_eq!(hex(&env.sig), x["sig"].as_str().unwrap(), "{name}: sig");
        assert_eq!(
            hex(&bytes),
            x["envelope"].as_str().unwrap(),
            "{name}: envelope"
        );
    }
    assert_eq!(
        vectors["E1-genesis"]["batch_id"].as_str().unwrap(),
        history.to_hex()
    );

    // Verdicts, each on a receiver that integrated what `after` lists.
    for (name, x) in &vectors {
        let Some(verdict) = x.get("verdict").and_then(Json::as_str) else {
            continue;
        };
        let mut h = History::open(MemoryStore::default(), space, history).unwrap();
        for a in x
            .get("after")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let e = unhex(vectors[a.as_str().unwrap()]["envelope"].as_str().unwrap());
            assert_eq!(h.receive(&e, &dir).unwrap().verdict, Verdict::Integrated);
        }
        let e = unhex(x["envelope"].as_str().unwrap());
        assert_eq!(
            h.receive(&e, &dir).unwrap().verdict.code(),
            verdict,
            "{name}"
        );
        if let Some(texts) = x.get("text_after") {
            let main = h.main().unwrap();
            let state = h.state(&At::Timeline(main.clone())).unwrap();
            for (obj, text) in texts.as_object().unwrap() {
                let (c, _) = obj.split_once('@').unwrap();
                let id = OpId::new(c.parse().unwrap(), main.replica.clone());
                assert_eq!(state.text(&ObjectId::Op(id)).as_deref(), text.as_str());
            }
        }
    }

    // Sync messages of a replica holding E1 and E2.
    let mut h = History::open(MemoryStore::default(), space, history).unwrap();
    for n in ["E1-genesis", "E2-replica-table"] {
        h.receive(&unhex(vectors[n]["envelope"].as_str().unwrap()), &dir)
            .unwrap();
    }
    assert_eq!(
        hex(&h.hello(None).encode()),
        vectors["E7-hello"]["hello"].as_str().unwrap()
    );
    let empty = han_timeline::Hello {
        space: space.into(),
        history,
        inventory: Default::default(),
        max_bytes: None,
    };
    let answer = h.answer(&empty.encode()).unwrap();
    assert_eq!(
        hex(&answer),
        vectors["E8-answer"]["answer"].as_str().unwrap()
    );

    // The merge digest (TL-74).
    let x = vectors["E11-merge-digest"];
    let a = Arc::new(ReplicaId::new("@alice:a.example", "laptop", 0));
    let b = Arc::new(ReplicaId::new("@bob:b.example", "desk", 0));
    let ch = |op: OpId, k| CharId {
        insertion: InsertionId { op, index: 0 },
        offset: k,
    };
    let draft = MergeDraft {
        target: OpId::new(1, a.clone()),
        op: MergeOp {
            source: OpId::new(3, a.clone()),
            incoming: Version::new(vec![OpId::new(5, b.clone())]),
            base: Version::new(vec![OpId::new(3, a.clone())]),
            changes: vec![Change::Text {
                object: ObjectId::Op(OpId::new(2, a.clone())),
                create: false,
                transcript: false,
                deletes: vec![IdRange {
                    insertion: InsertionId {
                        op: OpId::new(2, a.clone()),
                        index: 0,
                    },
                    from: 1,
                    to: 2,
                }],
                inserts: vec![
                    CopyInsert {
                        anchor: Some(ch(OpId::new(2, a.clone()), 0)),
                        text: "e".into(),
                        origin: Some(ch(OpId::new(4, b.clone()), 0)),
                    },
                    CopyInsert {
                        anchor: Some(ch(MergeDraft::this_op(), 0)),
                        text: "y".into(),
                        origin: Some(ch(OpId::new(5, b.clone()), 0)),
                    },
                ],
            }],
            conflicts: vec![],
            effect: None,
        },
    };
    assert_eq!(
        hex(&draft.digest_preimage(&history)),
        x["digest_preimage"].as_str().unwrap()
    );
    assert_eq!(
        hex(&draft.digest(&history)),
        x["merge_digest"].as_str().unwrap()
    );
}
