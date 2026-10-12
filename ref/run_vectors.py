"""Runs ref/vectors/*.json against a Policy implementation (default: ref/policy.py).

Vector format (language-neutral; see README.md):
  profiles: {actor: ProfileView-without-actor/seq}       initial pinned profile views
  steps: one of
    {entry, time, expect: allow|denied|invalid, check?, profiles?, predecessor?}
        decide(state[entry.space], entry, ctx); if allowed, fold with receipt {seq: head+1, time}.
        `profiles` overrides persist for later steps. `predecessor` names the space whose current state is ctx.predecessor.
        `check` paths are dotted (keys may themselves contain dots, e.g. actor ids) and read the entry's space state.
    {read: {space, actor, seq}, expect: bool}             can_read
    {relevantActors: entry, expect: [actor]}              relevant_actors(state[entry.space], entry)
    {guardStatus: {space, actor}, expect: {guard?, held, guarded, forced}}   guard_status(state[space], ctx, actor), ctx = the profiles in force
    {sessionAccess: {space, actor, session}, expect: {watch, read, steer}}   session_access(state[space], actor, session)
Every decide/fold is also checked for purity (inputs unchanged) and determinism (two runs agree).
"""
import copy, glob, hashlib, json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as P
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass

def resolve(obj, path):
    toks = path.split(".")
    i = 0
    while i < len(toks):
        if isinstance(obj, list):
            obj = obj[int(toks[i])]; i += 1; continue
        for j in range(len(toks), i, -1):
            k = ".".join(toks[i:j])
            if isinstance(obj, dict) and k in obj:
                obj = obj[k]; i = j; break
        else:
            raise KeyError(path)
    return obj

def run_vector(vec):
    fails = []
    if "preimage" in vec:  # encoding vector
        e = {k: v for k, v in vec["entry"].items() if k != "sig"}
        pre = b"han-entry-v1\n" + P.jcs(e)
        if pre.decode() != vec["preimage"]: fails.append("preimage mismatch")
        if P.entry_id(vec["entry"]) != hashlib.sha256(vec["preimage"].encode()).hexdigest(): fails.append("id mismatch")
        return fails
    profiles = {a: dict(v, actor=a, seq=1) for a, v in vec["profiles"].items()}
    states = {}
    for n, st in enumerate(vec["steps"], 1):
        if "read" in st:
            r = st["read"]
            got = P.can_read(states[r["space"]], r["actor"], r["seq"])
            if got != st["expect"]: fails.append(f"step {n} read {r}: expected {st['expect']} got {got}")
            continue
        if "sessionAccess" in st:
            q = st["sessionAccess"]
            got = P.session_access(states[q["space"]], q["actor"], q["session"])
            if got != st["expect"]: fails.append(f"step {n} sessionAccess {q['actor']}: expected {st['expect']} got {got}")
            continue
        if "guardStatus" in st:
            q = st["guardStatus"]
            got = P.guard_status(states[q["space"]], {"time": "2026-10-10T12:00:00Z", "profiles": copy.deepcopy(profiles)}, q["actor"])
            if got != st["expect"]: fails.append(f"step {n} guardStatus {q['actor']}: expected {st['expect']} got {got}")
            continue
        if "relevantActors" in st:
            e = st["relevantActors"]
            got = P.relevant_actors(states.get(e["space"]), e)
            if got != st["expect"]: fails.append(f"step {n} relevantActors: expected {st['expect']} got {got}")
            continue
        for a, v in st.get("profiles", {}).items():
            profiles[a] = dict(v, actor=a, seq=profiles.get(a, {}).get("seq", 1) + 1)
        e = st["entry"]
        sp = e["space"]
        ctx = {"time": st["time"], "profiles": copy.deepcopy(profiles)}
        if "predecessor" in st: ctx["predecessor"] = copy.deepcopy(states[st["predecessor"]])
        before = copy.deepcopy(states.get(sp)); ctx_before = copy.deepcopy(ctx)
        d1 = P.decide(states.get(sp), e, ctx)
        d2 = P.decide(copy.deepcopy(before), copy.deepcopy(e), copy.deepcopy(ctx_before))
        if states.get(sp) != before or ctx != ctx_before: fails.append(f"step {n}: decide mutated its inputs")
        if d1 != d2: fails.append(f"step {n}: decide not deterministic")
        got = "allow" if d1["allow"] else d1["code"]
        if got != st["expect"]:
            fails.append(f"step {n} {e['kind']} {e['id']}: expected {st['expect']} got {got} ({d1.get('reason', '')})" +
                         (f" [{st['note']}]" if "note" in st else ""))
            continue
        if d1["allow"]:
            head = states[sp]["head"] if sp in states else 0
            rec = {"entry": e, "content": None, "receipt": {"space": sp, "seq": head + 1, "id": e["id"], "time": st["time"]}}
            s1 = P.fold(states.get(sp), rec, ctx)
            s2 = P.fold(copy.deepcopy(before), copy.deepcopy(rec), copy.deepcopy(ctx_before))
            if states.get(sp) != before: fails.append(f"step {n}: fold mutated its input")
            if s1 != s2: fails.append(f"step {n}: fold not deterministic")
            states[sp] = s1
        for path, want in st.get("check", {}).items():
            try:
                have = resolve(states[sp], path)
            except (KeyError, IndexError, ValueError):
                have = "<missing>"
            if have != want: fails.append(f"step {n} check {path}: expected {want!r} got {have!r}")
    return fails

def main():
    files = sorted(glob.glob(os.path.join(HERE, "vectors", "*.json")))
    total_steps = bad = 0
    for f in files:
        vec = json.load(open(f, encoding="utf-8"))
        total_steps += len(vec.get("steps", [1]))
        try:
            fails = run_vector(vec)
        except Exception as ex:          # an implementation that crashes fails the vector
            fails = [f"exception: {type(ex).__name__}: {ex}"]
        if fails:
            bad += 1
            print(f"FAIL {os.path.basename(f)}")
            for x in fails: print("   ", x)
    print(f"{len(files) - bad}/{len(files)} vectors pass ({total_steps} steps)")
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main())
