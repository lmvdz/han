//! `han-conform`: replays Policy conformance vectors against `han-core` and prints the report the
//! differential fuzz (`ref/fuzz_diff.py --target rust`) reads.
//!
//! ```text
//! han-conform [DIR] [FILTER]
//! ```
//!
//! `DIR` holds vector files in the format of `ref/README.md` (default: `conformance/vectors` of
//! this repository). Only files whose name contains `FILTER` run. Each step is checked as
//! `ref/run_vectors.py` does: the verdict of `decide`, the answer of `can_read`,
//! `relevant_actors`, `session_access` or `guard_status`, and after an allowed entry the state
//! `fold` returns, compared as Json at every `check` path (the path `""` is the whole state).
//!
//! Output: one line `<file>.json#<step>: <what differs>` per failing step (steps count from 0),
//! then `steps pass N fail M; vectors pass A fail B`. The exit code is 1 if anything failed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use han_core::json::{self, Numbers, Value};
use han_core::policy::{
    ActorSet, Ctx, GuardInput, PolicyDoc, PolicyEntry, ProfileView, Record, SpaceState, Task,
    Verdict, can_read, decide, fold, guard_status, relevant_actors, session_access,
};

type Profiles = BTreeMap<String, ProfileView>;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).filter(|a| a != "-v");
    let dir = args.next().map_or_else(default_dir, PathBuf::from);
    let filter = args.next();
    let mut files: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect(),
        Err(e) => {
            eprintln!("cannot read {}: {e}", dir.display());
            return ExitCode::from(2);
        }
    };
    files.sort();
    let (mut pass, mut fail, mut vpass, mut vfail) = (0usize, 0usize, 0usize, 0usize);
    for path in files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let stem = name.strip_suffix(".json").unwrap_or(name).to_owned();
        let vector = match load(&path) {
            Ok(v) => v,
            Err(e) => {
                println!("{stem}.json#0: cannot load: {e}");
                fail += 1;
                vfail += 1;
                continue;
            }
        };
        let (p, failures) = run_vector(&vector);
        pass += p;
        fail += failures.len();
        if failures.is_empty() {
            vpass += 1;
        } else {
            vfail += 1;
        }
        for (step, what) in failures {
            println!("{stem}.json#{step}: {}", what.replace('\n', " "));
        }
    }
    println!("steps pass {pass} fail {fail}; vectors pass {vpass} fail {vfail}");
    if fail == 0 && vfail == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/vectors")
}

fn load(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    json::parse_with(&bytes, Numbers::Any).map_err(|e| e.to_string())
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A dotted path into a value; a key may itself hold dots (actor ids do), so the longest key
/// that leads somewhere wins. The empty path is the value itself.
fn resolve<'a>(obj: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(obj);
    }
    match obj {
        Value::Array(items) => {
            let (head, rest) = path.split_once('.').unwrap_or((path, ""));
            resolve(items.get(head.parse::<usize>().ok()?)?, rest)
        }
        Value::Object(members) => {
            let mut cuts: Vec<usize> = path.match_indices('.').map(|(i, _)| i).collect();
            cuts.push(path.len());
            cuts.iter().rev().find_map(|&i| {
                let rest = path.get(i + 1..).unwrap_or("");
                resolve(members.get(&path[..i])?, rest)
            })
        }
        _ => None,
    }
}

fn profile_view(v: &Value) -> Result<ProfileView, String> {
    json::from_value(v).map_err(|e| e.to_string())
}

fn profiles_of(v: &Value) -> Result<Profiles, String> {
    let mut out = Profiles::new();
    if let Some(m) = v.as_object() {
        for (a, p) in m {
            out.insert(a.clone(), profile_view(p)?);
        }
    }
    Ok(out)
}

fn verdict_of(
    state: Option<&SpaceState>,
    entry: &Value,
    ctx: &Ctx,
) -> (Verdict, Option<PolicyEntry>) {
    match PolicyEntry::from_value(entry) {
        Ok(e) => (decide(state, &e, ctx), Some(e)),
        Err(err) => (Verdict::Invalid(err.to_string()), None),
    }
}

/// Runs the steps of one vector: how many passed, and the failures as (step, message).
fn run_vector(vec: &Value) -> (usize, Vec<(usize, String)>) {
    let Some(steps) = vec.get("steps").and_then(Value::as_array) else {
        return preimage_vector(vec);
    };
    let mut failures: Vec<(usize, String)> = Vec::new();
    let mut pass = 0;
    let mut profiles = match profiles_of(vec.get("profiles").unwrap_or(&Value::Null)) {
        Ok(p) => p,
        Err(e) => return (0, vec![(0, format!("bad profiles: {e}"))]),
    };
    let mut states: BTreeMap<String, SpaceState> = BTreeMap::new();
    let mut last_time = "2026-10-10T12:00:00Z".to_owned();
    for (i, st) in steps.iter().enumerate() {
        match run_step(st, &mut profiles, &mut states, &mut last_time) {
            Ok(()) => pass += 1,
            Err(what) => failures.push((i, what)),
        }
    }
    (pass, failures)
}

fn run_step(
    st: &Value,
    profiles: &mut Profiles,
    states: &mut BTreeMap<String, SpaceState>,
    last_time: &mut String,
) -> Result<(), String> {
    if let Some(over) = st.get("profiles") {
        profiles.extend(profiles_of(over)?);
    }
    if let Some(entry) = st.get("entry") {
        return entry_step(st, entry, profiles, states, last_time);
    }
    if let Some(q) = st.get("read") {
        let state = states.get(str_of(q, "space")).ok_or("no state")?;
        let seq = q.get("seq").and_then(Value::as_u64).ok_or("bad seq")?;
        let got = can_read(state, str_of(q, "actor"), seq);
        return expect_eq(&Value::Bool(got), st, "read");
    }
    if let Some(e) = st.get("relevantActors") {
        let space = states.get(str_of(e, "space"));
        let pinned = st.get("pinned").and_then(Value::as_array);
        let known: Profiles = profiles
            .iter()
            .filter(|(a, _)| pinned.is_none_or(|p| p.iter().any(|x| x.as_str() == Some(a))))
            .map(|(a, p)| (a.clone(), p.clone()))
            .collect();
        let ctx = Ctx::new(str_of(st, "time"), known).map_err(|e| e.to_string())?;
        let entry = PolicyEntry::from_value(e).map_err(|e| e.to_string())?;
        let got = relevant_actors(space, &entry, &ctx);
        let got = Value::Array(got.into_iter().map(Value::Str).collect());
        return expect_eq(&got, st, "relevantActors");
    }
    if let Some(q) = st.get("sessionAccess") {
        let state = states.get(str_of(q, "space")).ok_or("no state")?;
        let got = session_access(state, str_of(q, "actor"), str_of(q, "session"));
        let got = json::to_value(&got).map_err(|e| e.to_string())?;
        return expect_eq(&got, st, "sessionAccess");
    }
    if let Some(q) = st.get("guardStatus") {
        let ctx = Ctx::new(last_time, profiles.clone()).map_err(|e| e.to_string())?;
        let got = if let Some(view) = q.get("view") {
            let policy: PolicyDoc = json::from_value(view.get("policy").unwrap_or(&Value::Null))
                .map_err(|e| e.to_string())?;
            let guarded: ActorSet = json::from_value(view.get("guarded").unwrap_or(&Value::Null))
                .map_err(|e| e.to_string())?;
            let tasks: BTreeMap<String, Task> = match view.get("tasks") {
                Some(t) => json::from_value(t).map_err(|e| e.to_string())?,
                None => BTreeMap::new(),
            };
            guard_status(
                GuardInput {
                    policy: &policy,
                    guarded: &guarded,
                    tasks: &tasks,
                },
                &ctx,
                str_of(q, "actor"),
            )
        } else {
            let state = states.get(str_of(q, "space")).ok_or("no state")?;
            guard_status(state, &ctx, str_of(q, "actor"))
        };
        let got = json::to_value(&got).map_err(|e| e.to_string())?;
        return expect_eq(&got, st, "guardStatus");
    }
    Err("unknown step form".to_owned())
}

fn expect_eq(got: &Value, st: &Value, what: &str) -> Result<(), String> {
    let want = st.get("expect").unwrap_or(&Value::Null);
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "{what}: expected {}, got {}",
            show(want),
            show(got)
        ))
    }
}

fn show(v: &Value) -> String {
    json::canonicalize(v).map_or_else(
        |_| format!("{v:?}"),
        |b| String::from_utf8_lossy(&b).into_owned(),
    )
}

fn entry_step(
    st: &Value,
    entry: &Value,
    profiles: &Profiles,
    states: &mut BTreeMap<String, SpaceState>,
    last_time: &mut String,
) -> Result<(), String> {
    let time = str_of(st, "time");
    *last_time = time.to_owned();
    let ctx = Ctx::new(time, profiles.clone()).map_err(|e| e.to_string())?;
    let space = str_of(entry, "space").to_owned();
    let kind = str_of(entry, "kind");
    let prev = states.get(&space);
    let (d1, parsed) = verdict_of(prev, entry, &ctx);
    let (d2, _) = verdict_of(prev, entry, &ctx);
    if d1 != d2 {
        return Err("decide nondeterministic".to_owned());
    }
    let expect = str_of(st, "expect");
    if d1.code() != expect {
        let note = st
            .get("note")
            .and_then(Value::as_str)
            .map_or(String::new(), |n| format!(" [{n}]"));
        return Err(format!(
            "{kind} {}: expected {expect}, got {} ({}){note}",
            str_of(entry, "id"),
            d1.code(),
            d1.reason()
        ));
    }
    if let (Verdict::Allow, Some(parsed)) = (&d1, parsed) {
        let record = Record {
            entry: parsed,
            id: str_of(entry, "id").to_owned(),
            seq: prev.map_or(0, |s| s.head) + 1,
            time: time.to_owned(),
        };
        let s1 = fold(prev, &record, &ctx).map_err(|e| format!("fold failed: {e}"))?;
        let s2 = fold(prev, &record, &ctx).map_err(|e| format!("fold failed: {e}"))?;
        if s1 != s2 {
            return Err("fold nondeterministic".to_owned());
        }
        states.insert(space.clone(), s1);
    }
    if let Some(checks) = st.get("check").and_then(Value::as_object) {
        let have_state = states
            .get(&space)
            .map(json::to_value)
            .transpose()
            .map_err(|e| e.to_string())?;
        for (path, want) in checks {
            let missing = Value::Str("<missing>".to_owned());
            let have = have_state
                .as_ref()
                .and_then(|s| resolve(s, path))
                .unwrap_or(&missing);
            if have != want {
                return Err(format!(
                    "check {path}: expected {} got {}",
                    show(want),
                    show(have)
                ));
            }
        }
    }
    Ok(())
}

/// The encoding vectors (`00-*`): the preimage of an entry id.
fn preimage_vector(vec: &Value) -> (usize, Vec<(usize, String)>) {
    let (Some(entry), Some(want)) = (
        vec.get("entry").and_then(Value::as_object),
        vec.get("preimage").and_then(Value::as_str),
    ) else {
        return (0, vec![(0, "neither steps nor a preimage".to_owned())]);
    };
    let mut bare = entry.clone();
    bare.remove("sig");
    match json::canonicalize(&Value::Object(bare)) {
        Ok(b) if format!("han-entry-v1\n{}", String::from_utf8_lossy(&b)) == want => (1, vec![]),
        _ => (0, vec![(0, "preimage mismatch".to_owned())]),
    }
}
