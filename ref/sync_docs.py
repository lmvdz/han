"""Keeps every count and status line in the docs equal to what the checks say (round 22, E9).

The docs mark each derived value as `<!--s:key-->value<!--/s-->`. This script computes every key from
  - ref/list_tests.py        what exists: the implementation's tests, the clean-room interop scenarios, the wire vectors;
  - ref/RESULTS.md           what the last run_all.py run measured (vectors, mutations, fuzz, wire scenarios, ...);
  - interop/RESULTS.md       the cross-implementation run and its closure section (recorded runs of the wire vectors);
  - HISTORY.md               the round number and the scores;
  - the sources of impl/     three claims that used to be written by hand (the PreToolUse gate, the advisory status, a Matrix test of §8.2)
and rewrites the marked values in architecture.md, README.md, coverage.md and ref/README.md.
  python ref/sync_docs.py             rewrite the docs
  python ref/sync_docs.py --check     change nothing; list every value that drifted and exit 1 (run_all.py runs this)
  python ref/sync_docs.py --facts     print the derived facts
A value that cannot be derived (a needed row of RESULTS.md is missing or not PASS, a figure disagrees with another source) is an error, not a guess."""
import glob, os, re, sys
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import list_tests as LT

DOCS = ["architecture.md", "README.md", "coverage.md", os.path.join("ref", "README.md")]
MARK = re.compile(r"<!--s:([a-z_0-9]+)-->(.*?)<!--/s-->", re.S)

class Underivable(Exception): pass

def read(*path): return open(os.path.join(ROOT, *path), encoding="utf-8").read()

def table_rows(text):
    """Cells of every markdown table row in `text`."""
    return [[c.strip() for c in l.strip().strip("|").split("|")] for l in text.splitlines() if l.strip().startswith("|")]

# ── what exists (list_tests.py) ──
def facts_exist(F):
    tests = LT.py_tests()
    F["impl_n"] = sum(t[4] for t in tests)
    F["impl_files"] = len({t[0] for t in tests})
    F["impl_live"] = sum(t[4] for t in tests if t[3])
    F["ts_n"] = len(LT.ts_scenarios())
    wires = LT.wire_vectors()
    F["wv_n"] = len(wires)
    F["wv_ops"] = sum(1 for n, _ in wires if n[:2].isdigit())
    F["wv_probes"] = sum(1 for n, _ in wires if n.startswith("probes.json::"))
    F["wv_decided"] = sum(1 for n, _ in wires if n.startswith("probes-decided.json::"))
    F["gate_tests"] = sum(t[4] for t in tests if t[0] == "test_host_claude_gate.py")
    return {t[1] for t in tests}, {n.split("::", 1)[1] for n, _ in wires}

# ── what the last run measured (ref/RESULTS.md) ──
def facts_results(F):
    rows = {r[0]: r for r in table_rows(read("ref", "RESULTS.md")) if len(r) == 3}
    def row(prefix):
        hit = [r for k, r in rows.items() if k.startswith(prefix)]
        if len(hit) != 1: raise Underivable(f"ref/RESULTS.md has no single row starting {prefix!r}")
        if hit[0][1] != "PASS": raise Underivable(f"ref/RESULTS.md: {prefix!r} is {hit[0][1]!r}; run python ref/run_all.py and fix it first")
        return hit[0][2]
    def num(prefix, rx):
        m = re.search(rx, row(prefix))
        if not m: raise Underivable(f"ref/RESULTS.md: cannot read {prefix!r} with {rx!r}")
        return m
    m = num("Conformance vectors", r"(\d+)/(\d+) vectors pass \((\d+) steps\)")
    if m[1] != m[2]: raise Underivable("ref/RESULTS.md: not every vector passes")
    F["vec_n"], F["vec_steps"] = int(m[2]), int(m[3])
    m = num("Policy mutations", r"(\d+)/(\d+) policy mutations")
    if m[1] != m[2]: raise Underivable("ref/RESULTS.md: a policy mutation was missed")
    F["mut_pol"] = int(m[2])
    m = num("Fold totality", r"(\d+) seeds × (\d+) steps")
    F["fuzz_n"], F["fuzz_steps"] = f"{int(m[1]):,}", int(m[2])
    m = num("Wire mutations", r"(\d+)/(\d+) wire mutations")
    if m[1] != m[2]: raise Underivable("ref/RESULTS.md: a wire mutation was missed")
    F["wire_mut"] = int(m[2])
    m = num("Signed layer", r"(\d+)/(\d+) wire scenarios")
    if m[1] != m[2]: raise Underivable("ref/RESULTS.md: a wire scenario fails")
    F["wire_scen"] = int(m[2])
    m = num("Guard model mutations", r"(\d+)/(\d+) guard mutations")
    if m[1] != m[2]: raise Underivable("ref/RESULTS.md: a guard mutation was missed")
    F["guard_mut"] = int(m[2])
    m = num("Wire conformance vectors", r"(\d+) vectors, (\d+) steps, (\d+) receipts")
    F["wv_steps"], F["wv_receipts"] = int(m[2]), int(m[3])
    if int(m[1]) != F["wv_n"]: raise Underivable(f"ref/RESULTS.md says {m[1]} wire vectors, list_tests.py finds {F['wv_n']}: RESULTS.md is stale, run python ref/run_all.py --quick")

# ── interop/RESULTS.md ──
def facts_interop(F):
    text = read("interop", "RESULTS.md")
    rows = table_rows(text)
    d = {r[0]: r for r in rows if r[0] in ("ts->py", "py->ts", "ts->ts", "py->py") and len(r) == 4}
    if len(d) != 4: raise Underivable("interop/RESULTS.md: the direction totals table is missing")
    t = int(d["ts->py"][3])
    F["interop_matrix"] = f"ts->py {d['ts->py'][1]}/{t}, py->ts {d['py->ts'][1]}/{t} ({d['py->ts'][2]} without a workaround)"
    m = re.search(r"Probes matching the spec reading: Python server (\d+)/(\d+), TS server (\d+)/(\d+)", text)
    if not m: raise Underivable("interop/RESULTS.md: the probe totals line is missing")
    F["interop_probes"] = f"Python server {m[1]}/{m[2]}, TS server {m[3]}/{m[4]}"
    # the closure section: C01..Cnn rows and the recorded wire-vector runs
    m = re.search(r"round 21 resolved all (\d+) failures", text)
    if not m: raise Underivable("interop/RESULTS.md: the closure section is missing")
    closed = [r for r in rows if re.fullmatch(r"C\d\d", r[0])]
    if len(closed) != int(m[1]): raise Underivable(f"interop/RESULTS.md: the closure says {m[1]} failures, its table lists {len(closed)}")
    if any(not r[-1].startswith("resolved") for r in closed if len(r) >= 5): raise Underivable("interop/RESULTS.md: a closure row is not resolved")
    F["closure_n"] = int(m[1])
    runs = {}
    for r in rows:
        if r[0].startswith("wire vectors, ") and len(r) == 3:
            runs[r[0][len("wire vectors, "):]] = r[1:]
    for key, tag in (("end of round 21", "r21"), ("round 22", "r22")):
        if key not in runs: raise Underivable(f"interop/RESULTS.md: no recorded wire-vector run {key!r}")
        py, ts = runs[key]
        F[f"wv_{tag}_py"], F[f"wv_{tag}_ts"] = py, ts
        for who, s in (("Python", py), ("TS", ts)):
            if not re.fullmatch(r"\d+/\d+ vectors, \d+/\d+ expectations", s): raise Underivable(f"interop/RESULTS.md: cannot read {who} run {s!r}")
    a = re.fullmatch(r"(\d+)/(\d+) vectors, (\d+)/(\d+) expectations", F["wv_r22_ts"])      # the latest recorded run covers the vectors that exist now; round 21's is history
    if int(a[2]) != F["wv_n"]: raise Underivable(f"interop/RESULTS.md records {a[2]} wire vectors for the latest run (round 22), list_tests.py finds {F['wv_n']}: record a new run")
    F["wv_exp"] = int(a[4])
    b = re.fullmatch(r"(\d+)/(\d+) vectors, (\d+)/(\d+) expectations", F["wv_r22_py"])
    F["wv_r22_py_fails"] = int(b[2]) - int(b[1])

# ── HISTORY.md: the round number and the scores ──
def facts_history(F):
    rounds = {}
    for r in table_rows(read("HISTORY.md")):
        if len(r) >= 3 and r[0].isdigit(): rounds[int(r[0])] = r[1]
    if not rounds: raise Underivable("HISTORY.md: no round rows")
    F["round"] = max(rounds)
    sc = {n: float(c) for n, c in rounds.items() if re.fullmatch(r"\d+(\.\d+)?", c)}
    pend = sorted(n for n, c in rounds.items() if "pending" in c)
    early = [sc[n] for n in range(1, 7) if n in sc]
    mid = [f"{sc[n]}" for n in range(8, 17) if n in sc]
    late = [n for n in sc if n >= 18]
    if not early or not mid or not late: raise Underivable("HISTORY.md: scores of rounds 1-6, 8-16 or 18+ are missing")
    parts = []
    for i, n in enumerate(sorted(late)):
        v = f"**{sc[n]}**" if n == max(late) else f"{sc[n]}"
        parts.append(f"round {n} {v}" + (" (round 17 was folded into it)" if n == 18 and "folded" in rounds.get(17, "") else ""))
    tail = parts[0] if len(parts) == 1 else ", ".join(parts[:-1]) + " and " + parts[-1]
    s = f"rounds 1–6 from {early[0]} to {early[-1]} / 100, rounds 8–16 (after the scope change) {', '.join(mid)}, {tail}"
    if pend: s += " (" + (f"round {pend[0]}" if len(pend) == 1 else "rounds " + ", ".join(map(str, pend[:-1])) + f" and {pend[-1]}") + " not graded yet)"
    F["scores"] = s

# ── claims that used to be hand-written, now read off the sources ──
def facts_claims(F, test_names):
    def src(*p):
        path = os.path.join(ROOT, *p)
        return open(path, encoding="utf-8").read() if os.path.exists(path) else ""
    hook = "PreToolUse" in src("impl", "han", "host", "agent.py") and "PreToolUse" in src("impl", "tests", "test_host_claude_gate.py")
    F["gate_claim"] = ("the implementation's gate runs on a `PreToolUse` hook that sees every call, with `canUseTool` as a second check (`test_host_claude_gate.py`)" if hook
                       else "the implementation's gate sits on `canUseTool`, which does not see every call (§8.4 requires a `PreToolUse` hook)")
    adv = "ADVISORY" in src("impl", "han", "host", "host.py") and "test_guarded_status_says_it_is_advisory_without_a_sandbox" in test_names
    F["advisory_claim"] = ("the host says so in its `status` items (`test_host_p15.py::test_guarded_status_says_it_is_advisory_without_a_sandbox`)" if adv
                           else "the host does not yet say so in `status`")
    matrix_impl = "/_han/v1/entry" in src("impl", "han", "matrix", "appservice.py")
    matrix_test = any(re.search(r"v1/entry|id, kind, author, seq", open(p, encoding="utf-8").read()) for p in glob.glob(os.path.join(ROOT, "impl", "tests", "test_matrix_*.py")))
    shape = "the §8.2 shape of this round (whole-entry upload, `{id}`-only room events, `{id, kind, author, seq}` from the sequencer)"
    F["matrix_claim"] = (f"{shape} is asserted by a Matrix test" if matrix_test else
                         f"{shape} is implemented but no Matrix test asserts it" if matrix_impl else f"{shape} is not implemented")

def check_vector_names(arch, vector_ids):
    """The wire-vector names the G-07…G-11 row of §10 cites must exist."""
    row = next((l for l in arch.splitlines() if l.startswith("| G-07 … G-11")), "")
    cited = set(re.findall(r"\b([PD]\d+[a-z]?)\b", row.split("**Results", 1)[0]))
    missing = sorted(c for c in cited if c not in vector_ids)
    if missing: raise Underivable(f"architecture.md §10 cites wire vectors that do not exist: {', '.join(missing)}")

def derive():
    F = {}
    names, vector_ids = facts_exist(F)
    facts_results(F); facts_interop(F); facts_history(F); facts_claims(F, names)
    check_vector_names(read("architecture.md"), vector_ids)
    return F

def sync(F, check):
    drift, unknown = [], []
    for doc in DOCS:
        path = os.path.join(ROOT, doc)
        text = open(path, encoding="utf-8", newline="").read()      # newline="": the docs keep their line endings
        def sub(m):
            k = m[1]
            if k not in F:
                unknown.append(f"{doc}: <!--s:{k}-->"); return m[0]
            new = str(F[k])
            if new != m[2]: drift.append(f"{doc}: {k}: {m[2]!r} -> {new!r}")
            return f"<!--s:{k}-->{new}<!--/s-->"
        out = MARK.sub(sub, text)
        if out != text and not check:
            with open(path, "w", encoding="utf-8", newline="") as fh: fh.write(out)
    return drift, unknown

def main():
    check, facts = "--check" in sys.argv, "--facts" in sys.argv
    try:
        F = derive()
    except Underivable as ex:
        print(f"sync_docs: cannot derive the facts: {ex}")
        return 1
    if facts:
        for k in sorted(F): print(f"{k} = {F[k]}")
        return 0
    drift, unknown = sync(F, check)
    for u in unknown: print(f"sync_docs: unknown marker {u}")
    if check:
        for d in drift: print(f"DRIFT {d}")
        print(f"docs drifted: {len(drift)} value(s); run python ref/sync_docs.py" if drift or unknown else f"docs match the checks ({sum(len(MARK.findall(open(os.path.join(ROOT, d), encoding='utf-8').read())) for d in DOCS)} derived values in {len(DOCS)} files)")
        return 1 if drift or unknown else 0
    print(f"sync_docs: rewrote {len(drift)} value(s)" if drift else "sync_docs: nothing to change")
    return 1 if unknown else 0

if __name__ == "__main__":
    sys.exit(main())
