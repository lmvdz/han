//! Shape and limit validation: the Log's step 3 (architecture.md section 4.1), R-09, R-25, and the
//! body schemas P-16 step 1 repeats. No Policy decision is made here.
//!
//! Every failure is [`Error`] (the protocol's `invalid`). A body is checked against its kind's closed
//! schema; an unknown field, a wrong type, an impossible date or an out-of-range number fails.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::grammar::{self as g, is_opaque_json};
use crate::ids;
use crate::json::{self, MAX_SAFE_INT, Value};
use crate::limits::{
    MAX_BLOB_BYTES, MAX_CONTENT_BYTES, MAX_DENIED_SERVERS, MAX_HEADER_BYTES, MAX_LIST,
};
use crate::policy::gate::{
    MAX_CHANGE_BYTES, MAX_CHANGE_FILES, MAX_GATE_RULES, PATH_OWNER, is_evidence_kind, is_glob,
    is_repo_name, is_repo_path,
};
use crate::sign::PublicKey;
use crate::types::{BlobRef, Entry, Link, LogRecord, Receipt};

type Check = fn(&Value) -> bool;

struct Field {
    name: &'static str,
    check: Check,
    required: bool,
}

const fn req(name: &'static str, check: Check) -> Field {
    Field {
        name,
        check,
        required: true,
    }
}

const fn opt(name: &'static str, check: Check) -> Field {
    Field {
        name,
        check,
        required: false,
    }
}

// ───────────────────────── value checks ─────────────────────────

fn int(v: &Value) -> bool {
    matches!(v, Value::Int(i) if (0..=MAX_SAFE_INT).contains(i))
}

fn positive_int(v: &Value) -> bool {
    matches!(v, Value::Int(i) if (1..=MAX_SAFE_INT).contains(i))
}

fn launch_window(v: &Value) -> bool {
    matches!(v, Value::Int(w) if (15..=3600).contains(w))
}

fn boolean(v: &Value) -> bool {
    matches!(v, Value::Bool(_))
}

fn str_with(v: &Value, ok: fn(&str) -> bool) -> bool {
    v.as_str().is_some_and(ok)
}

fn actor(v: &Value) -> bool {
    str_with(v, g::is_actor_id)
}

fn hex(v: &Value) -> bool {
    str_with(v, g::is_hex)
}

fn key_id(v: &Value) -> bool {
    str_with(v, g::is_key_id)
}

fn public_key(v: &Value) -> bool {
    str_with(v, g::is_public_key_text)
}

fn time(v: &Value) -> bool {
    str_with(v, g::is_time)
}

fn space(v: &Value) -> bool {
    str_with(v, g::is_space_id)
}

fn server(v: &Value) -> bool {
    str_with(v, g::is_server_name)
}

fn external(v: &Value) -> bool {
    str_with(v, g::is_external)
}

fn action(v: &Value) -> bool {
    str_with(v, g::is_action)
}

fn any_string(v: &Value) -> bool {
    v.as_str().is_some()
}

fn is_one_of(v: &Value, options: &[&str]) -> bool {
    v.as_str().is_some_and(|s| options.contains(&s))
}

fn max_bytes<const N: usize>(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.len() <= N)
}

fn list_of(v: &Value, max: usize, item: Check) -> bool {
    v.as_array()
        .is_some_and(|a| a.len() <= max && a.iter().all(item))
}

fn actors(v: &Value) -> bool {
    list_of(v, MAX_LIST, actor)
}

fn non_empty_actors(v: &Value) -> bool {
    actors(v) && v.as_array().is_some_and(|a| !a.is_empty())
}

/// The members of an object whose keys are exactly `names`.
fn exactly<'a>(v: &'a Value, names: &[&str]) -> Option<&'a BTreeMap<String, Value>> {
    let m = v.as_object()?;
    (m.len() == names.len() && names.iter().all(|n| m.contains_key(*n))).then_some(m)
}

fn budget(v: &Value) -> bool {
    exactly(v, &["unit", "amount"])
        .is_some_and(|m| str_with(&m["unit"], g::is_unit) && int(&m["amount"]))
}

fn blob_ref(v: &Value) -> bool {
    json::from_value::<BlobRef>(v).is_ok_and(|b| {
        g::is_hex(&b.hash)
            && b.size <= MAX_BLOB_BYTES
            && g::is_media_type(&b.media_type)
            && b.uri.as_ref().is_none_or(|u| u.len() <= 1024)
    })
}

fn blobs(v: &Value) -> bool {
    list_of(v, MAX_LIST, blob_ref)
}

fn link(v: &Value) -> bool {
    json::from_value::<Link>(v).is_ok_and(|l| {
        g::is_space_id(&l.space)
            && g::is_hex(&l.id)
            && i64::try_from(l.seq).is_ok_and(|s| s <= MAX_SAFE_INT)
    })
}

fn links(v: &Value) -> bool {
    list_of(v, MAX_LIST, link)
}

fn plan(v: &Value) -> bool {
    let Some(m) = v.as_object() else {
        return false;
    };
    m.keys()
        .all(|k| matches!(k.as_str(), "action" | "target" | "args" | "argsBlob"))
        && m.get("action").is_some_and(action)
        && m.get("target").is_some_and(max_bytes::<1024>)
        && m.get("argsBlob").is_none_or(blob_ref)
        && m.get("args").is_none_or(is_opaque_json)
}

// ───────────────────────── policy documents ─────────────────────────

const READ_ROLES: [&str; 3] = ["owner", "member", "guest"];
const SESSION_LEVELS: [&str; 4] = ["guest", "member", "owner", "nobody"]; // least to most restrictive

/// The members of an object that has the `required` keys and no keys outside `allowed`.
fn within<'a>(
    v: &'a Value,
    required: &[&str],
    allowed: &[&str],
) -> Option<&'a BTreeMap<String, Value>> {
    let m = v.as_object()?;
    (required.iter().all(|n| m.contains_key(*n)) && m.keys().all(|k| allowed.contains(&k.as_str())))
        .then_some(m)
}

fn approval_rule(v: &Value) -> bool {
    let Some(m) = within(
        v,
        &["action", "approvers"],
        &[
            "action",
            "approvers",
            "minTier",
            "maxTier",
            "humans",
            "evidence",
            "needs",
        ],
    ) else {
        return false;
    };
    let tier = |k: &str| {
        m.get(k)
            .is_none_or(|t| matches!(t, Value::Int(i) if (0..=3).contains(i)))
    };
    let bound = |k: &str| match m.get(k) {
        Some(Value::Int(i)) => Some(*i),
        _ => None,
    };
    m["action"]
        .as_str()
        .is_some_and(|a| a == "*" || g::is_action(a))
        && m["approvers"].as_array().is_some_and(|a| {
            (1..=MAX_LIST).contains(&a.len())
                && a.iter().all(|x| {
                    x.as_str().is_some_and(|s| {
                        READ_ROLES.contains(&s) || s == PATH_OWNER || g::is_actor_id(s)
                    })
                })
        })
        && tier("minTier")
        && tier("maxTier")
        && bound("minTier")
            .zip(bound("maxTier"))
            .is_none_or(|(lo, hi)| lo <= hi)
        && m.get("humans").is_none_or(boolean)
        && m.get("evidence")
            .is_none_or(|e| str_with(e, is_evidence_kind))
        && m.get("needs")
            .is_none_or(|n| matches!(n, Value::Int(i) if (1..=3).contains(i)))
}

fn rule_repo(m: &BTreeMap<String, Value>) -> bool {
    m.get("repo").is_none_or(|r| str_with(r, is_repo_name))
}

fn gates_doc(v: &Value) -> bool {
    let Some(m) = exactly(v, &["small", "large", "paths", "owners"]) else {
        return false;
    };
    let path_rule = |r: &Value| {
        within(r, &["glob", "class"], &["glob", "class", "repo"]).is_some_and(|r| {
            str_with(&r["glob"], is_glob)
                && is_one_of(
                    &r["class"],
                    &[
                        "docs",
                        "tests",
                        "generated",
                        "code",
                        "sensitive",
                        "critical",
                    ],
                )
                && rule_repo(r)
        })
    };
    let owner_rule = |r: &Value| {
        within(r, &["glob", "owners"], &["glob", "owners", "repo"]).is_some_and(|r| {
            str_with(&r["glob"], is_glob)
                && r["owners"].as_array().is_some_and(|o| {
                    (1..=8).contains(&o.len())
                        && o.iter().all(actor)
                        && o.iter().enumerate().all(|(i, x)| !o[..i].contains(x))
                })
                && rule_repo(r)
        })
    };
    int(&m["small"])
        && int(&m["large"])
        && matches!((&m["small"], &m["large"]), (Value::Int(s), Value::Int(l)) if s <= l)
        && list_of(&m["paths"], MAX_GATE_RULES, path_rule)
        && list_of(&m["owners"], MAX_GATE_RULES, owner_rule)
}

/// P-17: the summary of a change (`effect.propose.change`, and the plan's `args.change`).
pub fn change_summary(v: &Value) -> bool {
    let Some(m) = within(
        v,
        &["repo", "deltaDigest", "total", "files"],
        &["repo", "deltaDigest", "total", "files", "truncated"],
    ) else {
        return false;
    };
    let (Value::Int(total), Some(files)) = (&m["total"], m["files"].as_array()) else {
        return false;
    };
    if !(str_with(&m["repo"], is_repo_name)
        && hex(&m["deltaDigest"])
        && int(&m["total"])
        && files.len() <= MAX_CHANGE_FILES)
    {
        return false;
    }
    if m.get("truncated").is_some_and(|t| *t != Value::Bool(true)) {
        return false;
    }
    let mut last: Option<&str> = None;
    for f in files {
        let Some(f) = within(
            f,
            &["path", "kind", "lines"],
            &["path", "kind", "lines", "fromPath"],
        ) else {
            return false;
        };
        let (Some(path), true) = (f["path"].as_str(), int(&f["lines"])) else {
            return false;
        };
        if !is_repo_path(path) {
            return false;
        }
        match f["kind"].as_str() {
            Some("rename") => {
                let from = f.get("fromPath").and_then(Value::as_str);
                if !from.is_some_and(|p| is_repo_path(p) && p != path) {
                    return false;
                }
            }
            Some("add" | "modify" | "delete") if !f.contains_key("fromPath") => {}
            _ => return false,
        }
        if last.is_some_and(|l| path.as_bytes() <= l.as_bytes()) {
            return false; // strictly ascending by UTF-8 bytes: one entry per path
        }
        last = Some(path);
    }
    let listed = files.len() as i64;
    if (*total > listed) != m.contains_key("truncated") || *total < listed {
        return false; // truncated exactly when files are left out
    }
    json::canonicalize(v).is_ok_and(|b| b.len() <= MAX_CHANGE_BYTES)
}

fn evidence_ref(v: &Value) -> bool {
    exactly(v, &["kind", "over"])
        .is_some_and(|m| str_with(&m["kind"], is_evidence_kind) && hex(&m["over"]))
}

fn sessions_doc(v: &Value) -> bool {
    let Some(m) = v.as_object() else {
        return false;
    };
    let keys: BTreeSet<&str> = m.keys().map(String::as_str).collect();
    let base = BTreeSet::from(["watch", "read", "steer", "modes"]);
    let with_force = BTreeSet::from(["watch", "read", "steer", "modes", "forceGuarded"]);
    if keys != base && keys != with_force {
        return false;
    }
    let level = |k: &str| {
        m[k].as_str()
            .and_then(|s| SESSION_LEVELS.iter().position(|l| *l == s))
    };
    let (Some(watch), Some(read), Some(steer)) = (level("watch"), level("read"), level("steer"))
    else {
        return false;
    };
    let modes_ok = m["modes"].as_array().is_some_and(|a| {
        let names: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
        names.len() == a.len()
            && !names.is_empty()
            && names.iter().all(|n| matches!(*n, "auto" | "guarded"))
            && names.iter().collect::<BTreeSet<_>>().len() == names.len()
    });
    // Read and steer need watch: watch may not be more restrictive than either.
    watch <= read && watch <= steer && modes_ok && m.get("forceGuarded").is_none_or(boolean)
}

fn policy_doc(v: &Value) -> bool {
    const FIELDS: [&str; 13] = [
        "visibility",
        "join",
        "membersMayInvite",
        "allowAgents",
        "approvals",
        "separateApprover",
        "budget",
        "maxDelegationDepth",
        "minDurability",
        "launchWindow",
        "extensionsMinRole",
        "sessions",
        "servers",
    ];
    let Some(m) = within(v, &FIELDS, &[&FIELDS[..], &["gates"]].concat()) else {
        return false;
    };
    m.get("gates").is_none_or(gates_doc)
        && is_one_of(&m["visibility"], &["members", "public"])
        && is_one_of(&m["join"], &["invite", "open", "closed"])
        && boolean(&m["membersMayInvite"])
        && boolean(&m["separateApprover"])
        && is_one_of(&m["allowAgents"], &["any", "with-operator", "none"])
        && is_one_of(&m["minDurability"], &["local", "replicated"])
        && list_of(&m["approvals"], MAX_LIST, approval_rule)
        && (m["budget"] == Value::Null || budget(&m["budget"]))
        && matches!(&m["maxDelegationDepth"], Value::Int(d) if (0..=16).contains(d))
        && launch_window(&m["launchWindow"])
        && is_one_of(&m["extensionsMinRole"], &READ_ROLES)
        && sessions_doc(&m["sessions"])
        && exactly(&m["servers"], &["deny"])
            .is_some_and(|s| list_of(&s["deny"], MAX_DENIED_SERVERS, server))
}

fn profile(v: &Value) -> bool {
    let Some(m) = v.as_object() else {
        return false;
    };
    m.keys()
        .all(|k| matches!(k.as_str(), "actorKind" | "operator" | "key"))
        && m.get("actorKind")
            .is_some_and(|k| is_one_of(k, &["human", "agent", "service"]))
        && m.get("operator").is_none_or(actor)
        && m.get("key").is_some_and(|k| {
            exactly(k, &["keyId", "publicKey"])
                .is_some_and(|k| key_id(&k["keyId"]) && public_key(&k["publicKey"]))
        })
}

fn fork_of(v: &Value) -> bool {
    exactly(v, &["space", "through"]).is_some_and(|m| space(&m["space"]) && int(&m["through"]))
}

fn accept(v: &Value) -> bool {
    exactly(v, &["keyId", "sig"]).is_some_and(|m| key_id(&m["keyId"]) && any_string(&m["sig"]))
}

// ───────────────────────── schemas ─────────────────────────

fn member_role(v: &Value) -> bool {
    is_one_of(v, &["owner", "member", "guest", "banned", "none"])
}

fn task_state(v: &Value) -> bool {
    is_one_of(
        v,
        &[
            "accepted",
            "progress",
            "done",
            "failed",
            "declined",
            "cancelled",
        ],
    )
}

fn effect_result(v: &Value) -> bool {
    is_one_of(v, &["succeeded", "failed", "cancelled", "unknown"])
}

fn started(v: &Value) -> bool {
    is_one_of(v, &["yes", "no", "unknown"])
}

fn resolve_result(v: &Value) -> bool {
    is_one_of(v, &["succeeded", "failed"])
}

fn steer_mode(v: &Value) -> bool {
    is_one_of(v, &["auto", "guarded"])
}

/// The closed body schema of a core kind, or `None` if `kind` is not one.
fn body_schema(kind: &str) -> Option<Vec<Field>> {
    Some(match kind {
        "space.create" => vec![
            req("policy", policy_doc),
            opt("forkOf", fork_of),
            opt("profile", profile),
        ],
        "member.set" => vec![req("actor", actor), req("role", member_role)],
        "policy.set" => vec![req("policy", policy_doc)],
        "key.add" => vec![req("keyId", key_id), req("publicKey", public_key)],
        "key.revoke" => vec![req("keyId", key_id)],
        "message" => vec![opt("to", actors)],
        "task" => vec![
            req("to", non_empty_actors),
            opt("budget", budget),
            opt("parent", hex),
            opt("deadline", time),
            opt("external", external),
        ],
        "task.update" => vec![req("task", hex), req("state", task_state)],
        "task.cancel" => vec![req("task", hex)],
        "effect.propose" => vec![
            req("executor", actor),
            req("action", action),
            req("planDigest", hex),
            opt("cost", budget),
            req("repeatable", boolean),
            opt("task", hex),
            opt("expiresAt", time),
            opt("change", change_summary),
        ],
        "effect.approve" => vec![
            req("proposal", hex),
            req("planDigest", hex),
            opt("expiresAt", time),
            opt("evidence", evidence_ref),
        ],
        "effect.claim" => vec![
            req("proposal", hex),
            req("attempt", positive_int),
            req("instance", key_id),
            req("window", launch_window),
        ],
        "effect.cancel" => vec![req("proposal", hex)],
        "effect.outcome" => vec![
            req("claim", hex),
            req("result", effect_result),
            req("started", started),
            opt("cost", budget),
        ],
        "effect.resolve" => vec![
            req("proposal", hex),
            req("result", resolve_result),
            opt("cost", budget),
        ],
        "checkpoint" => vec![req("through", int)],
        "redact" => vec![req("target", hex)],
        "session.open" => vec![req("steerMode", steer_mode), opt("task", hex)],
        "session.close" | "session.input" => vec![req("session", hex)],
        "agent.unguard" => vec![req("agent", actor)],
        "space.move" => vec![req("to", server), req("accept", accept)],
        _ => return None,
    })
}

/// The content schema of a core kind: `None` for a kind without content.
fn content_schema(kind: &str) -> Option<Vec<Field>> {
    Some(match kind {
        "space.create" => vec![req("name", max_bytes::<256>)],
        "message" => vec![
            req("text", max_bytes::<32768>),
            opt("blobs", blobs),
            opt("links", links),
        ],
        "task" => vec![
            req("ask", max_bytes::<32768>),
            opt("blobs", blobs),
            opt("links", links),
        ],
        "task.update" => vec![opt("text", max_bytes::<32768>), opt("blobs", blobs)],
        "effect.propose" => vec![req("plan", plan)],
        "effect.approve" => vec![opt("note", max_bytes::<4096>), opt("evidence", blob_ref)],
        "effect.cancel" => vec![opt("reason", max_bytes::<1024>), opt("evidence", blob_ref)],
        "effect.outcome" => vec![opt("evidence", blob_ref)],
        "effect.resolve" => vec![req("evidence", blob_ref)],
        "checkpoint" => vec![req("blobs", blobs)],
        "redact" => vec![opt("reason", max_bytes::<1024>)],
        "session.open" => vec![opt("title", max_bytes::<256>)],
        "session.input" => vec![req("text", max_bytes::<32768>), opt("blobs", blobs)],
        _ => return None,
    })
}

fn check_object(v: &Value, schema: &[Field], what: &str) -> Result<(), Error> {
    let m = v
        .as_object()
        .ok_or_else(|| Error::invalid(format!("{what}: not an object")))?;
    if let Some(unknown) = m
        .keys()
        .find(|k| !schema.iter().any(|f| f.name == k.as_str()))
    {
        return Err(Error::invalid(format!("{what}: unknown field {unknown}")));
    }
    for f in schema {
        match m.get(f.name) {
            Some(x) if !(f.check)(x) => {
                return Err(Error::invalid(format!("{what}: bad field {}", f.name)));
            }
            None if f.required => {
                return Err(Error::invalid(format!("{what}: missing field {}", f.name)));
            }
            _ => {}
        }
    }
    Ok(())
}

// ───────────────────────── entries ─────────────────────────

/// R-25, §2: the Log owns the canonical form of a public key: canonical base64url of 32 bytes
/// that are a curve point of large order, judged here at parse.
fn canonical_key_bytes(key: &Value) -> Result<(), Error> {
    PublicKey::from_base64url(key.as_str().unwrap_or_default()).map(|_| ())
}

/// The shape check of a core kind's body against its closed schema; `None` if `kind` is not a core kind.
pub(crate) fn check_core_body(kind: &str, body: &Value) -> Option<Result<(), Error>> {
    body_schema(kind).map(|schema| check_object(body, &schema, "body"))
}

impl Entry {
    /// Parses one entry from JSON bytes: strict I-JSON, the exact `Entry` fields, then
    /// [`Entry::validate`].
    pub fn from_json(bytes: &[u8]) -> Result<Entry, Error> {
        let entry: Entry = json::from_value(&json::parse(bytes)?)?;
        entry.validate()?;
        Ok(entry)
    }

    /// Checks the header: every field in its grammar, the body against its kind's closed schema,
    /// the canonical form of any public key it carries, and the header size limit (measured with
    /// `sig` included). It does not check the signature, `seen <= head`, or `refs` existence.
    pub fn validate(&self) -> Result<(), Error> {
        if self.v != 1 {
            return Err(Error::invalid("v: unsupported version"));
        }
        if !g::is_nonce(&self.nonce) {
            return Err(Error::invalid("nonce"));
        }
        if !g::is_time(&self.time) {
            return Err(Error::invalid("time"));
        }
        if !(self.content_hash.is_empty() || g::is_hex(&self.content_hash)) {
            return Err(Error::invalid("contentHash"));
        }
        if !g::is_actor_id(&self.author) {
            return Err(Error::invalid("author"));
        }
        if !g::is_key_id(&self.key_id) {
            return Err(Error::invalid("keyId"));
        }
        if !g::is_space_id(&self.space) {
            return Err(Error::invalid("space"));
        }
        if self.refs.len() > MAX_LIST || !self.refs.iter().all(|r| g::is_hex(r)) {
            return Err(Error::invalid("refs"));
        }
        if self.seen > MAX_SAFE_INT as u64 {
            return Err(Error::invalid("seen"));
        }
        let body = &self.body;
        if !matches!(body, Value::Object(_)) {
            return Err(Error::invalid("body: not an object"));
        }
        match body_schema(&self.kind) {
            Some(schema) => check_object(body, &schema, "body")?,
            None if g::is_extension_kind(&self.kind) => {
                if !is_opaque_json(body) {
                    return Err(Error::invalid("extension body: only non-negative integers"));
                }
            }
            None => return Err(Error::invalid("kind")),
        }
        if self.kind == "key.add" {
            canonical_key_bytes(&body["publicKey"])?;
        }
        if self.kind == "space.create"
            && let Some(key) = body.get("profile").and_then(|p| p.get("key"))
        {
            canonical_key_bytes(&key["publicKey"])?;
        }
        if json::canonicalize(&json::to_value(self)?)?.len() > MAX_HEADER_BYTES {
            return Err(Error::invalid("header over the size limit"));
        }
        Ok(())
    }

    /// Checks `content` against this entry: it is present exactly when `contentHash` is not `""`
    /// (JSON `null` counts as absent), hashes to it, fits the size limit and the kind's content
    /// schema, and for `effect.propose` carries a plan with `plan.action = body.action` and
    /// `sha256(JCS(plan)) = body.planDigest`. Call [`Entry::validate`] first.
    pub fn validate_content(&self, content: Option<&Value>) -> Result<(), Error> {
        let content = content.filter(|c| **c != Value::Null);
        if content.is_none() != self.content_hash.is_empty() {
            return Err(Error::invalid("content presence"));
        }
        let Some(content) = content else {
            // Absent content is fine only if the kind needs none.
            return match content_schema(&self.kind) {
                Some(schema) if schema.iter().any(|f| f.required) => {
                    Err(Error::invalid("content missing"))
                }
                _ => Ok(()),
            };
        };
        let bytes = json::canonicalize(content)?;
        if bytes.len() > MAX_CONTENT_BYTES {
            return Err(Error::invalid("content over the size limit"));
        }
        if ids::sha256_hex(&bytes) != self.content_hash {
            return Err(Error::invalid("contentHash does not match the content"));
        }
        if body_schema(&self.kind).is_none() && g::is_extension_kind(&self.kind) {
            // Opaque to the core, but `blobs` is the generic blob position.
            if !is_opaque_json(content) || content.as_object().is_none() {
                return Err(Error::invalid(
                    "extension content must be an object of plain Json",
                ));
            }
            if content.get("blobs").is_some_and(|b| !blobs(b)) {
                return Err(Error::invalid("content: bad field blobs"));
            }
            return Ok(());
        }
        let Some(schema) = content_schema(&self.kind) else {
            return Err(Error::invalid("this kind has no content"));
        };
        check_object(content, &schema, "content")?;
        if self.kind == "effect.propose" {
            let plan = &content["plan"];
            if plan.get("action") != self.body.get("action") {
                return Err(Error::invalid("plan.action differs from action"));
            }
            let digest = ids::json_digest(plan)?;
            if self.body.get("planDigest").and_then(Value::as_str) != Some(digest.as_str()) {
                return Err(Error::invalid("planDigest does not match the plan"));
            }
        }
        Ok(())
    }
}

impl Receipt {
    /// Checks the receipt's shape: grammar of every field, `seq >= 1`, `prev` empty exactly when
    /// `seq` is 1. The signature and chain are the verifier's.
    pub fn validate(&self) -> Result<(), Error> {
        let max = MAX_SAFE_INT as u64;
        let ok = g::is_space_id(&self.space)
            && (1..=max).contains(&self.seq)
            && g::is_hex(&self.id)
            && (self.prev.is_empty() == (self.seq == 1))
            && (self.prev.is_empty() || g::is_hex(&self.prev))
            && g::is_server_name(&self.home)
            && g::is_key_id(&self.home_key)
            && g::is_time(&self.time)
            && self
                .profiles
                .iter()
                .all(|(a, n)| g::is_actor_id(a) && *n <= max);
        if ok {
            Ok(())
        } else {
            Err(Error::invalid("receipt"))
        }
    }
}

impl LogRecord {
    /// Checks a record's shape and its internal consistency: the entry (and its content unless
    /// redacted), the receipt, `receipt.id` equal to the entry's id, and `receipt.space` equal to
    /// the entry's. A redacted record must carry `redactedBy` and no content; checking that proof
    /// is the verifier's job (R-14).
    pub fn validate(&self) -> Result<(), Error> {
        self.entry.validate()?;
        self.receipt.validate()?;
        if self.redacted {
            if self.redacted_by.is_none() || self.content.is_some() {
                return Err(Error::invalid("redacted record"));
            }
        } else {
            if self.redacted_by.is_some() {
                return Err(Error::invalid(
                    "redactedBy on a record that is not redacted",
                ));
            }
            self.entry.validate_content(self.content.as_ref())?;
        }
        if self.receipt.space != self.entry.space {
            return Err(Error::invalid("receipt for another space"));
        }
        if self.receipt.id != self.entry.id()? {
            return Err(Error::invalid("receipt id differs from the entry id"));
        }
        Ok(())
    }
}
