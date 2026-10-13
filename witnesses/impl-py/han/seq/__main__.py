"""han-seq:  python -m han.seq --server a.localhost --port 8701 --db PATH --key PATH [--host 127.0.0.1] [--register ACTOR=KEYFILE[,kind[,operator]]]...
                [--peers han-peers.json] [--peer NAME=URL]... [--advertise ws://host:port/han]
            python -m han.seq register --server S --db PATH --key PATH --actor @x:S --pub KEYFILE|PUBKEY [--kind human] [--operator @o:S]   (server stopped)"""
import argparse, asyncio, logging, sys
from .. import keys
from ..core import P
from .sequencer import Sequencer
from .server import SeqServer
from .federation import Federation, load_peers

def _common(ap):
    ap.add_argument("--server", required=True, help="this server's name, e.g. a.localhost")
    ap.add_argument("--db", required=True, help="SQLite file")
    ap.add_argument("--key", required=True, help="the server's Ed25519 key file (created if missing)")

def _register(seq, actor, pub, kind="human", operator=None):
    key_id, pk = keys.public_of(pub)
    seq.register(actor, kind, key_id, pk, operator or None)
    print(f"registered {actor} ({kind}) key {key_id}")

def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["register"]:
        ap = argparse.ArgumentParser(prog="han-seq register"); _common(ap)
        ap.add_argument("--actor", required=True); ap.add_argument("--pub", required=True)
        ap.add_argument("--kind", default="human", choices=["human", "agent", "service"]); ap.add_argument("--operator")
        a = ap.parse_args(argv[1:])
        if a.kind != "human" and not a.operator: ap.error(f"--operator is required for --kind {a.kind} (a non-human profile names its operator)")
        seq = Sequencer(a.server, keys.load_or_generate(a.key, "h1"), a.db)
        _register(seq, a.actor, a.pub, a.kind, a.operator); seq.close(); return 0
    ap = argparse.ArgumentParser(prog="han-seq"); _common(ap)
    ap.add_argument("--port", type=int, default=8701); ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--register", action="append", default=[], metavar="ACTOR=KEYFILE[,kind[,operator]]")
    ap.add_argument("--peers", help="han-peers config: JSON {server name: http(s) base URL of its /.well-known/han}")
    ap.add_argument("--peer", action="append", default=[], metavar="NAME=URL", help="one peer, as in --peers")
    ap.add_argument("--advertise", help="the sequencer endpoint published in /.well-known/han (default ws://HOST:PORT/han)")
    a = ap.parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    seq = Sequencer(a.server, keys.load_or_generate(a.key, "h1"), a.db)
    for spec in a.register:
        actor, rest = spec.split("=", 1)
        pub, *more = rest.split(",")
        _register(seq, actor, pub, *more)
    peers = load_peers(a.peers) if a.peers else {}
    peers.update(p.split("=", 1) for p in a.peer)
    async def run():
        srv = await SeqServer(seq, a.host, a.port, advertise=a.advertise).start()
        fed = Federation(seq, peers)
        print(f"han-seq {a.server} listening on {srv.url}", flush=True)
        try: await asyncio.Event().wait()
        finally: await fed.close()
    try: asyncio.run(run())
    except KeyboardInterrupt: pass
    finally: seq.close()
    return 0

if __name__ == "__main__":
    sys.exit(main())
