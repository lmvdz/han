"""python -m han.matrix.appservice --registration FILE --homeserver URL --server a.localhost --listen 127.0.0.1:9300 --db seq.db
       --key server-key.json [--ws-port 8701] [--register ACTOR=KEYFILE[,kind[,operator]]]...
FILE is the application-service registration (JSON, as written by matrix-dev/synapse_dev.py): as_token and hs_token are read from it.
--ws-port also serves the generic binding (§8.3) on the same sequencer, so executors and hosts can connect over WebSocket."""
import argparse, asyncio, json, logging, sys
from .. import keys
from ..seq import Sequencer, SeqServer
from .appservice import AppService

def main(argv=None):
    ap = argparse.ArgumentParser(prog="han.matrix.appservice")
    ap.add_argument("--registration", required=True); ap.add_argument("--homeserver", required=True)
    ap.add_argument("--server", required=True); ap.add_argument("--listen", default="127.0.0.1:9300")
    ap.add_argument("--db", required=True, help="the sequencer's SQLite file (the appservice's own state goes to DB.matrix)")
    ap.add_argument("--key", required=True); ap.add_argument("--ws-port", type=int)
    ap.add_argument("--register", action="append", default=[], metavar="ACTOR=KEYFILE[,kind[,operator]]")
    a = ap.parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    with open(a.registration, encoding="utf-8") as fh: reg = json.load(fh)
    host, port = a.listen.rsplit(":", 1)
    seq = Sequencer(a.server, keys.load_or_generate(a.key, "h1"), a.db)
    for spec in a.register:
        actor, rest = spec.split("=", 1); pub, *more = rest.split(",")
        key_id, pk = keys.public_of(pub)
        seq.register(actor, more[0] if more else "human", key_id, pk, (more[1] if len(more) > 1 else None) or None)
    async def run():
        svc = await AppService(seq, a.homeserver, reg["as_token"], reg["hs_token"], a.db + ".matrix").start(host, int(port))
        print(f"han-matrix {a.server} as {svc.user} listening on {host}:{svc.port}", flush=True)
        if a.ws_port:
            ws = await SeqServer(seq, host, a.ws_port).start(); print(f"generic binding on {ws.url}", flush=True)
        await asyncio.Event().wait()
    try: asyncio.run(run())
    except KeyboardInterrupt: pass
    finally: seq.close()
    return 0

if __name__ == "__main__":
    sys.exit(main())
