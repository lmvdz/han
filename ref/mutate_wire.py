"""Shows the wire scenarios have teeth: each mutation breaks one rule in wire.py; run_wire.py MUST then fail.
Runs patched copies in subprocesses; ref/wire.py on disk is never changed."""
import os, subprocess, sys, tempfile, shutil
HERE = os.path.dirname(os.path.abspath(__file__))
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass

MUTATIONS = [  # (rule, description, exact text in wire.py, replacement)
    ("R-01", "dedupe after validation", '''        if S:                                                               # 1 dedupe first (R-01)
            for rec in S["records"]:
                if rec["receipt"]["id"] == eid: return "ok", rec["receipt"]''', '''        pass'''),
    ("R-09", "extra header fields accepted", 'if not isinstance(entry, dict) or set(entry) != ENTRY_FIELDS: return "header fields"',
              'if not isinstance(entry, dict) or not ENTRY_FIELDS <= set(entry): return "header fields"'),
    ("R-09", "plan digest unchecked", 'if hashlib.sha256(P.jcs(content["plan"])).hexdigest() != entry["body"]["planDigest"]: return "planDigest mismatch"', "pass"),
    ("R-13", "blobs served without supply", 'if (space, ref["hash"]) not in self.supplied or ref["hash"] not in self.blobs: return "unavailable", None',
              'if ref["hash"] not in self.blobs: return "unavailable", None'),
    ("R-14", "mirror trusts any signer as home", 'if r["home"] != expected: return fault("receipt from a server that is not the home")', "pass"),
    ("R-14", "mirror skips policy replay", 'if not d["allow"]: return fault(f"home accepted a forbidden entry: {d[\'reason\']}")', "pass"),
    ("R-14", "mirror skips key windows", 'if not k or r["time"] < k["from"] or ("until" in k and r["time"] >= k["until"]): return False', 'if not k: return False'),
    ("R-03a", "time may decrease", 'if prev and r["time"] < prev["time"]: return fault("receipt time decreased")', "pass"),
    ("R-10", "profile pins may go backwards", 'if n < S["lastProfiles"].get(a, 0): return fault(f"profile seq for {a} went backwards")', "pass"),
    ("§7.3", "moves ignored when finding the home", '        if rec["entry"]["kind"] == "space.move": home = rec["entry"]["body"]["to"]', '        pass'),
    ("R-15", "no gossip proofs", 'if equivocation_proof(mine, pr, server_keys): proofs.append(("equivocation", mine, pr))', "pass"),
    ("R-20", "notices for non-members", 'named += [a for a in b["to"] if state["roles"].get(a) in P.READ_ROLES]', 'named += b["to"]'),
    ("R-22", "strangers may signal", 'if not P.can_read(S["state"], actor, S["state"]["head"]): return "denied", "cannot read this space"', "pass"),
    ("R-22", "anyone may follow a profile", 'if space[len("profile:"):] != as_: return "denied", "only the actor may follow its own profile, for its notices"', "pass"),
    ("R-08", "durability downgrade", 'return "unavailable", "minDurability not reachable"', 'pass'),
    ("§3", "hello ignores the server name", 'return verify_sig(pub, proof, ("han-hello-v1\\n" + challenge + "\\n" + server).encode())',
              'return any(verify_sig(pub, proof, ("han-hello-v1\\n" + challenge + "\\n" + s_).encode()) for s_ in (server, "a.example"))'),
    ("R-04", "redaction needs no proof", 'if not rb: return fault("redacted without proof")', "pass"),
    ("§7.3", "home ignores move acceptance", '                return "invalid", "no valid acceptance from the target server"', "                pass"),
    ("§7.3", "mirror ignores move acceptance", 'return fault("move without the target\'s acceptance")', "pass"),
    ("R-15", "heads to any peer", 'if S0 and (peer is None or P.can_read(S0["state"], "@:" + peer, S0["state"]["head"])):', "if S0:"),
    ("R-22", "servers relay anyone's signals", 'return "denied", "a server may relay signals only from its own actors"', "pass"),
    ("R-21", "completion by claim, not bytes", 'done = len(buf) == ref["size"] and hashlib.sha256(bytes(buf)).hexdigest() == ref["hash"]', "done = True"),
    ("R-03a", "slow new home stamps earlier time", 'if S and now < S["records"][-1]["receipt"]["time"]: return "unavailable", "clock behind the chain; wait (R-03a)"', "pass"),
    ("R-08", "policy may demand impossible durability", 'return "unavailable", "this home can\'t meet the requested minDurability"', "pass"),
]

def main():
    src = open(os.path.join(HERE, "wire.py"), encoding="utf-8").read()
    missed = 0
    for rule, desc, old, new in MUTATIONS:
        if src.count(old) != 1:
            print(f"BAD ANCHOR {rule} {desc}"); missed += 1; continue
        d = tempfile.mkdtemp()
        try:
            for f in ("policy.py", "run_wire.py"):
                shutil.copy(os.path.join(HERE, f), d)
            open(os.path.join(d, "wire.py"), "w", encoding="utf-8").write(src.replace(old, new))
            r = subprocess.run([sys.executable, os.path.join(d, "run_wire.py")], capture_output=True, text=True,
                               encoding="utf-8", errors="replace", env=dict(os.environ, PYTHONIOENCODING="utf-8"))
            last = (r.stdout.strip().splitlines() or ["(no output)"])[-1]
            caught = r.returncode != 0
            missed += not caught
            print(f"{'caught' if caught else 'MISSED'}  {rule:6} {desc:38} {last}")
        finally:
            shutil.rmtree(d, ignore_errors=True)
    print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} wire mutations caught by the scenarios")
    return 1 if missed else 0

if __name__ == "__main__":
    sys.exit(main())
