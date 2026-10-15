# R33: `han.land/1`, landing at scale

Status: **outline for round 33.** Nothing here is normative yet. Decisions: PLAN.md §12 (land lanes, liveness, conflicts as data, cross-repo changes, workspace pruning, landing profile), plus the rebase rule below (2026-10-15). Inputs: the dogfood findings below; Anthropic's "Redesigning source control" and the open-source Scion model of it; Zuul; bors and SubmitQueue.

## Problem

Agents land work through the executor: `effect.propose` with a `git_push` plan, then approval, `effect.claim`, a compare-and-swap ref move, and `effect.outcome` (§4.5, studies/r15-changeset-bundle.md). Three things break when thousands of agents land on many repositories:

1. **Throughput.** Optimistic landing keeps main broken about N·p of the time (N landings an hour, a fraction p of them breaking it). One-at-a-time queues cap out at the CI time each change takes.
2. **Silent waiting.** In the dogfood, PR #99 conflicted after #100 landed. GitHub ran no `pull_request` CI on it, so a required check never appeared and auto-merge waited forever. Nothing failed, so nothing reported it.
3. **Lost work.** A commit made on #81's branch after #81 auto-merged never landed (found by the pruner a day later). A change kept growing after it was queued.

## What is authority and what is profile

A profile carries no authority (profiles/README.md). Landing keeps all of its authority in the core, unchanged:

- the approval binds `planDigest`;
- the claim is the exclusive fence for each land effect;
- the executor alone moves the ref.

`han.land/1` defines only data: the plan's shape, the changeset's states, lane status items, conflict records and pruning reports. The core changes it needs are listed at the end. **Correction to PLAN.md §12:** "a lane held by an exclusive `effect.claim` on the ref" is not a core concept. The fences are the per-effect claim plus the ref's compare-and-swap; a lane lease (below) only avoids wasted CI runs.

## 1. Changesets and the delta digest

A **delta** is the sorted list of `{path, kind, from?, to?, mode?}`:
- `kind` is `add`, `modify`, `delete` or `rename` (a rename names `fromPath`);
- `from` is the path's content id **at the base**;
- `to` is the new content id.

The ids are the `Vcs` backend's own object ids. `deltaDigest = sha256(JCS(delta))` does not depend on the base commit. Today's `changeset_digest(base, files)` remains as the **landed** digest, recorded in evidence.

**Path-disjoint.** A changeset with base B is path-disjoint from a later commit B′ when every path in its delta (both sides of a rename) has content `from` in B′, or is absent in B′ where `from` is absent. Every ancestor directory of those paths must also be the same kind (file or directory) in B′. Then rebasing onto B′ writes exactly the approved contents, and the delta, and so `deltaDigest`, is unchanged. This check is exact and deterministic, and costs O(|delta| × depth) lookups (the same bound as Scion's rebase).

## 2. The land plan

The reference plan for a lane (round 33 replaces `git_push` for landing; `git_push` stays for branch pushes):

```
{action: "vcs.land",
 target: {repo: "github:owner/repo", ref: "refs/heads/main"},
 args: {base, to, delta: <BlobRef or inline>, deltaDigest, tier,
        rebase: "path-disjoint" | "none",
        dependsOn?: [{repo, proposal} ...], rebaseOf?: <proposal id>},
 argsBlob: {hash, size, type: "application/x-git-bundle"}}
```

**The rebase rule (decided 2026-10-15).** With `rebase: "path-disjoint"`, the executor may land the approved changeset onto any later B′ of `target.ref` from which it is path-disjoint. The plan, and so `planDigest`, is unchanged; the approval and the CI and review evidence over `deltaDigest` still count. The outcome's evidence records the landed base, the commit and the landed digest. When B′ is not path-disjoint, the change is **gated again**:
- the host (or a resolver agent) makes a new proposal with `rebaseOf` naming the old one;
- tiers 0–1 take their automated gates again;
- for tiers 2–3, the approver is shown only the **interdiff**: the difference between the approved result and the rebased one, file by file.

A clean three-way merge in a shared file is not path-disjoint. It changes what the approved code does, so a person sees the interdiff.

**Frozen once proposed (the #81 lesson).** A changeset is fixed by its proposal. The host MUST NOT add commits to a proposed workspace. Further work becomes a new changeset and a new proposal, based on the old one's `to`, with `dependsOn` naming the old proposal.

## 3. Changeset states and liveness

| State | Pending while in it | Who moves it | Default deadline |
|---|---|---|---|
| `proposed` | approvals that tier and Policy ask for | approvers (E3) | per tier: 0–1 30 min; 2–3 the path owner's SLA (1 working day) |
| `queued` | a lane slot | lane | 30 min × lane depth / batch size |
| `testing` | a speculative run that includes it | lane | the repo's CI timeout + 10 min |
| `landing` | the claim and the ref compare-and-swap | lane | `launchWindow` |
| `landed`, `rejected`, `cancelled` | final | | |

Faces and the node's views derive **`stalled`** the way they derive `stale`: the change has nothing pending, or it is past its state's deadline. Its agent, the agent's operator and the path owner are told as data. `rejected` always carries its reason as data:
- `conflict`, with a conflict record (§5);
- `test`, with the failing checks and their logs as evidence;
- `policy`;
- `dependency`, naming the dependency that failed;
- `timeout`.

The **liveness invariant**: every changeset is in a state with something pending and within its deadline, or it is final, or it is `stalled` and someone has been told. Conformance tests this invariant, not single events.

States are folded from the log (proposal, approvals, claim, outcome) and from the lane's status items. Nothing is claimed by an agent.

## 4. Lanes

**One lane per (repo, ref).** The lane is the executor that holds that repo's write credential (PLAN.md §6.7: han-exec is its only holder).

**Order.** Changes leave `proposed` for `queued` in the sequencer's order of the entry that completed their approval. Order is deterministic and global across lanes, and every replica agrees on it.

**Speculative batches.** The lane takes up to B queued changes c1…cB. It rebases them in order onto the ref's current target T, applying each change's path-disjoint check against T plus the changes before it. A change that fails the check leaves the batch and is gated again (§2). The lane then runs the repo's checks on the prefixes c1, c1+c2, …, up to S runs in parallel. Testing happens in executor-private workspaces and is not a world action, so it needs no claim.

**Land.** The lane takes the longest green prefix:
- it claims each member's effect (X-01…X-09, so `launchWindow` covers only the push, never CI);
- it moves the ref from T with one compare-and-swap;
- it reports one `effect.outcome` per member.

If the compare-and-swap loses (T moved), the lane claims nothing new and starts again from the new target.

**Bisect.** In a red batch, the first red prefix names the culprit. To rule out a flaky test, the culprit is re-run alone on T once:
- red again: it is `rejected` (`test`), with the evidence;
- green: the culprit is marked flaky, retried once in the next batch, and only then rejected.

B, S and the flake retry count are per-repo settings, and the lane's status items show them.

**Evidence.** A green run is a blob signed by the executor instance key over `{target tree, [deltaDigest…], checks}`. It is the CI evidence for every member, and the land attestation (PLAN.md §10.2) names it.

**Lease.** Two instances of one lane are safe, because claims and the compare-and-swap fence them, but they waste CI. An instance announces itself on the lane's status stream; others stay hot standby until its items stop for `2 × stallBound`. This is advisory and has no authority.

**On GitHub** (the git backend):
- the lane pushes each speculative prefix to `refs/heads/han/lane/<ref>/<n>` and reads the check runs (through the webhook relay; polling only as a fallback);
- it moves the protected branch with the write App, which branch protection lists as allowed to bypass the merge requirements.

GitHub's own merge queue is not used: it can neither do path-disjoint carrying nor `dependsOn`.

## 5. Conflicts as data

A rebase or merge simulation that conflicts never writes conflict markers. It returns:

```
{changeset: <proposal>, onto: <commit>, clean: [path...],
 conflicts: [{path, kind: "content"|"add_add"|"modify_delete"|"file_directory"|"binary"|"symlink",
              base?, ours?, theirs?, hunks?: [{baseRange, oursRange, theirsRange}]}],
 against: [{proposal, task}]}
```

`against` names the landed changes that touched the conflicting paths since the base, and their tasks. The resolver agent gets this record and both tasks' intent. It does not get a broken worktree. Its fix is a new proposal with `rebaseOf` (§2). `Vcs::merge_simulate` returns this shape. Overlap tier 3 (PLAN.md §6.6) uses the same record before anything lands.

## 6. Cross-repo `dependsOn`

- `dependsOn` lists proposals in other repos (or in the same repo).
- A lane does not queue a change until every dependency is `landed`. When a dependency is `rejected` or `cancelled`, the change is rejected (`dependency`).
- If a repo's CI can take them, the lane tests a change with its unlanded dependencies applied (through the other repo's lane prefix refs: the Zuul model). Otherwise it waits for them to land first.
- A cycle is refused at `plan_check`.
- Nothing lands atomically across repos.
- `dependsOn` is inside the plan, so the approval binds it.

## 7. Workspace pruning

When a changeset's effect is final, its host removes:
- the session's workspace;
- its build output (`CARGO_TARGET_DIR`);
- its local refs.

It keeps a workspace with uncommitted changes, or one whose tip has commits that the target lacks by patch id after the proposal's `to`. For that kept workspace it reports an **`unlanded`** item `{workspace, proposal, commits: [{id, subject}]}`, which is an event for the agent and the operator, not housekeeping. A periodic sweep catches what a crashed host left behind. Reference: the stopgap `prune-worktrees.sh` outside the repo found 37 merged branches and the lost #81 commit on its first run.

## 8. Profile surface

- `han.land/1` status items: one per changeset, `{proposal, state, lane?, position?, batch?, deadline, reason?, conflict?: BlobRef, evidence?: BlobRef, seq, at}`. The lane publishes them on the lane's stream; faces derive `stalled` from them.
- Lane items: `{repo, ref, instance, depth, batchSize, parallel, running: [{n, members}], seq, at}`.
- The conflict record (§5) and the `unlanded` item (§7), as JSON blobs.

## Core changes for round 33

1. **§5 Plans:** the informative `vcs.land` plan above. **X-rule:** an executor MAY land a `rebase: "path-disjoint"` plan onto a later base only after checking path-disjointness, and MUST record the landed base, commit and digest in the outcome evidence.
2. **X-rule:** a lane claims only at push time, after its tests have run. A batch is claimed member by member, and a lost compare-and-swap leaves the batch's later members unclaimed.
3. **Host rule (frozen changeset):** no commits are added to a proposed workspace. Unlanded commits are reported (§7).
4. **Plan check:** a `dependsOn` cycle is refused. Policy is unchanged, because ordering is the executor's job, bound through the plan.
5. Semantic gates and risk tiers in PolicyDoc, already queued: `tier` in the plan is only informative; Policy derives the tier itself from the delta.

## Tests the build needs (e2e, one per finding)

- A queued change whose base is overtaken by a conflicting land becomes `rejected (conflict)` with a conflict record within its deadline. It never waits silently (#99).
- A commit added after a change is proposed is refused locally, and becomes a new proposal. After landing, leftover commits produce `unlanded` (#81).
- A path-disjoint rebase lands under the original approval. A rebase that is not path-disjoint asks again for a tier-2 approval with the interdiff.
- A batch of 8 with one breaking change lands 7 and rejects the culprit. A flaky failure is retried once before it rejects anything.
- Two lane instances never land twice, and the standby takes over when the first one's items stop.
- `dependsOn` across two repos lands in order. A dependency cycle is refused. A rejected dependency rejects the change that depends on it.

## Open

- The action's name (`vcs.land`) and whether `git_push` keeps a landing mode.
- Defaults for B, S and the deadlines: measure them on the dogfood board.
- Whether overlap data should feed task assignment (PLAN.md §12 open items).
- How a non-git backend (Sapling, Scion) satisfies `merge_simulate`'s record and the path-disjoint check. Scion's server-side `Rebase` already returns conflicts in this form.
