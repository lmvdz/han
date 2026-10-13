"""Checks conformance/carrier-vectors/*.json against ref/carrier.py (and ref/wire.py's strict Ed25519 and header check), offline.
  python ref/run_carrier_vectors.py               every file; exit 1 on any failure
  python ref/run_carrier_vectors.py --mutations   each single-rule mutation of ref/carrier.py or ref/wire.py must make some file fail
The format is in conformance/carrier-vectors/README.md."""
import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import carrier as C
import wire as W
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass
DIR = os.path.join(os.path.dirname(HERE), "conformance", "carrier-vectors")

def check_admit(v, fail):
    for c in v["cases"]:
        got = C.admit(c["conn"], c.get("spaces", v["spaces"]))
        want = c["expect"]
        if got["allow"] != want["allow"] or (want["allow"] and got["tier"] != want["tier"]): fail(c["name"], want, got)

def check_frames(v, fail):
    for c in v["encode"]:
        if C.encode_frame(c["text"]).hex() != c["hex"]: fail("encode " + c["text"], c["hex"], C.encode_frame(c["text"]).hex())
    for c in v["decode"]:
        r = C.decode_frame(bytes.fromhex(c["hex"]))
        got = {"frame": r[1], "rest": r[2].hex()} if r[0] == "frame" else r[0]
        if got != c["expect"]: fail("decode " + c["hex"], c["expect"], got)
    for c in v["requests"]:
        r = C.request_verdict(c["text"], c.get("test_mode", False))
        got = None if r is None else [r[0], r[1]]
        if got != c["expect"]: fail("request " + c["text"][:60], c["expect"], got)

def check_control(v, fail):
    for c in v["challenges"]:
        r = C.challenge_verdict(c["frames"], c["dialed"], c["node"], c["nodes"])
        got = "ok" if r == "ok" else r[0]
        if got != c["expect"]: fail(c["name"], c["expect"], r)
    for c in v["proofs"]:
        m = C.hello_message(c["challenge"], c["dialed"])
        if m.hex() != c["message"]: fail("proof bytes " + c["dialed"], c["message"], m.hex())
        if W.verify_sig(c["publicKey"], c["proof"], m) != c["valid"]: fail("proof " + c.get("note", "genuine"), c["valid"], not c["valid"])

def check_wellknown(v, fail):
    for c in v["cases"]:
        err = C.parse_wellknown(c["doc"])
        if ("ok" if err is None else "invalid") != c["expect"]: fail(c["name"], c["expect"], err)

def check_tickets(v, fail):
    for c in v["cases"]:
        r = C.parse_ticket(c["text"])
        got = "invalid" if isinstance(r, tuple) else r
        if got != c["expect"]: fail(c["name"], c["expect"], r)
        if isinstance(c["expect"], dict) and C.ticket_text(c["expect"]) != c["text"]: fail(c["name"] + " (encoding)", c["text"], C.ticket_text(c["expect"]))

def check_stream_items(v, fail):
    for c in v["cases"]:
        err = C.stream_item_verdict(c["item"])
        if ("ok" if err is None else "invalid") != c["expect"]: fail(c["name"], c["expect"], err)
        if "right" in c and C.stream_right(c["item"]) != c["right"]: fail(c["name"] + " (right)", c["right"], C.stream_right(c["item"]))

def check_bulk(v, fail):
    if C.bulk_message("han.timeline/1", bytes.fromhex(v["cases"][0]["batch"]["data"])).hex() != v["message"]: fail("message bytes", v["message"], "")
    for c in v["cases"]:
        b = dict(c["batch"], data=bytes.fromhex(c["batch"]["data"]))
        if C.verify_bulk(b, v["view"]) != c["valid"]: fail(c["name"], c["valid"], not c["valid"])

def check_ed25519(v, fail):
    for c in v["cases"]:
        try:
            k = "ok" if W.strict_key(W.unb64(c["publicKey"])) is None else "invalid"
        except ValueError:
            k = "invalid"
        if k != c["key"]: fail(c["name"] + " (key)", c["key"], k)
        got = W.verify_sig(c["publicKey"], c["sig"], bytes.fromhex(c["msg"]))
        if got != c["valid"]: fail(c["name"], c["valid"], got)

def check_header(v, fail):
    for c in v["cases"]:
        err = W.validate_header(c["entry"])
        if ("ok" if err is None else "invalid") != c["expect"]: fail(c["name"], c["expect"], err)

CHECKS = {"admit": check_admit, "frames": check_frames, "control": check_control, "wellknown": check_wellknown, "tickets": check_tickets,
          "stream-items": check_stream_items, "bulk": check_bulk, "ed25519": check_ed25519, "header": check_header}

def count(v):
    return sum(len(v[k]) for k in ("cases", "encode", "decode", "requests", "challenges", "proofs") if k in v)

def main():
    files = sorted(f for f in os.listdir(DIR) if f.endswith(".json"))
    bad_files, total = 0, 0
    for f in files:
        v = json.load(open(os.path.join(DIR, f), encoding="utf-8"))
        errs = []
        def fail(what, want, got): errs.append(f"    {what}: expected {want!r} got {got!r}")
        if v["name"] not in CHECKS: errs.append(f"    unknown vector file {v['name']}")
        else:
            try:
                CHECKS[v["name"]](v, fail)
            except Exception as ex:                       # a crash is a failure, never a pass
                errs.append(f"    raised {type(ex).__name__}: {ex}")
        total += count(v)
        if errs:
            bad_files += 1
            print(f"FAIL {f}"); print("\n".join(errs))
        else:
            print(f"PASS {f}")
    print(f"{len(files) - bad_files}/{len(files)} carrier vector files pass ({total} cases)")
    return 1 if bad_files or not files else 0

MUTATIONS = [   # (file, rule, description, exact text, replacement). Not listed, being equivalent here: dropping the S < L check (the
                # backing library, OpenSSL, refuses S ≥ L too; vector "S + L" pins it for other implementations) and the x = 0 sign check
                # (its only points, y = ±1, are of small order and refused anyway)
    ("carrier", "N-02", "join before hello", 'if not hello: return {"allow": False, "reason": "hello first"}', 'if not hello and call != "join": return {"allow": False, "reason": "hello first"}'),
    ("carrier", "N-02", "any node vouches for any actor", 'who, bound = hello["who"], conn["node"] in hello["nodes"]', 'who, bound = hello["who"], True'),
    ("carrier", "N-02", "banned actors are members", 'if bound and any(s["roles"].get(who) in READ_ROLES for s in spaces.values()):', 'if bound and any(who in s["roles"] for s in spaces.values()):'),
    ("carrier", "N-02", "strangers read members-only spaces", 'if s is None or sp.startswith("profile:") or s["policy"]["visibility"] == "public":', 'if True:'),
    ("carrier", "N-02", "strangers make any call", """    return {"allow": False, "reason": "not a member's device"}""", '    return {"allow": True, "tier": "open"}'),
    ("carrier", "N-02", "no open joins", 'if call == "append" and sp in spaces and spaces[sp]["policy"]["join"] == "open":', 'if False:'),
    ("carrier", "G-07", "lengths unchecked", 'if n > max_bytes: return ("close", "frame too long")', 'pass'),
    ("carrier", "G-08", "any server name", 'if c["server"] != dialed: return ("abort", "server is not the dialed ServerName")', 'pass'),
    ("carrier", "G-08", "any node", """if node not in nodes: return ("abort", "node is not one of dialed's")""", 'pass'),
    ("carrier", "G-07", "more frames on the control stream", 'if len(frames) > 1: return ("abort", "the control stream carries only the challenge")', 'pass'),
    ("carrier", "X10", "nodes without address or relay", 'if not v["addrs"] and not v["relays"]: return "no address and no relay"', 'pass'),
    ("carrier", "X10", "IPv4 addresses out of range", 'except ValueError:\n        return False\n    return (ip.version', 'except ValueError:\n        return True\n    return (ip.version'),
    ("carrier", "N-03", "tickets need not be JCS", 'if P.jcs(t) != raw: return ("invalid", "not JCS")', 'pass'),
    ("carrier", "X13", "tickets without a server", 'if not (isinstance(t, dict) and set(t) == {"server", "node", "space", "secret"}): return ("invalid", "members")', 'if not isinstance(t, dict): return ("invalid", "members")'),
    ("carrier", "X11", "profile bodies unchecked", '            err = check(item["body"])', '            err = None'),
    ("carrier", "G-05", "context needs only watch", 'return "read" if item["type"] == "context" else "watch"', 'return "watch"'),
    ("carrier", "X15", "bulk signs no profile name", 'profile.encode("utf-8") + b"\\n" + data', 'b"han.timeline/1" + b"\\n" + data'),
    ("carrier", "X15", "revoked keys sign bulk", 'if not k or k["status"] != "valid": return False', 'if not k: return False'),
    ("wire", "§2", "small-order keys accepted", '    if small_order(pt): return "publicKey of small order"', '    pass'),
    ("wire", "§2", "small-order R accepted", 'if r is None or small_order(r) or int.from_bytes(sg[32:], "little") >= _L: return False', 'if r is None or int.from_bytes(sg[32:], "little") >= _L: return False'),
    ("wire", "§2", "-0 accepted", '    if t == "-0": raise _NotIJson("-0")', '    pass'),
    ("wire", "§2", "frames nest without bound", 'if not isinstance(v, dict) or depth(v) > MAX_FRAME_DEPTH: return 0, True', 'if not isinstance(v, dict): return 0, True'),
    ("wire", "R-25", "key.add keys not strict", '        err = strict_key(raw)                                  # §2', '        err = None                                  # §2'),
    ("wire", "G-07", "join is no call", '"cancel", "invite", "join")', '"cancel", "invite")'),
]

def mutations():
    import types
    srcs = {n: open(os.path.join(HERE, n + ".py"), encoding="utf-8").read() for n in ("carrier", "wire")}
    missed = 0
    for fname, rule, desc, old, new in MUTATIONS:
        if srcs[fname].count(old) != 1: print(f"BAD ANCHOR {desc}"); missed += 1; continue
        mods = {}
        for n in ("wire", "carrier"):
            src = srcs[n].replace(old, new) if n == fname else srcs[n]
            m = types.ModuleType(n); m.__file__ = os.path.join(HERE, n + ".py")
            sys.modules[n] = m
            exec(compile(src, n + "_mutant.py", "exec"), m.__dict__)
            mods[n] = m
        global C, W
        C, W = mods["carrier"], mods["wire"]
        import io, contextlib
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf): rc = main()
        caught = rc != 0
        missed += not caught
        print(f"{'caught' if caught else 'MISSED'}  {rule:5} {desc:40} {buf.getvalue().strip().splitlines()[-1]}")
    print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} carrier mutations caught by the vectors")
    return 1 if missed else 0

if __name__ == "__main__":
    sys.exit(mutations() if "--mutations" in sys.argv else main())
