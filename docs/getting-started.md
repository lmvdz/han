# Getting started

This page runs HAN on one machine: a home (the sequencer of your spaces), a node for you, one space, one agent and an executor, and the live board in the terminal. A teammate joins the same home later with its ticket. It uses only the commands in `han` today; the [status page](status.md) says what is missing.

The `init`, `register` and `add` setup commands check existing state and print what they changed (`changed:`) and what they left (`unchanged:`). Space creation makes a new space each time: inspect the result before running it again. Where a step belongs to somebody else (the home's admin, the agent's operator on another device) enrollment prints `to do:` and the exact command that person runs. None of them needs root; the one root-level command is `han runner setup` on a Linux runner ([shared runner](runner.md)).

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

## 2. Start your local team

```sh
han setup --person me
```

This creates a home, registers your human profile through its local admin
endpoint, starts your person node, and opens the TUI's space setup form. Your
device key uses the OS keychain. The home and node run in one managed background
process and keep running when you leave the TUI. Normal device-presence checks
remain enabled.

The default state folder is `han-team` in the current directory. Repeating the
command with the same folder resumes the saved identity. Use `--data <folder>`
to choose another location, `--no-tui` for scripted startup, or `--file-keys` on
a headless machine without an OS keychain. File keys are private credentials;
keep the state folder private. A missing or changed saved key is an error, not
an instruction to silently create a replacement.

```sh
han setup status --data han-team
han setup stop --data han-team       # retains state
han setup --data han-team            # resumes and opens the TUI
han tui --data han-team/person       # opens just the TUI
```

Startup failures point to `han-team/setup.log`; correct the cause and repeat
the same setup command. The default home uses a unique reserved local server
name and the n0 relay. Its ticket is in `han-team/home/home.ticket`. This flow
sets up a new local home; joining someone else's home still uses registration
and a home ticket as described below. Fresh-user timings and two-person
acceptance remain unverified; see the [status page](status.md).

After a Trusted team is confirmed, Enter continues to local runner setup on
Linux with enforced bubblewrap. Setup asks for an agent name, harness (default
`codex`), one of the team's repositories, landing branch, required GitHub check
names, and an installed write GitHub App's IDs and private PEM file path. Prepare
that App using step 7; setup does not create a GitHub App for you. Subscription
login opens once when the selected agent has no saved login. A separate executor
process holds the write credentials. Once its live startup is acknowledged,
the TUI opens a prefilled task form; submitting the form starts the task.

Confirmed choices are retained in `workflow.json`. Repeat the same setup command
to resume. Queued enrollment is not treated as confirmed. Resuming and starting
the executor recheck current ownership, the Trusted team policy, and the saved
configuration; changed permissions or configuration require inspection. A failed
executor leaves the home and node available and does not silently retry. Inspect
`executor.log`, correct the cause, then resume explicitly. `han setup stop` asks
the executor to settle its current pass before stopping the home and node.

This local runner continuation requires Linux, bubblewrap, GitHub, and a
subscription harness. Windows, remote runner pairing, API-key harness setup,
and advanced policies use the separate commands in steps 5–7. Joining another
home does not grant local permission to provision its agents or executor.

After the TUI opens, continue at step 4. For later CLI commands, set `HAN_DATA`
to your `han-team/person` folder or supply `--data` explicitly. The following
manual home and node commands are an alternative to `han setup`.

### Manual home setup

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

With your person node running, open `han tui` and type `:setup`. Choose **Create a
Trusted team** or **Join with an invite ticket**. For a new team, enter its name,
the exact repository identifiers (for example `github:owner/repo`), and an
optional home server or ticket. Enter opens a permission preview; Enter again
creates the space. The node requires a device presence confirmation for the
automatic-landing grant. Esc cancels before submission; PgUp/PgDn scroll a long
preview.

Trusted team permits ordinary changes of at most 500 changed lines to land
automatically in those repositories after the executor's required checks pass.
Built-in sensitive paths, larger or incomplete changes, other repositories and
other effects require human owner approval. A session guard still requires its
person's approval. Membership is invite-only and agents need confirmed
operators. This creates the space and policy together; it does not configure
your agent, executor, credentials or required checks. Continue with steps 5–7.

The CLI can preview the same policy without connecting to a node:

```sh
han space create-trusted --name team --repo github:owner/repo
han space create-trusted --name team --repo github:owner/repo --apply
```

Repeat `--repo` for more repositories. The preview prints the complete policy.
Use the repository identifier your executor uses, exactly; wildcards are not
accepted. A queued creation prints its space and outbox id rather than success.
The TUI saves a recovery notice before sending setup. After an uncertain or
queued result, inspect `han space list` and `han outbox list`; do not create the
space again. A TUI restart retains the notice. Once you have checked the result,
`:setup-reset` clears the notice and opens a new form. Invite tickets are not
saved in TUI preferences.

For advanced custom permissions, the existing base-policy command is:

```sh
han space create --name team --home <ticket>     # prints the space id
han space list
```

That base policy has no approval rules or risk gates; review and configure its
authority before starting a runner. `:setup` links to these custom policy tools.

`han space member <space> <actor> <role>` sets a role (`owner`, `member`, `guest`, `banned` or `none`; setting a role someone holds already changes nothing), and `han space policy <space> <file>` replaces the space's policy with a JSON policy document. A new space admits an agent or a service only once its operator has confirmed it (`allowAgents: "with-operator"`; `han agent add` and `han executor add` do that when the operator runs them), and an agent whose operator withdraws stops proposing and claiming. `han dev up` uses a policy that makes `git_push`, `open_pr` and `http_request` wait for an owner's approval.

Any program that runs as your OS user can open the node's local pipe, so the node does not take the pipe as proof that you are there. A connection starts **locked** for what governs (members, policy, invites, devices, agent keys, pinning a home), for starting an agent and for steering a session. The first such command on a connection makes the node ask your operating system to confirm you (Windows Hello, Touch ID or polkit), the same check as for approvals, and then that connection may govern and steer for ten minutes. `han space member` and the other commands ask once per command; `han tui` asks at your first steering input and not again for ten minutes. A program that cannot answer the prompt is refused with `presence_required`. `han node presence --unlock-secs <10-3600>` sets the window, and `--require` lists the action kinds that need you (`effect.approve`, `effect.cancel`, `face.unlock`; `all` by default). Changing either needs you at the device, too, and a running node keeps the settings it started with, so restart it to apply a change. If you set an explicit `--require` list, add `face.unlock` to it, or the pipe stays open to any program of your OS user (`han doctor` says so). Reading the board, publishing an agent's own status and creating tasks do not need a confirmation.

## 5. Add an agent

Every agent is its own actor with a human operator. One command does the whole job where it can:

```sh
han agent add coder --operator @me:han.home.arpa --space <space> --home <ticket> \
    --home-data home --harness claude --sandbox auto
```

It writes the agent's launch settings (`agents.json` in the node's data folder), installs the selected declarative harness if it is not already set up, makes the agent's key, registers its profile (here, because `--home-data` names the home that runs on this machine), has you confirm it as its operator, and makes it a member of the space. Without `--home-data` it prints, for the home's admin, `han home register @coder:… --public-key=… --operator …`; run on a device that is not the operator's, it prints `han actor confirm @coder:… --operator … --home <ticket>` for the operator. Run `han agent add` again afterwards: it resumes where it stopped and ends with `ready:`.

Setup, login, diagnostics and launch share the same [declarative driver](../crates/han-host/drivers/README.md). `han agent setup-harness` accepts a bundled name (`claude`, `codex` or `pi`), an installed name, or a JSON/TOML driver file. For Codex, install the repository's pinned adapter definition, then select it for the agent:

```sh
han agent setup-harness crates/han-host/drivers/codex.json
han agent set @coder:han.home.arpa --harness codex
han agent login @coder:han.home.arpa
han agent login @coder:han.home.arpa --check
```

Run these from the repository root with the same node data folder (`HAN_DATA` or `--data`) used above. Claude uses the same `han agent login <actor>` command. Login runs outside the sandbox in an isolated per-agent directory; `--check` checks only the cached credential file, not account access or quota. On an OS-user runner, run login as the agent's OS user with `--dir <login-root>` and `--harness <name>`; see the driver guide for shared installation access. Driver files are trusted operator configuration. The Codex profile requires Linux bubblewrap. pi instead uses `--model-key-file <private-key-file>` on `han agent add` or `set`; the proxy holds the API key.

`han actor confirm` is what the operator runs; it works for agents and for the executor (`han agent confirm` still works). `--os-user`, `--allow-advisory` and `--sandbox` record how the agent runs; see the [shared runner guide](runner.md) for OS-user agents.

## 6. Add an executor

Agents hold no repository-write credentials. They propose effects and the **executor** runs authorized work with its configured checks. The Trusted team preset permits ordinary repository-scoped landing without per-change approval, within the limits in step 4. Sensitive, larger or incomplete changes, other repositories and other effects require human owner approval; a session guard also requires its person's approval. The `han dev up` policy requires owner approval for `git_push`, `open_pr` and `http_request`.

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

`--issue` starts the session on the open task for that issue that is addressed to the agent (`han-github` creates one for every agent-ready issue). The issue's title and body reach the agent in that task, as a data block marked untrusted; the agent has no network to fetch them. With no such task the start is refused ("no open task for github:owner/repo#12 ... carries the issue text"); `--task <id>` names a task directly, and its own issue is used.

`--harness` defaults to the driver recorded for the agent: bundled `pi` (proxy-held API key), `claude` (Claude Code through `claude-agent-acp`, on a Claude subscription), or `codex` (Codex ACP, on a ChatGPT subscription). Subscription token counts may be reported while cost remains **unmeasured**; see [usage accounting](usage-accounting.md). The session runs in a sandbox whose only way out is the egress proxy, and its only way to act on the outside world is `propose`.

**Build and test inside the sandbox.** An agent has no Rust toolchain unless the node gives it one. `han agent set @coder:<server> --toolchains rust` turns it on for that agent (the file-wide default is the `"toolchains"` list of `agents.json`, in the node's data directory, `%LOCALAPPDATA%\han` on Windows). The agent then gets your `%USERPROFILE%\.rustup` and `.cargo\bin` read-only (on Windows also the MSVC linker and Windows SDK it finds), a cache of its own for downloaded crates, and `crates.io` on its proxy allow-list; `"toolchainPaths": {"rust": {"rustupHome": "...", "cargoBin": "..."}}` points elsewhere. Install the toolchains your repository pins (`rust-toolchain.toml`) first; the sandbox never downloads one. On a Linux runner `sudo han runner setup --toolchain rust --repo <path>` does the installing and writes `toolchainPaths` ([runner.md](runner.md) section 7.2).

`han agent set` changes how an agent is launched and nothing else; never edit `agents.json` by hand. It takes `--os-user`, `--sandbox`, `--allow-advisory`, `--toolchains` and `--harness`, needs no running node, and prints `changed:` or `unchanged:`.

`han tui` shows the live board, the session pane (Enter) and the approvals inbox, where you read and approve proposals that require human approval. Press **n** (or `:task`) to create and start work: fill in the agent, repository and request, then Enter. **T** (or `:tasks`) lists open tasks; Enter opens an existing task for starting. A failed start keeps the task id, so retrying start does not create another task. A lost reply requires inspecting the board/tasks before retrying; the TUI does not repeat it automatically.

The CLI equivalent, once the node and agent are set up, is:

```sh
han task create <space> "Fix the README links" --to @me-coder:han.home.arpa --repo github:owner/repo --start
```

Use `--file request.txt` instead of the quoted request for multiline work, or omit `--start` and `--repo` to create a task for later. `--to` accepts comma-separated or repeated assignees; immediate start requires one agent. A queued task is not started until the home accepts it. The CLI prints the retained task id and a recovery command if start does not succeed.

`han tui <space> --print 120x30` prints one screen as text and exits, which is handy over SSH and in scripts. `han board` prints the board as plain text. `han agent stop <agent>` stops its sessions.

## 8. Add a teammate, a device, GitHub

- **A teammate** gets registered on the home like you did (`han home register`), runs `han node --actor <them> --home <ticket>`, and follows the space with `han space follow <space> --home <ticket>`. An owner gives them a role with `han space member`. If they are an agent's operator they run the `han actor confirm` line that `han agent add` printed.
- **A second device**: on the new device, `han device offer` prints a pairing code; on an existing device, `han device pair <code>` adds it to your profile. `han device revoke <keyId>` removes a lost one.
- **GitHub**: `han-github run --config han-github.toml` polls the configured repositories with conditional requests and turns open `agent-ready` issues into tasks for the space's shared agent pool; the issue text travels with the task as untrusted data. `han-github request` asks for one issue now. The config is described in `integrations/han-github/README.md`.

## 9. A shared runner

A runner is a team-owned Linux VM that hosts the home and runs agent sessions for members. The [shared runner guide](runner.md) sets one up: `sudo han runner setup` for the OS users, units and polkit rule, then the same `han home`, `han agent add` and `han executor add` commands as above. Remote runner pairing is not implemented; this remains a separate manual setup.

## 10. When something is stuck

`han doctor` checks the machine in one go and, for every problem, prints the cause and the exact command that fixes it (a node that is down, a link that does not fit its home, an outbox entry that holds a space back, a missing harness or login, a pinned Rust toolchain that is not installed, a replaced binary that still runs). It exits 0 when all is well and 1 otherwise; `--json` prints the same for scripts (schema in [runner.md](runner.md) section 8). `han node status` says whether the node runs, and its outbox. `han dev up` keeps logs in `<dir>/logs`. A home that was upgraded but not restarted runs the old binary (on Linux `/proc/<pid>/exe` reads `(deleted)`); `han home register` and `sudo han runner setup` say so and name the pid and the unit to restart.

## 11. Landing your own pull requests: `han dev prs` and `han dev prune`

Two commands for the people and agents who land HAN's own pull requests. They use your `gh` login (`gh auth login`) and the `git` on your path; the default repository is the `origin` remote of your main checkout.

`han dev prs [PR...]` watches the open pull requests that have auto-merge enabled (or only the ones you name), with one GraphQL query per round (`--interval`, default 120 s). It keeps one invariant: every such PR is progressing or reported. It is stateless, so a new session just runs it again, and it exits 0 on the first round that has something to say, one line per event:

| Line | Meaning |
|---|---|
| `MERGED #n`, `CLOSED #n` | a PR seen open in this run has merged or closed without a merge; `han dev prune` then runs on that PR's head branch only (other sessions' worktrees are left alone; a full sweep is an explicit `han dev prune`) |
| `CONFLICT #n …` | the head conflicts with the base. GitHub runs no `pull_request` CI on a conflicting PR, so its required check would never appear; this is reported at once |
| `CI_FAILED #n checks` | the named checks failed, were cancelled or timed out |
| `STALLED #n reason` | nothing is pending and the last push is older than `--grace` (default 900 s); also when auto-merge was switched off or `update-branch` failed |
| `RETRIGGERED #n` | not an exit: auto-merge was switched off and on again, see below |
| `GH_ERROR msg` | GitHub could not be asked |
| `IDLE no open auto-merge PRs`, `OK watching: …` | nothing is watched; or `--once` found nothing to report |

GitHub sometimes never fires an auto-merge that is ready. When a PR has auto-merge on, mergeState `CLEAN`, no pending and no failed checks, and its last push is older than `--grace`, `han dev prs` switches auto-merge off and on again with the same merge method (`gh pr merge --disable-auto`, then `--auto` with the method GitHub reports), prints `RETRIGGERED #n` (`{"event":"RETRIGGERED","pr":n}`) and keeps watching. It never merges directly; branch protection still applies. If the PR is still open and idle one more grace period after the retrigger, it is `STALLED` as above and the command exits. If re-enabling fails, auto-merge is left off and the `STALLED` line says `auto-merge is now OFF`. The retrigger time is kept in memory for the run only.

A PR that is `BEHIND` its base gets `gh pr update-branch`. Run it in the background and read its output when it exits. `--json` prints one JSON object per line (`{"event":"CI_FAILED","pr":7,"detail":"lint","checks":["lint"]}`), and ends with `{"event":"WATCHING","prs":[…]}`.

`han dev prune [BRANCH...]` removes the worktrees and local branches whose PR has merged or closed, with each worktree's `target` directory (and a sibling `<worktree>-target`). One `gh pr list` call per run; `--dry-run` changes nothing. Options shared with `prs`: `--repo owner/name`, `--main DIR` (the main checkout, by default the first worktree of the repository you are in), `--root PREFIX` (only worktrees whose path starts with it are touched, by default `wt-` next to the main checkout) and `--base REF` (default `origin/main`). One line per candidate:

| Line | Meaning |
|---|---|
| `PRUNED path (branch, PR state)` | worktree, target directory and branch removed (`; directory … is held open` when the OS keeps an empty directory) |
| `PRUNED_BRANCH branch (PR state)` | a branch with no worktree removed |
| `SWITCHED path from branch … to main; branch deleted` | the main checkout was clean (untracked files ignored) and on a landed branch: it went back to the base, fast-forwarded, and the branch was deleted. The main checkout itself is never removed |
| `UNLANDED path (branch, PR #n STATE, commits not on base:` … `)` | commits made after the PR's recorded head that the base lacks by patch id (`git cherry`), listed with their subjects. A squash merge never matches the PR's own commits, so only commits after the head are judged; a branch that was rewritten is judged whole. This is an event, not housekeeping |
| `PRUNE_FAILED path (branch): why` | removal failed; the worktree and branch stay |
| `PRUNE_BUSY …` | another pruner holds the lock in the git directory (`han-prune.lock`); the operating system releases it when its holder dies |
| `KEPT_DIRTY`, `KEPT_OPEN`, `KEPT_NO_PR` | uncommitted changes (untracked files count in a worktree); the PR is still open; the branch never had a PR |

With `--dry-run` the removals show as `WOULD_PRUNE`, `WOULD_PRUNE_BRANCH` and `WOULD_SWITCH`. `--json` prints one object per outcome: `{"event":"UNLANDED","path":…,"branch":…,"pr":81,"prState":"MERGED","detail":"commits not on origin/main","commits":[{"id":"1a2b3c4d","subject":"…"}]}`.
