# Shared runner setup

This guide sets up a HAN shared runner on a Linux VM: a machine the team owns that hosts a space home and,
from M4 on, runs agent sessions for members (PLAN §3, §6.4 Shared runners, §6.8; security requirements §10,
SR-83 to SR-88). It was first done on a boat.dev sandbox, so the boat.dev notes come first; every other step
works on any Ubuntu 24.04 VM where you have root.

Everything that needs root is one command, `sudo han runner setup`; the rest is `han home`, `han agent add` and `han executor add`, which are safe to run again and print what they changed. Nothing in this guide is specific to one machine. Replace the placeholders:

| Placeholder | Meaning |
|---|---|
| `<server>` | the ServerName of the home, a lowercase DNS name. Use a name you control; until it has DNS and a TLS front for `/.well-known/han`, a reserved local name such as `runner.han.home.arpa` works for direct dials |
| `<you>` | your actor name, for example `@alice` |
| `<pubkey>` | the public key (node id) of your device key, printed by `han key <file>` |
| `<udp-port>` | the UDP port of the home's QUIC endpoint; this guide uses 7841 |

## 1. The machine

**Size.** Two vCPUs and 4 GB of RAM build `han` in release mode with `-j2` and run the home comfortably. Give
it more when it starts running agent sessions (M4).

**boat.dev specifics.** A boat.dev "VM" is a sandbox, and three of its habits matter for a runner:

- **Auto-stop.** A new sandbox stops one hour after it starts unless you create it with `ttlSeconds: null`
  (this needs a payment method on the account). Create the runner with auto-stop off:
  `POST /sandboxes {"type":"small","ttlSeconds":null,"noEnv":true}`. `noEnv` keeps the account's secrets,
  GitHub access and Boat credentials off the runner: it should hold nothing but what HAN gives it.
- **A resume is a new machine.** Stop and resume keep the disk (enabled systemd units start again by
  themselves) but run on a new machine with a new SSH host key and possibly a new address. The runner's HAN
  keys live on disk, so its node ids survive; its direct addresses do not, which is one more reason to rely on
  a relay (§5).
- **Boat's own access.** Boat reaches the sandbox through its agent on TCP 8911 and an outgoing WireGuard
  tunnel. A firewall rule that blocks either marks the sandbox degraded. Keep both open (§4).

**SSH access.** Generate an Ed25519 key on your machine, keep the private half readable only by you, and
authorize the public half through the API, which also returns the host key to pin:

```sh
ssh-keygen -t ed25519 -N "" -f ~/.ssh/han_runner
curl -sS -X POST "https://boat.dev/api/v1/sandboxes/<sandbox>/sshkey" \
  -H "Authorization: Bearer $BOAT_API_KEY" -H "Content-Type: application/json" \
  -d "{\"key\":\"$(cat ~/.ssh/han_runner.pub)\"}" > sshkey.json
# hostKey -> a known_hosts file under a fixed alias; sshEndpoint (host:port) when set, else machineIp:22
ssh -i ~/.ssh/han_runner -p <port> -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile=~/.ssh/known_hosts_han_runner -o HostKeyAlias=han-runner user@<host>
```

Load the API key only inside the command that needs it, never echo it, and never copy it to the runner: no
tool on the runner needs it. Fetch and pin the host key again after every resume.

## 2. Accounts and hardening

Two OS accounts, and later one per agent session:

| Account | What it is | Login |
|---|---|---|
| `user` (or your admin account) | the administrator; sudo | SSH, key only |
| `han` | owns and runs `han seq` and `han node`; owns `/var/lib/han`; no sudo, no password | none |
| one per agent session (M4) | runs one sandboxed agent | none |

`sudo han runner setup` (§7.1) makes the `han`, `han-exec` and `han-agentN` users with the modes below, and is safe to run again. These are the commands it runs for `han`:

```sh
sudo useradd --create-home --shell /bin/bash --comment "HAN runner" han
sudo passwd -l han
sudo chmod 750 /home/han
sudo install -d -o han -g han -m 700 /var/lib/han
```

**SSH: key only, no root, admin account only.** sshd keeps the first value it reads for each option, and reads
`sshd_config.d` in name order, so give the file a name that sorts before the image's own (the boat.dev image
ships `00-ascii-lockdown.conf`): `/etc/ssh/sshd_config.d/00-0-han-hardening.conf`:

```
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
PubkeyAuthentication yes
AllowUsers user
```

Check with `sudo sshd -t`, reload with `sudo systemctl reload ssh`, confirm with
`sudo sshd -T | grep -E '^(passwordauthentication|permitrootlogin|allowusers)'`, and check that a second SSH
login works before you close the first. On boat.dev, keys authorized through the sshkey API are for `user`, so
`AllowUsers user` keeps that path working.

**Unattended security updates.**

```sh
sudo apt-get install -y unattended-upgrades
printf 'APT::Periodic::Update-Package-Lists "1";\nAPT::Periodic::Unattended-Upgrade "1";\n' \
  | sudo tee /etc/apt/apt.conf.d/20auto-upgrades
sudo systemctl enable --now apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service
```

Ubuntu's default `50unattended-upgrades` installs from the security pocket only, which is what a runner
wants.

## 3. Software

```sh
sudo apt-get install -y bubblewrap git build-essential pkg-config curl ca-certificates xz-utils
```

**Node 22**, checksum-verified, root-owned and read-only to `han` (the image's own Node may be a different
major version; leave it alone):

```sh
NV=$(curl -fsSL https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt \
     | grep -oE 'node-v22\.[0-9]+\.[0-9]+-linux-x64\.tar\.xz' | head -1)
cd /tmp && curl -fsSLO "https://nodejs.org/dist/latest-v22.x/$NV"
curl -fsSL https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt | grep " $NV\$" | sha256sum -c -
sudo mkdir -p /opt/node22 && sudo tar -xJf "$NV" -C /opt/node22 --strip-components=1
```

**pi**, in its own root-owned prefix on that Node:

```sh
sudo env PATH=/opt/node22/bin:$PATH npm install -g --prefix /opt/pi @mariozechner/pi-coding-agent
```

From M4, `han` manages its own pinned Node and adapter versions in its data folder (PLAN §6.4); `/opt/node22`
and `/opt/pi` are for trying pi by hand and for the M4 spike.

**Rust and han**, built as `han` with its own rustup (the toolchain is pinned by `rust-toolchain.toml`), then
installed root-owned so the service account cannot replace its own binary:

```sh
sudo -iu han bash -c '
  curl --proto "=https" --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh
  sh /tmp/rustup-init.sh -y -q --profile minimal --default-toolchain none --no-modify-path
  export PATH=$HOME/.cargo/bin:$PATH
  git clone https://github.com/lmvdz/han ~/src/han && cd ~/src/han
  cargo build --release --locked -p han -j2'
sudo install -o root -g root -m 755 /home/han/src/han/target/release/han /usr/local/bin/han
```

To update: `git pull --ff-only` and rebuild as `han`, `install` again, then restart the services. Signed
self-update arrives with cargo-dist releases (PLAN §6.8).

## 4. Firewall

Open SSH, the home's QUIC port, and on boat.dev the Boat agent port. Nothing else.

```sh
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
sudo ufw allow 8911/tcp comment 'boat agent (boat.dev only)'
sudo ufw allow <udp-port>/udp comment 'han seq QUIC'
sudo ufw enable
```

The boat.dev image also opens its desktop-streaming ports (Sunshine and Moonlight, and the Moonlight gateway on 8090/tcp). A runner needs no
desktop: delete those rules (`sudo ufw status numbered`, then `sudo ufw delete <n>`) and check that the sandbox
still reports `"health": "ok"`. The well-known HTTP server listens on `127.0.0.1:8787` only; publish it through
a TLS front on your `<server>` name when the home needs to be found by name, never by opening 8787.

## 5. Relays

Every iroh endpoint is reachable by node id through a relay, and directly when the network allows. A VM behind
NAT with no public IPv4 of its own (on boat.dev, when the sshkey call returns `sshEndpoint`) takes no inbound
UDP, so QUIC to the runner goes through the relay; iroh still tries to hole-punch to a direct path. No trust
decision depends on the relay (SR-20): the connection is authenticated end to end by node id.

The services below start with `--relay n0`, n0's public relays. To self-host, run `iroh-relay` on a host with a
public address and TLS, then pass its URL (`--relay https://relay.example`) to every node of the team, and turn
the public fallback off (`--relay <url>` alone does that).

## 6. Services

`han seq` is the space home: the sequencer and its well-known document. `han node` is the runner's own device
daemon, acting as the runner actor (a `service` actor with an owner as operator, PLAN §6.4 Shared runners). It
follows the runner's spaces and, from M4, supervises agent sessions.

**The home.** Make the home as `han`. `han home init` creates its data folder, keys and `home.json` (the server name, ports and relays) and prints the ticket file:

```sh
sudo -u han han home init --server <server> --data /var/lib/han/seq --bind 0.0.0.0:<udp-port> --http 127.0.0.1:8787 --relay n0
sudo -u han install -d -m 700 /var/lib/han/node /var/lib/han/node/keys
sudo -u han han key /var/lib/han/node/keys/device.key   # the runner actor's device key (prints its public key)
```

Run it again and it changes nothing. `han seq --data /var/lib/han/seq` then needs no other flag.

**Profiles.** The first profiles are registered on the running home over its admin socket (reachable by user `han` only; no restart). The same command registers every later profile:

```sh
sudo -u han han home register <you> --kind human --key-id d1 --public-key=<pubkey> --data /var/lib/han/seq
sudo -u han han home register @runner:<server> --kind service --key-id d1 --public-key=<runner-pubkey> --operator <you> --data /var/lib/han/seq
```

(`han seq register` is the old name. Write `--public-key=<key>`: a key may begin with `-`.) If the home was upgraded but not restarted, it still runs the old binary and has no admin socket: the command says so and names the pid; restart the unit.

**Configuration.** `/etc/han/seq.env` (root-owned, mode 644; it holds names, no secrets) and `/etc/han/node.env`:

```sh
# seq.env
HAN_RELAY=n0
# node.env
HAN_ACTOR=@runner:<server>
HAN_RELAY=n0
```

**Units.** `/etc/systemd/system/han-seq.service`:

```ini
[Unit]
Description=HAN space home (han seq)
After=network-online.target
Wants=network-online.target

[Service]
User=han
Group=han
EnvironmentFile=/etc/han/seq.env
ExecStart=/usr/local/bin/han seq --data /var/lib/han/seq
KillSignal=SIGINT
Restart=always
RestartSec=5
UMask=0077
NoNewPrivileges=yes
CapabilityBoundingSet=
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/han/seq
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
ProtectProc=invisible
ProcSubset=pid
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources

[Install]
WantedBy=multi-user.target
```

`han-node.service` is the same with `Description=HAN runner node (han node)`,
`After=network-online.target han-seq.service`, `EnvironmentFile=/etc/han/node.env`,
`ReadWritePaths=/var/lib/han/node` and:

```ini
ExecStart=/usr/local/bin/han node --actor ${HAN_ACTOR} --key-id d1 --data /var/lib/han/node --file-keys --relay ${HAN_RELAY}
```

`KillSignal=SIGINT` matters: `han` shuts down cleanly on Ctrl-C. `--file-keys` keeps the keys in the data folder
(mode 600, owned by `han`), since a headless runner has no keychain.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now han-seq han-node
journalctl -u han-seq -u han-node -f
```

`systemd-analyze security han-seq` scores the sandbox of each unit.

Agent sessions (§7): `han-node` supervises `han host` children that sandbox agents with
bubblewrap. bubblewrap needs user namespaces, which `RestrictNamespaces=yes` forbids, so that unit's sandbox is
relaxed then, and only for `han-node`; `han-seq` keeps it.

## 7. Agent sessions

SR §10 asks for isolation between sessions, and between sessions and the runner's own processes:

- **One OS user per agent actor** (SR-83): `han-agent1`, `han-agent2`, … with no login and no sudo. Sessions of
  the same agent actor share that user, but each still gets its own bubblewrap sandbox, worktree, temp
  directory and proxy. The user is named in the node's `agents.json` (below).
- **`han` is never an agent user** (SR-85): `/var/lib/han` is mode 700, so no session can read the home's
  database or any key.
- **Each session's proxy holds only that session's model key** (SR-84); the key never enters the sandbox
  (PLAN §6.4 Model credentials). A Claude subscription is the exception the driver records in
  `isolation.gaps`: the agent has its own **per-agent login directory** (`<state>/claude-config`, the agent's
  `CLAUDE_CONFIG_DIR`), bound read-write into that agent's sandbox and nothing else of any home directory is.
  The login is inside the agent's trust boundary, so use an account you would let that agent use.
- **A host or an executor cannot approve or govern** (SR-06, SR-106): `han host` and `han exec` each run
  their own node as their own actor (the agent's key, the executor's key), and work through it held to their
  class. A host sends agent commands only (sessions, tasks, status, leases, proposals, and its changeset upload);
  an executor adds claims and outcomes. Approvals, policy, membership, invites and devices are refused before
  they reach the node, whatever the process asks. This holds the same for an agent that runs as another OS
  user. The `-host` and `-exec` pipes of `han node` exist, but roles do not use them yet: the person's node
  signs as the person, so a role cannot work as its agent through it until a connection can be bound to an
  actor. `han role-probe host|exec` (conformance builds only) shows the refusals.

- **Clean-up** (SR-87): after a session ends, its worktree, temp directory, proxy and credentials are removed;
  shared package caches are mounted read-only into sandboxes.
- **Egress**: the sandbox reaches only its local proxy; with a tailnet exit node, follow the Tailscale recipe
  in PLAN §6.4 and SR-88 (tag `tag:han-runner`, only `autogroup:internet` through the exit node, no route to
  members' devices).

### 7.1 The root-level setup, once: `sudo han runner setup`

The node never needs root at run time. It starts a host as another user through a **systemd template unit** and a polkit rule that lets the node's user start and stop exactly those units. Everything that needs root is one command, and it is the only `han` command that does:

```sh
sudo han runner setup --agents 2            # add --dry-run to see the plan and change nothing
```

It makes the OS users (`han`, `han-exec`, `han-agent1` … `han-agent<N>`, each with a locked password), the `han-github` directory `/var/lib/han/github` (mode 700, owned by `han`), the agents directory `/var/lib/han-agents` (mode 755) with one state directory per agent (`2770`, setgid, in the agent's own group; the node's user joins that group and no other agent's), and installs `han-agent@.service`, `han-exec.service`, `han-review@.service` and `han-github.service` (all installed, **left disabled**; `han-github` also does not start until its config exists), `/etc/polkit-1/rules.d/50-han-agent.rules` (the node's user may start and stop `han-agent@han-agent<N>` and nothing else) and the node's drop-in `/etc/systemd/system/han-node.service.d/10-agents.conf` (`UMask=0007`, `ReadWritePaths=/var/lib/han/node /var/lib/han-agents`), then reloads systemd. Each step says `changed:` or `unchanged:`, so a second run reports no change. The unit files are in `integrations/runner/`; `install-agent-user.sh` and `install-executor.sh` there are thin wrappers around the command.

It also looks at the running services. If `han-seq`, `han-node`, `han-exec`, `han-github` or a running `han-agent@<user>` unit runs a binary that has since been replaced on disk (after you installed a new `han`), or the node has not yet picked up its new group, it says which and how to restart it, or restarts it with `--restart-stale`. A home that kept running the old binary has no admin socket, and everything that registers profiles fails against it; this is how you find out before that. An agent unit that has a session running (its cgroup holds more than the host) is reported as `busy:` and not restarted, even with `--restart-stale`: restarting would kill the session, so restart it when the session ends. Each restarted unit is waited for until it serves (an agent unit: until its host has written a fresh admission file).

An agent unit outlives a restart of `han-node`, and the restarted node takes it back: at start it looks, for each agent of `agents.json` that has an `osUser`, whether `han-agent@<user>` is active, reads the spawn file the node left for it and lists the host again under its old name (`adopted host-… (its unit was already running)` in the node's log). So `han agent stop`, the node's roles and the board are right after `--restart-stale`.

A host that exits without closing its session (a `kill`, a stop that ran into systemd's timeout, the OOM killer) leaves the session open with its last status, `idle`. The node watches the roles (its own children, and the `han-agent@` units by systemd's state): ten seconds after a host has gone, and if the session's own `session.close` has not arrived by then, the board shows that session as `failed` with the reason `host exited` (`state_reason`). This is the node's reading, not a log entry (only the agent signs its session), and a `session.close` that arrives later wins. A host asked to stop (SIGINT, as the units send, or SIGTERM) closes its session on the way out; one that fails says why in the session's last status and in its own log (`session … ended Failed: <reason>`).

Per agent, as the node's user (no root):

```sh
sudo -u han han agent add coder --operator <you> --os-user han-agent1 --sandbox bwrap --harness claude \
    --space <space> --home <ticket> --home-data /var/lib/han/seq --data /var/lib/han/node
```

`--os-user` records the agent in `agents.json` (the node reads it at every start; no restart) and puts the agent's key in `/var/lib/han-agents/han-agent1/keys`, where only that user and the group can read it. `han agent add` then registers the profile (`--home-data` is this machine's home), has the operator confirm it (only if this node acts as the operator; otherwise it prints `han actor confirm …` for the operator's device) and makes the agent a member of the space. `stateDir` defaults to `/var/lib/han-agents/<osUser>`; the agent's key, sessions, taint store and login live there. `--harness claude` installs the pinned adapter into the node's data folder; for an OS-user agent install it where the agent can read it: `sudo -u han han agent setup-harness claude --data /var/lib/han/node --dir /opt/han/claude` (then `chmod -R a+rX /opt/han`). To change an agent later, use `han agent set` (it needs no root and no running node, and prints `changed:` or `unchanged:`); do not edit `agents.json` by hand:

```sh
sudo -u han han agent set @coder:<server> --os-user han-agent1 --sandbox bwrap --toolchains rust --harness claude --data /var/lib/han/node
```

`--toolchains` takes a comma separated list (`rust`; `""` for none), `--allow-advisory` lets the agent run with no OS sandbox (advisory), and only what is given changes. A value no launch could use (`--sandbox chroot`) is refused and nothing is written.

One session per OS user runs at a time (the unit is the user's); a second `han agent start` for the same user answers `conflict`.

Log the agent in once, as its own user (this runs `claude` interactively, outside any sandbox):

```sh
sudo -u han-agent1 han agent login @coder:<server> --dir /var/lib/han-agents/han-agent1/claude-config
sudo -u han-agent1 han agent login @coder:<server> --dir /var/lib/han-agents/han-agent1/claude-config --check
```

`--check` prints only whether a login exists, never its contents.

### 7.2 A Rust toolchain inside the agent sandboxes (admin, once)

Without it an agent cannot run `cargo`, and proposes code it never built. The sandbox gets the toolchain
**read-only**, a writable `CARGO_HOME` of its own (`<state>/cargo-home`, kept between that agent's sessions),
and `crates.io`, `static.crates.io` and `index.crates.io` on its proxy allow-list, and nothing else. Nothing is
downloaded into the toolchain inside the sandbox: install everything a repository pins first.

The agent runs as `han-agent1` under `ProtectHome=yes`, so it cannot read `/home/han/.rustup` (mode 750, owned by `han`). Share one root-owned toolchain instead; bubblewrap already binds `/opt` read-only. One command provisions it:

```sh
sudo han runner setup --toolchain rust --repo /srv/repos/han --repo /srv/repos/other --data /var/lib/han/node
sudo han runner setup --toolchain rust --rust-toolchain 1.97.1 --dry-run     # see the plan, change nothing
```

It installs rustup into `/opt/rust` (`RUSTUP_HOME=/opt/rust/rustup`, `CARGO_HOME=/opt/rust/cargo`, the minimal profile plus clippy and rustfmt), installs the toolchain each `--repo` pins in its `rust-toolchain.toml` and every `--rust-toolchain` you name, makes `/opt/rust` readable by all and writable by root only (`chmod -R a+rX`), and records `toolchains` and `toolchainPaths` in `agents.json` (below). It is idempotent: a second run says `unchanged:` for each step and installs only what is new, so run it again when a repository bumps its pin. It also does the users, units and polkit rule of 7.1; `--toolchain-only` skips those (for a container with no systemd). It is Linux and root only, like the rest of `han runner setup`. `han doctor --repo <path>` tells you when a pin is not installed and prints this command with the repository in it.

That leaves this in `agents.json` (read at every start; `han agent set` changes the per agent part):

```json
{
  "sandbox": "bwrap",
  "toolchains": ["rust"],
  "toolchainPaths": { "rust": { "rustupHome": "/opt/rust/rustup", "cargoBin": "/opt/rust/cargo/bin" } },
  "agents": { "@coder:<server>": { "osUser": "han-agent1" } }
}
```

`"toolchains"` at the top is every agent's; an agent's own `"toolchains": []` turns it off for that agent. To
reuse `han`'s own `~/.rustup` instead: `sudo chmod 711 /home/han`, `sudo chmod -R a+rX /home/han/.rustup
/home/han/.cargo/bin`, change `ProtectHome=yes` to `ProtectHome=read-only` in `han-agent@.service`, and point
`toolchainPaths` at those directories. A session whose repository pins a toolchain that is not installed
refuses to start and names it. Check from the agent's side: `sudo -u han-agent1 /opt/rust/cargo/bin/cargo --version`
works, and `cargo build` in a session reaches crates.io through the proxy only.

### 7.3 MXC and the in-sandbox loopback port (Windows)

An agent in an MXC container reaches its tools through one loopback port inside the container (the shim's). Since Windows 11 Insider build 26220 (October 2026) the default MXC network rule fails every loopback connect from inside a container with error 10013, even to its own listener. HAN does not trust a build number. The first time a process needs MXC, it runs `han shim selftest` in one container (a few seconds, once per process) under the policy a launch uses: it binds a loopback port and connects to it. HAN tries two policies and uses the first that works: the strict one (egress allowed to that one port only), then MXC's identity-less loopback proxy (egress denied, the container's proxy set to the shim's port; Microsoft documents it as a development and testing mode). Its `ingress` and `hostLoopback` settings are `Allow`, so `sandbox_probe` checks that nothing connects into the container, from the host or from the PC's LAN address, and fails if anything does. `ingress.hostLoopback = Allow` alone, with an allow rule, is never used.

If neither works, MXC is unavailable on this build and HAN fails closed:

- `han agent start` for an agent set to `mxc` (or to `auto` without `--allow-advisory`) is refused before any host starts, with the reason: `MXC on this Windows build blocks the in-sandbox loopback port (10013)`.
- `han node status` lists it as a problem, and `han doctor` reports `sandbox.mxc.loopback` with the agents it stops.
- Fix: use WSL2+bubblewrap when available (`han agent set <agent> --sandbox bwrap`), or run the agent on the runner, or `--allow-advisory` (not recommended: no confinement).

`HAN_MXC_FORCE_UNAVAILABLE=loopback` makes the probe report this state without running the container; it can only refuse.

**Where the sandbox is checked.** The check runs where the host will run, because the answer differs. A host that is the node's own child (an agent with no `osUser`; always the case on Windows) is probed in the node's process, so `han agent start` fails at once. An OS-user agent's host runs in `han-agent@<user>`, not in `han-node.service`, whose hardening (`RestrictNamespaces=yes`, `ProcSubset=pid`, `ProtectKernelTunables=yes`) makes bubblewrap fail there by design (`Can't read /proc/sys/kernel/overflowuid`). The node therefore does not probe for such an agent. `han host` probes at its own start, before it starts the agent, and leaves `<state>/spawn/admission.json` (`Admitted`, or `Refused` with `SandboxUnavailable` or `LoopbackBlocked` and the reason). `han agent start` waits a moment (5 s at most) for it: a refusal is the command's error, and it stays in `han node status` as `host-<agent>#<issue> refused to start (...)`, and so in `han doctor`. Nothing falls back to advisory on either path: advisory needs `--allow-advisory`. `HAN_BWRAP_FORCE_UNAVAILABLE=1` makes a process report bwrap as unusable, as the node's unit does; it can only refuse (the e2e tests use it).

## 8. Check it

### 8.1 `han doctor`

```sh
sudo -u han han doctor --data /var/lib/han/node --repo /srv/repos/han
```

`han doctor` reads, and changes nothing. For every problem it prints one line of cause and the exact command that fixes it, and it exits 0 when all is well and 1 when it found any. It looks at:

- the node: whether it answers, the links of its spaces (a link whose setup does not fit its home, `Misconfigured`, with the reason), the outbox entries that hold a space back (R-07), and what else the node lists as stuck (a profile that has not arrived);
- `agents.json` and the harness records: an unreadable file, an OS user name or sandbox or toolchain no launch could use, an OS user that does not exist on this machine, a harness that is not set up or whose files are gone;
- logins: whether an agent that uses Claude Code has one (as `han agent login --check` does, without reading it);
- the device's presence setting: approvals that need a person but a backend with no check (or `pkcheck` missing);
- Windows: whether MXC can give a container a working loopback port (`sandbox.mxc.loopback`, see 7.3);
- sandboxes: the decision `han agent start` makes, per agent (`sandbox.unavailable`), made where the host runs: an OS-user agent is left to its own host, whose refusal shows as a node problem (7.3);
- the Rust toolchain the sandboxes get: enabled but not installed, or a `--repo` whose `rust-toolchain.toml` pins a toolchain that is not installed;
- `han-github` (`/var/lib/han/github`, or `--github-dir`; checked when the directory exists): prints whether it is configured and running. A config without an `[app] private_key`, a missing key file, a key readable by group or others, or a configured service that is not running is a finding (`github.config-invalid`, `github.key-missing`, `github.key-mode`, `github.not-running`; `github.config-missing` when the service runs without a config). Unconfigured and disabled, as `han runner setup` leaves it, is not a problem;
- the executor (`/var/lib/han-exec`, or `--exec-dir`): `exec.json` names repositories but has no `github` block, or the write App's key file is missing or not mode 600 (the fix is the `han executor add … --github-app … --github-installation … --github-key …` of 9);
- the `land` finish flow, when `agents.json` turns it on (9.1): the executor has no such repository or does not list the target ref in `landing_refs`, has no required checks, or runs a full stage the flow says it does not have; the executor's lane session cannot be watched (the space's `sessions.watch`); with `--check-github-app`, the write App lacks Contents write, Checks read or Commit statuses read;
- the machine reviewer (9.2; checked when a `review.json` is under `/var/lib/han-agents/*/review` or `--review-dir`): a `review.json` that cannot be used (`review.config.invalid`, `review.config.empty`), an enabled reviewer whose model key is not set in `review.env` (`review.key.missing`) or whose `review.env` is readable by others (`review.env.mode`), and, only with `--check-review-route`, a model route that does not answer (`review.route.unreachable`). The note line names whether it approves and the independence mode;
- Linux: `han seq`, `han node` or `han exec` processes whose binary was replaced on disk (`/proc/<pid>/exe` reads `(deleted)`).

**Who runs it.** On a runner two users each see half of the machine: `han` opens the node's socket and reads its data, `han-exec` reads the executor's key. No single user gets every check, so `han doctor` says so instead of failing: its first line is `checked as <user>`, then every check prints `ok <check>` when it passed or `skipped <check>: <why>` when this user could not look, with the command that covers it. A skipped check is not a finding (nothing is known to be wrong), and `healthy` says how many were skipped. The checks are `node`, `registry`, `sandbox`, `toolchain`, `presence`, `process.stale-binary`, `github.service`, `review`, `review.route`, `land`, `land.watch`, `executor.github` and `land.github.permissions`; one that does not apply on the machine (no executor, `land` off) is not listed.

```sh
sudo -u han      han doctor --data /var/lib/han/node --repo /srv/repos/han
sudo -u han-exec han doctor --exec-only --check-github-app
```

`--exec-only` runs only `executor.github` and `land.github.permissions` (the App check, with `--check-github-app`) and skips the rest with `not part of --exec-only; run as han, or sudo -u han han doctor`. The plain run, as `han`, skips those two with `needs the executor key: ... run as han-exec, or sudo -u han-exec han doctor --exec-only`, and as `han-exec` skips `node` (the socket refuses it) and the checks on `agents.json`. Together the two runs cover everything. A passing App check prints `ok land.github.permissions`.

`--json` prints the report for scripts. The schema is `han-doctor/1`, and fields are only added, never renamed or removed, within it:

```json
{
  "schema": "han-doctor/1",
  "healthy": false,
  "data": "/var/lib/han/node",
  "user": "han",
  "mode": "full",
  "checks": [
    { "id": "node", "status": "ok" },
    { "id": "land.github.permissions", "status": "skipped", "reason": "needs the executor key: ..." }
  ],
  "findings": [
    {
      "id": "agent.login.missing",
      "subject": "@coder:han.example",
      "cause": "@coder:han.example has no Claude Code login in /var/lib/han-agents/han-agent1/claude-config",
      "fix": "han agent login @coder:han.example --data /var/lib/han/node"
    }
  ]
}
```

`id` is a stable name of the kind of problem (`node.down`, `link.misconfigured`, `outbox.blocking`, `profile.missing`, `node.problem`, `registry.agents-json`, `registry.os-user`, `registry.os-user-missing`, `registry.sandbox`, `registry.toolchain`, `harness.missing`, `harness.broken`, `harness.files-missing`, `agent.login.missing`, `toolchain.rust.location`, `toolchain.rust.missing`, `toolchain.rust.pin-missing`, `presence.settings`, `presence.backend`, `presence.polkit`, `process.stale-binary`, `github.config-invalid`, `github.config-missing`, `github.key-missing`, `github.key-mode`, `github.not-running`, `land.executor.repo-missing`, `land.executor.landing-ref`, `land.checks.none`, `land.checks.full-stage`, `land.watch.denied`, `land.github.permissions`, `land.github.unreadable`); `subject` is what it is about (an agent, a space, a path, a process); `cause` is one line; `fix` is the command to run (a part in `<angle brackets>` is for you to fill in). `healthy` is `findings` being empty, and the exit code follows it. `user` is who ran the checks, `mode` is `full` or `exec-only`, and `checks` lists every check with its `status` (`ok`, `problem` or `skipped`) and, for a skipped one, the `reason`.

### 8.2 By hand

On the runner, the well-known document must list the home's node id:

```sh
curl -s http://127.0.0.1:8787/.well-known/han
```

From your machine, dial the home by node id over QUIC (through the relay when there is no direct path), as the
actor you registered, with the device key whose public key is `<pubkey>`:

```sh
han dial --key-file <your.key> --actor <you>:<server> --server <server> --node <home-node-id> hello
han dial --key-file <your.key> --actor <you>:<server> --server <server> --node <home-node-id> \
  read 'profile:<you>:<server>'
```

`hello ok` and a JSON line holding your profile record mean the home is up, the peer filter admits your device,
and QUIC works end to end. Without `--addr` or `--relay-url`, the dial finds the node through n0's discovery.
With `RUST_LOG=iroh=debug`, the `path::selected` events show whether the connection stayed on the relay or
moved to a direct path; on the first boat.dev runner it opened on the relay and moved to a direct path within a
second.

Also check, on the runner:

- `systemctl is-active han-seq han-node` reports `active` twice, and `ps -o user= -C han` shows only `han`.
- `sudo -l -U han` says `han` may not run sudo.
- `systemd-analyze security han-seq` reports a low exposure level.
- `sudo ufw status` lists only the rules from §4.
- On boat.dev, `GET /sandboxes/<sandbox>` reports `"health": "ok"` and `"archiveAfter": null`.


## 9. The executor

Agents never hold credentials. An agent proposes an effect (`git_push`, `open_pr`, `http_request`), an owner approves it, and the **executor** runs it. The executor is its own process and actor, `@exec:<server>`, a `service` actor with an owner as operator. It runs on the runner as its own OS user, `han-exec`, because it is the one process that holds the **write** GitHub App's private key. `han-github` uses a different, read-only App with its own key (section 10). Register it **before** agents propose: a proposal that names an executor with no profile cannot be delivered.

`sudo han runner setup` (§7.1) made the user and `/var/lib/han-exec` and installed the unit, disabled. The rest needs no root:

```sh
sudo -u han-exec han executor add exec --operator <you> --os-user han-exec --space <space> \
    --home <ticket> --home-data /var/lib/han/seq \
    --repo github:<owner>/<repo>=https://github.com/<owner>/<repo>.git
```

With `--os-user` the directory is `/var/lib/han-exec`, where the unit reads everything: `keys/` (the executor's key, at `keys/agent-exec-<server>.key`: the one place `han exec --keys /var/lib/han-exec` looks), `state/`, `exec.json` and `exec.env`. `han executor add` then registers the profile (here, if `--home-data` names this machine's home; otherwise it prints `han home register …` for the home's admin), has the operator confirm it, and makes it a member of the space. Where the operator is somebody else it prints the exact `han actor confirm @exec:<server> --operator <you> --home <ticket>` to run on their device. Run it again afterwards: it resumes and ends with `ready:`.

`exec.json` (it names repositories and the App's key path, never a key) is what `--repo <name>=<remote>` edits. A `--repo` named `github:<owner>/<repo>` also gets `"github": "<owner>/<repo>"`. The write GitHub App (§10) is set with three more flags of the same command, which write the `github` block (`app_id`, `installation_id`, `private_key`) and install the key:

```sh
sudo -u han-exec han executor add exec --operator <you> --os-user han-exec \
    --github-app <write-app-id> --github-installation <installation-id> --github-key <path-to-pem> \
    --repo github:<owner>/<repo>=https://github.com/<owner>/<repo>.git
```

The flags go together. The command copies the PEM to `/var/lib/han-exec/github-app.pem` with mode 600, owned by the executor's OS user, and never prints it. It refuses a source key that is readable by group or others (`chmod 600` it first), a file whose first line is not a PEM private key header, and, when `--read-app <id>` (or `HAN_GITHUB_READ_APP`) names the read App `han-github` uses, a `--github-app` equal to it. A second run with the same inputs prints `unchanged:`. Delete the source key once it is installed. `han doctor` reports a missing `github` block, a missing key file or a key with the wrong mode.

`exec.env` holds `HAN_SERVER`, `HAN_SPACE`, `HAN_RELAY` and `HAN_HOME` (a ticket holds public keys and addresses, no secret). The unit is `integrations/runner/han-exec.service`:

```ini
[Unit]
Description=HAN executor (han exec)
After=network-online.target han-seq.service
Wants=network-online.target

[Service]
User=han-exec
Group=han-exec
EnvironmentFile=/var/lib/han-exec/exec.env
ExecStart=/usr/local/bin/han exec --actor @exec:${HAN_SERVER} --space ${HAN_SPACE} --home ${HAN_HOME} --keys /var/lib/han-exec --data /var/lib/han-exec/state --config /var/lib/han-exec/exec.json --relay ${HAN_RELAY}
KillSignal=SIGINT
Restart=always
RestartSec=5
UMask=0077
NoNewPrivileges=yes
CapabilityBoundingSet=
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/han-exec
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
RestrictNamespaces=yes
LockPersonality=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources

[Install]
WantedBy=multi-user.target
```

`AF_NETLINK` is what the QUIC endpoint needs to watch the network, the same as `han-seq` and `han-node`. When the key, profile, config and env are ready, an admin enables it: `sudo systemctl enable --now han-exec`. On a device with a desktop the same executor runs by hand: `han exec --actor @exec:<server> --space <space> --home <ticket> --keys <dir> --data <dir>/state --config <dir>/exec.json`.

### 9.1 The landing lane (`land`), end to end

Landing has three settings, each set by a command, none by hand. The default is all off: an agent proposes `git_push` and `open_pr` as before.

1. **The finish flow** (the node, `agents.json`, read at every session start; no restart). It is per space, with an optional override per repository, and the repository's layer sits over the space's:

   ```sh
   han agent finish <space> --repo github:<owner>/<repo> land --target-ref refs/heads/main        --tier-rule 'docs/**=0' --tier-rule 'crates/core/**=3' [--no-full-stage] [--settle-secs 600] --data <node data>
   han agent finish <space> [--repo <repo>] off | reset | show
   ```

   It is the `work.finish.land` of the host spec: `finish.<space>.land` for the space, `finish.<space>.repos.<repo>` for a repository (`enabled`, `targetRef`, `tierRules`, `noFullStage`, `settleSecs`). A repository's tier rules come after the space's; every other field is the repository's if it sets one, else the space's. Only sessions started afterwards use it. `--tier-rule` is the informative tier only: Policy derives the real one.
2. **The executor** (`exec.json`): the refs the lane may move, the checks it gates on and its deadlines, per repository:

   ```sh
   han executor set github:<owner>/<repo> --dir /var/lib/han-exec --landing-refs refs/heads/main        --required quick --full full [--rerun 1] [--landed-secs 7200] [--remote <url> adds the repository]
   ```

   `--landing-refs`, `--required` and `--full` take comma separated lists (`""` empties one). With no `landing_refs` nothing lands and no lane runs. The target ref of step 1 must be listed here.
3. **The watch right.** The agent's host follows the lane through the `han.land lane` session that the executor opens in the space (one per space, opened when the lane first has something to say). There is no per-session grant in Policy: watching a session is the space's `sessions.watch` level (default `member`), judged against the watcher's role, and the agents `han agent add --space` makes are members. So nothing needs setting up unless an owner raised the level to `owner` or `nobody`; then an owner replaces the policy with `sessions.watch` at `member` (`han space policy <space> <file>`).

`han doctor [--check-github-app]` checks the lot (`land.*` in 8.1) and prints the fix for each. The GitHub App check asks GitHub, with the App's JWT, what the installation may do (Contents write, Checks read, Commit statuses read; Checks write when a repository sets `rerun` above 0), so it is only done when asked.

### 9.2 The machine reviewer (tier 1)

A landing of tier 0 or 1 (P-17) may be approved by a **machine reviewer** instead of a person: an independent model reads the change and returns a typed verdict, recorded as a `han.review/1` record over the landing's delta digest. It is off unless configured, it never approves tier 2 or 3, and Policy decides whether its approval counts. `han review run` is the service (crate `han-review`; the pipeline and the record are described there).

**What it does**, for each open `vcs.land` of a configured space whose derived tier (the `gateTier` of the node's `Effects` row) is 0 or 1:

1. refuses, before any model call, a proposal whose body does not match its plan, an author it knows nothing about, a reviewer that is not independent of the author (`strict`: a different vendor and no shared operator; `dogfood_relaxed`: a different actor and vendor only, and the record says so), and a bundle whose delta is not the plan's `deltaDigest`;
2. fetches the bundle, recomputes the delta, asks the model once for a typed answer (an answer that does not parse, or is cut off at `max_tokens`, is `needs_human`), and uploads the record as a blob;
3. on `approve` (with `approve` on, a confidence at or above `min_confidence`, and the model named in the record): sends `ReviewApprove`, which the node turns into `effect.approve` with `evidence {kind: "han.review/1", over: <deltaDigest>}` and the record as the content `evidence`. On `request_changes` or `needs_human`: the record is uploaded and the proposer gets a task whose text says it is review data, not an instruction.

One review is made per proposal and reviewer version (model, prompt, schema and independence mode); each step is written to `state/ledger.json` first, and the record is kept in `state/records/`, so a restart neither asks the model again nor loses its place.

**Which connection approves.** The service runs its own node as the reviewer's actor and works through it held to the **executor** class, like `han exec`: `EffectApprove` stays refused to it (SR-06, SR-106; a headless node never approves for a person). `ReviewApprove` is a second executor-only command next to the lane's `RevertApprove`: no person is asked, so the node checks everything itself. The proposal must be open, a `vcs.land`, proposed by another actor, of derived tier at most 1; the evidence must be `han.review/1` over the gate's `deltaDigest`. Then Policy decides, as for any approval: the reviewer must be listed in an approval rule with `evidence: "han.review/1"`, a non-human approval never counts at tier 2 or more, and `separateApprover` and guards still apply. A face or an agent's host cannot send it.

The space's policy needs the rule (the dogfood space `han-dev` has it), and the reviewer must be a member:

```json
{"action": "vcs.land", "maxTier": 1, "approvers": ["@reviewer:<server>", "owner"], "evidence": "han.review/1"}
```

**Setup**, in order. The reviewer is an agent actor with the operator as its operator, running as its own OS user (here `han-agent2`); it starts no sessions.

```sh
# 1. root, once: the OS users and the units (installs han-review@.service, not enabled)
sudo han runner setup --agents 2

# 2. the actor: as the node's user (key in /var/lib/han-agents/han-agent2/keys, profile, operator confirmation, membership)
sudo -u han han agent add reviewer --operator @lars:runner.han.home.arpa --server runner.han.home.arpa \
    --os-user han-agent2 --space iwufdph6fg:runner.han.home.arpa \
    --home <ticket> --home-data /var/lib/han/seq --data /var/lib/han/node

# 3. the configuration and the model key: as the reviewer's user. --key-file is a file holding the key (mode 600);
#    it is copied to review.env (mode 600), never into review.json and never printed. Delete the file afterwards.
sudo -u han-agent2 han review set --dir /var/lib/han-agents/han-agent2/review \
    --actor @reviewer:runner.han.home.arpa --operators @lars:runner.han.home.arpa \
    --model agy/gemini-flash --max-tokens 16384 \
    --base-url http://127.0.0.1:20128 --route-name omniroute --key-env HAN_REVIEW_KEY \
    --spaces iwufdph6fg:runner.han.home.arpa \
    --repo github:lmvdz/han=https://github.com/lmvdz/han.git \
    --independence dogfood_relaxed \
    --default-author claude-code:claude-opus-4-1:@lars:runner.han.home.arpa \
    --home <ticket> --relay n0 --key-file <path-to-key-file> --enabled

# 4. check it: the model route is asked only with --check-review-route (one request, no key sent)
sudo -u han-agent2 han doctor --review-dir /var/lib/han-agents/han-agent2/review --check-review-route

# 5. start it (an admin)
sudo systemctl enable --now han-review@han-agent2
journalctl -u han-review@han-agent2 -f
```

Run first without `--approve`: the service reviews, uploads the record and messages the proposer, and approves nothing (a shadow run, `recorded: the service is not set to approve` in the ledger). When the records look right, `sudo -u han-agent2 han review set --dir … --approve` and `sudo systemctl restart han-review@han-agent2`. Every `han review set` prints `changed:` or `unchanged:` and only what is given changes; `--enabled=false` turns the service off (`han review run` then exits 0 and does nothing).

The settings:

| Option | Meaning |
|---|---|
| `--model`, `--max-tokens` | The model and its output ceiling, always sent explicitly. For the agy Gemini Flash route use about 16384: a larger or smaller value gives truncation (`needs_human`) or spurious compaction. The model must be of a different vendor from the author's (the vendor table refuses a name it does not know). |
| `--base-url`, `--path`, `--key-env`, `--key-header` | The OpenAI-compatible route (OmniRoute). TLS only, except to a loopback host. `--key-env` names the environment variable that holds the key; the unit's `review.env` sets it. |
| `--spaces`, `--repo name=remote` | What is watched, and where a bundle's base is fetched from (a remote readable without credentials, for example the public HTTPS URL; the bundle itself comes from the space). A proposal for another repository is refused. |
| `--independence` | `strict` (default) or `dogfood_relaxed`. |
| `--default-author`, `--author actor=harness:model[:operators]` | What the reviewer knows of the author: the node does not say which harness and model ran an actor, so this does. Without an entry the review is refused (independence can't be shown). |
| `--approve`, `--min-confidence`, `--max-diff-bytes`, `--poll-secs` | Approval on or off (default off), the confidence an `approve` needs (default 80), the largest diff sent to the model (default 200000 bytes; over it the record is `needs_human`) and the pause between reads (default 15 s). |

The files, all under `--dir`: `review.json`, `review.env` (`HAN_HOME`, `HAN_RELAY`, the key), `state/` (ledger and kept records), `node/` and `objects/`. If the proposal's author is guarded (`sessions.forceGuarded`, or the agent chose it), the guard must approve as well: the reviewer satisfies the rule, not the guard. A reviewer that is itself guarded (it chose guarding, or the space has `sessions.forceGuarded`) cannot approve (P-08, P-15): leave `forceGuarded` off in the space the reviewer works in, or tier 1 needs an owner.

## 10. GitHub App permissions for writes

There are two Apps, and the key of each is held by one process only (PLAN 6.7, SR-49).

* **The read App** is what `han-github` uses, configured as its own `[app]` in `han-github.toml`, with
  **Issues: read** and **Metadata: read** only. Its id is what `han executor add --read-app <id>`
  refuses as the executor's `--github-app`. `han-github` has no `han-exec` code and cannot mint a write token.
* **The write App** is a *second* App, to be created later, whose key only `han-exec` holds (below). Do not
  point `han-github` at it, and do not give the executor the read App.

The executor mints a token per action with exactly the permission that action needs (SR-67), so writes need these **repository
permissions** on the write App, for the repositories it is installed on:

| Action | Permission | Level |
|---|---|---|
| `git_push` | Contents | Read and write |
| `open_pr` | Pull requests | Read and write |
| `vcs.land` | Contents | Read and write |
| landing lane (reads the checks of the commit it tests) | Checks, Commit statuses | Read-only (Checks: read and write when `rerun` is above 0) |
| (both, automatic) | Metadata | Read-only |

Nothing else: no `workflows`, no `administration`, no `issues` write. Changing App permissions makes GitHub ask
the installation's owner to accept them, and the executor does nothing until they do. This repository never
changes the App; that is the owner's decision.

### 10.1 Running `han-github` as a service

`sudo han runner setup` (§7.1) makes `/var/lib/han/github` (mode 700, owned by `han`) and installs `han-github.service` (`integrations/runner/han-github.service`), **disabled**. The unit has `ConditionPathExists=/var/lib/han/github/han-github.toml`, so it does not start until it is configured.

It runs as `han`, not as a user of its own: it reaches the node through the node's owner-only socket (`/var/lib/han/node/node.sock`), which only `han` can open. Its sandbox is the same family as `han-seq`: no capabilities, `ProtectSystem=strict` (it writes nothing), no namespaces, `AF_INET`/`AF_INET6`/`AF_UNIX` only, a system call filter. The runner's node is headless (§7), which does not matter to it: `han-github` only reads the space's tasks, makes tasks and publishes `github.issue/1` items.

The key is the **read** App's (5309257, Issues and Metadata read-only), never the executor's write App. It sits in the same directory, which no agent user can enter:

```sh
sudo -u han install -m 600 <read-app.pem> /var/lib/han/github/app.pem      # then delete the source
sudo -u han tee /var/lib/han/github/han-github.toml >/dev/null <<'TOML'
space = "!abc:home.example"
data_dir = "/var/lib/han/node"
repos = ["lmvdz/han"]
[app]
app_id = 5309257
installation_id = <installation-id>
private_key = "/var/lib/han/github/app.pem"
[tasks]
pool = ["@runner-agent1:home.example"]
TOML
sudo install -m 755 <path>/han-github /usr/local/bin/han-github
sudo systemctl enable --now han-github
```

`han doctor` (§8.1) says whether it is configured and running. The service restarts itself: `han-github` has no reconnect, so after three polls in a row that fail at the node (a restarted node) it exits and systemd starts it again.

Which tasks it makes: an open issue with the trigger label and **no open task** gets one, also when the issue has not changed since (every poll looks at every known agent-ready issue, not only the changed ones). If the issue's earlier tasks all closed without landing (`failed`, `declined`, `cancelled`, or an owner's force-close of a dead session), it gets a new one, at most three such tasks in all (`MAX_ATTEMPTS`). A task that closed as `done` while the issue is still open waits for a person: they close the issue, or ask for a new task with `han-github request`. See `integrations/han-github/README.md`.

## 11. A person's device

On a Windows or Linux device the node runs as a per-user background service, no admin rights:

```sh
han node install-service --actor <you>:<server> --data <dir> --file-keys   # scheduled task at logon / systemd user unit
han node status --data <dir>
han node uninstall-service
```

`install-service` takes the options of `han node` and starts it now (`--no-start` to wait for the next logon,
`--dry-run` to print what it would install). On a headless Linux box, an admin runs `loginctl enable-linger
$USER` once so the user unit runs without a login.
