//! `StreamItem` shapes (T-07, G-05, X11): output, status, context and `profile` items, with the members a
//! `han.status/1` body must carry, and the `sessionAccess` right each type needs.

use han_core::grammar;
use han_core::json::{self, Value};

use crate::calls::MAX_STREAM_ITEM_BYTES;
use crate::services::parse_blobref;

const STATUS_STATES: [&str; 7] = [
    "working",
    "waiting-approval",
    "blocked",
    "idle",
    "done",
    "stale",
    "failed",
];

/// Whether `s` is a profile's name, `<prefix>.<name>/<version>`, such as `han.status/1`.
pub fn is_profile_name(s: &str) -> bool {
    let Some((name, version)) = s.rsplit_once('/') else {
        return false;
    };
    let version_ok = (1..=6).contains(&version.len())
        && version.bytes().all(|b| b.is_ascii_digit())
        && !version.starts_with('0');
    let mut parts = name.split('.');
    let prefix_ok = parts.next().is_some_and(|p| {
        (1..=32).contains(&p.len())
            && p.as_bytes()[0].is_ascii_lowercase()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    let rest: Vec<&str> = parts.collect();
    prefix_ok
        && version_ok
        && (1..=4).contains(&rest.len())
        && rest.iter().all(|p| {
            (1..=32).contains(&p.len())
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// The members a face relies on in a `han.status/1` body; unknown members are ignored.
fn status_body_verdict(b: &Value) -> Option<String> {
    let Value::Object(m) = b else {
        return Some("body is not an object".into());
    };
    type Check = fn(&Value) -> bool;
    let required: [(&str, Check); 8] = [
        ("session", |v| v.as_str().is_some_and(grammar::is_hex)),
        ("agent", |v| v.as_str().is_some_and(grammar::is_actor_id)),
        ("harness", |v| v.as_str().is_some()),
        ("state", |v| {
            v.as_str().is_some_and(|s| STATUS_STATES.contains(&s))
        }),
        ("step", |v| v.as_str().is_some()),
        ("files", |v| v.as_array().is_some()),
        ("seq", |v| v.as_u64().is_some_and(|n| n >= 1)),
        ("at", |v| v.as_str().is_some_and(grammar::is_time)),
    ];
    for (name, ok) in required {
        match m.get(name) {
            None => return Some(format!("missing {name}")),
            Some(v) if !ok(v) => return Some(format!("bad {name}")),
            Some(_) => {}
        }
    }
    if m.get("delegate_of")
        .is_some_and(|v| !v.as_str().is_some_and(grammar::is_actor_id))
    {
        return Some("bad delegate_of".into());
    }
    if m.get("issue")
        .is_some_and(|v| !v.as_str().is_some_and(grammar::is_external))
    {
        return Some("bad issue".into());
    }
    None
}

/// G-05's shape check of a `StreamItem`: the common members, then one of the four types, and its JCS at
/// most `maxStreamItemBytes`. `None` means it is well formed; otherwise the reason (free text).
pub fn verdict(item: &Value) -> Option<String> {
    let Value::Object(m) = item else {
        return Some("not an object".into());
    };
    const COMMON: [&str; 5] = ["space", "session", "n", "time", "type"];
    if !COMMON.iter().all(|k| m.contains_key(*k)) {
        return Some("missing members".into());
    }
    let common_ok = item["space"].as_str().is_some_and(grammar::is_space_id)
        && item["session"].as_str().is_some_and(grammar::is_hex)
        && item["n"].as_u64().is_some_and(|n| n >= 1)
        && item["time"].as_str().is_some_and(grammar::is_time);
    if !common_ok {
        return Some("common members".into());
    }
    let rest: std::collections::BTreeSet<&str> = m
        .keys()
        .map(String::as_str)
        .filter(|k| !COMMON.contains(k))
        .collect();
    let has = |names: &[&str]| rest == names.iter().copied().collect();
    match item["type"].as_str() {
        Some("output") => {
            if !(has(&["text"]) && item["text"].as_str().is_some()) {
                return Some("output".into());
            }
        }
        Some("status") => {
            let members_ok = has(&["state"]) || has(&["state", "detail"]);
            let state_ok = item["state"]
                .as_str()
                .is_some_and(|s| ["idle", "working", "blocked", "done", "error"].contains(&s));
            if !(members_ok && state_ok) {
                return Some("status".into());
            }
            if m.get("detail").is_some_and(|d| d.as_str().is_none()) {
                return Some("status detail".into());
            }
        }
        Some("context") => {
            let what_ok = item["what"]
                .as_str()
                .is_some_and(|w| ["transcript", "files", "plan"].contains(&w));
            if !(has(&["blob", "what"]) && parse_blobref(&item["blob"]).is_some() && what_ok) {
                return Some("context".into());
            }
        }
        Some("profile") => {
            if !(has(&["name", "body"]) && item["name"].as_str().is_some_and(is_profile_name)) {
                return Some("profile name".into());
            }
            if !grammar::is_opaque_json(&item["body"]) {
                return Some("profile body is not Json".into());
            }
            if item["name"].as_str() == Some("han.status/1")
                && let Some(why) = status_body_verdict(&item["body"])
            {
                return Some(format!("han.status/1: {why}"));
            }
        }
        _ => return Some("unknown type".into()),
    }
    if json::canonicalize(item).map_or(usize::MAX, |b| b.len()) > MAX_STREAM_ITEM_BYTES {
        return Some("too large".into());
    }
    None
}

/// The `sessionAccess` right an item needs to reach a watcher: `read` for `context`, `watch` for the rest.
pub fn right(item: &Value) -> &'static str {
    if item["type"].as_str() == Some("context") {
        "read"
    } else {
        "watch"
    }
}
