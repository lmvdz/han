"""Signed-layer and server-behaviour scenarios (real Ed25519). Each states its expected result from architecture.md.
Run: python ref/run_wire.py
"""
import base64, copy, hashlib, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass
import policy as P
import wire as W

O, M, X, Z = "@o:a.example", "@m:a.example", "@x:c.example", "@z:d.example"
S = "s1:a.example"
KEYS = {a: W.Key(a) for a in (O, M, X, Z)}
HOME_A, HOME_B, HOME_C, EVIL = W.Key("server:a.example"), W.Key("server:b.example"), W.Key("server:c.example"), W.Key("server:evil")
T0 = "2026-01-01T00:00:00Z"
SERVER_KEYS = {"a.example": {"h1": {"publicKey": HOME_A.pub, "from": T0}}, "b.example": {"h1": {"publicKey": HOME_B.pub, "from": T0}},
               "c.example": {"h1": {"publicKey": HOME_C.pub, "from": T0}}, "evil.example": {"h1": {"publicKey": EVIL.pub, "from": T0}}}
REVOKED = set()

def view(actor, seq=1):
    k = KEYS[actor]
    st = "revoked" if (actor, seq) in REVOKED else "valid"
    return {"actor": actor, "seq": seq, "actorKind": "service" if actor == X else "human", "operatorConfirmed": actor == X,
            "keys": {"k1": {"publicKey": k.pub, "status": st}}}

def profiles(seq=1): return {a: view(a, seq) for a in KEYS}

POLICY = {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any", "approvals": [],
          "separateApprover": False, "budget": None, "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30,
          "extensionsMinRole": "member", "servers": {"deny": []}}

N = [0]
def nonce():
    N[0] += 1
    return base64.b32encode(hashlib.sha256(str(N[0]).encode()).digest()).decode().lower()[:26]

def entry(kind, author, body, content=None, space=S, seen=0):
    e = {"v": 1, "space": space, "kind": kind, "author": author, "keyId": "k1", "body": body,
         "contentHash": W.content_hash(content), "refs": [], "seen": seen, "nonce": nonce(), "time": "2026-10-10T12:00:00Z"}
    return W.sign_entry(e, KEYS[author]), content

def ticking(start=0):
    t = iter(range(start, 100000))
    return lambda: "2026-10-10T12:%02d:%02dZ" % divmod(next(t), 60)

def base_home(network=None, clock=None, server="a.example", key=HOME_A, space=S):
    h = W.Home(server, key, clock=clock or ticking(), network=network, server_keys=SERVER_KEYS)
    for kind, author, body, content in [("space.create", O, {"policy": POLICY}, {"name": "room"}),
                                        ("member.set", O, {"actor": M, "role": "member"}, None),
                                        ("member.set", O, {"actor": X, "role": "member"}, None)]:
        e, c = entry(kind, author, body, content, space=space)
        code, r = h.append(e, c, profiles()); assert code == "ok", (code, r)
    return h

def mirror(): return W.Mirror(SERVER_KEYS, lambda a, n: view(a, n))

TARGET_KEYS = {"a.example": HOME_A, "b.example": HOME_B, "c.example": HOME_C}
def move(home, to):
    """A space.move entry carrying the target's signed acceptance of the current head (section 7.3)."""
    head = home.spaces[S]["records"][-1]["receipt"]
    return entry("space.move", O, {"to": to, "accept": {"keyId": "h1", "sig": W.accept_move(TARGET_KEYS[to], S, head)}})

RESULTS = []
def scenario(name, rule, expect_desc):
    def deco(fn):
        try:
            ok, detail = fn()
        except Exception as ex:
            ok, detail = False, f"{type(ex).__name__}: {ex}"
        RESULTS.append((ok, name, rule, expect_desc, detail))
        return fn
    return deco

@scenario("valid chain verifies", "R-02, R-14", "a mirror accepts every record of an honest home")
def _():
    h = base_home(); e, c = entry("message", M, {}, {"text": "hi"}); h.append(e, c, profiles())
    m = mirror(); res = [m.ingest(r) for r in h.spaces[S]["records"]]
    return all(ok for ok, _ in res), str(res[-1])

@scenario("dedupe precedes every check", "R-01", "resending an accepted entry returns its receipt even after the author's key is revoked; a new entry is denied")
def _():
    h = base_home(); e, c = entry("message", M, {}, {"text": "x"})
    code1, r1 = h.append(e, c, profiles())
    REVOKED.add((M, 2)); p2 = profiles(); p2[M] = view(M, 2)
    code2, r2 = h.append(e, c, p2)
    e2, c2 = entry("message", M, {}, {"text": "new"}); code3, _ = h.append(e2, c2, p2)
    REVOKED.discard((M, 2))
    return code1 == code2 == "ok" and r1 == r2 and code3 == "denied", f"{code1} {code2} same={r1 == r2} fresh={code3}"

@scenario("header is exact", "R-09", "an extra unsigned field, or a malformed nonce, is invalid at the home and rejected by mirrors")
def _():
    h = base_home(); e, c = entry("message", M, {}, {"text": "hi"})
    extra = dict(e, id="0" * 64)
    bad_nonce = dict(e, nonce="NOT-BASE32")
    a = h.append(extra, c, profiles())[0]; b = h.append(bad_nonce, c, profiles())[0]
    h.append(e, c, profiles())
    recs = copy.deepcopy(h.spaces[S]["records"]); recs[-1]["entry"]["id"] = "0" * 64
    m = mirror(); out = [m.ingest(r) for r in recs]
    return a == "invalid" and b == "invalid" and not out[-1][0], f"{a} {b} mirror={out[-1][1]}"

@scenario("tampered body rejected", "R-14", "a mirror rejects a record whose body changed after signing")
def _():
    h = base_home(); e, c = entry("message", M, {"to": [O]}, {"text": "hi"}); h.append(e, c, profiles())
    recs = copy.deepcopy(h.spaces[S]["records"]); recs[-1]["entry"]["body"]["to"] = [X]
    m = mirror(); out = [m.ingest(r) for r in recs]
    return (not out[-1][0]) and all(o[0] for o in out[:-1]), out[-1][1]

@scenario("reordering rejected", "R-02", "swapping two records breaks the chain")
def _():
    h = base_home()
    for t in ("a", "b"):
        e, c = entry("message", M, {}, {"text": t}); h.append(e, c, profiles())
    recs = h.spaces[S]["records"][:]; recs[-1], recs[-2] = recs[-2], recs[-1]
    m = mirror(); out = [m.ingest(r) for r in recs]
    return not out[-2][0], out[-2][1]

@scenario("forbidden accept caught by replay", "R-11, R-14", "a home that skips Policy is caught by the mirror")
def _():
    h = base_home(); e, c = entry("policy.set", M, {"policy": POLICY})
    code, r = h.append(e, c, profiles(), skip_policy=True)
    m = mirror(); out = [m.ingest(x) for x in h.spaces[S]["records"]]
    return code == "ok" and not out[-1][0] and "forbidden" in out[-1][1], out[-1][1]

@scenario("first home is pinned by the space id", "R-14", "a server that re-signs another home's genuine records is refused from seq 1")
def _():
    h = base_home()
    forged, prev = [], None
    for rec in h.spaces[S]["records"]:
        r = dict(rec["receipt"], home="evil.example", prev=W.receipt_hash(prev) if prev else "")
        r = W.sign_receipt(r, EVIL); prev = r
        forged.append(dict(rec, receipt=r))
    m = mirror(); ok, why = m.ingest(forged[0])
    return not ok and "not the home" in why, why

@scenario("server key validity window", "§4.3, R-14", "a receipt signed with a key outside its [from, until) window is rejected")
def _():
    keys = copy.deepcopy(SERVER_KEYS); keys["a.example"]["h1"]["until"] = "2026-10-10T12:00:02Z"
    h = base_home()                                              # receipts at 12:00:00, :01, :02
    m = W.Mirror(keys, lambda a, n: view(a, n)); out = [m.ingest(r) for r in h.spaces[S]["records"]]
    return out[0][0] and out[1][0] and not out[2][0], out[2][1]

@scenario("equivocation proof", "R-02", "two receipts for one seq from one home prove misbehaviour; from different homes they don't")
def _():
    h1 = base_home(); h2 = base_home()
    e1, c1 = entry("message", M, {}, {"text": "left"}); e2, c2 = entry("message", M, {}, {"text": "right"})
    _, r1 = h1.append(e1, c1, profiles()); _, r2 = h2.append(e2, c2, profiles())
    rb = W.sign_receipt(dict(r2, home="b.example"), HOME_B)
    return W.equivocation_proof(r1, r2, SERVER_KEYS) and not W.equivocation_proof(r1, rb, SERVER_KEYS), f"seq {r1['seq']}"

@scenario("head gossip finds a fork, privately", "R-15", "exchanging heads surfaces an equivocation proof; heads go only to peers that named the space and may read it")
def _():
    h1 = base_home(); h2 = base_home()
    e1, c1 = entry("message", M, {}, {"text": "left"}); e2, c2 = entry("message", M, {}, {"text": "right"})
    h1.append(e1, c1, profiles()); h2.append(e2, c2, profiles())
    mine, proofs = h1.hello(h2.heads(), SERVER_KEYS, peer="c.example")
    _, none = h1.hello(h1.heads(), SERVER_KEYS, peer="c.example")
    outsider, _ = h1.hello([dict(h1.heads()[0], seq=1)], SERVER_KEYS, peer="evil.example")
    unnamed, _ = h1.hello([], SERVER_KEYS, peer="c.example")
    return (len(proofs) == 1 and not none and outsider == [] and unnamed == [] and len(mine) == 1), f"{len(proofs)} proof(s), outsider={outsider}, unnamed={unnamed}"

@scenario("move then old-home receipt is a proof", "R-02, §7.3", "after space.move at n, an old-home receipt above n is a proof; the home answers `moved`; mirrors refuse it")
def _():
    h = base_home(); e, c = move(h, "b.example"); code, rm = h.append(e, c, profiles())
    e2, c2 = entry("message", M, {}, {"text": "late"}); code2, _ = h.append(e2, c2, profiles())
    forged = W.sign_receipt({"space": S, "seq": rm["seq"] + 1, "id": W.entry_id(e2), "prev": W.receipt_hash(rm), "home": "a.example",
                             "homeKey": "h1", "time": "2026-10-10T12:01:00Z", "durability": "local", "profiles": {M: 1}}, HOME_A)
    m = mirror(); [m.ingest(r) for r in h.spaces[S]["records"]]
    ok_m, why = m.ingest({"entry": e2, "content": c2, "receipt": forged})
    proof = W.wrong_home_proof(h.spaces[S]["records"], forged, SERVER_KEYS)
    return code2 == "moved" and proof and not ok_m, f"home={code2}; proof={proof}; mirror: {why}"

@scenario("new home continues the chain", "§7.3", "after a move the mirror accepts records signed by the new home, chained to the move")
def _():
    h = base_home(); e, c = move(h, "b.example"); h.append(e, c, profiles())
    hb = W.Home("b.example", HOME_B, clock=ticking(300), server_keys=SERVER_KEYS); hb.spaces[S] = copy.deepcopy(h.spaces[S])
    e2, c2 = entry("message", M, {}, {"text": "on b"}); code, rb = hb.append(e2, c2, profiles())
    m = mirror(); out = [m.ingest(r) for r in hb.spaces[S]["records"]]
    return code == "ok" and all(o[0] for o in out), str(out[-1])

@scenario("profile pin monotonic", "R-10", "a receipt pinning an older profile seq than an earlier receipt is rejected")
def _():
    h = base_home(); e, c = entry("message", M, {}, {"text": "a"}); h.append(e, c, profiles(seq=3))
    e2, c2 = entry("message", M, {}, {"text": "b"}); h.append(e2, c2, profiles(seq=2))
    m = mirror(); out = [m.ingest(r) for r in h.spaces[S]["records"]]
    return out[-2][0] and not out[-1][0], out[-1][1]

@scenario("receipt time non-decreasing", "R-03a", "a receipt with an earlier time than its predecessor is rejected (forged: an honest home refuses to make one)")
def _():
    h = base_home(clock=ticking(10))
    for t in ("a", "b"):
        e, c = entry("message", M, {}, {"text": t}); h.append(e, c, profiles())
    recs = copy.deepcopy(h.spaces[S]["records"])
    recs[-1]["receipt"] = W.sign_receipt(dict(recs[-1]["receipt"], time="2026-10-10T12:00:01Z"), HOME_A)
    m = mirror(); out = [m.ingest(r) for r in recs]
    return out[-2][0] and not out[-1][0] and "time" in out[-1][1], out[-1][1]

@scenario("content binding and redaction", "R-04, §4.1 step 3", "wrong content is invalid at the home and rejected by mirrors; redacted content still verifies")
def _():
    h = base_home(); e, c = entry("message", M, {}, {"text": "secret"})
    bad = h.append(e, {"text": "other"}, profiles())[0]
    h.append(e, c, profiles())
    recs = copy.deepcopy(h.spaces[S]["records"])
    m1 = mirror(); sw = copy.deepcopy(recs); sw[-1]["content"] = {"text": "other"}; out1 = [m1.ingest(r) for r in sw]
    er, cr = entry("redact", M, {"target": W.entry_id(e)}); h.append(er, cr, profiles())
    m2 = mirror(); out2 = [m2.ingest(r) for r in h.spaces[S]["records"]]
    fake = copy.deepcopy(recs); fake[-1]["content"], fake[-1]["redacted"] = None, True
    m3 = mirror(); out3 = [m3.ingest(r) for r in fake]
    return (bad == "invalid" and not out1[-1][0] and all(o[0] for o in out2) and h.spaces[S]["records"][-2]["redacted"]
            and not out3[-1][0]), f"home={bad} swapped={out1[-1][1]} redacted={out2[-2:]} fake={out3[-1][1]}"

@scenario("no silent downgrade", "R-08", "a home that can't currently reach the space's minDurability refuses ordinary appends with unavailable")
def _():
    h = base_home(); h.durability = "replicated"
    e, c = entry("policy.set", O, {"policy": dict(POLICY, minDurability="replicated")}); h.append(e, c, profiles())
    h.durability = "local"                               # e.g. a replica went down
    e2, c2 = entry("message", M, {}, {"text": "x"}); a = h.append(e2, c2, profiles())[0]
    h.durability = "replicated"; b = h.append(e2, c2, profiles())[0]
    return (a, b) == ("unavailable", "ok"), f"{a} {b}"

@scenario("hello proof", "§3", "the proof binds challenge and server")
def _():
    p = W.hello_proof(KEYS[M], "CHALLENGE", "a.example")
    return (W.verify_hello(KEYS[M].pub, p, "CHALLENGE", "a.example") and not W.verify_hello(KEYS[M].pub, p, "CHALLENGE", "b.example")
            and not W.verify_hello(KEYS[O].pub, p, "CHALLENGE", "a.example")), "binds"

@scenario("first contact across servers", "R-20, P-02", "adding a member delivers a notice to its server, which can then read the space as its server actor")
def _():
    net = {}
    hc = W.Home("c.example", HOME_C, network=net); hd = W.Home("d.example", W.Key("server:d"), network=net)
    h = base_home(network=net)
    e, c = entry("member.set", O, {"actor": Z, "role": "guest"}); h.append(e, c, profiles())
    got = hd.inbox.get(Z, []); gotx = hc.inbox.get(X, [])
    code, items = h.read(S, 0, "@:d.example")
    stranger = h.read(S, 0, "@:evil.example")
    return (bool(got) and got[-1]["body"]["space"] == S and bool(gotx) and code == "ok" and len(items) == 4
            and stranger[1] == []), f"notice={got[-1] if got else None} read={code}/{len(items)}"

@scenario("signals ride follow streams", "R-22, §3", "followers receive signals as ReadItems; strangers can neither follow nor signal")
def _():
    h = base_home()
    code, q = h.follow(S, O); denied = h.follow(S, Z)[0]
    a = h.signal(S, M, {"typing": True})[0]; b = h.signal(S, Z, {"typing": True})[0]
    e, c = entry("message", M, {}, {"text": "hi"}); h.append(e, c, profiles())
    kinds = [i["type"] for i in q]
    pn = h.signal("profile:@m:a.example", Z, {"type": "notice"})[0]
    relay_ok = h.signal(S, M, {"typing": True}, relayed_by="@:a.example")[0]
    relay_bad = h.signal(S, M, {"typing": True}, relayed_by="@:d.example")[0]
    return ((code, denied, a, b, pn, relay_ok, relay_bad) == ("ok", "denied", "ok", "denied", "denied", "ok", "denied")
            and kinds[:2] == ["signal", "record"]), f"{kinds} {a} {b} {pn} relay={relay_ok}/{relay_bad}"

@scenario("state needs read rights at head", "§3 read, P-02", "a former member reads old records but gets no current state")
def _():
    h = base_home()
    e, c = entry("member.set", O, {"actor": M, "role": "none"}); h.append(e, c, profiles())
    code, _ = h.read(S, 0, M, state=True)
    code2, items = h.read(S, 0, M)
    code3, items3 = h.read(S, 0, O, state=True)
    return (code == "denied" and code2 == "ok" and 0 < len(items) < 5 and items3[0]["type"] == "state"
            and "entries" not in items3[0]["state"]), f"{code} {code2}/{len(items)}"

@scenario("blob refs per kind and transfer", "R-13, R-21", "argsBlob in a plan is a reference; a server fetches as itself, skips a server with wrong bytes, and verifies the hash")
def _():
    net = {}
    data = b"patch bytes"; ref = {"hash": hashlib.sha256(data).hexdigest(), "size": len(data), "type": "text/x-diff"}
    h = base_home(network=net)
    hc = W.Home("c.example", HOME_C, network=net)
    plan = {"action": "git.push", "target": "repo", "argsBlob": ref}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    e, c = entry("effect.propose", M, body, {"plan": plan}); code = h.append(e, c, profiles())[0]
    rec = h.spaces[S]["records"][-1]
    h.blob_put(data, M)                                # the author uploads before posting (R-21)
    he = W.Home("e.example", W.Key("server:e"), network=net)
    he.spaces[S] = copy.deepcopy(h.spaces[S])          # e.example mirrors the space and holds the right bytes
    h.blobs[ref["hash"]] = b"WRONG"                    # the author's server (a.example, also the home) serves corrupted bytes
    he.blobs[ref["hash"]] = data; he.supplied.add((S, ref["hash"]))
    who, got = W.fetch_blob(ref, rec, "a.example", ["e.example"], net, "c.example")     # c.example hosts member @x
    outsider = W.fetch_blob(ref, rec, "a.example", ["e.example"], net, "f.example")[0]  # f.example hosts nobody
    refs = W.blob_refs("effect.propose", c)
    return (code == "ok" and refs == [ref] and who == "e.example" and got == data and outsider is None), f"{code} refs={len(refs)} from={who} outsider={outsider}"

@scenario("blob theft by hash is refused", "R-13", "mentioning someone else's blob hash in your own space doesn't get you the bytes")
def _():
    h = base_home()
    secret = b"private design doc"; ref = {"hash": hashlib.sha256(secret).hexdigest(), "size": len(secret), "type": "text/plain"}
    h.blob_put(secret, M)
    e, c = entry("message", M, {}, {"text": "spec", "blobs": [ref]}); h.append(e, c, profiles())
    ok_member = h.blob_get(ref, S, O)[0]
    T = "t2:a.example"
    for kind, author, body, content in [("space.create", Z, {"policy": POLICY}, {"name": "mine"})]:
        e2, c2 = entry(kind, author, body, content, space=T); h.append(e2, c2, profiles())
    e3, c3 = entry("message", Z, {}, {"text": "gimme", "blobs": [ref]}, space=T); h.append(e3, c3, profiles())
    thief = h.blob_get(ref, T, Z)[0]
    return ok_member == "ok" and thief == "unavailable", f"member={ok_member} thief={thief}"

@scenario("moving back to a former home", "§7.3, R-02", "A→B→A is legal: after the second move A is home again, and mirrors accept its receipts")
def _():
    net = {}
    h = base_home(network=net)
    hb = W.Home("b.example", HOME_B, clock=ticking(300), network=net, server_keys=SERVER_KEYS)
    e, c = move(h, "b.example"); h.append(e, c, profiles())
    hb.spaces[S] = copy.deepcopy(h.spaces[S])
    e2, c2 = move(hb, "a.example"); code_b = hb.append(e2, c2, profiles())[0]
    h.clock = ticking(600)
    h.adopt(S, hb.spaces[S]["records"], hb.spaces[S]["state"])
    e3, c3 = entry("message", M, {}, {"text": "home again"}); code_a = h.append(e3, c3, profiles())[0]
    m = mirror(); out = [m.ingest(r) for r in h.spaces[S]["records"]]
    later = h.spaces[S]["records"][-1]["receipt"]
    no_proof = not W.wrong_home_proof(h.spaces[S]["records"][:-1], later, SERVER_KEYS)
    return code_b == "ok" and code_a == "ok" and all(o[0] for o in out) and no_proof, f"{code_b} {code_a} {out[-1]} no_proof={no_proof}"

@scenario("content is validated", "R-09, §5", "a plan that doesn't match planDigest, or over-long text, is invalid at the home and rejected by mirrors")
def _():
    h = base_home()
    plan = {"action": "git.push", "target": "repo"}
    good = hashlib.sha256(P.jcs(plan)).hexdigest()
    body = {"executor": X, "action": "git.push", "planDigest": good, "repeatable": False}
    e1, c1 = entry("effect.propose", M, body, {"plan": dict(plan, target="other-repo")})
    a = h.append(e1, c1, profiles())[0]
    e2, c2 = entry("message", M, {}, {"text": "x" * 40000}); b = h.append(e2, c2, profiles())[0]
    e3, c3 = entry("effect.propose", M, body, {"plan": plan}); ok = h.append(e3, c3, profiles())[0]
    recs = copy.deepcopy(h.spaces[S]["records"])
    bad = copy.deepcopy(recs); bad[-1]["content"] = {"plan": dict(plan, target="other-repo")}
    bad[-1]["entry"] = dict(bad[-1]["entry"])
    m = mirror(); out = [m.ingest(r) for r in bad]
    return a == "invalid" and b == "invalid" and ok == "ok" and not out[-1][0], f"{a} {b} {ok} mirror={out[-1][1]}"

@scenario("notices stay private", "R-20, R-22", "profile spaces can't be followed for signals; notices go only to the actor's inbox, and only for role holders")
def _():
    net = {}
    hd = W.Home("d.example", W.Key("server:d"), network=net)
    h = base_home(network=net)
    hd.spaces["profile:" + Z] = {"records": [], "state": {"space": "profile:" + Z, "head": 1, "policy": dict(POLICY, visibility="public"), "roles": {}, "roleHistory": {}}}
    follow = hd.follow("profile:" + Z, "@nosy:d.example")[0]       # the profile is public to read, but only Z may follow it
    own, inbox = hd.follow("profile:" + Z, Z)
    e, c = entry("message", M, {"to": [Z]}, {"text": "hey stranger"}); h.append(e, c, profiles())
    none_yet = len(inbox) == 0
    e2, c2 = entry("member.set", O, {"actor": Z, "role": "guest"}); h.append(e2, c2, profiles())
    return (follow == "denied" and own == "ok" and none_yet and len(inbox) == 1 and inbox[0]["type"] == "signal"
            and inbox[0]["body"]["type"] == "notice"), f"follow={follow} own={own} before={none_yet} inbox={inbox}"

@scenario("hello binds the dialed name", "§3", "a proof made for the server the client dialed fails at any other server")
def _():
    p = W.hello_proof(KEYS[M], "CH", "a.example")
    return W.verify_hello(KEYS[M].pub, p, "CH", "a.example") and not W.verify_hello(KEYS[M].pub, p, "CH", "evil.example"), "ok"

@scenario("moves need the target's acceptance", "7.3, R-14", "a move the target didn't sign is invalid at the home; a mirror faults a move without it")
def _():
    h = base_home()
    head = h.spaces[S]["records"][-1]["receipt"]
    e, c = entry("space.move", O, {"to": "b.example", "accept": {"keyId": "h1", "sig": W.accept_move(EVIL, S, head)}})
    a = h.append(e, c, profiles())[0]
    real = W.verify_accept; W.verify_accept = lambda *a: True       # a careless home that skips the check...
    e2, c2 = entry("space.move", O, {"to": "b.example", "accept": {"keyId": "h1", "sig": "forged"}})
    careless = h.append(e2, c2, profiles())[0]
    W.verify_accept = real
    m = mirror(); out = [m.ingest(r) for r in h.spaces[S]["records"]]
    return a == "invalid" and careless == "ok" and not out[-1][0], f"home={a} careless={careless} mirror={out[-1][1]}"

@scenario("minDurability can't lock a space", "R-08", "a home refuses a policy it can't meet; policy.set and space.move always go through")
def _():
    h = base_home()
    e, c = entry("policy.set", O, {"policy": dict(POLICY, minDurability="replicated")}); a = h.append(e, c, profiles())[0]
    h.durability = "replicated"; b = h.append(e, c, profiles())[0]; h.durability = "local"
    e2, c2 = entry("message", M, {}, {"text": "x"}); blocked = h.append(e2, c2, profiles())[0]
    e3, c3 = entry("policy.set", O, {"policy": POLICY}); rescue = h.append(e3, c3, profiles())[0]
    ok = h.append(e2, c2, profiles())[0]
    return (a, b, blocked, rescue, ok) == ("unavailable", "ok", "unavailable", "ok", "ok"), f"{a} {b} {blocked} {rescue} {ok}"

@scenario("uploads complete only on the uploader's own bytes", "R-13, R-21", "claiming a known hash without sending its bytes gives no supply")
def _():
    h = base_home()
    data = b"secret file"; ref = {"hash": hashlib.sha256(data).hexdigest(), "size": len(data), "type": "text/plain"}
    r1 = h.blob_put_part(ref, 0, data, M)
    r2 = h.blob_put_part(ref, 0, b"", Z); r3 = h.blob_put_part(ref, 0, b"x" * len(data), Z)
    return r1["complete"] and not r2["complete"] and not r3["complete"] and Z not in h.uploads[ref["hash"]], f"{r1} {r2} {r3}"

@scenario("a new home with a slow clock waits", "R-03a, 7.3", "a home whose clock is behind the chain answers unavailable instead of stamping an earlier time")
def _():
    h = base_home(clock=ticking(100))
    h.clock = lambda: "2026-10-10T12:00:00Z"
    e, c = entry("message", M, {}, {"text": "x"}); a = h.append(e, c, profiles())[0]
    return a == "unavailable", a

def main():
    bad = 0
    for ok, name, rule, desc, detail in RESULTS:
        bad += not ok
        print(f"{'PASS' if ok else 'FAIL'}  {rule:18} {name}: {desc}" + ("" if ok else f"\n      got: {detail}"))
    print(f"{len(RESULTS) - bad}/{len(RESULTS)} wire scenarios pass (real Ed25519)")
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main())
