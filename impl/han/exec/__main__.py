"""python -m han.exec --url ws://… --actor @x:… --key KEYFILE --instance k1 --journal PATH --spaces s1:a.localhost[,…]
--actions file.write,http.post [--allow-url PREFIX]… [--sandbox DIR]"""
import argparse, asyncio, os, sys
from .executor import Executor
from .journal import StartRefused

def main(argv=None):
    ap = argparse.ArgumentParser(prog="han-exec", description="HAN executor (architecture.md §4.5)")
    ap.add_argument("--url", required=True)
    ap.add_argument("--actor", required=True)
    ap.add_argument("--key", required=True, help="the instance's key file (its keyId is the instance)")
    ap.add_argument("--instance", help="the instance keyId (must equal the key file's)")
    ap.add_argument("--journal", required=True)
    ap.add_argument("--spaces", required=True, help="comma-separated spaces")
    ap.add_argument("--actions", default="file.write,http.post")
    ap.add_argument("--allow-url", action="append", default=[], help="URL prefix http.post may reach (repeatable)")
    ap.add_argument("--sandbox", default="han-exec-sandbox", help="directory file.write is confined to")
    ap.add_argument("--server", help="server name to dial (default: the actor's)")
    ap.add_argument("--crash-at", help="test hook: die (os._exit 77) at the named journal point, e.g. after-launching")
    a = ap.parse_args(argv)
    hook = None
    if a.crash_at:
        def hook(point, **_):
            if point == a.crash_at: os._exit(77)
    ex = Executor(a.url, a.actor, a.key, a.journal, [s for s in a.spaces.split(",") if s], a.sandbox,
                  [x for x in a.actions.split(",") if x], a.allow_url, a.instance, a.server, hook=hook)
    async def go():
        await ex.start()
        print(f"han-exec {a.actor} instance {ex.instance} ready", flush=True)
        try: await asyncio.Event().wait()
        finally: await ex.stop()
    try: asyncio.run(go())
    except StartRefused as e:
        print(f"han-exec: refusing to start: {e}", file=sys.stderr); return 3
    except KeyboardInterrupt: pass
    return 0

if __name__ == "__main__":
    sys.exit(main())
