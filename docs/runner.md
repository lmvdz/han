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

Registering is idempotent: an existing profile is left as it is. `/etc/han/node.env`:

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

When agent sessions arrive (M4), `han-node` supervises `han host` children that sandbox agents with
bubblewrap. bubblewrap needs user namespaces, which `RestrictNamespaces=yes` forbids, so that unit's sandbox is
relaxed then, and only for `han-node`; `han-seq` keeps it.

## 7. Agent sessions (plan for M4)

SR §10 asks for isolation between sessions, and between sessions and the runner's own processes:

- **One OS user per agent actor** (SR-83): `han-agent-<n>` accounts, created on demand by a small root helper
  that `han-node` calls with a signed, Policy-allowed task (SR-86), with no login and no sudo. Sessions of the
  same agent actor share that user, but each still gets its own bubblewrap sandbox, worktree, temp directory
  and proxy.
- **`han` is never an agent user** (SR-85): `/var/lib/han` is mode 700, so no session can read the home's
  database or any key.
- **Each session's proxy holds only that session's model key** (SR-84); the key never enters the sandbox
  (PLAN §6.4 Model credentials).
- **Clean-up** (SR-87): after a session ends, its worktree, temp directory, proxy and credentials are removed;
  shared package caches are mounted read-only into sandboxes.
- **Egress**: the sandbox reaches only its local proxy; with a tailnet exit node, follow the Tailscale recipe
  in PLAN §6.4 and SR-88 (tag `tag:han-runner`, only `autogroup:internet` through the exit node, no route to
  members' devices).

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
