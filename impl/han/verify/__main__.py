"""han-verify:
  python -m han.verify follow --url ws://… --actor @a:S --key KEYFILE --space SP [--state PATH] --server-key S=KEYFILE|PUBKEY[,keyId]
                              [--once | --until SEQ] [--json] [--proof-out PATH] [--gossip-interval SECONDS]
  python -m han.verify proof FILE [--server-key S=KEY…]        (also: `verify proof FILE`)
Exit codes: 0 ok (or a proof accepted), 2 faulty, 3 stopped, 4 moved, 1 usage or a proof rejected."""
import argparse, asyncio, json, os, sys
from ..client import Client
from . import proof as pf
from .verifier import Verifier, StateError

EXIT = {"ok": 0, "faulty": 2, "stopped": 3, "moved": 4}

def describe(st: dict) -> str:
    s = st["state"]
    if s == "ok": return f"ok through {st['through']}"
    if s == "faulty": return f"faulty {st['proof']['kind']} through {st['through']}: {st['proof']['reason']}"
    if s == "moved": return f"moved to {st['to']} through {st['through']}"
    return f"stopped through {st['through']}: {st.get('reason', '')}"

def _keys_args(ap):
    ap.add_argument("--server-key", action="append", default=[], metavar="SERVER=KEYFILE|PUBKEY[,keyId]", help="trusted server key (repeatable)")
    ap.add_argument("--server-keys", action="append", default=[], metavar="FILE", help="JSON {server: {keyId: {publicKey, from}}}")

async def follow(a) -> int:
    keys = pf.load_server_keys(a.server_key, a.server_keys)
    if not keys:
        print("need --server-key SERVER=KEYFILE (the server's public key): a verifier does not trust a key it was not given", file=sys.stderr); return 1
    client = Client(a.url, a.actor, a.key, ":memory:", server=a.server)
    try: v = Verifier(client, keys, a.state, gossip_interval=a.gossip_interval)
    except StateError as ex:
        print(f"state file rejected: {ex}", file=sys.stderr); return 1
    final = {}
    def on_status(st):
        final["st"] = st
        brief = dict(st, proof={"kind": st["proof"]["kind"], "reason": st["proof"]["reason"]}) if st["state"] == "faulty" else st
        print(json.dumps(brief) if a.json else describe(st), flush=True)
    try:
        await client.connect()
        if a.once: final["st"] = await v.verify(a.space); on_status(final["st"])
        else:
            task = asyncio.create_task(v.follow(a.space, on_status))
            while not task.done():
                await asyncio.sleep(0.1)
                st = v.status(a.space)
                if a.until and st["state"] == "ok" and st["through"] >= a.until: task.cancel(); break
            try: final["st"] = await task
            except asyncio.CancelledError: pass
    except KeyboardInterrupt: pass
    except Exception as ex:
        print(f"cannot follow: {ex}", file=sys.stderr); return 3
    finally:
        v.close(); await client.close()
    st = final.get("st") or v.status(a.space)
    if st["state"] == "faulty":
        path = a.proof_out or (a.state + ".proof.json" if a.state else None)
        if path:
            with open(path, "w", encoding="utf-8") as fh: fh.write(pf.dumps(st["proof"]))
            print(f"proof written to {path} (check it with: python -m han.verify proof {path})", flush=True)
        else: print(pf.dumps(st["proof"]), flush=True)
    return EXIT[st["state"]]

def check(a) -> int:
    with open(a.file, encoding="utf-8") as fh: proof = json.load(fh)
    keys = pf.load_server_keys(a.server_key, a.server_keys) or None
    ok, msg = pf.check(proof, keys)
    print(("accepted: " if ok else "rejected: ") + msg)
    return 0 if ok else 1

def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else list(argv)
    if argv[:1] == ["verify"]: argv = argv[1:]
    ap = argparse.ArgumentParser(prog="han-verify")
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("follow", help="verify a space live (R-14) and gossip heads (R-15)")
    for n in ("url", "actor", "key", "space"): f.add_argument("--" + n, required=True)
    f.add_argument("--server", help="the server name to dial (default: the actor's)")
    f.add_argument("--state", help="file where verified records are kept; it is re-verified on start")
    f.add_argument("--once", action="store_true", help="pull to the current head, report and exit")
    f.add_argument("--until", type=int, default=0, help="exit once verified through this seq")
    f.add_argument("--json", action="store_true"); f.add_argument("--proof-out")
    f.add_argument("--gossip-interval", type=float, default=600.0, help="seconds between head gossip (R-15: at most 600)")
    _keys_args(f)
    p = sub.add_parser("proof", help="check a portable proof offline"); p.add_argument("file"); _keys_args(p)
    a = ap.parse_args(argv)
    if a.cmd == "proof": return check(a)
    try: return asyncio.run(follow(a))
    except KeyboardInterrupt: return 0

if __name__ == "__main__":
    sys.exit(main())
