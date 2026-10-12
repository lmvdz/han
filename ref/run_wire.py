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
          "extensionsMinRole": "member", "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]}, "servers": {"deny": []}}

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

def base_home(network=None, clock=None, server="a.example", key=HOME_A, space=S, steady=None, limiter=None):
    h = W.Home(server, key, clock=clock or ticking(), network=network, server_keys=SERVER_KEYS, steady=steady, limiter=limiter)
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

@scenario("first contact across servers", "G-03, P-02", "adding a member delivers a notice to its server (to a live subscription only; X was added before it subscribed, so its notice was dropped), which can then read the space as its server actor")
def _():
    net = {}
    hc = W.Home("c.example", HOME_C, network=net); hd = W.Home("d.example", W.Key("server:d"), network=net)
    h = base_home(network=net)
    hd.follow("profile:" + Z, Z); hc.follow("profile:" + X, X)         # a notice reaches only a live subscription (F4)
    e, c = entry("member.set", O, {"actor": Z, "role": "guest"}); h.append(e, c, profiles())
    got = hd.inbox.get(Z, []); gotx = hc.inbox.get(X, [])
    code, items = h.read(S, 0, "@:d.example")
    stranger = h.read(S, 0, "@:evil.example")
    return (bool(got) and got[-1]["body"]["space"] == S and not gotx and code == "ok" and len(items) == 4
            and stranger[0] == "not_found"), f"notice={got[-1] if got else None} read={code}/{len(items)}"

@scenario("signals ride follow streams", "G-04, §3", "followers receive signals as ReadItems; strangers can neither follow nor signal")
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

@scenario("state needs read rights at head", "§3 state, P-02", "a former member reads old records but gets no current state; a stranger who may read nothing gets not_found from read and state alike")
def _():
    h = base_home()
    e, c = entry("member.set", O, {"actor": M, "role": "none"}); h.append(e, c, profiles())
    code, _ = h.state(S, M)
    code2, items = h.read(S, 0, M)
    code3, reply = h.state(S, O)
    stranger = (h.read(S, 0, Z)[0], h.state(S, Z)[0])
    return (code == "denied" and code2 == "ok" and 0 < len(items) < 5 and "state" in reply and "entries" not in reply["state"] and "next" not in reply
            and stranger == ("not_found", "not_found")), f"{code} {code2}/{len(items)} {stranger}"

@scenario("blob refs per kind and transfer", "G-01, G-02", "argsBlob in a plan is a reference; a server fetches as itself, skips a server with wrong bytes, and verifies the hash")
def _():
    net = {}
    data = b"patch bytes"; ref = {"hash": hashlib.sha256(data).hexdigest(), "size": len(data), "type": "text/x-diff"}
    h = base_home(network=net)
    hc = W.Home("c.example", HOME_C, network=net)
    plan = {"action": "git.push", "target": "repo", "argsBlob": ref}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    e, c = entry("effect.propose", M, body, {"plan": plan}); code = h.append(e, c, profiles())[0]
    rec = h.spaces[S]["records"][-1]
    h.blob_put(data, M)                                # the author uploads before posting (G-01)
    he = W.Home("e.example", W.Key("server:e"), network=net)
    he.spaces[S] = copy.deepcopy(h.spaces[S])          # e.example mirrors the space and holds the right bytes
    h.blobs[ref["hash"]] = b"WRONG"                    # the author's server (a.example, also the home) serves corrupted bytes
    he.blobs[ref["hash"]] = data; he.supplied.add((S, ref["hash"]))
    who, got = W.fetch_blob(ref, rec, "a.example", ["e.example"], net, "c.example")     # c.example hosts member @x
    outsider = W.fetch_blob(ref, rec, "a.example", ["e.example"], net, "f.example")[0]  # f.example hosts nobody
    refs = W.blob_refs("effect.propose", c)
    return (code == "ok" and refs == [ref] and who == "e.example" and got == data and outsider is None), f"{code} refs={len(refs)} from={who} outsider={outsider}"

@scenario("blob theft by hash is refused", "G-01", "mentioning someone else's blob hash in your own space doesn't get you the bytes")
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

@scenario("notices stay private", "G-03, G-04", "profile spaces can't be followed for signals; notices go only to the actor's inbox, and only for role holders")
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

@scenario("uploads complete only on the uploader's own bytes", "G-01", "claiming a known hash without sending its bytes gives no supply")
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

# ── round 19: R-16 code, state at a seq, follow + limit, gossip signals, future time, context blobs, Policy-owned seen/refs ──
import datetime as _dt
def _fmt(sec): return (_dt.datetime(2026, 10, 10, 12, 0, 0) + _dt.timedelta(seconds=int(sec))).strftime("%Y-%m-%dT%H:%M:%SZ")

def claimed_home(wall, steady):
    """A home with a proposal claimed by X at time 0 (window 30, so A = 33)."""
    h = base_home(clock=lambda: _fmt(wall[0]), steady=lambda: steady[0])
    plan = {"action": "git.push", "target": "repo"}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    e, c = entry("effect.propose", M, body, {"plan": plan}); assert h.append(e, c, profiles())[0] == "ok"
    pid = W.entry_id(e)
    e, c = entry("effect.claim", X, {"proposal": pid, "attempt": 1, "instance": "k1", "window": 30}); code, rc = h.append(e, c, profiles())
    assert code == "ok", (code, rc)
    return h, rc["id"]

def abandon(h, claim, author=O):
    e, c = entry("effect.outcome", author, {"claim": claim, "result": "unknown", "started": "unknown"})
    return h.append(e, c, profiles())

@scenario("an early abandon is unavailable, with retryAfterMs", "R-16, P-09", "an owner's abandon before A steady seconds is `unavailable` (not `denied`) even if the wall clock jumped; a non-owner's is denied; it succeeds at A; the executor's own report is never delayed")
def _():
    wall, steady = [0], [0.0]
    h, claim = claimed_home(wall, steady)
    steady[0], wall[0] = 10.0, 10
    a, da = abandon(h, claim)                                    # 10 s in: 23 s to wait
    steady[0], wall[0] = 11.0, 111                               # a wall-clock step forward: receipt time says A has passed
    b, db = abandon(h, claim)
    nonowner, _ = abandon(h, claim, author=M)
    steady[0] = 33.0
    c, _ = abandon(h, claim)
    h2, claim2 = claimed_home([0], [0.0])
    e2, c2 = entry("effect.outcome", X, {"claim": claim2, "result": "unknown", "started": "unknown"})
    own = h2.append(e2, c2, profiles())[0]
    return ((a, da["retryAfterMs"], b, db["retryAfterMs"], nonowner, c, own) == ("unavailable", 23000, "unavailable", 22000, "denied", "ok", "ok"),
            f"{a}/{da} {b}/{db} nonowner={nonowner} at-A={c} executor={own}")

@scenario("an abandon after a restart waits a full A again", "R-16", "after a restart or failover the sequencer counts every earlier claim from its new start")
def _():
    wall, steady = [0], [0.0]
    h, claim = claimed_home(wall, steady)
    steady[0], wall[0] = 40.0, 40
    h.restart()                                                  # claim is now older than A by receipt time, but this run knows nothing of it
    steady[0], wall[0] = 45.0, 45
    a, da = abandon(h, claim)
    steady[0], wall[0] = 73.0, 73
    b, _ = abandon(h, claim)
    return (a, da["retryAfterMs"], b) == ("unavailable", 28000, "ok"), f"{a}/{da} {b}"

@scenario("state at a past seq", "§3 state, P-02", "`at` returns the state after that record, needs read rights at `at` (not at the head), and equals a verifier's replay")
def _():
    h = base_home()
    e, c = entry("message", M, {}, {"text": "while a member"}); h.append(e, c, profiles())
    e, c = entry("member.set", O, {"actor": M, "role": "none"}); h.append(e, c, profiles())
    code, reply = h.state(S, M, at=4)
    m = mirror(); [m.ingest(r) for r in h.spaces[S]["records"][:4]]
    want = {k: v for k, v in m.spaces[S]["state"].items() if k != "entries"}
    at_head = h.state(S, M)[0]
    at5 = h.state(S, M, at=5)[0]                    # the removal itself is not readable by the removed member
    bad = (h.state(S, O, at=0)[0], h.state(S, O, at=99)[0], h.state(S, O, at="2")[0], h.state(S, O, at=True)[0])
    owner = h.state(S, O, at=1)[1]["state"]
    return (code == "ok" and reply["state"] == want and reply["state"]["head"] == 4 and at_head == "denied" and at5 == "denied"
            and bad == ("invalid",) * 4 and owner["head"] == 1 and M not in owner["roles"] and O in owner["roles"]), f"{code} head={reply['state']['head']} {at_head} {at5} {bad}"

@scenario("follow with a short limit leaves a visible gap", "§3 read", "a backlog longer than `limit` is cut at `limit`; live records follow from the head at registration; re-reading the gap closes it")
def _():
    h = base_home()
    for i in range(5):
        e, c = entry("message", M, {}, {"text": str(i)}); h.append(e, c, profiles())          # seqs 4..8
    code, items, q = h.read_follow(S, 0, O, limit=3)
    e, c = entry("message", M, {}, {"text": "live"}); h.append(e, c, profiles())            # seq 9
    seqs = [i["record"]["receipt"]["seq"] for i in items]
    live = [i["record"]["receipt"]["seq"] for i in q if i["type"] == "record"]
    refill = [i["record"]["receipt"]["seq"] for i in h.read(S, seqs[-1], O)[1]]
    return (seqs, live, refill) == ([1, 2, 3], [9], [4, 5, 6, 7, 8, 9]), f"{seqs} {live} {refill}"

def _world():
    """Two forked homes (equivocation at seq 4)."""
    h1 = base_home(); h2 = base_home()
    e1, c1 = entry("message", M, {}, {"text": "left"}); e2, c2 = entry("message", M, {}, {"text": "right"})
    _, r1 = h1.append(e1, c1, profiles()); _, r2 = h2.append(e2, c2, profiles())
    return h1, h2, r1, r2

@scenario("gossip signals: heads compare, faults are adopted only if they check", "R-15, T-05", "a heads signal surfaces an equivocation; a fault signal is adopted only when its proof checks against the receiver's own keys; signals change no state")
def _():
    h1, h2, r1, r2 = _world()
    mine = {S: h1.spaces[S]["records"]}
    heads = W.receive_gossip(W.heads_signal([r2]), mine, SERVER_KEYS)
    nohead = W.receive_gossip(W.heads_signal([r1]), mine, SERVER_KEYS)
    proof = {"v": 1, "kind": "equivocation", "space": S, "through": 3, "reason": "", "receipts": [r1, r2], "serverKeys": SERVER_KEYS}
    real = W.receive_gossip(W.fault_signal(proof), mine, SERVER_KEYS)
    forged = [W.sign_receipt(dict(r, time="2026-10-10T13:00:00Z"), EVIL) for r in (r1, r2)]          # EVIL signs as if it were a.example
    fake = W.receive_gossip(W.fault_signal(dict(proof, receipts=forged, serverKeys={"a.example": {"h1": {"publicKey": EVIL.pub, "from": T0}}})), mine, SERVER_KEYS)
    junk = W.receive_gossip({"type": "fault", "proof": "trust me"}, mine, SERVER_KEYS)
    before = h1.spaces[S]["state"]["head"]
    sent = h1.signal(S, M, W.fault_signal(proof))[0]; stranger = h1.signal(S, Z, W.heads_signal([r1]))[0]
    return (heads == ("proofs", ["equivocation"]) and nohead == ("proofs", []) and real == ("adopted", "equivocation")
            and fake[0] == "ignored" and junk[0] == "ignored" and sent == "ok" and stranger == "denied" and h1.spaces[S]["state"]["head"] == before,
            f"{heads} {nohead} {real} {fake} {junk} {sent} {stranger}")

@scenario("future-dated receipts: one 60 s tolerance", "R-03a", "a verifier tolerates a receipt dated up to 60 s after its own clock and faults one second more")
def _():
    h = base_home()                                              # first receipt at 12:00:00
    first = h.spaces[S]["records"][0]
    ok = mirror().ingest(first, now="2026-10-10T11:59:00Z")       # exactly 60 s ahead
    m = mirror(); bad = m.ingest(first, now="2026-10-10T11:58:59Z")
    proof = {"v": 1, "kind": "future-time", "space": S, "through": 0, "reason": "", "receipts": [first["receipt"]], "serverKeys": SERVER_KEYS, "observedAt": "2026-10-10T11:58:59Z"}
    adopted = W.receive_gossip(W.fault_signal(proof), {}, SERVER_KEYS)
    return ok[0] and not bad[0] and bad[1] == "future-time" and adopted == ("adopted", "future-time"), f"{ok} {bad} {adopted}"

@scenario("context blobs follow the session's read right", "G-01, G-05", "a context item's blob is readable by exactly the actors with `read` on the live session, supplied only by the session's actor, and not after it closes")
def _():
    h = base_home()
    e, c = entry("policy.set", O, {"policy": dict(POLICY, sessions={"watch": "member", "read": "owner", "steer": "owner", "modes": ["auto", "guarded"]})}); h.append(e, c, profiles())
    e, c = entry("session.open", X, {"steerMode": "auto"}); code, r = h.append(e, c, profiles()); assert code == "ok", (code, r)
    sid = r["id"]
    data, other = b"transcript bytes", b"someone else's bytes"
    ref = {"hash": hashlib.sha256(data).hexdigest(), "size": len(data), "type": "text/plain"}
    ref2 = {"hash": hashlib.sha256(other).hexdigest(), "size": len(other), "type": "text/plain"}
    h.blob_put(data, X); h.blob_put(other, O)                    # ref2 is uploaded by an owner, not by the session's actor
    item = lambda n, ref_: {"space": S, "session": sid, "n": n, "time": "2026-10-10T12:00:00Z", "type": "context", "blob": ref_, "what": "transcript"}
    s1 = h.stream(item(1, ref), X)[0]; s2 = h.stream(item(2, ref2), X)[0]; not_actor = h.stream(item(3, ref), O)[0]
    owner = h.blob_get(ref, S, O)[0]; watcher = h.blob_get(ref, S, M)[0]; stranger = h.blob_get(ref, S, Z)[0]
    unsupplied = h.blob_get(ref2, S, O)[0]
    e, c = entry("session.close", X, {"session": sid}); h.append(e, c, profiles())
    closed = h.blob_get(ref, S, O)[0]
    return ((s1, s2, not_actor, owner, watcher, stranger, unsupplied, closed) == ("ok", "ok", "denied", "ok", "denied", "denied", "unavailable", "denied"),
            f"{s1} {s2} {not_actor} owner={owner} watcher={watcher} stranger={stranger} unsupplied={unsupplied} closed={closed}")

@scenario("seen and refs are Policy's verdict alone", "R-25, P-16, R-05", "`seen` beyond the head and a missing ref are invalid at the home (from Policy), and a home that skipped Policy is caught by the mirror's replay with the same verdict")
def _():
    h = base_home()
    e, _c = entry("message", M, {}, {"text": "x"}, seen=999)
    a = h.append(e, {"text": "x"}, profiles())[0]
    e2 = dict(entry("message", M, {}, {"text": "y"})[0]); e2.pop("sig"); e2["refs"] = ["0" * 63 + "1"]; e2 = W.sign_entry(e2, KEYS[M])
    b = h.append(e2, {"text": "y"}, profiles())[0]
    forged = h.append(e, {"text": "x"}, profiles(), skip_policy=True)[0]       # a careless home orders it anyway
    m = mirror(); out = [m.ingest(r) for r in h.spaces[S]["records"]]
    return (a, b, forged) == ("invalid", "invalid", "ok") and not out[-1][0] and "forbidden" in out[-1][1], f"{a} {b} {forged} mirror={out[-1][1]}"

# ── round 20: R-16 verdict class, P-01 pinning, P-02 on state at a past seq ──
@scenario("an abandon that could never be allowed gets Policy's verdict now", "R-16, P-16", "`unavailable` only if Policy would allow the abandon at claim.time + A; a settled claim, an unknown claim and a non-owner get `denied`/`invalid` at once")
def _():
    wall, steady = [0], [0.0]
    h, claim = claimed_home(wall, steady)
    e, c = entry("effect.outcome", X, {"claim": claim, "result": "succeeded", "started": "yes"}); settled = h.append(e, c, profiles())[0]
    steady[0], wall[0] = 5.0, 5
    early_settled = abandon(h, claim)[0]                          # phase is `succeeded`: no later time makes it allowed
    unknown = abandon(h, "0" * 64)[0]
    h2, claim2 = claimed_home([0], [0.0])
    open_early = abandon(h2, claim2)[0]                           # control: the same abandon on an open claim is merely early
    return ((settled, early_settled, unknown, open_early) == ("ok", "denied", "invalid", "unavailable"),
            f"settled={settled} abandon-of-settled={early_settled} unknown-claim={unknown} open={open_early}")

def approved_home(approvers=("owner", "member")):
    """A home with a proposal of X approved first by O, then by Z (a member on another server); returns (home, proposal id)."""
    h = base_home()
    for kind, author, body in [("policy.set", O, {"policy": dict(POLICY, approvals=[{"action": "git.push", "approvers": list(approvers)}])}),
                               ("member.set", O, {"actor": Z, "role": "member"})]:
        e, c = entry(kind, author, body); assert h.append(e, c, profiles())[0] == "ok"
    plan = {"action": "git.push", "target": "repo"}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    e, c = entry("effect.propose", M, body, {"plan": plan}); assert h.append(e, c, profiles())[0] == "ok"
    pid = W.entry_id(e)
    for who in (O, Z):
        e, c = entry("effect.approve", who, {"proposal": pid, "planDigest": body["planDigest"]}); code, r = h.append(e, c, profiles()); assert code == "ok", (code, r)
    return h, pid

@scenario("approvers are pinned up to the first valid key", "P-01, E3", "an unreachable later approver does not block a claim while an earlier candidate's key is valid, and a reachable one is fetched but not pinned; it does block once every earlier candidate's key is revoked")
def _():
    out = {}
    for name, revoked, with_z in [("first-valid", False, False), ("first-valid-z-there", False, True), ("first-revoked-z-there", True, True), ("first-revoked-z-missing", True, False)]:
        h, pid = approved_home()
        if revoked: REVOKED.add((O, 2))
        try:
            p = profiles()
            if revoked: p[O] = view(O, 2)
            if not with_z: del p[Z]
            e, c = entry("effect.claim", X, {"proposal": pid, "attempt": 1, "instance": "k1", "window": 30})
            code, r = h.append(e, c, p)
            out[name] = (code, sorted(r["profiles"]) if code == "ok" else r)
            if name == "first-valid":
                m = mirror(); out["mirror"] = all(m.ingest(rec)[0] for rec in h.spaces[S]["records"])
        finally:
            REVOKED.discard((O, 2))
    want = {"first-valid": ("ok", sorted([X, M, O])), "first-valid-z-there": ("ok", sorted([X, M, O])), "first-revoked-z-there": ("ok", sorted([X, M, O, Z])),
            "first-revoked-z-missing": ("unavailable", "profile"), "mirror": True}
    return out == want, str(out)

@scenario("a guard that is not an approval candidate is pinned", "P-01, P-15, E3", "a claim by a forced agent whose operator approved but is not a rule candidate pins the operator's profile and is allowed; without the fix it was `invalid` (guard profile not pinned)")
def _():
    h = base_home()
    for kind, author, body in [("policy.set", O, {"policy": dict(POLICY, approvals=[{"action": "git.push", "approvers": ["owner"]}], sessions=dict(POLICY["sessions"], forceGuarded=True))}),
                               ("member.set", O, {"actor": Z, "role": "member"})]:
        e, c = entry(kind, author, body); assert h.append(e, c, profiles())[0] == "ok"
    def pf():
        p = profiles(); p[X] = dict(p[X], operator=Z); return p
    plan = {"action": "git.push", "target": "repo"}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    e, c = entry("effect.propose", M, body, {"plan": plan}); code, r = h.append(e, c, pf()); assert code == "ok", (code, r)
    pid = W.entry_id(e)
    for who in (O, Z):
        e, c = entry("effect.approve", who, {"proposal": pid, "planDigest": body["planDigest"]}); code, r = h.append(e, c, pf()); assert code == "ok", (who, code, r)
    e, c = entry("effect.claim", X, {"proposal": pid, "attempt": 1, "instance": "k1", "window": 30})
    code, r = h.append(e, c, pf())
    return code == "ok" and Z in r["profiles"], f"{code} {sorted(r['profiles']) if code == 'ok' else r}"

@scenario("a role held now reads the state at an earlier seq", "P-02, §3 state", "a member who joined at seq 3 reads the state after seq 2 (rights are judged on the current state); a stranger does not")
def _():
    h = base_home()                                               # 1 create, 2 member M, 3 member X
    e, c = entry("message", M, {}, {"text": "later"}); h.append(e, c, profiles())
    code, reply = h.state(S, X, at=2)
    early = [i["record"]["receipt"]["seq"] for i in h.read(S, 0, X)[1]]
    state2 = reply["state"]
    stranger = h.state(S, Z, at=2)[0]
    return (code == "ok" and state2["head"] == 2 and X not in state2["roles"] and early == [1, 2, 3, 4] and stranger == "not_found",
            f"{code} head={state2['head']} X-in-roles={X in state2['roles']} records={early} stranger={stranger}")

# ── round 22: one order of verdicts (§4.1, E5) — one scenario per adjacent pair — and paged state (E8) ──
def msg(author=M, text="x", **kw): return entry("message", author, {}, {"text": text}, **kw)
def replicated_home():
    """A home that ordered a `replicated` policy while it could, and can't reach that level now (R-08)."""
    h = base_home(); h.durability = "replicated"
    e, c = entry("policy.set", O, {"policy": dict(POLICY, minDurability="replicated")}); assert h.append(e, c, profiles())[0] == "ok"
    h.durability = "local"
    return h
def bad_nonce(e): return dict(e, nonce="NOT-BASE32")

@scenario("verdict order 1/2: dedupe before routing", "§4.1 steps 1-2, R-01", "a stored entry sent again after the space moved away gets its receipt, not `moved`; a new entry gets `moved`")
def _():
    h = base_home(); e, c = msg(); code1, r1 = h.append(e, c, profiles())
    mv, cm = move(h, "b.example"); h.append(mv, cm, profiles())
    again = h.append(e, c, profiles()); e2, c2 = msg(text="y"); fresh = h.append(e2, c2, profiles())
    return (code1, again[0], again[1] == r1, fresh) == ("ok", "ok", True, ("moved", "b.example")), f"{code1} {again[0]} same={again[1] == r1} fresh={fresh}"

@scenario("verdict order 2/3: routing before shape", "§4.1 steps 2-3", "a malformed entry for a foreign or a moved space is `moved`; a well-formed `space.create` naming a foreign server is `moved`, never `invalid`; a bad shape or a non-create into an unknown local space is `invalid`")
def _():
    h = base_home()
    ec, cc = entry("space.create", O, {"policy": POLICY}, {"name": "x"}, space="q1:b.example")
    foreign_create = h.append(ec, cc, profiles())
    foreign_bad = h.append(bad_nonce(ec), cc, profiles())[0]
    eu, cu = msg(space="q2:a.example"); unknown_local = h.append(eu, cu, profiles())[0]
    em, cm_ = msg(); own_bad = h.append(bad_nonce(em), cm_, profiles())[0]
    mv, cmv = move(h, "b.example"); h.append(mv, cmv, profiles())
    moved_bad = h.append(bad_nonce(em), cm_, profiles())[0]
    got = (foreign_create, foreign_bad, unknown_local, own_bad, moved_bad)
    return got == (("moved", "b.example"), "moved", "invalid", "invalid", "moved"), f"{got}"

@scenario("verdict order 3/4: shape before profile and key", "§4.1 steps 3-4", "a malformed entry is `invalid` even when the author's profile is missing or its keyId is unknown; a well-formed one is `unavailable` / `denied`")
def _():
    h = base_home(); p = profiles(); del p[Z]
    e, c = msg(Z); e9 = W.sign_entry(dict(e, keyId="k9"), KEYS[Z])
    got = (h.append(e, c, p)[0], h.append(bad_nonce(e), c, p)[0], h.append(e9, c, profiles())[0], h.append(bad_nonce(e9), c, profiles())[0])
    return got == ("unavailable", "invalid", "denied", "invalid"), f"{got}"

@scenario("verdict order 4/5: signature before Policy", "§4.1 steps 4-5, R-12", "a forged signature is `invalid` even from a non-member, a genuine one is Policy's `denied`; in a `replicated` space on a home that can't reach it, neither becomes `unavailable`; the signature is judged before the other actors' profiles are pinned")
def _():
    h = base_home(); e, c = msg(Z); forged = W.sign_entry(dict(e), KEYS[O])
    plain = (h.append(e, c, profiles())[0], h.append(forged, c, profiles())[0])
    r = replicated_home(); e2, c2 = msg(Z); forged2 = W.sign_entry(dict(e2), KEYS[O]); e3, c3 = msg(M); forged3 = W.sign_entry(dict(e3), KEYS[O])
    rep = (r.append(e2, c2, profiles())[0], r.append(forged2, c2, profiles())[0], r.append(forged3, c3, profiles())[0], r.append(e3, c3, profiles())[0])
    ha, pid = approved_home(); p = profiles(); del p[M]                              # the claim needs the proposer's profile, which is missing
    ec, cc = entry("effect.claim", X, {"proposal": pid, "attempt": 1, "instance": "k1", "window": 30})
    pins = (ha.append(W.sign_entry(dict(ec), KEYS[O]), cc, p)[0], ha.append(ec, cc, p)[0])
    return (plain, rep, pins) == (("denied", "invalid"), ("denied", "invalid", "invalid", "unavailable"), ("invalid", "unavailable")), f"{plain} {rep} {pins}"

@scenario("verdict order 5/6: Policy before the rate limit", "§4.1 steps 3, 5-6", "with the author over its limit, a malformed entry is `invalid` and a forbidden one `denied`; only a well-formed, allowed entry is `limit`, with retryAfterMs")
def _():
    h = base_home(); h.limiter = lambda author: 1500
    e, c = msg(Z); em, cm_ = msg(); ok_e, ok_c = msg()
    got = (h.append(bad_nonce(em), cm_, profiles())[0], h.append(e, c, profiles())[0], h.append(ok_e, ok_c, profiles()))
    h.limiter = None; after = h.append(ok_e, ok_c, profiles())[0]
    return (got[0], got[1], got[2], after) == ("invalid", "denied", ("limit", {"reason": "rate limit", "retryAfterMs": 1500}), "ok"), f"{got} then {after}"

@scenario("verdict order 5/7: Policy before the moment-dependent unavailable", "§4.1 steps 5, 7, R-08, R-03a", "a forbidden entry is `denied` in a `replicated` space on a home that can't reach it, and on a home whose clock is behind the chain; an allowed one is `unavailable`")
def _():
    r = replicated_home()
    nonmember = r.append(*msg(Z), profiles())[0]
    e1, c1 = entry("policy.set", M, {"policy": dict(POLICY, minDurability="replicated")}); member_policy = r.append(e1, c1, profiles())[0]
    allowed = r.append(*msg(M), profiles())[0]
    r2 = base_home(); r2.durability = "local"
    e2, c2 = entry("policy.set", M, {"policy": dict(POLICY, minDurability="replicated")}); nonowner_create = r2.append(e2, c2, profiles())[0]
    e3, c3 = entry("policy.set", O, {"policy": dict(POLICY, minDurability="replicated")}); owner_create = r2.append(e3, c3, profiles())[0]
    slow = base_home(clock=ticking(100)); slow.clock = lambda: "2026-10-10T12:00:00Z"
    slow_denied = slow.append(*msg(Z), profiles())[0]; slow_ok = slow.append(*msg(M), profiles())[0]
    got = (nonmember, member_policy, allowed, nonowner_create, owner_create, slow_denied, slow_ok)
    return got == ("denied", "denied", "unavailable", "denied", "unavailable", "denied", "unavailable"), f"{got}"

@scenario("verdict order 6/7: the rate limit before the moment-dependent unavailable", "§4.1 steps 6-7, R-16, R-03a, R-08", "an allowed entry from an author over its limit is `limit` even where the home would answer `unavailable` (durability, clock, early abandon); once under the limit it is `unavailable`")
def _():
    lim = lambda author: 800
    r = replicated_home(); r.limiter = lim; e, c = msg(); a = r.append(e, c, profiles())[0]
    r.limiter = None; b = r.append(e, c, profiles())[0]
    slow = base_home(clock=ticking(100)); slow.clock = lambda: "2026-10-10T12:00:00Z"; slow.limiter = lim
    e2, c2 = msg(); c1 = slow.append(e2, c2, profiles())[0]; slow.limiter = None; c2_ = slow.append(e2, c2, profiles())[0]
    wall, steady = [0], [0.0]
    h, claim = claimed_home(wall, steady); steady[0], wall[0] = 5.0, 5
    h.limiter = lim; d1 = abandon(h, claim)[0]; h.limiter = None; d2 = abandon(h, claim)[0]
    got = (a, b, c1, c2_, d1, d2)
    return got == ("limit", "unavailable", "limit", "unavailable", "limit", "unavailable"), f"{got}"

@scenario("state is paged, never refused", "§3 state, E8", "`state` returns the maps of the state in pages of maxStateItems with `next`; `cursor` pages join to the full state; `effect` returns that one effect with space, head, home and policy only; bad cursors are invalid, a stranger is not_found")
def _():
    h = base_home(); h.max_state_items = 2
    plan = {"action": "git.push", "target": "repo"}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    pids = []
    for _i in range(5):
        e, c = entry("effect.propose", M, body, {"plan": plan}); assert h.append(e, c, profiles())[0] == "ok"; pids.append(W.entry_id(e))
    for i in range(3):
        e, c = entry("task", O, {"to": [M]}, {"ask": str(i)}); assert h.append(e, c, profiles())[0] == "ok"
    code, first = h.state(S, O)
    pages, cur = [first], first.get("next")
    while cur and len(pages) < 10:
        c2, it = h.state(S, O, cursor=cur); assert c2 == "ok", (c2, it)
        pages.append(it); cur = it.get("next")
    effects = {k: v for pg in pages for k, v in pg["state"]["effects"].items()}; tasks = {k: v for pg in pages for k, v in pg["state"]["tasks"].items()}
    sizes = [(len(pg["state"]["effects"]), len(pg["state"]["tasks"])) for pg in pages]
    head = h.spaces[S]["state"]["head"]
    n = len(W.PAGED)
    bad = (h.state(S, O, cursor="junk")[0], h.state(S, O, cursor=f"{head + 5}." + ".".join("0" * n))[0], h.state(S, O, cursor=f"3.{n}")[0], h.state(S, O, cursor=f"{head}.99" + ".0" * (n - 1))[0], h.state(S, O, cursor=5)[0],
           h.state(S, O, effect="0" * 64)[0], h.state(S, O, effect=5)[0], h.state(S, Z, cursor=first["next"])[0])
    h.max_state_items = 1000
    full = h.state(S, O)[1]
    ce, one = h.state(S, O, effect=pids[3]); one = one["state"]
    return (code == "ok" and sizes == [(2, 2), (2, 1), (1, 0)] and effects == full["state"]["effects"] and tasks == full["state"]["tasks"]
            and "next" not in full and len(first["next"].split(".")) == n + 1 and list(one["effects"]) == [pids[3]] and set(one) == {"space", "head", "home", "policy", "effects"}
            and bad == ("invalid", "invalid", "invalid", "invalid", "invalid", "not_found", "invalid", "not_found")), f"{code} sizes={sizes} bad={bad}"

def _joined(h, reader, **kw):
    """Every page of `state` (following `next` with `cursor`), merged the way a client does: each paged map is the union over the pages, the rest is the last page's."""
    code, first = h.state(S, reader, **kw); assert code == "ok", (code, first)
    pages = [first]
    while pages[-1].get("next") and len(pages) < 400:
        c2, it = h.state(S, reader, cursor=pages[-1]["next"]); assert c2 == "ok", (c2, it)
        pages.append(it)
    merged = dict(pages[-1]["state"])
    for k in W.PAGED:
        if not W._has(merged, k): continue
        parts = [W._get_map(pg["state"], k) for pg in pages]
        W._put_map(merged, k, {a: b for m in parts for a, b in m.items()} if isinstance(parts[0], dict) else [x for m in parts for x in m])      # a map, or a set sent as an array
    return pages, merged

def _guard_facts(st, a, held=False, guarded=False, last=0):
    """Fill the per-actor members of P-15 directly: these scenarios are about the wire, not about how an actor gets into them. `held` and `guarded` are sets (arrays in the view)."""
    for name, on in (("held", held), ("guarded", guarded)):
        if not on: continue
        if isinstance(st[name], dict): st[name][a] = True
        else: st[name].append(a); st[name].sort()
    if last: st.setdefault("lastOutside", {})[a] = last

def _fill(h, effects=0, tasks=0, sessions=0):
    plan = {"action": "git.push", "target": "repo"}
    body = {"executor": X, "action": "git.push", "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
    for _i in range(effects):
        e, c = entry("effect.propose", M, body, {"plan": plan}); assert h.append(e, c, profiles())[0] == "ok"
    for i in range(tasks):
        e, c = entry("task", O, {"to": [M]}, {"ask": str(i)}); assert h.append(e, c, profiles())[0] == "ok"
    for i in range(sessions):
        e, c = entry("session.open", O, {"steerMode": "auto"}, {"title": f"s{i}"}, seen=h.spaces[S]["state"]["head"]); assert h.append(e, c, profiles())[0] == "ok"

def _sizes(pages): return [len(P.jcs({"id": W.MAX_INT, "ok": pg})) for pg in pages]

@scenario("state pages are bounded by bytes", "§3 state, G-09, H3", "every state page, and every effect reply, fits maxFrameBytes as a frame `{id, ok}`; the maps are filled in PAGED order and the first item that does not fit closes the page; each item is sent once; only the scalars and `policy` repeat; the pages join to the full state")
def _():
    h = base_home(); _fill(h, effects=8, tasks=30, sessions=3)
    h.max_frame_bytes = W.MAX_FRAME_BYTES; full = h.state(S, O)[1]
    h.max_frame_bytes = 2500
    pages, merged = _joined(h, O)
    sizes = _sizes(pages)
    once = all(sum(1 for pg in pages if k in W._get_map(pg["state"], key)) == 1 for key in W.PAGED if W._has(merged, key) for k in W._get_map(merged, key))
    repeated = all(all(pg["state"][k] == full["state"][k] for k in ("space", "head", "home", "policy")) for pg in pages)
    order_ok = True       # the fill order: a page that took items of map j has every earlier map complete in the offsets of its `next`
    for pg in pages[:-1]:
        offs = list(map(int, pg["next"].split(".")[1:]))
        taken = [len(W._get_map(pg["state"], key)) if W._has(pg["state"], key) else 0 for key in W.PAGED]
        j = max((i for i, t in enumerate(taken) if t), default=-1)
        for i in range(j):
            total = len(W._get_map(merged, W.PAGED[i])) if W._has(merged, W.PAGED[i]) else 0
            if offs[i] != total: order_ok = False
    pid = next(iter(merged["effects"]))
    h.max_frame_bytes = 1300
    ce, ci = h.state(S, O, effect=pid)
    effect_ok = ce == "ok" and "next" not in ci and len(P.jcs({"id": 1, "ok": ci})) <= 1300 and list(ci["state"]["effects"]) == [pid]
    return (len(pages) > 3 and max(sizes) <= 2500 and merged == dict(full["state"]) and once and repeated and order_ok and effect_ok), f"pages={len(pages)} max={max(sizes)} once={once} repeated={repeated} order={order_ok} effect={ce}"

@scenario("every map of a state is paged", "§3 state, H3", "roles, roleHistory, tasks, effects, sessions, held, guarded, lastOutside and a profile's keys are cut like effects and tasks, at most maxStateItems each per page, and join to the full state; a map in the state that is not in PAGED (apart from policy) is a bug")
def _():
    h = base_home(); h.max_state_items = 2; _fill(h, sessions=4)
    st = h.spaces[S]["state"]
    for i in range(5):        # the per-actor maps of P-15, filled directly: this scenario is about paging, not about how an actor gets into them
        a = f"@a{i}:a.example"
        _guard_facts(st, a, held=True, guarded=True, last=i + 1)
    unpaged = sorted(k for k, v in st.items() if isinstance(v, dict) and k not in W.PAGED and k not in ("policy", "entries"))
    pages, merged = _joined(h, O)
    h.max_state_items = 1000; full = h.state(S, O)[1]
    shape = [(len(pg["state"]["roleHistory"]), len(pg["state"]["sessions"]), len(pg["state"]["held"])) for pg in pages]
    pk = W.Home("a.example", HOME_A)
    pstate = P.fold(None, {"entry": {"v": 1, "space": "profile:" + M, "kind": "space.create", "author": "@:a.example", "keyId": "h1", "refs": [], "seen": 0,
                                     "body": {"policy": POLICY, "profile": {"actorKind": "human", "key": {"keyId": "k1", "publicKey": KEYS[M].pub}}}, "id": "0" * 64},
                           "content": None, "receipt": {"seq": 1, "time": T0, "id": "0" * 64}},
                   {"time": T0, "profiles": {"@:a.example": {"actor": "@:a.example", "seq": 0, "actorKind": "service", "operatorConfirmed": False, "keys": {}}}})
    for i in range(5): pstate["profile"]["keys"][f"k{i + 2}"] = {"publicKey": KEYS[M].pub, "status": "valid"}
    pk.spaces["profile:" + M] = {"records": [], "state": pstate, "pins": []}
    pk.max_state_items = 2
    code, first = pk.state("profile:" + M, Z); pp = [first]
    while pp[-1].get("next") and len(pp) < 50: pp.append(pk.state("profile:" + M, Z, cursor=pp[-1]["next"])[1])      # bounded: a server that never ends the pages fails the scenario, it does not hang it
    pjoined = {a: b for pg in pp for a, b in pg["state"]["profile"]["keys"].items()}
    return (not unpaged and merged == full["state"] and len(pages) > 1 and max(len(pg["state"]["held"]) for pg in pages) <= 2
            and pjoined == pstate["profile"]["keys"] and [len(pg["state"]["profile"]["keys"]) for pg in pp] == [2, 2, 2]
            and all(pg["state"]["profile"]["actorKind"] == "human" for pg in pp)), f"unpaged={unpaged} shape={shape} profile pages={[len(pg['state']['profile']['keys']) for pg in pp]}"

@scenario("a state item that cannot fit is impossible, never `unavailable`", "§3 state, H3", "the repeated members alone not fitting, or one item not fitting an empty page, is a server bug that raises (the server asserts); it is never answered `unavailable`, and the bounds guarantee it cannot happen: the largest legal items fit the real maxFrameBytes")
def _():
    h = base_home(); _fill(h, effects=3, tasks=3, sessions=1)
    h.max_frame_bytes = 600
    try: h.state(S, O); base = "answered"
    except AssertionError: base = "asserts"
    h.max_frame_bytes = len(P.jcs({"id": W.MAX_INT, "ok": {"state": {k: ({} if k in W.PAGED else v) for k, v in h.spaces[S]["state"].items() if k != "entries"}}})) + W.FRAME_RESERVE + 10
    try: h.state(S, O); item = "answered"
    except AssertionError: item = "asserts"
    h.max_frame_bytes = W.MAX_FRAME_BYTES
    # the largest legal items: a task with 64 assignees, an effect with 18 approvals and 64 claims, 64 role intervals: each is far below the frame
    actor = f"@{'a' * 64}:{'b' * 61}.example"
    biggest = {"author": actor, "to": [actor] * 64, "open": True, "state": "pending", "cancelRequested": False, "depth": 4, "seq": 2 ** 53 - 1}
    claim = {"id": "0" * 64, "attempt": 64, "instance": "k" * 64, "time": T0, "window": 3600, "approval": "0" * 64, "guardApprovals": ["0" * 64] * 2,
             "reserved": {"unit": "u" * 32, "amount": 2 ** 53 - 1}, "source": ["task", "0" * 64], "settled": True, "outcome": "unknown", "started": "unknown"}
    appr = {"id": "0" * 64, "author": actor, "keyId": "k" * 64, "expiresAt": T0, "consumedBy": "0" * 64}
    eff = {"proposer": actor, "proposerKey": "k" * 64, "executor": actor, "action": "a" * 128, "planDigest": "0" * 64, "seq": 2 ** 53 - 1, "repeatable": True,
           "phase": "claimed", "cancelRequested": True, "guards": [actor] * 2, "approvals": [appr] * 18, "claims": [claim] * 64}
    interval = {"from": 2 ** 53 - 1, "to": 2 ** 53 - 1}
    room = {n: len(P.jcs({"id": W.MAX_INT, "ok": {"state": {"x": v}}})) for n, v in (("task", biggest), ("effect", eff), ("roleHistory", [interval] * 64))}
    fits = all(v < W.MAX_FRAME_BYTES - W.FRAME_RESERVE - 20000 for v in room.values())       # 20,000: a generous allowance for the 16 KiB policy and the scalars
    return base == "asserts" and item == "asserts" and fits, f"base={base} item={item} {room}"

@scenario("state selectors exclude each other by shape", "§3 state, G-09, H10, K7", "cursor, effect and agent are exclusive selectors of `state`, and `at` may join `agent` only: any other two present are `invalid` (judged first, whatever the values), each alone works")
def _():
    h = base_home(); h.max_state_items = 1; _fill(h, effects=1)
    pid = next(iter(h.spaces[S]["state"]["effects"]))
    cur = h.state(S, O)[1]["next"]
    got = {"effect+at": h.state(S, O, effect=pid, at=2)[0], "effect+cursor": h.state(S, O, effect=pid, cursor=cur)[0], "cursor+at": h.state(S, O, cursor=cur, at=2)[0],
           "all three": h.state(S, O, at=2, cursor=cur, effect=pid)[0], "effect=5": h.state(S, O, effect=5)[0],
           "effect": h.state(S, O, effect=pid)[0], "cursor": h.state(S, O, cursor=cur)[0], "at": h.state(S, O, at=2)[0], "none": h.state(S, O)[0],
           "follow": h.read_follow(S, 0, O)[0],
           "agent+effect": h.state(S, O, agent=O, effect=pid)[0], "agent+cursor": h.state(S, O, agent=O, cursor=cur)[0], "agent+at+effect": h.state(S, O, agent=O, at=2, effect=pid)[0],
           "agent+at": h.state(S, O, agent=O, at=2)[0], "agent": h.state(S, O, agent=O)[0]}
    return got == {"effect+at": "invalid", "effect+cursor": "invalid", "cursor+at": "invalid", "all three": "invalid", "effect=5": "invalid",
                   "effect": "ok", "cursor": "ok", "at": "ok", "none": "ok", "follow": "ok",
                   "agent+effect": "invalid", "agent+cursor": "invalid", "agent+at+effect": "invalid", "agent+at": "ok", "agent": "ok"}, str(got)

@scenario("state {agent} is one agent's guard facts, never cut", "§3 state, G-09, K7, M5", "`agent` (an ActorId) gives `{space, head, home, policy, held, guarded, lastOutside}` with the three maps cut to that actor and nothing that grows, in one reply whatever maxStateItems is, without `next`; an actor in none of them gets `{}` three times; `at` gives the facts after that record; the reply is guardStatus's input as is; a non-ActorId is `invalid`, a stranger `not_found`")
def _():
    h = base_home(); h.max_state_items = 1; _fill(h, tasks=2)
    st = h.spaces[S]["state"]
    for i in range(4): _guard_facts(st, f"@a{i}:a.example", held=True, guarded=True, last=i + 1)
    _guard_facts(st, "@g:a.example", guarded=True)
    ca, a = h.state(S, O, agent="@a2:a.example"); cg, g = h.state(S, O, agent="@g:a.example"); cn, n = h.state(S, O, agent="@nobody:a.example")
    ct, early = h.state(S, O, agent="@a2:a.example", at=2)
    bad = (h.state(S, O, agent=5)[0], h.state(S, O, agent="a2")[0], h.state(S, O, agent="@a2:a.example", effect="0" * 64)[0], h.state(S, O, agent="@a2:a.example", at=99)[0], h.state(S, Z, agent="@a2:a.example")[0])
    facts = lambda r: {k: r["state"][k] for k in ("held", "guarded", "lastOutside")}
    shape = lambda r, f: set(r["state"]) == {"space", "head", "home", "policy", "held", "guarded", "lastOutside"} and "next" not in r and facts(r) == f
    none = {"held": {}, "guarded": {}, "lastOutside": {}}
    ctx = {"time": "2026-10-10T12:00:00Z", "profiles": {}}
    return (ca == cg == cn == ct == "ok" and shape(a, {"held": {"@a2:a.example": True}, "guarded": {"@a2:a.example": True}, "lastOutside": {"@a2:a.example": 3}})
            and shape(g, {"held": {}, "guarded": {"@g:a.example": True}, "lastOutside": {}}) and shape(n, none) and early["state"]["head"] == 2 and facts(early) == none
            and P.guard_status(a["state"], ctx, "@a2:a.example") == {"held": True, "guarded": True, "forced": False}
            and bad == ("invalid", "invalid", "invalid", "invalid", "not_found")), f"{a} {g} {n} {early} {bad}"

@scenario("a notice is never replayed", "G-03, G-09, F4", "a notice for an actor with no live subscription to its profile is dropped; one that subscribes later sees none of it; a live subscription gets its notices once; after it ends, notices are dropped again")
def _():
    net = {}
    hd = W.Home("d.example", W.Key("server:d"), network=net)
    h = base_home(network=net)
    e, c = entry("member.set", O, {"actor": Z, "role": "guest"}); h.append(e, c, profiles())                    # Z is not subscribed: dropped
    box = hd.follow("profile:" + Z, Z)[1]
    late = len(box)
    e, c = entry("member.set", O, {"actor": Z, "role": "member"}); h.append(e, c, profiles())                    # live: delivered once
    live = len(box)
    hd.unfollow("profile:" + Z, Z)
    e, c = entry("message", O, {"to": [Z]}, {"text": "again"}); h.append(e, c, profiles())                     # Z ended its subscription: dropped
    box2 = hd.follow("profile:" + Z, Z)[1]
    return (late, live, len(box2)) == (0, 1, 0), f"{late} {live} {len(box2)}"

@scenario("frames: an integer beyond 2^53 in either direction", "G-07 (1a), F4", "an integer above 2^53-1 or below -(2^53-1) anywhere in a frame is `invalid` on a usable id (the connection stays open) or `{id: 0}` and a close when there is none; 2^53-1 and -(2^53-1) are not; `test.clock` and `test.limits` are calls only in test mode")
def _():
    B = 2 ** 53
    cases = {
        "ok": ('{"id":4,"call":"cancel","args":{"target":1}}', None), "max": ('{"id":4,"call":"cancel","args":{"target":%d}}' % (B - 1), None),
        "-max": ('{"id":4,"call":"cancel","args":{"target":%d}}' % -(B - 1), None),
        "arg 2^53": ('{"id":4,"call":"cancel","args":{"target":%d}}' % B, (4, False)), "arg -2^53": ('{"id":4,"call":"cancel","args":{"target":%d}}' % -B, (4, False)),
        "nested -2^53-1": ('{"id":4,"call":"signal","args":{"space":"s","body":{"a":[%d]}}}' % (-B - 1), (4, False)),
        "no id": ('{"call":"cancel","args":{"target":%d}}' % -B, (0, True)), "id -2^53": ('{"id":%d,"call":"cancel","args":{"target":1}}' % -B, (0, True)),
        "id 2^53": ('{"id":%d,"call":"cancel","args":{"target":1}}' % B, (0, True)), "id 0": ('{"id":0,"call":"cancel","args":{"target":%d}}' % -B, (0, True)),
        "fraction": ('{"id":4,"call":"cancel","args":{"target":0.5}}', (0, True)), "array": ("[1]", (0, True)), "dup": ('{"id":4,"id":5}', (0, True)),
        "extra member": ('{"id":4,"call":"cancel","args":{"target":1},"x":1}', (4, False)), "unknown call": ('{"id":4,"call":"test.clock","args":{}}', (4, False)),
    }
    got = {k: W.frame_verdict(t) for k, (t, _w) in cases.items()}
    bad = {k: (got[k], w) for k, (_t, w) in cases.items() if got[k] != w}
    cases2 = {"state": ('{"id":4,"call":"state","args":{}}', None), "test.limits outside test mode": ('{"id":4,"call":"test.limits","args":{}}', (4, False))}
    bad.update({k: (W.frame_verdict(t), w) for k, (t, w) in cases2.items() if W.frame_verdict(t) != w})
    test_mode = [W.frame_verdict('{"id":4,"call":"%s","args":{}}' % c, test_mode=True) for c in ("test.clock", "test.limits")]
    return not bad and test_mode == [None, None], f"{bad} test_mode={test_mode}"

@scenario("public keys are canonical base64url", "R-25, §2", "a key.add whose publicKey is padded, non-canonical or not 32 bytes is `invalid` at the Log (step 3), before any signature or Policy; a canonical one reaches Policy (`denied` in a plain space)")
def _():
    h = base_home(); good = KEYS[M].pub
    cases = {"padded": good + "=", "short": good[:-4], "alphabet": good[:-1] + "+", "non-canonical": good[:-1] + "B"}      # "B": the unused low bits of the 43rd character are not zero
    out = {k: h.append(*entry("key.add", M, {"keyId": "k2", "publicKey": v}), profiles())[0] for k, v in cases.items()}
    out["canonical"] = h.append(*entry("key.add", M, {"keyId": "k2", "publicKey": good}), profiles())[0]
    return out == {"padded": "invalid", "short": "invalid", "alphabet": "invalid", "non-canonical": "invalid", "canonical": "denied"}, str(out)

def main():
    bad = 0
    for ok, name, rule, desc, detail in RESULTS:
        bad += not ok
        print(f"{'PASS' if ok else 'FAIL'}  {rule:18} {name}: {desc}" + ("" if ok else f"\n      got: {detail}"))
    print(f"{len(RESULTS) - bad}/{len(RESULTS)} wire scenarios pass (real Ed25519)")
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main())
