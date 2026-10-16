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

*Implementation note (git backend, `crates/han-vcs`).* "Content" in the path-disjoint check is mode and id together; an ancestor that the delta itself replaces (a directory that becomes a file) is checked as the delta path it is. Two things in the entry differ from the list above. A rename is detected on **exact content only**: a deleted and an added path with the same content id and the same type (regular file or symlink), paired in path order when several share an id. The backend's similarity matching is not used, so any implementation reproduces the delta, and the digest, from the two trees; a moved and edited file is a delete and an add, which the path-disjoint check treats the same way. And an entry carries `fromMode`, the mode at the base: without it a mode change landed on the target in the meantime (for example the executable bit) would be undone by an approved content edit, because `from` alone is the same blob id. `deltaDigest` is `sha256(JCS(entries))` over the bare array, absent fields left out.

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

**The bundle of a follow-up (dogfood, 2026-10-15).** The lane squashes, so the old `to` never reaches the remote. A follow-up's bundle therefore does not start at it: the host bundles from the session's first base, `bundleBase..to`, and the plan names that commit as `args.bundleBase` when it differs from `base`. `base` stays the old `to` (the delta and its digest are `base..to`), and it is inside the bundle. The bundle imports on any executor that can fetch `bundleBase`: after a restart, with a new object store, or on another machine. The cost is a bundle that grows with the session's history, up to the blob limit.

## 3. Changeset states and liveness

| State | Pending while in it | Who moves it | Default deadline |
|---|---|---|---|
| `proposed` | approvals that tier and Policy ask for | approvers (E3) | per tier: 0–1 30 min; 2–3 the path owner's SLA (1 working day) |
| `queued` | a lane slot | lane | 30 min × lane depth / batch size |
| `testing` | a speculative run that includes it | lane | the repo's CI timeout + 10 min |
| `landing` | the claim and the ref compare-and-swap | lane | `launchWindow` |
| `landed` | the post-merge suite on a target that contains it (§4, two stages) | lane | the full suite's timeout + 30 min |
| `confirmed`, `reverted`, `rejected`, `cancelled` | final | | |

Faces and the node's views derive **`stalled`** the way they derive `stale`: the change has nothing pending, or it is past its state's deadline. Its agent, the agent's operator and the path owner are told as data. `rejected` always carries its reason as data:
- `conflict`, with a conflict record (§5);
- `test`, with the failing checks and their logs as evidence;
- `policy`;
- `dependency`, naming the dependency that failed;
- `timeout`.

`reverted` carries the post-merge run that failed, as evidence, and the revert's own changeset.

The **liveness invariant**: every changeset is in a state with something pending and within its deadline, or it is final, or it is `stalled` and someone has been told. Conformance tests this invariant, not single events.

States are folded from the log (proposal, approvals, claim, outcome) and from the lane's status items. Nothing is claimed by an agent.

**Missed items (dogfood, 2026-10-15).** The home does not replay stream items, so a host that was away when the lane published a change's item would wait for ever. Two measures, both required of a lane and a host:
- the lane publishes the current item of every change that is not final again every `republish_secs` (default 60), and every change's item once more after it restarts;
- the host also reads how its proposals' effects ended in the log. An effect cancelled with a lane reason (`<reason>: <detail>`, `reason` one of the rejection reasons above) is `rejected` with it, any other cancel is `cancelled`, and an outcome of `succeeded` is `landed`. The log only moves a change on; a later item (`confirmed`) still counts.

## 4. Lanes

**One lane per (repo, ref).** The lane is the executor that holds that repo's write credential (PLAN.md §6.7: han-exec is its only holder).

**Order.** Changes leave `proposed` for `queued` in the sequencer's order of the entry that completed their approval. Order is deterministic and global across lanes, and every replica agrees on it.

**Two stages (decided 2026-10-15).** Anthropic's experience at about 30,000 merges a day is that a full pre-merge queue does not hold at agent scale ("a proper merge queue… doesn't make much sense"); they test post-merge and pre-deploy. HAN splits each repo's checks in two:
- **Pre-merge** (`fast`): formatting, lints, and the tests of the crates the change affects plus their reverse dependencies. These run on the exact commit that lands. They are the lane's gate and the evidence below.
- **Post-merge** (`full`): the whole suite, including end-to-end and dogfood tests, on a target that contains the change. It can cover several landed changes at once.
  - Green: those changes become `confirmed`.
  - Red: the lane bisects among the changes the run covered, as below. The culprit is reverted, as a pre-approved effect of the lane's executor (PLAN.md §12 "Optimistic landing"), and becomes `reverted`. A fix-forward task goes to its agent with the failing run as evidence.
  - *Implementation note (A2, decided 2026-10-15).* The covered changes are the ones still `landed` up to the red one, in the order they landed. The lane runs `full` (through the `Checks` backend) on the middle candidate and keeps the half that holds the first red one; the culprit is the first red candidate. The revert is a squash commit on the culprit's landed commit with the tree the culprit landed on, so its delta is the exact inverse; it is a `vcs.land` whose `change.revertOf` names the culprit, with the failing run as the approval's evidence (architecture.md P-17 *Revert*). The lane approves it itself at tier 0 and 1, where Policy lets a machine count, through a narrow executor-class node command (`RevertApprove`, decided 2026-10-15: the node signs `effect.approve` as its own actor, with no prompt, only for an open proposal its own actor made whose change names `revertOf`, on `han.ci/1` evidence over the reverted delta; `EffectApprove` stays refused to an executor, and Policy still decides, so a tier 2 or 3 culprit, or one on a never-auto path, has its revert proposed at once and waits for a person). Before a candidate is reverted, `full` is run on it once more (decided 2026-10-15): only a red that reproduces makes it the culprit; a green re-run marks it `flaky`, escalated and not reverted, and a backend that cannot re-run it leaves the red unconfirmed and escalated. A revert Policy refuses to take (a budgeted space asks a non-human proposer for a budgeted task assigned to it, which the lane does not have) is not retried: the culprit is escalated with the reason for a person. The fix-forward task's ask starts with a marker line naming the culprit, so a task whose reply was lost is adopted. In every case the culprit's proposer gets the task. The revert lands like any change (fast checks, compare-and-swap); if later changes touched its paths it stays queued, stalled, with the conflict record, and nothing is forced. The culprit becomes `reverted` when the revert lands; the changes behind it that the red run covered become `confirmed` when the revert's own `full` run is green. A revert whose own `full` run is red is escalated, never reverted again. Off by default (`checks.auto_revert`).
- *Implementation note (machine review runtime, decided 2026-10-15).* `han review run` (crate `han-review`, docs/runner.md 9.2) watches the open `vcs.land` proposals of its configured spaces and reviews those whose derived tier is at most 1, once per reviewer version. The tier is read from the node's `Effects` row (`gateTier`, derived from the body's change summary and the policy in force at the proposal with the same function Policy uses, `han_core::policy::change_tier`; `deltaDigest` is the gate's). The row fields are additive and informative: Policy still pins the real gate, and a mistaken tier can only cost a model call, never an approval. A record becomes an approval through `ReviewApprove`, a second narrow executor-class node command shaped like `RevertApprove`: the reviewer runs its own node in process, held to the executor class (`EffectApprove` stays refused, SR-06, SR-106, and a headless node never approves for a person); the node signs `effect.approve` as its own actor with no prompt only for an open `vcs.land` that another actor proposed, of derived tier at most 1, on `han.review/1` evidence over the gate's `deltaDigest`, with the record as the content `evidence`. Policy still decides: the reviewer must be an approver of a rule with `evidence: "han.review/1"`, a non-human approval never counts at tier 2 or more, and `separateApprover` and guards apply, so no Policy change was needed. A reviewer that is guarded (`sessions.forceGuarded`) can not approve (P-08, P-15). The node does not know which harness and model ran an author or who operates it beyond the guard, so the reviewer's configuration declares them, and an author it knows nothing about is refused before any model call (independence fails closed). `request_changes` and `needs_human` upload the record and send the proposer a task that states it is review data, not an instruction; no log entry yet refers to a record that is not an approval's evidence.
- Which checks are `fast` and which are `full` is per-repo configuration, and the lane's status items show it. A repo with no `full` set treats `landed` as `confirmed`.
- Deploys stay gated on `confirmed`, never on `landed`.

**Conflict sets (decided 2026-10-15).** One lock per ref would make every change wait on every other. Instead, a change's **test scope** is the set of units whose tests its `fast` checks run (for cargo: the affected crates). Its **conflict set** is the test scope's dependency closure. Two queued changes are **independent** when neither changes a unit in the other's conflict set and they are path-disjoint from each other.
- Independent changes are tested in parallel, each on the ref's tip T.
- When the ref has moved from T to T′ only by changes independent of change c, c's `fast` evidence carries to T′: nothing its tests depend on changed. The lane then squash-rebases c onto T′ and lands it without testing again.
- This is the one place where the landed commit is not the exact commit tested. The evidence names both commits and the independence check, and the post-merge stage covers it.
- Within a conflict set, the exact-commit rule holds without exception: a change whose conflict set the move touched is tested again on T′.
- The repo's configuration names the units and how to compute the closure (for cargo, from `cargo metadata`). A change outside every unit (workspace manifests, toolchain, CI configuration) conflicts with everything.

This follows Uber's SubmitQueue, which cut its CI resources by 53% by running changes that cannot conflict in parallel.

**Speculative batches.** Within one conflict set, the lane takes up to B queued changes c1…cB. It rebases them in order onto the ref's current target T, applying each change's path-disjoint check against T plus the changes before it. A change that fails the check leaves the batch and is gated again (§2). The lane then runs the repo's checks on the prefixes c1, c1+c2, …, up to S runs in parallel. Testing happens in executor-private workspaces and is not a world action, so it needs no claim.

**Land.** The lane takes the longest green prefix:
- it claims each member's effect (X-01…X-09, so `launchWindow` covers only the push, never CI);
- it moves the ref from T with one compare-and-swap;
- it reports one `effect.outcome` per member.

If the compare-and-swap loses (T moved), the lane claims nothing new and starts again from the new target.

**Bisect.** In a red batch, the first red prefix names the culprit. To rule out a flaky test, the culprit is re-run alone on T once:
- red again: it is `rejected` (`test`), with the evidence;
- green: the culprit is marked flaky, retried once in the next batch, and only then rejected.

B, S and the flake retry count are per-repo settings, and the lane's status items show them. The RECOMMENDED way to size batches is Zuul's window: start at a configured size, grow by one after each green batch, and halve after each red one. A red batch may be split k ways rather than two, as Mergify does, when S allows it.

**Evidence.** A green run is a blob signed by the executor instance key over `{target tree, [deltaDigest…], checks}`. It is the CI evidence for every member, and the land attestation (PLAN.md §10.2) names it.

**Lease.** Two instances of one lane are safe, because claims and the compare-and-swap fence them, but they waste CI. An instance announces itself on the lane's status stream; others stay hot standby until its items stop for `2 × stallBound`. This is advisory and has no authority.

**On GitHub** (the git backend):
- the lane pushes each speculative prefix to `refs/heads/han/lane/<ref>/<n>` and reads the check runs (through the webhook relay; polling only as a fallback);
- it moves the protected branch with the write App, which branch protection lists as allowed to bypass the merge requirements.

GitHub's own merge queue is not used: it can neither do path-disjoint carrying nor `dependsOn`, and it is only offered to repositories owned by an organization.

**Where checks run.** Checks run behind a `Checks` seam with backends:
- a **warm runner** the executor controls, with a persistent build directory, so a change rebased onto the last green tree builds incrementally;
- **GitHub Actions** on the same commit, as a backstop until the warm runner has proven itself.

A warm runner runs only changes proposed by identities with a role in the space and approved through Policy, never code from fork pull requests. GitHub's guidance against self-hosted runners on public repositories is about the second case.

## 5. Conflicts as data

A rebase or merge simulation that conflicts never writes conflict markers. It returns:

```
{changeset: <proposal>, onto: <commit>, clean: [path...],
 conflicts: [{path, kind: "content"|"add_add"|"modify_delete"|"file_directory"|"binary"|"symlink",
              base?, ours?, theirs?, hunks?: [{baseRange, oursRange, theirsRange}]}],
 against: [{proposal, task}]}
```

`against` names the landed changes that touched the conflicting paths since the base, and their tasks. The resolver agent gets this record and both tasks' intent. It does not get a broken worktree. Its fix is a new proposal with `rebaseOf` (§2). `Vcs::merge_simulate` returns this shape. Overlap tier 3 (PLAN.md §6.6) uses the same record before anything lands.

*Implementation note (git backend).* `Vcs::merge_record` returns the record; `merge_simulate` is the same data in its old `{clean, conflicts: [path]}` shape. `clean` lists the paths the change touches that merge cleanly. `base`, `ours` and `theirs` are tree entries `{mode, id}` (a directory is mode `040000`). `hunks` are filled for `content` conflicts: changes of both sides that overlap or touch in the base form one region, which is a hunk when the two sides' lines differ there. The line merge git itself runs may split a region finer, so the hunks are coarser than git's markers, never finer. Each hunk has an `ours` and a `theirs` candidate whose `contentId` is a blob of that side's lines; the `tool` source is a variant of the same enum and nothing fills it yet. A submodule pointer conflict is `binary`. A symbolic link on any side is `symlink`, taking precedence over the other kinds.

**Resolving: tools first, a model only to rank (proposed 2026-10-15, after a prototype on HAN's own conflicts).** A conflict record is resolved in this order:
1. **git's merge.**
2. **mergiraf**, a syntax-aware structural merge. It is optional and must be on `PATH`. It is GPL-3.0-only, so it runs only as a separate process and is never linked; an image that bundles it ships its licence and source offer. Its whole-file result becomes a `fileCandidates` entry `{id, source: tool, verified}`. Where its output aligns with a hunk, it becomes a hunk candidate `{source: {kind: tool, name: "mergiraf", version}}`. A whole-file result counts only once `verified` is `true`: a mergiraf exit status of 0 is not proof. On HAN's #92/#93 conflict in `bindings.rs` it exited 0 and produced code that fails to compile, with two `use super::{}` blocks.
3. **Deterministic candidate generators**, each recorded as a tool with name and version:
   - `line_split`: a finer split when every line is changed by at most one side;
   - an AST-equality check for sides that differ only in formatting;
   - an import-union generator.
4. **A model ranks the candidates.** It never writes code. It returns a typed choice of one candidate id, or `needs_human`, as a decision record:
   - `{requestDigest, selected, probabilities, confidence, reasonCode, backend, model, modelVersion, promptHash, schemaHash}`;
   - `modelVersion` is a resolved version, never an alias;
   - the question is asked twice, with the options in opposite order under anonymous labels, and two different answers mean `needs_human`.
   A decision's trust level is `judged`. It is at most tier-1 evidence and never replaces the tier 2–3 interdiff approval.
5. **The host builds and tests the chosen result.** It then becomes a new proposal with `rebaseOf` and goes through its gates (§2).

**Triage stays structural.** Whether a predicted conflict is real (overlap tier 3) is decided from structural signals, not by the model.

**Prototype findings.** The prototype was run on HAN's own conflicts:
- #99/#100 `sandbox/mod.rs`: mergiraf failed, `line_split` was correct, and the model chose it with 0.96 confidence;
- `agent_start.rs`: mergiraf was correct;
- `bindings.rs`: mergiraf was wrong, and the model said `needs_human` at 0.35;
- the model called every case "real" in triage, including the benign ones.
A call cost about $0.0001. Confidence thresholds come from an evaluation set built from HAN's resolved-conflict history. That set starts with `conformance/merge-cases/` (#118): the three cases above, each with base, ours, theirs, the accepted resolution, and the git, diff3 and mergiraf outputs.

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

It keeps a workspace with uncommitted changes, or one whose tip has commits that the target lacks by patch id after the proposal's `to`. For that kept workspace it reports an **`unlanded`** item `{workspace, proposal, commits: [{id, subject}]}`, which is an event for the agent and the operator, not housekeeping. A sweep catches what a crashed host left behind: a host that starts re-scans the session directories that have a land ledger and no running host, follows their proposals through the same log and prunes by the same rules (the session is over, so a rejected change no longer keeps the workspace).

A session that ends with work nobody proposed has it proposed by the host as a `vcs.land`, titled from the task (`autoPropose`, on with the flow): uncommitted changes are first committed as the agent (what `.gitignore` ignores is never added), unless the last proposal was rejected or cancelled, which is left for the agent's `rebaseOf`. A proposal the lane rejects for `test` or `conflict` gets the agent a new, host-originated turn (once per proposal, below an attempts cap) holding the failing checks' trimmed logs or the conflict record as data, and the instruction to propose again with `kind: "land"` and `rebaseOf`. A steer to a session whose workspace was pruned is refused as data (`steer_refused`), not run in a removed directory. Reference: the stopgap `prune-worktrees.sh` outside the repo found 37 merged branches and the lost #81 commit on its first run.

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
- A change that passes `fast` and breaks `full` lands, is reverted by the lane with the failing run as evidence, and its agent gets a fix-forward task. A change is never `confirmed` before a green `full` run that contains it.
- Two independent changes (disjoint conflict sets) are tested in parallel and both land without retesting. Two changes in one conflict set are tested in order, the second on the first's landed commit.
- A mergiraf result that compiles to an error is never chosen: `verified` stays `false` and the record goes to the model or a person (#92/#93 `bindings.rs`).

## Open

- The action's name (`vcs.land`) and whether `git_push` keeps a landing mode.
- Defaults for B, S and the deadlines: measure them on the dogfood board.
- Whether overlap data should feed task assignment (PLAN.md §12 open items).
- How a non-git backend (Sapling, Scion) satisfies `merge_simulate`'s record and the path-disjoint check. Scion's server-side `Rebase` already returns conflicts in this form.
