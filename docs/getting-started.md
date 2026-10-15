# Getting started

This page runs HAN on one machine: a home (the sequencer of your spaces), a node for you, one space, one agent and an executor, and the live board in the terminal. A teammate joins the same home later with its ticket. It uses only the commands in `han` today; the [status page](status.md) says what is missing.

Every setup command here is safe to run again: it looks at what is already there, changes only what differs, and prints what it changed (`changed:`) and what it left (`unchanged:`). Where a step belongs to somebody else (the home's admin, the agent's operator on another device) it prints `to do:` and the exact command that person runs. None of them needs root; the one root-level command is `han runner setup` on a Linux runner ([shared runner](runner.md)).

## 0. Try it first: `han dev up`

```sh
cargo build --workspace --bins        # han, han-github and the test agent han-fake-agent
target/debug/han dev up --dir han-dev --start
```

`han dev up` makes a complete HAN on loopback with the commands below, then prints how to use it: a home with a placeholder name (`dev.han.home.arpa`) and a ticket, a node for `@you`, a space with a policy that gates every push, an agent `@coder`, an executor `@exec`, a fake GitHub with one `agent-ready` issue, and a local bare git remote. The agent is `han-fake-agent`, driven over the real ACP path, the real sandbox shim and the real OS sandbox (MXC on Windows, bubblewrap on Linux); on a machine with no sandbox add `--advisory`. On a Windows build where MXC gives containers no loopback port (error 10013), `han agent start` refuses an MXC agent and says why; use WSL2+bubblewrap, a runner, or `--advisory` (see [runner.md](runner.md) 7.3). `--harness claude` uses the real Claude Code adapter instead (it needs `han agent login`; it is for people, never for CI). `--script propose` makes the fake agent propose a `git_push`. When you are done:

```sh
han dev down --dir han-dev            # stops everything and removes the folder
```

The rest of this page is what `han dev up` does, step by step.

## 1. Build

HAN is one Rust binary, `han`, plus the GitHub integration, `han-github`. The toolchain is pinned by `rust-toolchain.toml`.

```sh
git clone https://github.com/lmvdz/han && cd han
cargo build --release -p han -p han-github
export PATH=$PWD/target/release:$PATH
```

Signed releases are built with cargo-dist. `han update --check` asks the signed release manifest whether a newer version exists; it never installs anything.

## 2. Make and run a home

A home serves the spaces whose ids name its ServerName. Until your ServerName has DNS and a TLS front for `/.well-known/han`, a reserved local name such as `han.home.arpa` works, because everyone reaches the home through its **home ticket**.

```sh
han home init --server han.home.arpa --data home --bind 0.0.0.0:7841
han seq --data home                    # serves it; the settings come from `home init`
```

`han home init` makes the folder, the home's two keys and `home.json` (the server name, ports and relays that `han seq --data home` then needs no flags for), and prints the path of the ticket file and the ticket. Run it again and it says `unchanged`. `han seq` rewrites the ticket with the address it really binds; `han ticket --data home` prints it. Hand it to teammates over a channel you trust: it carries the home's ServerName, node id, addresses and receipt keys, nothing secret.

By default a home creates spaces only for actors it knows (`--hosting known` at `init`): its own users, agents and server actor, and holders of a role in a space it hosts.

## 3. Register yourself and run your node

`han node` is the daemon of your device. It holds the keys, dials homes by key over QUIC, syncs the spaces you follow, and serves the node API to the local faces on a named pipe (Windows) or a Unix socket. Your profile has to be on the home first, and a profile is registered by the home's admin, over a socket only the OS user that runs the home can reach (no restart):

```sh
han key me/keys/device.key             # creates the key, prints its public key (your node id)
han home register @me:han.home.arpa --kind human --key-id d1 --public-key=<your-public-key> --data home
export HAN_DATA=me                     # every later command finds the node through this folder
han node --actor @me:han.home.arpa --file-keys --home <ticket>
```

`han seq register` is the old name of `han home register`. Write the key as `--public-key=<key>`: a key can start with `-`. Registering twice prints `already registered` and changes nothing.

`--file-keys` keeps the keys under the data folder; without it the node uses the OS keychain. Given a ticket, the node pins the home's node id and receipt keys in its data folder, so a restart needs no `--home`.

## 4. Make a space

```sh
han space create --name team --home <ticket>     # prints the space id
han space list
```

`han space member <space> <actor> <role>` sets a role (`owner`, `member`, `guest`, `banned` or `none`; setting a role someone holds already changes nothing), and `han space policy <space> <file>` replaces the space's policy with a JSON policy document. `han dev up` uses a policy that makes `git_push`, `open_pr` and `http_request` wait for an owner's approval.

## 5. Add an agent

Every agent is its own actor with a human operator. One command does the whole job where it can:

```sh
han agent add coder --operator @me:han.home.arpa --space <space> --home <ticket> \
    --home-data home --harness claude --sandbox auto
```

It writes the agent's launch settings (`agents.json` in the node's data folder), installs the harness adapter if it is not there (`claude` or `pi`), makes the agent's key, registers its profile (here, because `--home-data` names the home that runs on this machine), has you confirm it as its operator, and makes it a member of the space. Without `--home-data` it prints, for the home's admin, `han home register @coder:… --public-key=… --operator …`; run on a device that is not the operator's, it prints `han actor confirm @coder:… --operator … --home <ticket>` for the operator. Run `han agent add` again afterwards: it resumes where it stopped and ends with `ready:`.

`han actor confirm` is what the operator runs; it works for agents and for the executor (`han agent confirm` still works). `--os-user`, `--allow-advisory` and `--sandbox` record how the agent runs; see the [shared runner guide](runner.md) for OS-user agents.

## 6. Add an executor

Agents hold no credentials. They propose an effect (`git_push`, `open_pr`, `http_request`), an owner approves it, and the **executor** runs it.

```sh
han executor add exec --operator @me:han.home.arpa --dir exec --space <space> --home <ticket> \
    --home-data home --repo <name>=<remote>
han exec --actor @exec:han.home.arpa --space <space> --home <ticket> --keys exec --data exec/state --config exec/exec.json
```

`han executor add` does for the executor what `han agent add` does for an agent: its key where `han exec` looks for it (`<dir>/keys/`), `exec.json` from `--repo` (the name is what proposals call the repository; for a local one it is its path), `exec.env`, then the profile, the confirmation and the membership. A proposal that names an executor with no profile has nowhere to go, so register the executor before agents propose.

## 7. Start a session and watch it

```sh
han agent start --space <space> --agent @me-coder:han.home.arpa --repo github:owner/repo --issue github:owner/repo#12
han tui <space>
```

`--harness` defaults to the one `han agent add` recorded; it is `pi` (with an API key the egress proxy injects; the agent only holds a dummy) or `claude` (the official Claude Code client through `claude-agent-acp`, on a Claude subscription). The session runs in a sandbox whose only way out is the egress proxy, and its only way to act on the outside world is `propose`.

**Build and test inside the sandbox.** An agent has no Rust toolchain unless the node gives it one. `han agent set @coder:<server> --toolchains rust` turns it on for that agent (the file-wide default is the `"toolchains"` list of `agents.json`, in the node's data directory, `%LOCALAPPDATA%\han` on Windows). The agent then gets your `%USERPROFILE%\.rustup` and `.cargo\bin` read-only (on Windows also the MSVC linker and Windows SDK it finds), a cache of its own for downloaded crates, and `crates.io` on its proxy allow-list; `"toolchainPaths": {"rust": {"rustupHome": "...", "cargoBin": "..."}}` points elsewhere. Install the toolchains your repository pins (`rust-toolchain.toml`) first; the sandbox never downloads one. On a Linux runner `sudo han runner setup --toolchain rust --repo <path>` does the installing and writes `toolchainPaths` ([runner.md](runner.md) section 7.2).

`han agent set` changes how an agent is launched and nothing else; never edit `agents.json` by hand. It takes `--os-user`, `--sandbox`, `--allow-advisory`, `--toolchains` and `--harness`, needs no running node, and prints `changed:` or `unchanged:`.

`han tui` shows the live board, the session pane (Enter) and the approvals inbox, where you read a proposal and approve it. `han tui <space> --print 120x30` prints one screen as text and exits, which is handy over SSH and in scripts. `han board` prints the board as plain text. `han agent stop <agent>` stops its sessions.

## 8. Add a teammate, a device, GitHub

- **A teammate** gets registered on the home like you did (`han home register`), runs `han node --actor <them> --home <ticket>`, and follows the space with `han space follow <space> --home <ticket>`. An owner gives them a role with `han space member`. If they are an agent's operator they run the `han actor confirm` line that `han agent add` printed.
- **A second device**: on the new device, `han device offer` prints a pairing code; on an existing device, `han device pair <code>` adds it to your profile. `han device revoke <keyId>` removes a lost one.
- **GitHub**: `han-github run --config han-github.toml` polls the configured repositories with conditional requests and turns open `agent-ready` issues into tasks for the space's shared agent pool; the issue text travels with the task as untrusted data. `han-github request` asks for one issue now. The config is described in `integrations/han-github/README.md`.

## 9. A shared runner

A runner is a team-owned Linux VM that hosts the home and runs agent sessions for members. The [shared runner guide](runner.md) sets one up: `sudo han runner setup` for the OS users, units and polkit rule, then the same `han home`, `han agent add` and `han executor add` commands as above.

## 10. When something is stuck

`han doctor` checks the machine in one go and, for every problem, prints the cause and the exact command that fixes it (a node that is down, a link that does not fit its home, an outbox entry that holds a space back, a missing harness or login, a pinned Rust toolchain that is not installed, a replaced binary that still runs). It exits 0 when all is well and 1 otherwise; `--json` prints the same for scripts (schema in [runner.md](runner.md) section 8). `han node status` says whether the node runs, and its outbox. `han dev up` keeps logs in `<dir>/logs`. A home that was upgraded but not restarted runs the old binary (on Linux `/proc/<pid>/exe` reads `(deleted)`); `han home register` and `sudo han runner setup` say so and name the pid and the unit to restart.
