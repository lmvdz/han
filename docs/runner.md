# Shared runner setup

This guide sets up a HAN shared runner on a Linux VM: a machine the team owns that hosts a space home and,
from M4 on, runs agent sessions for members (PLAN §3, §6.4 Shared runners, §6.8; security requirements §10,
SR-83 to SR-88). It was first done on a boat.dev sandbox, so the boat.dev notes come first; every other step
works on any Ubuntu 24.04 VM where you have root.

Nothing in this guide is specific to one machine. Replace the placeholders:

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

**Keys.** Create the runner's keys as `han`, and print the node ids:

```sh
sudo -u han install -d -m 700 /var/lib/han/seq /var/lib/han/node /var/lib/han/node/keys
sudo -u han han key /var/lib/han/seq/node.key          # the home's node id
sudo -u han han key /var/lib/han/node/keys/device.key  # the runner actor's device key
```

**Configuration.** `/etc/han/seq.env` (root-owned, mode 644; it holds names and public keys, no secrets):

```sh
HAN_SERVER=<server>
HAN_UDP_PORT=7841
HAN_RELAY=n0
# profiles the home serves: actor:kind:keyId:publicKey[:operator], one --register each.
HAN_REGISTER=--register <you>:<server>:human:d1:<pubkey> --register @runner:<server>:service:d1:<runner-pubkey>:<you>:<server>
```

Registering is idempotent: an existing profile is left as it is. `HAN_REGISTER` is for the first profiles; after that, a new profile (an agent, the executor) is registered on the running home with `sudo -u han han seq register --data /var/lib/han/seq --actor <actor> --kind agent --public-key <key> --operator <operator>` (an admin socket in the data directory, reachable by user `han` only; no restart). `/etc/han/node.env`:

```sh
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
ExecStart=/usr/local/bin/han seq --server ${HAN_SERVER} --data /var/lib/han/seq --bind 0.0.0.0:${HAN_UDP_PORT} --http 127.0.0.1:8787 --relay ${HAN_RELAY} $HAN_REGISTER
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

### 7.1 Running a host as another OS user (admin, once)

The node never needs root at run time. It starts a host as another user through a **systemd template unit**
that you install once, and a polkit rule that lets the node's user start and stop exactly those units. For
each agent the node writes the host's arguments to `<state>/spawn/host.json` and starts
`han-agent@<user>.service`; the unit runs `han host --args-file` as `<user>`.

```sh
# the agent's user, its own group, its state directory (setgid: the node's user shares only this group)
sudo useradd --system --create-home --home-dir /var/lib/han-agents/han-agent1 --shell /usr/sbin/nologin han-agent1
sudo passwd -l han-agent1
sudo usermod -aG han-agent1 han          # the node (user han) shares the agent's group, nobody else's
sudo chmod 2770 /var/lib/han-agents/han-agent1
sudo chmod 755 /var/lib/han-agents       # so units can reach their own subdirectory, no listing needed
```

`/etc/systemd/system/han-agent@.service`:

```ini
[Unit]
Description=HAN agent host as %i (han host)
After=network-online.target

[Service]
User=%i
Group=%i
Environment=HOME=/var/lib/han-agents/%i
WorkingDirectory=/var/lib/han-agents/%i
ExecStart=/usr/local/bin/han host --args-file /var/lib/han-agents/%i/spawn/host.json
KillSignal=SIGINT
Restart=no
UMask=0077
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/han-agents/%i
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
# no RestrictNamespaces: bubblewrap inside needs user namespaces
```

`/etc/polkit-1/rules.d/50-han-agent.rules` (the node's user may start and stop `han-agent@han-agent<N>` and
nothing else):

```js
polkit.addRule(function(action, subject) {
  if (action.id == "org.freedesktop.systemd1.manage-units" && subject.user == "han" &&
      /^han-agent@han-agent[0-9]+\.service$/.test(action.lookup("unit")) &&
      (action.lookup("verb") == "start" || action.lookup("verb") == "stop")) {
    return polkit.Result.YES;
  }
});
```

The node unit changes: `UMask=0007` (so the agent's key, which the node creates in the agent's state
directory, is shared with the agent's group), `ReadWritePaths=/var/lib/han/node /var/lib/han-agents` and
`RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK` as before (`systemctl` needs the system bus
socket).

Tell the node which agent runs as whom, in `/var/lib/han/node/agents.json` (read at every start, no restart):

```json
{
  "sandbox": "bwrap",
  "agents": {
    "@coder:<server>": { "osUser": "han-agent1" }
  }
}
```

`stateDir` defaults to `/var/lib/han-agents/<osUser>`; the agent's key, sessions, taint store and login live
there. Install the harness where the agent user can read it:
`sudo -u han han agent setup-harness claude --data /var/lib/han/node --dir /opt/han/claude` (then
`chmod -R a+rX /opt/han`). One session per OS user runs at a time (the unit is the user's); a second
`han agent start` for the same user answers `conflict`.

Log the agent in once, as its own user (this runs `claude` interactively, outside any sandbox):

```sh
sudo -u han-agent1 han agent login @coder:<server> --dir /var/lib/han-agents/han-agent1/claude-config
sudo -u han-agent1 han agent login @coder:<server> --dir /var/lib/han-agents/han-agent1/claude-config --check
```

`--check` prints only whether a login exists, never its contents.

## 8. Check it

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

Agents never hold credentials. An agent proposes an effect (`git_push`, `open_pr`, `http_request`), an owner
approves it, and the **executor** runs it. The executor is its own process and actor, `@exec:<server>`, a
`service` actor with an owner as operator. It runs on the runner as its own OS user, `han-exec`, because it
is the one process that holds the GitHub App's private key.

**Accounts, keys, profile.**

```sh
sudo useradd --system --create-home --home-dir /var/lib/han-exec --shell /usr/sbin/nologin han-exec
sudo install -d -o han-exec -g han-exec -m 700 /var/lib/han-exec/keys /var/lib/han-exec/state
sudo -u han-exec han key /var/lib/han-exec/keys/keys/agent-exec-<server>.key   # prints its public key
```

`han exec` finds its key as `<keys>/keys/agent-<name>-<server>.key` (the same naming as an agent's). Register
the profile on the running home (admin, no restart; the operator confirms it like an agent's):

```sh
sudo -u han han seq register --data /var/lib/han/seq --actor @exec:<server> --kind service \
  --key-id k1 --public-key <exec-public-key> --operator <you>:<server>
han agent confirm @exec:<server> --operator <you>:<server> --home <ticket>   # on your device
```

Add `@exec:<server>` to the space as `member` (`han space member <space> @exec:<server> member`), so the
effects it is the executor of reach it.

**Configuration**, `/etc/han/exec.json` (root-owned, mode 644: it names repositories and the App's key path,
never a key):

```json
{
  "repos": [
    {"name": "github:<owner>/<repo>", "remote": "https://github.com/<owner>/<repo>.git", "github": "<owner>/<repo>"}
  ],
  "branch_prefixes": ["han/"],
  "github": {"app_id": 5309257, "installation_id": <installation>, "private_key": "/var/lib/han-exec/github-app.pem"}
}
```

Put the App's PEM there as `han-exec` with mode 600 (`sudo -u han-exec install -m 600 /dev/stdin
/var/lib/han-exec/github-app.pem < key.pem`); nothing else on the runner can read it.

`/etc/systemd/system/han-exec.service`:

```ini
[Unit]
Description=HAN executor (han exec) as @exec:<server>
After=network-online.target han-seq.service
Wants=network-online.target

[Service]
User=han-exec
Group=han-exec
EnvironmentFile=/etc/han/exec.env
ExecStart=/usr/local/bin/han exec --actor @exec:${HAN_SERVER} --space ${HAN_SPACE} --home-ticket ${HAN_HOME_TICKET} --keys /var/lib/han-exec --data /var/lib/han-exec/state --config /etc/han/exec.json --relay ${HAN_RELAY}
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
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=yes
LockPersonality=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources

[Install]
WantedBy=multi-user.target
```

`/etc/han/exec.env` holds `HAN_SERVER`, `HAN_SPACE`, `HAN_RELAY=n0` and `HAN_HOME_TICKET=<ticket>` (a ticket
holds public keys and addresses, no secret). `integrations/runner/install-executor.sh` does the account,
directories and unit in one go; run it as root, then fill in the env file and the PEM. On a device with a
desktop the same command runs by hand: `han exec --actor @exec:<server> --space <space> --home <ticket> --keys
<dir> --data <dir> --config exec.json`.

## 10. GitHub App permissions for writes

The user's App (id 5309257) has **issues: read** today, which is all `han-github` needs. The executor mints a
token per action with exactly the permission that action needs (SR-67), so writes need these **repository
permissions** on the App, for the repositories it is installed on:

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
