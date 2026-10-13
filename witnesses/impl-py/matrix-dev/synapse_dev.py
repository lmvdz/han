"""Start and stop a throwaway Synapse in Podman for the Matrix binding (architecture.md §8.2).

    python matrix-dev/synapse_dev.py up   [--port 18008] [--as-url http://127.0.0.1:9300] [--state DIR]
    python matrix-dev/synapse_dev.py down
    python matrix-dev/synapse_dev.py status

`up` removes any earlier container and volume, generates a fresh config for server name a.localhost, patches it (SQLite, no
federation, shared-secret registration, rate limits off, one application service), copies it into a named volume and runs
the container on the host's network (so Synapse listens on 127.0.0.1:PORT and reaches the appservice at 127.0.0.1:AS_PORT; with
WSL "mirrored" networking that is also the Windows host). It writes `registration.yaml` (JSON, which is valid YAML) and
`synapse.json` (tokens, secret, URLs) to the state directory; `python -m han.matrix.appservice --registration` reads the first.

Podman is reached through a machine connection: env HAN_PODMAN_CONNECTION (default `actual-wsl`), with
CONTAINERS_MACHINE_PROVIDER=wsl. HAN_PODMAN="" (empty connection) uses the local default."""
from __future__ import annotations
import argparse, dataclasses, hashlib, hmac, json, os, re, secrets, socket, subprocess, sys, time, urllib.request

IMAGE = "docker.io/matrixdotorg/synapse:v1.120.2"
SERVER_NAME = "a.localhost"
NAME = "han-synapse"                    # container name; the volume is NAME-data. Tests use unique names so parallel runs don't collide.
HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_STATE = os.path.join(HERE, "state")

@dataclasses.dataclass
class Synapse:
    base_url: str
    server_name: str
    as_url: str
    as_token: str
    hs_token: str
    shared_secret: str
    registration_path: str
    sender: str = "@han-seq:" + SERVER_NAME
    name: str = NAME

def _podman_cmd(*args) -> list[str]:
    conn = os.environ.get("HAN_PODMAN_CONNECTION", "actual-wsl")
    return ["podman"] + (["--connection", conn] if conn else []) + list(args)

def podman(*args, check=True, timeout=300) -> subprocess.CompletedProcess:
    env = dict(os.environ, CONTAINERS_MACHINE_PROVIDER=os.environ.get("CONTAINERS_MACHINE_PROVIDER", "wsl"))
    r = subprocess.run(_podman_cmd(*args), capture_output=True, text=True, timeout=timeout, env=env)
    if check and r.returncode != 0:
        raise RuntimeError(f"podman {' '.join(args)} failed ({r.returncode}): {r.stderr.strip() or r.stdout.strip()}")
    return r

def unavailable() -> str | None:
    """None if Podman is reachable and the Synapse image is present; else the reason (used to skip tests)."""
    try:
        if podman("info", "--format", "{{.Host.Arch}}", check=False, timeout=60).returncode != 0: return "podman is not reachable (is the machine started?)"
        if podman("image", "exists", IMAGE, check=False, timeout=60).returncode != 0: return f"image {IMAGE} is not pulled"
    except (OSError, subprocess.SubprocessError) as ex:
        return f"podman is not usable: {ex}"
    return None

def free_port() -> int:
    with socket.socket() as sk:
        sk.bind(("127.0.0.1", 0)); return sk.getsockname()[1]

def down(name: str = NAME):
    podman("rm", "-f", name, name + "-cfg", check=False)
    podman("volume", "rm", "-f", name + "-data", check=False)

def _patch(cfg: str, port: int) -> str:
    cfg = re.sub(r"(?m)^(\s*)- port: \d+", rf"\1- port: {port}\n\1  bind_addresses: ['127.0.0.1']", cfg, count=1)
    cfg = re.sub(r"(?ms)^trusted_key_servers:.*?(?=^\S|\Z)", "trusted_key_servers: []\n", cfg)
    return cfg + """
suppress_key_server_warning: true
enable_registration: false
federation_domain_whitelist: []
app_service_config_files:
  - /data/han-appservice.yaml
rc_message: {per_second: 1000, burst_count: 100000}
rc_registration: {per_second: 1000, burst_count: 100000}
rc_login:
  address: {per_second: 1000, burst_count: 100000}
  account: {per_second: 1000, burst_count: 100000}
  failed_attempts: {per_second: 1000, burst_count: 100000}
rc_joins:
  local: {per_second: 1000, burst_count: 100000}
  remote: {per_second: 1000, burst_count: 100000}
rc_invites:
  per_room: {per_second: 1000, burst_count: 100000}
  per_user: {per_second: 1000, burst_count: 100000}
  per_issuer: {per_second: 1000, burst_count: 100000}
rc_admin_redaction: {per_second: 1000, burst_count: 100000}
"""

def up(port: int = 18008, as_url: str = "http://127.0.0.1:9300", state: str = DEFAULT_STATE, wait: float = 120.0, name: str = NAME) -> Synapse:
    """port 0 picks a free one. `name` names the container (and `name`-data the volume); an earlier one of that name is removed."""
    os.makedirs(state, exist_ok=True)
    port = port or free_port()
    down(name)
    VOLUME, CONTAINER = name + "-data", name
    podman("volume", "create", VOLUME)
    podman("run", "--rm", "-v", f"{VOLUME}:/data", "-e", f"SYNAPSE_SERVER_NAME={SERVER_NAME}", "-e", "SYNAPSE_REPORT_STATS=no", IMAGE, "generate")
    podman("create", "--name", CONTAINER + "-cfg", "-v", f"{VOLUME}:/data", "--entrypoint", "sleep", IMAGE, "1")
    try:
        gen = os.path.join(state, "homeserver.generated.yaml")
        podman("cp", f"{CONTAINER}-cfg:/data/homeserver.yaml", gen)
        with open(gen, encoding="utf-8") as fh: cfg = fh.read()
        secret = re.search(r'(?m)^registration_shared_secret: "(.*)"$', cfg).group(1)
        reg = {"id": "han-seq", "url": as_url, "as_token": secrets.token_hex(32), "hs_token": secrets.token_hex(32),
               "sender_localpart": "han-seq", "rate_limited": False,
               "namespaces": {"users": [{"exclusive": True, "regex": r"@han-seq:" + re.escape(SERVER_NAME)}], "aliases": [], "rooms": []}}
        reg_path, hs_path = os.path.join(state, "registration.yaml"), os.path.join(state, "homeserver.yaml")
        with open(reg_path, "w", encoding="utf-8") as fh: json.dump(reg, fh, indent=1)
        with open(hs_path, "w", encoding="utf-8") as fh: fh.write(_patch(cfg, port))
        podman("cp", hs_path, f"{CONTAINER}-cfg:/data/homeserver.yaml")
        podman("cp", reg_path, f"{CONTAINER}-cfg:/data/han-appservice.yaml")
    finally:
        podman("rm", "-f", CONTAINER + "-cfg", check=False)
    podman("run", "--rm", "-v", f"{VOLUME}:/data", "--user", "root", "--entrypoint", "chown", IMAGE, "-R", "991:991", "/data")
    podman("run", "-d", "--name", CONTAINER, "--network", "host", "-v", f"{VOLUME}:/data", "-e", "SYNAPSE_CONFIG_PATH=/data/homeserver.yaml", IMAGE)
    base = f"http://127.0.0.1:{port}"
    deadline = time.time() + wait
    while True:
        try:
            if urllib.request.urlopen(base + "/health", timeout=3).status == 200: break
        except OSError: pass
        if time.time() > deadline:
            raise RuntimeError("synapse did not become healthy:\n" + podman("logs", "--tail", "40", CONTAINER, check=False).stdout[-3000:])
        time.sleep(1)
    s = Synapse(base, SERVER_NAME, as_url, reg["as_token"], reg["hs_token"], secret, reg_path, name=name)
    with open(os.path.join(state, "synapse.json"), "w", encoding="utf-8") as fh: json.dump(dataclasses.asdict(s), fh, indent=1)
    return s

def logs(name: str = NAME, tail: int = 60) -> str:
    r = podman("logs", "--tail", str(tail), name, check=False)
    return r.stdout + r.stderr

def register_mac(secret: str, nonce: str, user: str, password: str) -> str:
    return hmac.new(secret.encode(), b"\0".join([nonce.encode(), user.encode(), password.encode(), b"notadmin"]), hashlib.sha1).hexdigest()

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("cmd", choices=["up", "down", "status"])
    ap.add_argument("--port", type=int, default=18008)
    ap.add_argument("--as-url", default="http://127.0.0.1:9300", help="where Synapse reaches the appservice")
    ap.add_argument("--state", default=DEFAULT_STATE)
    a = ap.parse_args(argv)
    if a.cmd == "up":
        s = up(a.port, a.as_url, a.state)
        print(f"synapse up at {s.base_url}; registration: {s.registration_path}")
        print(f"run: python -m han.matrix.appservice --registration {s.registration_path} --homeserver {s.base_url} --server {s.server_name} --listen {a.as_url.split('//')[1]} --db seq.db --key server-key.json")
    elif a.cmd == "down": down(); print("synapse removed")
    else:
        why = unavailable()
        print(why or podman("ps", "--filter", f"name={NAME}", "--format", "{{.Names}} {{.Status}}").stdout.strip() or "podman ok, synapse not running")
    return 0

if __name__ == "__main__":
    sys.exit(main())
