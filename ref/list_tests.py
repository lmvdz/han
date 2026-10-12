"""Lists the implementation's tests (impl/tests/*.py), the clean-room TypeScript interop scenarios (cleanroom-ts/interop.ts) and the wire
conformance vectors (ref/wire-vectors/) with the rule tags found in their docstrings or names, so architecture.md §10 can be checked against what actually exists.
ref/sync_docs.py imports py_tests, ts_scenarios and wire_vectors from here to derive the counts in the docs.
  python ref/list_tests.py            one line per test, then totals per file
  python ref/list_tests.py --rules    rule -> tests (the reverse index)
Reads source only; imports and runs nothing."""
import ast, glob, json, os, re, sys
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
TAG = re.compile(r"\b(?:[RPXTG]-\d+[a-z]?|E[1-6]|INV-\d+|§\d+(?:\.\d+)?)")

def tags(text): return sorted(set(TAG.findall(text or "")), key=lambda t: (t[0], t))

def cases(fn):
    """How many cases pytest collects for `fn`: the product of the literal lists of its parametrize decorators."""
    n = 1
    for d in fn.decorator_list:
        if (isinstance(d, ast.Call) and isinstance(d.func, ast.Attribute) and d.func.attr == "parametrize"
                and len(d.args) > 1 and isinstance(d.args[1], (ast.List, ast.Tuple))): n *= len(d.args[1].elts)
    return n

def py_tests():
    """(file, test name, tags, skipped-by-default, cases) for every test function and test method."""
    out = []
    for path in sorted(glob.glob(os.path.join(ROOT, "impl", "tests", "test_*.py"))):
        src = open(path, encoding="utf-8").read()
        mod = ast.parse(src)
        live = bool(re.search(r"HAN_LIVE|skipif|pytest\.skip|importorskip", src))
        def visit(nodes, prefix=""):
            for n in nodes:
                if isinstance(n, ast.ClassDef) and n.name.startswith("Test"): visit(n.body, n.name + "::")
                elif isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and n.name.startswith("test_"):
                    doc = ast.get_docstring(n) or ""
                    out.append((os.path.basename(path), prefix + n.name, tags(doc or n.name), live and "live" in os.path.basename(path), cases(n)))
        visit(mod.body)
    return out

def ts_scenarios():
    path = os.path.join(ROOT, "cleanroom-ts", "interop.ts")
    if not os.path.exists(path): return []
    src = open(path, encoding="utf-8").read()
    names = re.findall(r'scenario\(\s*(?:"((?:[^"\\]|\\.)*)"|`((?:[^`\\]|\\.)*)`)', src)
    return [(a or b, tags(a or b)) for a, b in names]

def wire_vectors():
    """(file::id, rule tags) for every wire conformance vector (ref/wire-vectors/, architecture.md G-11)."""
    d = os.path.join(HERE, "wire-vectors")
    if not os.path.exists(os.path.join(d, "index.json")): return []
    out = []
    for fn in json.load(open(os.path.join(d, "index.json"), encoding="utf-8"))["files"]:
        doc = json.load(open(os.path.join(d, fn), encoding="utf-8"))
        for v in (doc["vectors"] if "vectors" in doc else [dict(doc, id=doc["name"])]):
            out.append((f"{fn}::{v['id']}", sorted({t for r in v.get("rules", []) for t in TAG.findall(r)})))
    return out

FAMILIES = (("operations (golden)", lambda fn: fn[:2].isdigit()), ("agreed probes", lambda fn: fn == "probes.json"), ("decided probes", lambda fn: fn == "probes-decided.json"),
            ("round-22 probes", lambda fn: fn == "probes-r22.json"), ("round-23 probes", lambda fn: fn == "probes-r23.json"))

def wire_expectations():
    """Per vector family: (name, files, vectors, expectations, steps, generated appends). An expectation is what the runner counts as one: an `expect`, `expect_close`,
    `expect_silence`, `http_get` or `clock` step. A `generate` step is one step, its `count` appends are checked by the runner and are not expectations."""
    d = os.path.join(HERE, "wire-vectors")
    if not os.path.exists(os.path.join(d, "index.json")): return []
    rows = {name: [name, 0, 0, 0, 0, 0] for name, _ in FAMILIES}
    for fn in json.load(open(os.path.join(d, "index.json"), encoding="utf-8"))["files"]:
        doc = json.load(open(os.path.join(d, fn), encoding="utf-8"))
        fam = next((name for name, test in FAMILIES if test(fn)), None)
        if fam is None: raise SystemExit(f"list_tests: {fn} belongs to no wire-vector family; add it to FAMILIES")
        rows[fam][1] += 1
        for v in (doc["vectors"] if "vectors" in doc else [doc]):
            rows[fam][2] += 1
            for st in v["steps"]:
                rows[fam][4] += 1
                rows[fam][3] += any(k in st for k in ("expect", "expect_close", "expect_silence", "http_get", "clock"))
                rows[fam][5] += st["generate"]["count"] if "generate" in st else 0
    return [tuple(r) for r in rows.values() if r[2]]

def main():
    tests, scen, wires = py_tests(), ts_scenarios(), wire_vectors()
    if "--rules" in sys.argv:
        idx = {}
        for f, n, t, *_ in tests:
            for x in t: idx.setdefault(x, []).append(f"{f}::{n}")
        for n, t in scen:
            for x in t: idx.setdefault(x, []).append(f"interop.ts: {n}")
        for n, t in wires:
            for x in t: idx.setdefault(x, []).append(f"wire-vectors/{n}")
        for x in sorted(idx): print(f"{x:8} {len(idx[x]):3}  " + "; ".join(idx[x][:4]) + (" …" if len(idx[x]) > 4 else ""))
        return 0
    for f, n, t, live, k in tests:
        print(f"{f}::{n}  [{' '.join(t) or '-'}]" + (f"  ({k} cases)" if k > 1 else "") + ("  (live, skipped by default)" if live else ""))
    print()
    for n, t in scen: print(f"interop.ts: {n}  [{' '.join(t) or '-'}]")
    print()
    for n, t in wires: print(f"wire-vectors/{n}  [{' '.join(t) or '-'}]")
    print()
    for name, files, nv, ne, ns, ng in wire_expectations(): print(f"{nv:4} vectors, {ne:5} expectations, {ns:5} steps  {name} ({files} file{'s' if files != 1 else ''})" + (f"; {ng} generated appends, not expectations" if ng else ""))
    print()
    per = {}
    for f, _n, _t, _l, k in tests: per[f] = per.get(f, 0) + k
    for f, c in per.items(): print(f"{c:4}  {f}")
    print(f"{sum(t[4] for t in tests)} impl tests collected by pytest ({len(tests)} test functions) in {len(per)} files "
          f"({sum(t[4] for t in tests if t[3])} skipped by default as live); "
          f"{len(scen)} cleanroom-ts interop scenarios; {len(wires)} wire conformance vectors; {sum(1 for t in tests if not t[2])} tests carry no rule tag")
    return 0

if __name__ == "__main__":
    sys.exit(main())
