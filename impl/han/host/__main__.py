"""han-host:  python -m han.host --url ws://127.0.0.1:8701 --actor @bot:a.localhost --key KEYFILE --space s1:a.localhost
                 --mode auto|guarded --agent scripted|claude --state PATH [--executor @exec:a.localhost] [--outbox PATH] [--cwd DIR]"""
import argparse, asyncio, os
from ..client import Client
from .agent import ScriptedAgent, ClaudeCodeAgent
from .host import Host

def main(argv=None):
    ap = argparse.ArgumentParser(prog="han-host")
    ap.add_argument("--url", required=True); ap.add_argument("--actor", required=True); ap.add_argument("--key", required=True)
    ap.add_argument("--space", required=True); ap.add_argument("--state", required=True, help="the host's journal (SQLite)")
    ap.add_argument("--mode", choices=["auto", "guarded"], default="auto", help="steerMode of the session the host opens")
    ap.add_argument("--agent", choices=["scripted", "claude"], default="scripted")
    ap.add_argument("--executor", help="ActorId that world actions are proposed to")
    ap.add_argument("--outbox", help="client outbox (default: STATE.outbox.db)"); ap.add_argument("--server", help="server name to dial, if not the actor's")
    ap.add_argument("--cwd", default=os.getcwd(), help="workspace for --agent claude")
    a = ap.parse_args(argv)
    agent = ScriptedAgent() if a.agent == "scripted" else ClaudeCodeAgent(a.cwd)
    client = Client(a.url, a.actor, a.key, a.outbox or a.state + ".outbox.db", server=a.server)
    host = Host(client, a.space, agent, a.state, a.mode, a.executor)
    try: asyncio.run(host.run())
    except KeyboardInterrupt: pass
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
