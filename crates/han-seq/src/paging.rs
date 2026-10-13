//! `state` pages (architecture.md section 3, H3): the seven paged maps are cut by item count and by the
//! bytes of the reply frame, in a fixed fill order, so two servers cut one state into the same pages.

use std::collections::BTreeMap;

use han_core::json::{self, MAX_SAFE_INT, Value};
use han_core::policy::SpaceState;

/// The paged maps in cursor and fill order (`profile.keys` is the `keys` map of `profile`).
pub const PAGED: [&str; 7] = [
    "roles",
    "roleHistory",
    "tasks",
    "effects",
    "sessions",
    "guarded",
    "profile.keys",
];

/// Bytes kept for the `next` member of a state reply.
const FRAME_RESERVE: usize = 256;

/// The order in which the items of each paged map are sent. Section 3 says only that they are taken "in
/// order"; the reference model sends them in insertion order, oldest first, which is what this tracks: a key
/// joins the end of its list when a record adds it to the map and leaves it when a record removes it.
#[derive(Clone, Debug, Default)]
pub struct MapOrder {
    lists: [Vec<String>; 7],
}

fn keys_of(state: &SpaceState, map: usize) -> Vec<&str> {
    match map {
        0 => state.roles.keys().map(String::as_str).collect(),
        1 => state.role_history.keys().map(String::as_str).collect(),
        2 => state.tasks.keys().map(String::as_str).collect(),
        3 => state.effects.keys().map(String::as_str).collect(),
        4 => state.sessions.keys().map(String::as_str).collect(),
        5 => state.guarded.0.iter().map(String::as_str).collect(),
        _ => state
            .profile
            .as_ref()
            .map(|p| p.keys.keys().map(String::as_str).collect())
            .unwrap_or_default(),
    }
}

impl MapOrder {
    /// Updates the order for the record that took the state from `old` to `new`.
    pub fn advance(&mut self, old: Option<&SpaceState>, new: &SpaceState) {
        for (map, list) in self.lists.iter_mut().enumerate() {
            let now = keys_of(new, map);
            let before = old.map_or(0, |o| keys_of(o, map).len());
            if now.len() > before {
                let known: std::collections::HashSet<&str> =
                    list.iter().map(String::as_str).collect();
                let added: Vec<String> = now
                    .into_iter()
                    .filter(|k| !known.contains(k))
                    .map(str::to_owned)
                    .collect();
                list.extend(added);
            } else if now.len() < before {
                let keep: std::collections::HashSet<&str> = now.into_iter().collect();
                list.retain(|k| keep.contains(k.as_str()));
            }
        }
    }
}

/// How far each paged map has been sent.
pub type Offsets = [usize; 7];

/// One page: the state view, the offsets after it, and whether more remains.
pub struct Page {
    /// The `state` member of the reply.
    pub state: Value,
    /// The offsets after this page.
    pub offsets: Offsets,
    /// Whether another page follows.
    pub more: bool,
}

fn size(v: &Value) -> usize {
    json::canonicalize(v).map_or(0, |b| b.len())
}

fn map_of(view: &Value, map: usize) -> Option<&BTreeMap<String, Value>> {
    if map == 6 {
        view.get("profile")?.get("keys")?.as_object()
    } else {
        view.get(PAGED[map])?.as_object()
    }
}

fn map_mut(view: &mut Value, map: usize) -> Option<&mut BTreeMap<String, Value>> {
    let Value::Object(top) = view else {
        return None;
    };
    let target = if map == 6 {
        match top.get_mut("profile")? {
            Value::Object(p) => p.get_mut("keys")?,
            _ => return None,
        }
    } else {
        top.get_mut(PAGED[map])?
    };
    match target {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Cuts the page of `view` (the state without `entries`) that starts at `offsets`.
///
/// Every paged map is present on every page, a page holds at most `max_items` of each, and the reply as a
/// frame stays within `max_frame` bytes less a reserve for `next`. The first item that does not fit closes
/// the page: no later map adds anything to it. An item costs the JCS bytes of its key and of its value plus 2.
pub fn page(
    view: &Value,
    order: &MapOrder,
    offsets: Offsets,
    max_items: usize,
    max_frame: usize,
) -> Page {
    let present: Vec<usize> = (0..PAGED.len())
        .filter(|&m| map_of(view, m).is_some())
        .collect();
    let mut state = view.clone();
    for &m in &present {
        if let Some(map) = map_mut(&mut state, m) {
            map.clear();
        }
    }
    let envelope = |s: &Value| {
        size(&Value::Object(
            [
                ("id".to_owned(), Value::Int(MAX_SAFE_INT)),
                (
                    "ok".to_owned(),
                    Value::Object([("state".to_owned(), s.clone())].into()),
                ),
            ]
            .into(),
        ))
    };
    let mut used = envelope(&state);
    let cap = max_frame.saturating_sub(FRAME_RESERVE);
    let mut taken = [0usize; 7];
    'maps: for &m in &present {
        let Some(source) = map_of(view, m) else {
            continue;
        };
        let mut n = 0;
        for key in order.lists[m].iter().skip(offsets[m]) {
            if n >= max_items {
                break;
            }
            let Some(val) = source.get(key) else {
                continue;
            };
            let add = size(&Value::Str(key.clone())) + size(val) + 2;
            if used + add > cap {
                taken[m] = n;
                break 'maps;
            }
            if let Some(dst) = map_mut(&mut state, m) {
                dst.insert(key.clone(), val.clone());
            }
            used += add;
            n += 1;
        }
        taken[m] = n;
    }
    let mut next = offsets;
    for m in 0..PAGED.len() {
        next[m] = offsets[m] + taken[m];
    }
    let more = present
        .iter()
        .any(|&m| next[m] < order.lists[m].len().min(map_of(view, m).map_or(0, BTreeMap::len)));
    Page {
        state,
        offsets: next,
        more,
    }
}

/// The number of items of map `m` in `view`.
pub fn len_of(view: &Value, m: usize) -> usize {
    map_of(view, m).map_or(0, BTreeMap::len)
}

/// Parses a cursor `<head>.<o1>.….<o7>` (decimal, no leading zeros).
pub fn parse_cursor(s: &str) -> Option<(u64, Offsets)> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != PAGED.len() + 1 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in parts {
        let ok = !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p == "0" || !p.starts_with('0'));
        if !ok {
            return None;
        }
        nums.push(p.parse::<u64>().ok()?);
    }
    let mut offsets = [0usize; 7];
    for (slot, n) in offsets.iter_mut().zip(&nums[1..]) {
        *slot = usize::try_from(*n).ok()?;
    }
    Some((nums[0], offsets))
}

/// The cursor of the next page.
pub fn format_cursor(head: u64, offsets: &Offsets) -> String {
    let mut out = head.to_string();
    for o in offsets {
        out.push('.');
        out.push_str(&o.to_string());
    }
    out
}
