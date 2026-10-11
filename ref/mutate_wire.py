"""Shows the wire scenarios have teeth: each mutation breaks one rule in wire.py; run_wire.py MUST then fail.
Runs patched copies in subprocesses; ref/wire.py on disk is never changed."""
import os, subprocess, sys, tempfile, shutil
HERE = os.path.dirname(os.path.abspath(__file__))
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass

SIG = '        if not verify_entry(entry, profiles[entry["author"]]): return "invalid", "signature"\n'
POLICY5 = '        if not skip_policy and wait <= 0:                                   # 5'
LIMIT6 = """        if self.limiter:                                                    # 6
            retry = self.limiter(entry["author"])
            if retry: return "limit", {"reason": "rate limit", "retryAfterMs": retry}
"""
RECEIPT7 = '        prev = S["records"][-1]["receipt"] if S else None                    # 7'
MUTATIONS = [  # (rule, description, exact text in wire.py, replacement); text and replacement may both be tuples of several edits
    ("R-01", "dedupe after validation", '''        if S:                                                               # 1 dedupe first (R-01)
            for rec in S["records"]:
                if rec["receipt"]["id"] == eid: return "ok", rec["receipt"]''', '''        pass'''),
    ("R-09", "extra header fields accepted", 'if not isinstance(entry, dict) or set(entry) != ENTRY_FIELDS: return "header fields"',
              'if not isinstance(entry, dict) or not ENTRY_FIELDS <= set(entry): return "header fields"'),
    ("R-09", "plan digest unchecked", 'if hashlib.sha256(P.jcs(content["plan"])).hexdigest() != entry["body"]["planDigest"]: return "planDigest mismatch"', "pass"),
    ("G-01", "blobs served without supply", 'if (space, ref["hash"]) not in self.supplied or ref["hash"] not in self.blobs: return "unavailable", None',
              'if ref["hash"] not in self.blobs: return "unavailable", None'),
    ("R-14", "mirror trusts any signer as home", 'if r["home"] != expected: return fault("receipt from a server that is not the home")', "pass"),
    ("R-14", "mirror skips policy replay", 'if not d["allow"]: return fault(f"home accepted a forbidden entry: {d[\'reason\']}")', "pass"),
    ("R-14", "mirror skips key windows", 'if not k or r["time"] < k["from"] or ("until" in k and r["time"] >= k["until"]): return False', 'if not k: return False'),
    ("R-03a", "time may decrease", 'if prev and r["time"] < prev["time"]: return fault("receipt time decreased")', "pass"),
    ("R-10", "profile pins may go backwards", 'if n < S["lastProfiles"].get(a, 0): return fault(f"profile seq for {a} went backwards")', "pass"),
    ("§7.3", "moves ignored when finding the home", '        if rec["entry"]["kind"] == "space.move": home = rec["entry"]["body"]["to"]', '        pass'),
    ("R-15", "no gossip proofs", 'if equivocation_proof(mine, pr, server_keys): proofs.append(("equivocation", mine, pr))', "pass"),
    ("G-03", "notices for non-members", 'named += [a for a in b["to"] if state["roles"].get(a) in P.READ_ROLES]', 'named += b["to"]'),
    ("G-04", "strangers may signal", 'if not P.can_read(S["state"], actor, S["state"]["head"]): return "denied", "cannot read this space"', "pass"),
    ("G-04", "anyone may follow a profile", 'if space[len("profile:"):] != as_: return "denied", "only the actor may follow its own profile, for its notices"', "pass"),
    ("R-08", "durability downgrade", 'return "unavailable", "minDurability not reachable"', 'pass'),
    ("§3", "hello ignores the server name", 'return verify_sig(pub, proof, ("han-hello-v1\\n" + challenge + "\\n" + server).encode())',
              'return any(verify_sig(pub, proof, ("han-hello-v1\\n" + challenge + "\\n" + s_).encode()) for s_ in (server, "a.example"))'),
    ("R-04", "redaction needs no proof", 'if not rb: return fault("redacted without proof")', "pass"),
    ("§7.3", "home ignores move acceptance", '                return "invalid", "no valid acceptance from the target server"', "                pass"),
    ("§7.3", "mirror ignores move acceptance", 'return fault("move without the target\'s acceptance")', "pass"),
    ("R-15", "heads to any peer", 'if S0 and (peer is None or P.can_read(S0["state"], "@:" + peer, S0["state"]["head"])):', "if S0:"),
    ("G-04", "servers relay anyone's signals", 'return "denied", "a server may relay signals only from its own actors"', "pass"),
    ("G-01", "completion by claim, not bytes", 'done = len(buf) == ref["size"] and hashlib.sha256(bytes(buf)).hexdigest() == ref["hash"]', "done = True"),
    ("R-03a", "slow new home stamps earlier time", 'if S and now < S["records"][-1]["receipt"]["time"]: return "unavailable", "clock behind the chain; wait (R-03a)"', "pass"),
    ("R-08", "policy may demand impossible durability", 'return "unavailable", "this home can\'t meet the requested minDurability"', "pass"),
    ("R-16", "early abandon answered denied", 'return "unavailable", {"reason": "abandon before the launch window closed (R-16)"', 'return "denied", {"reason": "abandon before the launch window closed (R-16)"'),
    ("R-16", "no elapsed-time check on the steady clock", 'steady_wait = A - (self.steady() - started)', 'steady_wait = 0'),
    ("R-16", "a restart forgets nothing", 'started = self.claim_steady.get(c["id"], self.run_start)', 'started = self.claim_steady.get(c["id"], -10**9)'),
    ("R-16", "an abandon that can never be allowed is still unavailable", '        if not P.decide(state, dict(entry, id=eid), {"time": fmt(due), "profiles": pinned})["allow"]: return 0', "        pass"),
    ("P-01", "all approval candidates are pinned", '            if found_valid and a not in required: continue', "            pass"),
    ("P-02", "state at judged on the state at `at`", 'if not P.can_read(st, as_, where):', 'if not P.can_read(self.state_at(space, where) or st, as_, where):'),
    ("§3", "state at ignores `at`", 'view = st if at is None or at == st["head"] else self.state_at(space, at)', 'view = st'),
    ("§3", "state at needs rights at the head", 'where = st["head"] if at is None else at', 'where = st["head"]'),
    ("§3", "follow ignores limit", 'if P.can_read(st, as_, r["receipt"]["seq"])][:limit]', 'if P.can_read(st, as_, r["receipt"]["seq"])]'),
    ("R-15", "fault adopted without checking its proof", 'if pr.get("kind") == "equivocation" and len(rs) == 2 and equivocation_proof(rs[0], rs[1], server_keys): return "adopted", "equivocation"',
              'if pr.get("kind") == "equivocation" and len(rs) == 2: return "adopted", "equivocation"'),
    ("R-03a", "future-dated receipts not flagged", 'if now is not None and secs(r["time"]) > secs(now) + MAX_CLOCK_SKEW: return fault("future-time")', "pass"),
    ("R-03a", "future tolerance is 600 s", "MAX_CLOCK_SKEW = 60 ", "MAX_CLOCK_SKEW = 600 "),
    ("G-01", "context blobs readable by watchers", 'P.session_access(S["state"], as_, sid)["read"]', 'P.session_access(S["state"], as_, sid)["watch"]'),
    ("G-01", "context blobs supplied by anyone's upload", 'if as_ in self.uploads.get(h, ()): self.supplied.add((item["space"], h))', 'self.supplied.add((item["space"], h))'),
    # round 22: one order of verdicts (§4.1, E5), paged state (E8)
    ("§4.1", "routing answered before dedupe", '        if S:                                                               # 1 dedupe first (R-01)', '        if S and sp not in self.moved:                                      # 1 dedupe first (R-01)'),
    ("§4.1", "a foreign create is invalid, not moved", 'if not S and named and named != self.server: return "moved", named', 'if not S and named and named != self.server: return "invalid", "space id names another home"'),
    ("§4.1", "shape judged before a moved space", 'if sp in self.moved: return "moved", self.moved[sp]', 'if sp in self.moved and not validate_header(entry): return "moved", self.moved[sp]'),
    ("§4.1", "shape judged before a foreign space", 'if not S and named and named != self.server: return "moved", named', 'if not S and named and named != self.server and not validate_header(entry): return "moved", named'),
    ("§4.1", "the author's profile before shape", '        err = validate_header(entry)                                        # 3 shape', '        err = validate_header(entry) if entry.get("author") in profiles else None                    # 3 shape'),
    ("R-12", "an unknown keyId is invalid", 'return "denied", "no valid key"', 'return "invalid", "no valid key"'),
    ("§4.1", "signature after Policy", (SIG, LIMIT6), ("", '        if not verify_entry(entry, profiles[entry["author"]]): return "invalid", "signature"\n' + LIMIT6)),
    ("§4.1", "other profiles pinned before the signature", (SIG, '        if pinned is None: return "unavailable", "profile"\n'), ("", '        if pinned is None: return "unavailable", "profile"\n' + SIG)),
    ("§4.1", "rate limit before Policy", POLICY5, '        if self.limiter and self.limiter(entry["author"]): return "limit", {"reason": "rate limit", "retryAfterMs": 1500}\n' + POLICY5),
    ("§4.1", "rate limit after the moment-dependent unavailable", (LIMIT6, RECEIPT7), ("", LIMIT6 + RECEIPT7)),
    ("§4.1", "durability judged before Policy", POLICY5, '        if (state or {}).get("policy", {}).get("minDurability") == "replicated" and self.durability != "replicated" and entry["kind"] not in ("policy.set", "space.move"): return "unavailable", "minDurability not reachable"\n' + POLICY5),
    ("§4.1", "slow clock judged before Policy", POLICY5, '        if S and now < S["records"][-1]["receipt"]["time"]: return "unavailable", "clock behind the chain; wait (R-03a)"\n' + POLICY5),
    ("§3", "state pages ignore maxStateItems", 'page = dict(view, effects=dict(es[oe:oe + n]), tasks=dict(ts_[ot:ot + n]))', 'page = dict(view)'),
    ("§3", "state page size taken from limit", 'n, (oe, ot) = self.max_state_items, offs', 'n, (oe, ot) = limit, offs'),
    ("§3", "cursor pages restart from the first", 'at, offs = int(m[1]), (int(m[2]), int(m[3]))', 'at, offs = int(m[1]), (0, 0)'),
    ("§3", "cursor head not checked", 'if not m or not 1 <= int(m[1]) <= st["head"]: return "invalid", "cursor"', 'if not m: return "invalid", "cursor"'),
    ("§3", "effect read returns every effect", 'dict(one, effects={effect: view["effects"][effect]})', 'dict(one, effects=view["effects"])'),
    ("R-25", "a non-canonical public key is accepted", 'return "publicKey not canonical base64url"', 'pass'),
    ("R-25", "a public key of the wrong length is accepted", 'if len(unb64(k)) != 32: return "publicKey length"', 'pass'),
    ("§3", "effect read of an unknown effect is empty", 'if not (isinstance(effect, str) and effect in view["effects"]): return "not_found", "no such effect"', 'pass'),
]

def main():
    src = open(os.path.join(HERE, "wire.py"), encoding="utf-8").read()
    missed = 0
    for rule, desc, old, new in MUTATIONS:
        olds, news = (old, new) if isinstance(old, tuple) else ((old,), (new,))
        if any(src.count(o) != 1 for o in olds):
            print(f"BAD ANCHOR {rule} {desc}"); missed += 1; continue
        mutated = src
        for o, n_ in zip(olds, news): mutated = mutated.replace(o, n_)
        d = tempfile.mkdtemp()
        try:
            for f in ("policy.py", "run_wire.py"):
                shutil.copy(os.path.join(HERE, f), d)
            open(os.path.join(d, "wire.py"), "w", encoding="utf-8").write(mutated)
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
