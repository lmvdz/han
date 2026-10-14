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

It makes the OS users (`han`, `han-exec`, `han-agent1` … `han-agent<N>`, each with a locked password), the agents directory `/var/lib/han-agents` (mode 755) with one state directory per agent (`2770`, setgid, in the agent's own group; the node's user joins that group and no other agent's), and installs `han-agent@.service`, `han-exec.service` (installed, **left disabled**), `/etc/polkit-1/rules.d/50-han-agent.rules` (the node's user may start and stop `han-agent@han-agent<N>` and nothing else) and the node's drop-in `/etc/systemd/system/han-node.service.d/10-agents.conf` (`UMask=0007`, `ReadWritePaths=/var/lib/han/node /var/lib/han-agents`), then reloads systemd. Each step says `changed:` or `unchanged:`, so a second run reports no change. The unit files are in `integrations/runner/`; `install-agent-user.sh` and `install-executor.sh` there are thin wrappers around the command.

It also looks at the running services. If `han-seq` or `han-node` runs a binary that has since been replaced on disk (after you installed a new `han`), or the node has not yet picked up its new group, it says which and how to restart it, or restarts it with `--restart-stale`. A home that kept running the old binary has no admin socket, and everything that registers profiles fails against it; this is how you find out before that.

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
- the Rust toolchain the sandboxes get: enabled but not installed, or a `--repo` whose `rust-toolchain.toml` pins a toolchain that is not installed;
- Linux: `han seq`, `han node` or `han exec` processes whose binary was replaced on disk (`/proc/<pid>/exe` reads `(deleted)`).

`--json` prints the report for scripts. The schema is `han-doctor/1`, and fields are only added, never renamed or removed, within it:

```json
{
  "schema": "han-doctor/1",
  "healthy": false,
  "data": "/var/lib/han/node",
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

`id` is a stable name of the kind of problem (`node.down`, `link.misconfigured`, `outbox.blocking`, `profile.missing`, `node.problem`, `registry.agents-json`, `registry.os-user`, `registry.os-user-missing`, `registry.sandbox`, `registry.toolchain`, `harness.missing`, `harness.broken`, `harness.files-missing`, `agent.login.missing`, `toolchain.rust.location`, `toolchain.rust.missing`, `toolchain.rust.pin-missing`, `presence.settings`, `presence.backend`, `presence.polkit`, `process.stale-binary`); `subject` is what it is about (an agent, a space, a path, a process); `cause` is one line; `fix` is the command to run (a part in `<angle brackets>` is for you to fill in). `healthy` is `findings` being empty, and the exit code follows it.

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

`exec.json` (it names repositories and the App's key path, never a key) is what `--repo <name>=<remote>` edits. For a GitHub App add the `github` block by hand (§10):

```json
{
  "repos": [
    {"name": "github:<owner>/<repo>", "remote": "https://github.com/<owner>/<repo>.git", "github": "<owner>/<repo>"}
  ],
  "branch_prefixes": ["han/"],
  "github": {"app_id": 5309257, "installation_id": <installation>, "private_key": "/var/lib/han-exec/github-app.pem"}
}
```

Put the App's PEM there as `han-exec` with mode 600 (`sudo -u han-exec install -m 600 /dev/stdin /var/lib/han-exec/github-app.pem < key.pem`); nothing else on the runner can read it.

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

## 10. GitHub App permissions for writes

There are two Apps, and the key of each is held by one process only (PLAN 6.7, SR-49).

* **The read App** is what `han-github` uses, configured as its own `[app]` in `han-github.toml`, with
  **Issues: read** and **Metadata: read** only. The user's current App (id 5309257) is exactly this and is already
  used by `han-github`. `han-github` has no `han-exec` code and cannot mint a write token.
* **The write App** is a *second* App, to be created later, whose key only `han-exec` holds (below). Do not
  point `han-github` at it, and do not give the executor the read App.

The executor mints a token per action with exactly the permission that action needs (SR-67), so writes need these **repository
permissions** on the write App, for the repositories it is installed on:

| Action | Permission | Level |
|---|---|---|
| `git_push` | Contents | Read and write |
| `open_pr` | Pull requests | Read and write |
| (both, automatic) | Metadata | Read-only |

Nothing else: no `workflows`, no `administration`, no `issues` write. Changing App permissions makes GitHub ask
the installation's owner to accept them, and the executor does nothing until they do. This repository never
changes the App; that is the owner's decision.

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
