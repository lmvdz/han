# ACP harness drivers

The host runs ACP adapters described by `DriverConfig` (JSON or TOML). Setup, login,
diagnostics and launch read the same installed definition. An adapter does not need
a new protocol method, a registry special case, or a provider-specific login command
in HAN's CLI.

From the repository root, install the Codex example:

```sh
han agent setup-harness crates/han-host/drivers/codex.json --data <node-data> --dir <adapter-install>
han agent set <actor> --harness codex --data <node-data>
han agent login <actor> --data <node-data>
han agent login <actor> --check --data <node-data>
```

On an OS-user runner, run login as the agent's OS user and pass `--dir` for that
agent's login root and `--harness codex`. The command also needs read access to the
installed harness record. Never copy an entire personal Codex configuration into
the agent's login directory. A cached credential file is only evidence of a prior
login; `--check` does not prove current account access or remaining quota.

Setup installs the pinned npm adapter, verifies its npm integrity, and saves
`driver.json` beside the installation. The registry points to that snapshot. Later
`setup-harness codex` uses the installed definition. Driver files are trusted
operator configuration: their installation and login commands run outside the
agent sandbox. Do not accept them from an agent as a way to widen its permissions.

With an explicit `--dir`, a downloaded Node runtime also lives under that shared
installation. The agent's OS user needs read/execute access to both the runtime
and adapter. `--node` can select an existing shared Node 22 installation.

The contract includes:

- `command` and `env`: argument vectors and explicit environment values, with
  `{node}`, `{adapter_dir}`, `{agent_dir}`, `{worktree}` and proxy placeholders.
- `adapter`: npm package, pinned version, integrity and entry script.
- `mcp`: HAN tools passed through ACP or an adapter configuration file.
- `credentials`: subscription CONNECT hosts, or a proxy-held API key route.
- `isolation.login_dir_env` and `isolation.login`: the environment variable,
  relative credential filename, harness-specific subdirectory and login argument
  vector. Login commands support `{node}`, `{adapter_dir}` and `{login_dir}`.
- `isolation.reject_config`: workspace and login-relative paths whose presence
  refuses admission, for adapters that cannot disable inherited settings themselves.
  Nothing is deleted. This is a startup check, not a runtime write filter.
- `isolation.volatile_login_dirs`: direct child directory names hidden beneath empty,
  writable session-only mounts. Existing contents remain untouched; generated files
  disappear when the session ends. Currently requires Linux bubblewrap; other
  backends refuse rather than expose inherited contents. Symlink targets are refused.
- `strict_mode`, `auto_mode`, `gate_fidelity` and `tested_version`: declared ACP
  capabilities and the version to compare with the adapter's handshake.

Old Claude drivers without `isolation.login` retain their original credential
filename and login command. The historical agent login root is still named
`claude-config` for compatibility; Codex's own files live in its `codex/`
subdirectory, and only that subdirectory is exposed to its sandbox.

## Codex example

The example pins `@agentclientprotocol/codex-acp` 2.2.2. That adapter starts its
bundled Codex app-server and forwards HAN's MCP server through ACP. It uses a
ChatGPT subscription login and device authentication. It has no Claude dependency.

Codex's inner sandbox is disabled **inside HAN's enforced OS sandbox** to avoid
nesting incompatible sandboxes. HAN still confines files and network access and
holds all repository-write credentials in its executor. The example declares
`prompt_only`; it does not claim that Codex asks HAN about every local tool call.
Never use this driver with an advisory sandbox for a guarded-work claim.

This profile requires Linux bubblewrap. Codex generates bundled skills during a
session, so `skills` and `plugins` use session-only mounts rather than a presence
check that would reject the next start. Credential refreshes remain persistent,
including atomic replacement of `auth.json`. Existing skills/plugins are hidden,
not deleted. Login settings and hooks still refuse admission before the host
reports success; project configuration is checked after checkout, before launch.

The example refuses a pre-existing workspace `.codex` directory and login-level
configuration, hooks, skills and plugins. This prevents silently inheriting those
configuration sources at admission. Repository instructions remain untrusted
input; this does not make agent-generated commands trustworthy.

### Live evidence

On 2026-10-20, this driver completed a real task on the Linux OS-user runner:
repository inspection, a documentation edit, local validation, a HAN MCP proposal,
human approval, executor checks, and [landing PR #173](https://github.com/lmvdz/han/pull/173).
The landed commit is `96da3fa10d49add75e2af90d1430ca434bebc130`. The host recorded
the result as confirmed before pruning the workspace. The run used bubblewrap
and a ChatGPT subscription; it did not use an API key or Claude quota.

This is evidence for that Linux completion path. Fresh onboarding, Windows Codex
sandboxing, steering, quota exhaustion and interrupted-turn recovery have not been
established by this trial. Its approval was manual; it does not demonstrate the
planned Trusted team preset. Subscription cost is unmeasured even if the existing
status counter displays zero.

Upstream references:
[Codex ACP](https://github.com/agentclientprotocol/codex-acp),
[Codex authentication](https://learn.chatgpt.com/docs/auth).
