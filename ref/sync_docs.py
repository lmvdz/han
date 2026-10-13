"""Keeps every count and status line in the docs equal to what the checks say (round 22, E9).

The docs mark each derived value as `<!--s:key-->value<!--/s-->`. This script computes every key from
  - ref/list_tests.py        what exists: the implementation's tests, the clean-room interop scenarios, the wire vectors;
  - ref/RESULTS.md           what the last run_all.py run measured (vectors, mutations, fuzz, wire scenarios, ...);
  - conformance/interop/RESULTS.md       the cross-implementation run and its closure section (recorded runs of the wire vectors);
  - ref/logs/*               the newest complete log of each model (guard model: depth, transitions; timing model: the round it last ran on, its counts);
  - conformance/wire-vectors/        the expectations per vector family (list_tests.wire_expectations);
  - conformance/carrier-vectors/     the files, cases and mutations of ref/run_carrier_vectors.py, which it runs (round 31);
  - spec/decisions/DECISIONS-rNN.md the round being built (the newest such file, H9); spec/decisions/HISTORY.md the scores of the rounds already graded;
  - the sources of witnesses/impl-py/     two claims that used to be written by hand (the PreToolUse gate, the advisory status)
and rewrites the marked values in spec/core/architecture.md, README.md, spec/goal.md, spec/core/coverage.md, ref/README.md and conformance/wire-vectors/README.md.
  python ref/sync_docs.py             rewrite the docs
  python ref/sync_docs.py --check     change nothing; list every value that drifted and exit 1 (run_all.py runs this)
  python ref/sync_docs.py --facts     print the derived facts
A value that cannot be derived (a needed row of RESULTS.md is missing or not PASS, a figure disagrees with another source) is an error, not a guess.
So is a test the docs cite as `test_*.py::test_*` that witnesses/impl-py/tests does not have (R8): both modes fail and name it."""
import glob, os, re, sys
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import list_tests as LT

DOCS = [os.path.join("spec", "core", "architecture.md"), "README.md", os.path.join("spec", "goal.md"), os.path.join("spec", "core", "coverage.md"), os.path.join("ref", "README.md"), os.path.join("conformance", "wire-vectors", "README.md")]
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
    fam = LT.wire_expectations()
    F["wv_exp"] = sum(f[3] for f in fam)
    F["wv_generated"] = f"{sum(f[5] for f in fam):,}"
    F["wv_families"] = "; ".join(f"{f[2]} {f[0]}: {f[3]:,} expectations" + (f" (and {f[5]:,} generated appends the runner checks, not counted)" if f[5] else "") for f in fam)
    F["wv_family_ops"], F["wv_family_ops_exp"] = next((f[1], f[3]) for f in fam if f[0].startswith("operations"))
    for key, name in (("agreed", "agreed probes"), ("decided", "decided probes"), ("r22", "round-22 probes"), ("r23", "round-23 probes"), ("r24", "round-24 probes"), ("r25", "round-25 probes"), ("r27", "round-27 probes")):
        row = next((f for f in fam if f[0] == name), None)
        F[f"wv_{key}_vec"], F[f"wv_{key}_exp"] = (row[2], row[3]) if row else (0, 0)
    return {t[1] for t in tests}, {n.split("::", 1)[1] for n, _ in wires}

# ── han.ts: how many calls, functions and kinds the interfaces declare (K8) ──
WORDS = {1: "one", 2: "two", 3: "three", 4: "four", 5: "five", 6: "six", 7: "seven", 8: "eight", 9: "nine", 10: "ten"}
def word(n): return WORDS.get(n, str(n))

def facts_interfaces(F):
    ts = read("spec", "core", "han.ts")
    def body(head):
        i = ts.find(head + " {")
        if i < 0: raise Underivable(f"han.ts: {head!r} is missing")
        j = ts.index("\n}", i)
        return ts[i:j]
    def members(head):
        """Names declared at the first level of an interface body (two spaces of indent; comment and continuation lines start otherwise)."""
        return re.findall(r'^  "?([A-Za-z_.]+)"?\??[(:]', body(head), re.M)
    F["kinds_n"] = len(members("export interface Bodies"))
    core = members("export interface Calls")
    F["calls_core"] = len(core)
    F["calls_core_w"] = word(len(core))
    log, pol, ident, ver, tb = (members(f"export interface {n}") for n in ("Log", "Policy", "Identity", "Verifier", "TransportBinding"))
    F["log_fns"], F["policy_fns"], F["policy_fns_w"], F["identity_fns"], F["verifier_fns"], F["binding_fns"], F["binding_fns_w"] = len(log), len(pol), word(len(pol)), len(ident), len(ver), len(tb), word(len(tb))
    F["limits_w"] = word(len(re.findall(r"(?<![\w/])max[A-Za-z]+\s*:", body("export interface Limits"))))
    actor = len(members("export interface Actor"))
    F["actor_fns"] = actor
    F["ledger_total"] = len(log) + len(pol) + len(ident) + len(ver) + 2 * actor + len(tb)
    F["ports_total"] = F["ledger_total"]

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
    num("Guard model mutations", r"(\d+)/(\d+) guard mutations")      # must be PASS; the counts come from the newest guard log (facts_models)
    m = num("Wire conformance vectors", r"(\d+) vectors, (\d+) steps, (\d+) receipts")
    F["wv_steps"], F["wv_receipts"] = int(m[2]), int(m[3])
    if int(m[1]) != F["wv_n"]: raise Underivable(f"ref/RESULTS.md says {m[1]} wire vectors, list_tests.py finds {F['wv_n']}: RESULTS.md is stale, run python ref/run_all.py --quick")

# ── conformance/interop/RESULTS.md ──
def facts_interop(F):
    text = read("conformance", "interop", "RESULTS.md")
    rows = table_rows(text)
    d = {r[0]: r for r in rows if r[0] in ("ts->py", "py->ts", "ts->ts", "py->py") and len(r) == 4}
    if len(d) != 4: raise Underivable("conformance/interop/RESULTS.md: the direction totals table is missing")
    t = int(d["ts->py"][3])
    F["interop_matrix"] = f"ts->py {d['ts->py'][1]}/{t}, py->ts {d['py->ts'][1]}/{t} ({d['py->ts'][2]} without a workaround)"
    m = re.search(r"Probes matching the spec reading: Python server (\d+)/(\d+), TS server (\d+)/(\d+)", text)
    if not m: raise Underivable("conformance/interop/RESULTS.md: the probe totals line is missing")
    F["interop_probes"] = f"Python server {m[1]}/{m[2]}, TS server {m[3]}/{m[4]}"
    # the closure section: C01..Cnn rows and the recorded wire-vector runs
    m = re.search(r"round 21 resolved all (\d+) failures", text)
    if not m: raise Underivable("conformance/interop/RESULTS.md: the closure section is missing")
    closed = [r for r in rows if re.fullmatch(r"C\d\d", r[0])]
    if len(closed) != int(m[1]): raise Underivable(f"conformance/interop/RESULTS.md: the closure says {m[1]} failures, its table lists {len(closed)}")
    if any(not r[-1].startswith("resolved") for r in closed if len(r) >= 5): raise Underivable("conformance/interop/RESULTS.md: a closure row is not resolved")
    F["closure_n"] = int(m[1])
    runs = {}
    for r in rows:
        if r[0].startswith("wire vectors, ") and len(r) == 3:
            runs[r[0][len("wire vectors, "):]] = r[1:]
    for key, tag in (("end of round 21", "r21"), ("round 22", "r22")):
        if key not in runs: raise Underivable(f"conformance/interop/RESULTS.md: no recorded wire-vector run {key!r}")
        py, ts = runs[key]
        F[f"wv_{tag}_py"], F[f"wv_{tag}_ts"] = py, ts
        for who, s in (("Python", py), ("TS", ts)):
            if not re.fullmatch(r"\d+/\d+ vectors, \d+/\d+ expectations", s): raise Underivable(f"conformance/interop/RESULTS.md: cannot read {who} run {s!r}")
    latest = max((int(k.split()[1]) for k in runs if re.fullmatch(r"round \d+", k)), default=0)       # the newest recorded run covers the vectors that exist now; earlier ones are history
    py, ts = runs[f"round {latest}"]
    F["wv_latest_round"], F["wv_latest_py"], F["wv_latest_ts"] = latest, py, ts
    for who, s_ in (("Python", py), ("TS", ts)):
        if not re.fullmatch(r"\d+/\d+ vectors, \d+/\d+ expectations", s_): raise Underivable(f"conformance/interop/RESULTS.md: cannot read the round {latest} {who} run {s_!r}")
        a = re.fullmatch(r"(\d+)/(\d+) vectors, (\d+)/(\d+) expectations", s_)
        if int(a[2]) > F["wv_n"] or int(a[4]) > F["wv_exp"]: raise Underivable(f"conformance/interop/RESULTS.md records more wire vectors or expectations for round {latest} ({who}: {s_}) than the vector files hold ({F['wv_n']}, {F['wv_exp']})")
    a = re.fullmatch(r"(\d+)/(\d+) vectors, (\d+)/(\d+) expectations", py)
    F["wv_latest_note"] = ("These are the vectors of the current files." if (int(a[2]), int(a[4])) == (F["wv_n"], F["wv_exp"]) else
                           f"The vector files have changed since: {F['wv_n']} vectors and {F['wv_exp']} expectations now, {a[2]} and {a[4]} when that run was recorded (round {latest}). Vectors that read the state were rewritten for the call `state` and others were added in round {F['round']}; none of that has been run against either server yet, so a server that does not implement the rules they pin fails them.")
    rounds = [int(a or b) for a, b in re.findall(r"^### 6\.\d+ (?:What the round-(\d+) refresh|Round (\d+))", text, re.M)]
    if not rounds: raise Underivable("conformance/interop/RESULTS.md: no section '6.N Round NN' or 'What the round-NN refresh changed and found'; the round of the last matrix run cannot be derived")
    F["interop_round"] = max(rounds)
    b = re.fullmatch(r"(\d+)/(\d+) vectors, (\d+)/(\d+) expectations", F["wv_r22_py"])
    F["wv_r22_py_fails"] = int(b[2]) - int(b[1])
    for who, s_ in (("py", py), ("ts", ts)):
        a = re.fullmatch(r"(\d+)/(\d+) vectors, (\d+)/(\d+) expectations", s_)
        F[f"wv_latest_{who}_fails"] = int(a[2]) - int(a[1])

# ── HISTORY.md: the round number and the scores ──
def facts_history(F):
    rounds = {}
    for r in table_rows(read("spec", "decisions", "HISTORY.md")):
        if len(r) >= 3 and r[0].isdigit(): rounds[int(r[0])] = r[1]
    if not rounds: raise Underivable("HISTORY.md: no round rows")
    decided = [int(m[1]) for f in glob.glob(os.path.join(ROOT, "spec", "decisions", "DECISIONS-r*.md")) if (m := re.fullmatch(r"DECISIONS-r(\d+)\.md", os.path.basename(f)))]
    if not decided: raise Underivable("spec/decisions/: no DECISIONS-rNN.md; the round being built cannot be derived")
    F["round"] = max(decided + [max(rounds)])      # the round being built: the newest decisions file (HISTORY.md only lists graded rounds)
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

# ── ref/logs: what the model checkers last did ──
def logs(prefix):
    """[(round, depth or 0, name, text)] for ref/logs/<prefix>*-rNN.log, newest first."""
    out = []
    for path in glob.glob(os.path.join(HERE, "logs", prefix + "*.log")):
        name = os.path.basename(path)
        m = re.fullmatch(prefix + r"(?:-depth(\d+))?-r(\d+)\.log", name)
        if m: out.append((int(m[2]), int(m[1] or 0), name, open(path, encoding="utf-8").read()))
    return sorted(out, reverse=True)

def link(name, plain=False): return f"`ref/logs/{name}`" if plain else f"[ref/logs/{name}](ref/logs/{name})"      # plain: for ref/README.md, where that relative link would be wrong

def facts_models(F):
    """The guard model and the timing model, as the logs show them. A log counts only if it is complete; a newer log that is not (out of memory, aborted) is named, never skipped silently.
    The `rNN` in a log's name is the round of the Policy it ran on."""
    GUARD = re.compile(r"^\[guard\] (\d+) transitions to depth (\d+); (.*) HOLD", re.M)
    runs = [(r, d, n, t, GUARD.search(t)) for r, d, n, t in logs("model_guard")]
    # M9: "last complete depth-4 run" is the newest log that completed depth 4 cleanly: a [guard] line for depth 4 and no NOT COMPLETED
    clean4 = [(r, n, int(m[1])) for r, d, n, t, m in runs if m and int(m[2]) == 4 and "NOT COMPLETED" not in t]
    if not clean4: raise Underivable("ref/logs has no model_guard log that completed depth 4 cleanly: run python ref/model_guard.py --depth 4 > ref/logs/model_guard-depth4-rNN.log")
    r4, n4, t4 = clean4[0]
    # the mutation count: the newest ref/logs/model_guard-mutations-rNN.log, written by python ref/model_guard.py --all-mutations --depth 4
    muts = sorted(((int(re.search(r"-r(\d+)\.log$", f)[1]), f) for f in glob.glob(os.path.join(HERE, "logs", "model_guard-mutations-r*.log"))), reverse=True)
    if not muts: raise Underivable("ref/logs has no model_guard-mutations-rNN.log")
    mm = re.search(r"(\d+)/(\d+) guard mutations caught", open(muts[0][1], encoding="utf-8").read())
    if not mm or mm[1] != mm[2]: raise Underivable(f"ref/logs/{os.path.basename(muts[0][1])}: the mutation count (N/N guard mutations caught) is missing or not all caught")
    src = read("ref", "model_guard.py")
    tail = src[src.index('print(f"[guard]'):]
    names = re.findall(r"[A-Z][A-Za-z]+", tail[:tail.index("HOLD")].split("depth {a.depth}; ", 1)[1])
    F["guard4_round"], F["guard4_trans"], F["guard4_props"], F["guard_mut"] = r4, f"{t4:,}", len(names), int(mm[2])
    F["guard4_log"], F["guard4_log_plain"] = link(n4), link(n4, True)
    import model_guard                                        # the log must describe the mutations the code has now
    if len(model_guard.MUTATIONS) != int(mm[2]): raise Underivable(f"ref/model_guard.py has {len(model_guard.MUTATIONS)} mutations, ref/logs/{os.path.basename(muts[0][1])} says {mm[2]}: re-run python ref/model_guard.py --all-mutations --depth 4")
    F["guard_props"] = F["guard4_props"]
    # depth 5 is opt-in: name the newest complete deep run, if any, and say plainly whether the current Policy has been explored that deep
    GUARD_MODEL_SINCE = 27                                    # round 27 rewrote the model (no holds): older deep runs explored another model
    deep = [(r, d, n, m) for r, d, n, t, m in runs if r >= GUARD_MODEL_SINCE and m and int(m[2]) >= 5 and "NOT COMPLETED" not in t]
    undone = [(r, d, n) for r, d, n, t, m in runs if r >= GUARD_MODEL_SINCE and d >= 5 and ("NOT COMPLETED" in t or not m)]
    for plain in (False, True):
        if not deep:
            F["guard_deep_plain" if plain else "guard_deep"] = f"no deeper run is recorded; the current Policy (round {F['round']}) has not been explored to depth 5"; continue
        r, d, n, m = deep[0]
        later = [f"round {r2}'s depth-{d2} run did not complete ({link(n2, plain)})" for r2, d2, n2 in undone if r2 >= r]
        F["guard_deep_plain" if plain else "guard_deep"] = (f"the newest complete deep run is depth {m[2]}, {int(m[1]):,} transitions, on the round-{r} Policy ({link(n, plain)})"
                       + ("; " + "; ".join(later) if later else "") + (f"; the current Policy (round {F['round']}) has not been explored to depth {m[2]}" if r < F["round"] else ""))
    # the timing model
    MC = [(r, d, n, t) for r, d, n, t in logs("model_check") if "all mutations caught" in t and all(f"[{k}]" in t and "ALL PROPERTIES HOLD" in t for k in ("timing", "drift", "single"))]
    if not MC: raise Underivable("ref/logs has no complete model_check log")
    r, _d, n, t = MC[0]
    states = {k: int(re.search(r"\[%s\] (\d+) states explored; ALL PROPERTIES HOLD" % k, t)[1]) for k in ("timing", "drift", "single")}
    muts = len(re.findall(r"^caught ", t, re.M))
    F["timing_round"] = r
    F["timing_muts"], F["timing_muts_w"] = muts, word(muts)
    F["timing_log"] = link(n)
    F["timing_counts"] = f"timing ({states['timing']:,} states), drift ({states['drift']:,}), single ({states['single']:,}), all properties hold; {muts}/{muts} mutations caught"
    F["timing_status"] = (f"The log is by the round in its name: `ref/policy.py` has been edited since (the current round is {F['round']}), the model has not been re-run on it, and this is a round-{r} result, not a statement about the current Policy" if r < F["round"]
                          else f"It ran on the Policy of the current round ({r})")
    F["timing_run"] = f"round {r}, {F['timing_log']}"
    F["timing_run_plain"] = f"round {r}, {link(n, True)}"
    F["timing_mark"] = "✓" if r >= F["round"] else "partial"

# ── claims that used to be hand-written, now read off the sources ──
def facts_claims(F, test_names):
    def src(*p):
        path = os.path.join(ROOT, *p)
        return open(path, encoding="utf-8").read() if os.path.exists(path) else ""
    hook = "PreToolUse" in src("witnesses", "impl-py", "han", "host", "agent.py") and "PreToolUse" in src("witnesses", "impl-py", "tests", "test_host_claude_gate.py")
    F["gate_claim"] = ("the implementation's gate runs on a `PreToolUse` hook that sees every call, with `canUseTool` as a second check (`test_host_claude_gate.py`)" if hook
                       else "the implementation's gate sits on `canUseTool`, which does not see every call (§8.4 requires a `PreToolUse` hook)")
    adv = "ADVISORY" in src("witnesses", "impl-py", "han", "host", "host.py") and "test_guarded_status_says_it_is_advisory_without_a_sandbox" in test_names
    F["advisory_claim"] = ("the host says so in its `status` items (`test_host_p15.py::test_guarded_status_says_it_is_advisory_without_a_sandbox`)" if adv
                           else "the host does not yet say so in `status`")
    # round 30: the Matrix binding left the spec (§8.3 is informative), so its claim is no longer derived

def check_vector_names(arch, vector_ids):
    """The wire-vector names the G-07…G-11 row of §10 cites must exist."""
    row = next((l for l in arch.splitlines() if l.startswith("| G-07 … G-11")), "")
    cited = set(re.findall(r"\b([PD]\d+[a-z]?)\b", row.split("**Recorded runs", 1)[0]))
    missing = sorted(c for c in cited if c not in vector_ids)
    if missing: raise Underivable(f"architecture.md §10 cites wire vectors that do not exist: {', '.join(missing)}")

CITED_TEST = re.compile(r"\b(test_[A-Za-z0-9_]+\.py)::((?:Test[A-Za-z0-9_]+::)?test_[A-Za-z0-9_]+)")

def check_test_names():
    """R8: every `test_*.py::test_*` the docs name must be a test that exists (witnesses/impl-py/tests, read by list_tests.py as pytest collects it)."""
    have = {(f, n) for f, n, *_ in LT.py_tests()}
    missing = sorted({f"{d}: {f}::{n}" for d in DOCS for f, n in CITED_TEST.findall(read(d)) if (f, n) not in have})
    if missing: raise Underivable("the docs name tests that do not exist: " + "; ".join(missing))

# ── conformance/carrier-vectors (round 31, X16): run here, offline and in a second, since run_all.py does not run them yet ──
def facts_carrier(F):
    import io, contextlib
    import run_carrier_vectors as RC
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf): rc = RC.main()
    last = buf.getvalue().strip().splitlines()[-1]
    m = re.fullmatch(r"(\d+)/(\d+) carrier vector files pass \((\d+) cases\)", last)
    if rc or not m or m[1] != m[2]: raise Underivable(f"conformance/carrier-vectors: python ref/run_carrier_vectors.py does not pass: {last}")
    F["cv_files"], F["cv_cases"], F["cv_muts"] = int(m[2]), int(m[3]), len(RC.MUTATIONS)

def derive():
    F = {}
    names, vector_ids = facts_exist(F)
    facts_interfaces(F); facts_results(F); facts_history(F); facts_interop(F); facts_models(F); facts_claims(F, names); facts_carrier(F)
    check_vector_names(read("spec", "core", "architecture.md"), vector_ids)
    check_test_names()
    return F

def sync(F, check):
    drift, unknown = [], []
    for doc in DOCS:
        path = os.path.join(ROOT, doc)
        text = open(path, encoding="utf-8", newline="").read()      # newline="": the docs keep their line endings
        up = os.path.relpath(ROOT, os.path.dirname(path)).replace(os.sep, "/")      # a doc below the root links to ref/logs/ from its own directory
        up = "" if up == "." else up + "/"
        def sub(m):
            k = m[1]
            if k not in F:
                unknown.append(f"{doc}: <!--s:{k}-->"); return m[0]
            new = str(F[k]).replace("](ref/", "](" + up + "ref/")
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
