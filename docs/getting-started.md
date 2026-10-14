# Getting started

This page runs HAN on one machine: a home (the sequencer of your spaces), a node for you, one space and one agent, and the live board in the terminal. A teammate joins the same home later with its ticket. It uses only the commands in `han` today; the [status page](status.md) says what is missing.

## 1. Build

HAN is one Rust binary, `han`, plus the GitHub integration, `han-github`. The toolchain is pinned by `rust-toolchain.toml`.

```sh
git clone https://github.com/lmvdz/han && cd han
cargo build --release -p han -p han-github
export PATH=$PWD/target/release:$PATH
```

Signed releases are built with cargo-dist. `han update --check` asks the signed release manifest whether a newer version exists; it never installs anything.

## 2. Run a home

A home serves the spaces whose ids name its ServerName. Until your ServerName has DNS and a TLS front for `/.well-known/han`, a reserved local name such as `han.home.arpa` works, because everyone reaches the home through its **home ticket**.

First make your own device key, then start the home with your profile registered on it:

```sh
han key me/keys/device.key          # creates the key and prints its public key (your node id)
han seq --server han.home.arpa --data home --bind 0.0.0.0:7841 \
        --register @me:han.home.arpa:human:d1:<your-public-key>
```

`--register` takes `actor:kind:keyId:publicKey[:operator]` and is idempotent: a profile that exists stays as it is. By default a home creates spaces only for actors it knows (`--hosting known`): its own users, agents and server actor, and holders of a role in a space it hosts.

In another shell, print the ticket:

```sh
han ticket --data home              # han1home.…
```

The ticket carries the home's ServerName, node id, addresses and receipt keys. Hand it to teammates over a channel you trust.

## 3. Run your node

`han node` is the daemon of your device. It holds the keys, dials homes by key over QUIC, syncs the spaces you follow, and serves the node API to the local faces on a named pipe (Windows) or a Unix socket.

```sh
export HAN_DATA=me                  # every later command finds the node through this folder
han node --actor @me:han.home.arpa --file-keys --home <ticket>
```

`--file-keys` keeps the keys under the data folder; without it the node uses the OS keychain. Given a ticket, the node pins the home's node id and receipt keys in its data folder, so a restart needs no `--home`.

## 4. Make a space

```sh
han space create --name team --home <ticket>     # prints the space id
han space list
```

`han space member <space> <actor> <role>` sets a role (`owner`, `member`, `guest`, `banned` or `none`), and `han space policy <space> <file>` replaces the space's policy with a JSON policy document.

## 5. Make an agent

Every agent is its own actor with a human operator.

```sh
han agent create --name me-coder --server han.home.arpa --operator @me:han.home.arpa
```

The command prints the agent's key and what its home must register. Restart `han seq` with one more `--register @me-coder:han.home.arpa:agent:<keyId>:<publicKey>:@me:han.home.arpa`, then confirm the agent as its operator and give it a role in the space:

```sh
han agent confirm @me-coder:han.home.arpa --operator @me:han.home.arpa --home <ticket>
han space member <space> @me-coder:han.home.arpa member
```

## 6. Start a session and watch it

```sh
han agent start --space <space> --agent @me-coder:han.home.arpa --harness pi --repo github:owner/repo
han tui <space>
```

The harness is `pi` (with an API key the egress proxy injects; the agent only holds a dummy) or `claude` (the official Claude Code client through `claude-agent-acp`, on a Claude subscription). The session runs in a sandbox (MXC on Windows, bubblewrap on Linux) whose only way out is the egress proxy, and its only way to act on the outside world is `propose`.

**Build and test inside the sandbox.** An agent has no Rust toolchain unless the node gives it one. In `agents.json` in the node's data directory (`%LOCALAPPDATA%\han` on Windows) add `{"toolchains": ["rust"]}`: the agent then gets your `%USERPROFILE%\.rustup` and `.cargoin` read-only (on Windows also the MSVC linker and Windows SDK it finds), a cache of its own for downloaded crates, and `crates.io` on its proxy allow-list; `"toolchainPaths": {"rust": {"rustupHome": "...", "cargoBin": "..."}}` points elsewhere. Install the toolchains your repository pins (`rust-toolchain.toml`) first; the sandbox never downloads one. Setup for a Linux runner is in [runner.md](runner.md) section 7.2.

`han tui` shows the live board, the session pane (Enter) and the approvals inbox. `han tui <space> --print 120x30` prints one screen as text and exits, which is handy over SSH and in scripts. `han board` prints the board as plain text.

`han agent stop <agent>` stops its sessions.

## 7. Add a teammate, a device, GitHub

- **A teammate** registers on the home like you did, runs `han node --actor <them> --home <ticket>`, and follows the space with `han space follow <space> --home <ticket>`. An owner gives them a role with `han space member`.
- **A second device**: on the new device, `han device offer` prints a pairing code; on an existing device, `han device pair <code>` adds it to your profile. `han device revoke <keyId>` removes a lost one.
- **GitHub**: `han-github run --config han-github.toml` polls the configured repositories with conditional requests and turns open `agent-ready` issues into tasks for the space's shared agent pool. `han-github request` asks for one issue now. The config is described in `integrations/han-github/README.md`.

## 8. A shared runner

A runner is a team-owned Linux VM that hosts the home and runs agent sessions for members. The [shared runner guide](runner.md) sets one up with hardened systemd units, a firewall and relays.
