# R15: the changeset reaches the executor as a bundle blob

Status: implemented behind `git_push` (exec/bundle-blob-r15). **Spec change for round 33**: the informative `git_push` plan, below.

## Problem

The host built a `git_push` plan whose `args.source` was the host's own absolute workspace path, and the executor opened that path to check the changeset and to push from it. On a Linux runner set up by `han runner setup`:

- an OS-user agent's state directory is `/var/lib/han-agents/han-agentN`, mode 2770, group `han-agentN`;
- the executor runs as `han-exec`, which is not in that group.

So the executor can't read the workspace. Reproduced with real users in a systemd container (`apps/han/tests/e2e/os_user.rs`):

- after the approval, the executor refused at preflight with `not found: no commit <to>`;
- the effect stayed `open` and nothing landed;
- the error said nothing about permissions.

A path is also the wrong thing to put in a signed, replicated plan. It names one machine's filesystem, it means nothing to an executor on another host, and it leaks the layout.

## Design

The plan carries the commits themselves, through the space's existing blob path (G-01, `plan.argsBlob`):

```
{action: "git_push", target, args: {branch, base, from, to, changeset, changes?},
 argsBlob: {hash, size, type: "application/x-git-bundle"}}
```

**Host.** At propose time, the host:
- commits the workspace and computes the changeset as before;
- makes `git bundle create … refs/han/bundle ^base`: one ref at `to`, with `base` as the only prerequisite;
- uploads it through its node with `BlobPut`, as the agent that authors the proposal;
- puts its `BlobRef` in the plan.

`planDigest` covers the `BlobRef`, so an approval binds the exact bytes. If the home is away, the proposal waits in the outbox as before and the upload is retried in the background. Any other upload refusal refuses the proposal. Node API blobs are at most 4 MiB, and so is a bundle.

**Home.** Supply follows authors (G-01): the bytes are served for the space because the proposal's author uploaded them. The home now honours this whatever the order of the upload and the proposal; before, only an upload that came first counted. That's what the spec text already says.

**Executor.** Before preflight, the executor:
1. fetches the blob through its node with `BlobFetch`; the node checks the size and the hash, and the executor checks the hash again;
2. checks the bundle header before git sees it: v2 or v3, exactly one ref (`refs/han/bundle` at `to`), and no prerequisite but `base`;
3. imports the bundle into a repository of its own for the target's remote (`<objects>/landing-<id>.git`, `fetch.fsckObjects`). If that repository lacks `base`, it fetches `base` from the remote first, by id and then by branches, with the push credential;
4. checks that `base..to` there has the approved `changeset` digest, then pushes `to` from that repository by compare-and-swap, as before.

An import that fails the header or head check is a `changeset` refusal. A blob the home doesn't serve yet is a preflight failure, tried again at the next head.

**Fallback.** A plan with `args.source` and no `argsBlob` still works when the executor can read that path (same OS user, older hosts). A plan with both, or with neither, is refused at `plan_check`. Other actions take no `argsBlob`.

## Round 33

The following change the spec:

- **§5 Plans:** the reference `git_push` plan becomes the shape above, and `argsBlob` is a bundle of `base..to`. The executor MUST verify the bundle against `to` and `changeset` before landing, and MUST NOT read a path named by the plan.
- **G-01:** say explicitly that supply holds whether the author's upload comes before or after the referencing record is ordered.
- **Executor (X-09):** a plan whose `argsBlob` is not yet served is retried, not refused for good.

## Not done here

- Executors that hold no blob through a node.
- Bundles over 4 MiB: the node API carries one blob in one message. A chunked `BlobPut`/`BlobFetch` would lift the limit.
