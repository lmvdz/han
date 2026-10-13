"""Checks that the three descriptions of the entry kinds agree field by field:
  architecture.md §5 table  ·  han.ts `Bodies` / `Contents` / `PolicyDoc`  ·  ref/policy.py `SCHEMA`
Optional fields: `x?` in the table and han.ts ⇔ required=False in SCHEMA.
"""
import os, re, sys
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import policy as P

def ts_block(src, name):
    i = src.index(f"export interface {name} {{") + len(f"export interface {name} {{")
    depth, j = 1, i
    while depth:
        depth += {"{": 1, "}": -1}.get(src[j], 0); j += 1
    return src[i:j - 1]

def top_fields(obj_src):
    """Top-level `name?:` / `name:` keys of a TS object-type body."""
    out, depth, i = {}, 0, 0
    while i < len(obj_src):
        c = obj_src[i]
        if c in "{([": depth += 1
        elif c in "})]": depth -= 1
        elif depth == 0:
            m = re.match(r'\s*"?([A-Za-z.]+)"?(\?)?\s*:', obj_src[i:])
            if m and (i == 0 or obj_src[i - 1] in "{;\n ,"):
                out[m.group(1)] = m.group(2) != "?"
                i += m.end(); continue
        i += 1
    return out

def kinds(block):
    """`"kind": <type>;` entries of Bodies/Contents → {kind: {field: required} | None}."""
    res, i = {}, 0
    for m in re.finditer(r'"([a-z.]+)":\s*', block):
        if m.start() < i: continue
        j = m.end()
        if block[j:j + 4] == "null":
            res[m.group(1)] = None; i = j; continue
        if block[j] == "{":
            depth, k = 0, j
            while True:
                depth += {"{": 1, "}": -1}.get(block[k], 0); k += 1
                if depth == 0: break
            res[m.group(1)] = top_fields(block[j + 1:k - 1]); i = k
    return res

def md_table():
    md = open(os.path.join(ROOT, "spec", "core", "architecture.md"), encoding="utf-8").read()
    sec = md[md.index("## 5. Entry kinds"):md.index("## 6. Policy")]
    rows = {}
    for line in sec.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) != 3 or not cells[0].startswith("`"): continue
        names = re.findall(r"`([a-z.]+)`", cells[0])
        def fields(cell):
            if cell in ("—", ""): return None
            return {f.rstrip("?"): not f.endswith("?") for f in re.findall(r"`([A-Za-z]+\??)`", cell)}
        if len(names) == 2:   # "key.add / key.revoke" row: split on " / "
            b = cell_split = cells[1].split(" / ")
            for n, bc in zip(names, cell_split): rows[n] = (fields(bc), fields(cells[2]))
        else:
            rows[names[0]] = (fields(cells[1]), fields(cells[2]))
    return rows

def block_after(src, marker):
    i = src.index(marker) + len(marker)
    while src[i] != "{": i += 1
    depth, j = 0, i
    while True:
        depth += {"{": 1, "}": -1}.get(src[j], 0); j += 1
        if depth == 0: return src[i + 1:j - 1]

def state_shapes(src):
    st = ts_block(src, "SpaceState")
    return {
        "state": top_fields(st),
        "task": top_fields(block_after(st[st.index("tasks:"):], "[id: EntryId]:")),
        "effect": top_fields(block_after(st, "[proposal: EntryId]:")),
        "approval": top_fields(block_after(st, "approvals:")),
        "claim": top_fields(block_after(st, "claims:")),
        "profile": top_fields(block_after(st, "profile?:")),
    }

def conform(obj, shape, where, errs):
    for k in obj:
        if k not in shape: errs.append(f"{where}: field '{k}' produced by ref fold is not in han.ts")
    for k, req in shape.items():
        if req and k not in obj: errs.append(f"{where}: required han.ts field '{k}' missing from ref fold")

def check_states(src, errs):
    """Fold every vector and check each resulting SpaceState against han.ts field by field."""
    import glob, json, copy
    import run_vectors
    shapes = state_shapes(src)
    seen = 0
    for f in sorted(glob.glob(os.path.join(ROOT, "conformance", "vectors", "*.json"))):
        vec = json.load(open(f, encoding="utf-8"))
        if "steps" not in vec: continue
        states = {}
        profiles = {a: dict(v, actor=a, seq=1) for a, v in vec["profiles"].items()}
        for st in vec["steps"]:
            if "entry" not in st: continue
            for a, v in st.get("profiles", {}).items(): profiles[a] = dict(v, actor=a, seq=2)
            e, sp = st["entry"], st["entry"]["space"]
            ctx = {"time": st["time"], "profiles": profiles}
            if "predecessor" in st: ctx["predecessor"] = states[st["predecessor"]]
            if not P.decide(states.get(sp), e, ctx)["allow"]: continue
            head = states[sp]["head"] if sp in states else 0
            states[sp] = P.fold(states.get(sp), {"entry": e, "content": None, "receipt": {"seq": head + 1, "time": st["time"]}}, ctx)
            S = states[sp]; seen += 1
            w = os.path.basename(f)
            conform(S, shapes["state"], f"{w} state", errs)
            for t in S["tasks"].values(): conform(t, shapes["task"], f"{w} task", errs)
            for eff in S["effects"].values():
                conform(eff, shapes["effect"], f"{w} effect", errs)
                for a in eff["approvals"]: conform(a, shapes["approval"], f"{w} approval", errs)
                for c in eff["claims"]: conform(c, shapes["claim"], f"{w} claim", errs)
            if "profile" in S: conform(S["profile"], shapes["profile"], f"{w} profile", errs)
    return seen

def main():
    src = open(os.path.join(ROOT, "spec", "core", "han.ts"), encoding="utf-8").read()
    bodies, contents = kinds(ts_block(src, "Bodies")), kinds(ts_block(src, "Contents"))
    table = md_table()
    errs = []
    schema = {k: {f: req for f, (_, req) in v.items()} for k, v in P.SCHEMA.items()}
    allk = set(bodies) | set(schema) | set(table) | set(contents)
    for k in sorted(allk):
        tb, tc = table.get(k, ("<missing>", "<missing>"))
        if bodies.get(k) != schema.get(k): errs.append(f"{k}: han.ts body {bodies.get(k)} != ref SCHEMA {schema.get(k)}")
        if tb != bodies.get(k): errs.append(f"{k}: §5 body {tb} != han.ts {bodies.get(k)}")
        if tc != contents.get(k, "<missing>"): errs.append(f"{k}: §5 content {tc} != han.ts {contents.get(k, '<missing>')}")
    pd = top_fields(ts_block(src, "PolicyDoc"))
    req = P.POLICY_FIELDS
    if set(pd) != req or not all(pd.values()): errs.append(f"PolicyDoc fields {sorted(pd)} != ref {sorted(req)}")
    n_states = check_states(src, errs)
    errs = list(dict.fromkeys(errs))
    for e in errs: print("MISMATCH", e)
    print(f"{len(allk)} kinds, PolicyDoc and {n_states} folded SpaceStates compared across architecture.md, han.ts, ref/policy.py: "
          + ("consistent" if not errs else f"{len(errs)} mismatches"))
    return 1 if errs else 0

if __name__ == "__main__":
    sys.exit(main())
