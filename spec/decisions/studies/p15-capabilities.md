# Study: P-15 as rooted capabilities

Status: paper study, round 17 baseline. Nothing here is normative. Compares against architecture.md §6.6 (P-15), R-24, §7.5, §8.4 and the attacks in HISTORY.md rounds 8–17.

## 1. The design

**In one paragraph.** An agent has no authority of its own. Everything it does in a space runs on a capability rooted in a human, and only two humans can be roots: its **operator**, whose standing capability is the confirmation it already signs in the agent's profile (§4.3), and the approvers named by the space's approval rules (P-08). The operator signs one per-space **grant** that says what the root capability extends to. `auto` extends it to anyone who may instruct the agent. `guarded` extends it only to the agent and the operator. Under `guarded`, an instruction from anyone else is **foreign**: it carries its author's authority, not the operator's, and the agent stays **unrooted** until the operator vouches for everything up to a `seen`. An unrooted agent may talk, report and propose, and each proposal needs the operator's own approval. It may not pass authority on: no tasks, approvals, steering others, or governance. Policy works out rootedness from the log. **The agent never cites the basis it acts on**, because a cited basis can't be checked (§1.3). One rule ties it together: *anyone may attenuate; only the root amplifies.*

### 1.1 Entries

| Change | What |
|---|---|
| **added** kind `agent.grant` | body `{agent, steers: "auto" \| "guarded"}`; no content. The header's existing `seen` is the **vouch frontier**. |
| **removed** `session.open.steerMode` | sessions carry no mode; they are only watch/read/steer access |
| **removed** `session.input.release` | releasing is now `agent.grant` with a newer `seen` |
| **changed** `policy.sessions.modes` → `policy.steers: "auto" \| "guarded"` | the owners' ceiling: `guarded` forces every agent guarded; `auto` leaves the choice to each operator |
| unchanged | `refs`/`seen` on every entry stay audit data (R-17). Citations may *narrow* (a proposal naming a task charges that task), but they never grant authority. |

There are 21 kinds today; this design has 22.

### 1.2 State and derived predicates

```
agents: { [X]: { steers: SteerMode, foreign: Seq, vouched: Seq } }   -- replaces held, cleared, outside, guarded
effects[p].roots?: ActorId[]                                         -- replaces guards (operators whose own approval it needs)
sessions[s]: drops steerMode and guard
mode(X)      = guarded if agents[X].steers = guarded or policy.steers = guarded, else auto   (default steers: auto)
foreignTo(e,X) = e is an instruction to X (session.input into X's session, task naming X, effect.propose naming X executor)
                 ∧ e.author ∉ {X, op(X)} ∧ mode(X) = guarded at e's seq
rooted(X)    = X is human, or agents[X].foreign ≤ agents[X].vouched
```

### 1.3 Checks by kind

- **`agent.grant {agent: X, steers}`.** If the author is `op(X)`, it must be X's confirmed operator and hold a role; either value is allowed, and `vouched := max(vouched, seen)`. If the author is X, only `guarded` is allowed (attenuation) and nothing is vouched. Anyone else is denied.
- **Any instruction `e` to X.** If `foreignTo(e, X)`, then `foreign[X] := seq(e)`.
- **Tightening** (`agent.grant` guarded, or a `policy.set` to `steers: guarded`) at seq `t`. For each agent X whose mode turns guarded, if X has open outside work (an open task naming X, or an unfinished effect naming X as executor, authored by anyone outside {X, op(X)}), then `foreign[X] := t`.
- **Authored by an unrooted X.** Only `message`, its own `session.*`, `task.update`, `effect.outcome/resolve`, `effect.claim`, `effect.cancel`, `task.cancel` and `effect.propose` are allowed. Everything else is denied. This is today's held list, unchanged.
- **`effect.propose` (p).** `roots := {op(proposer) if the proposer is an unrooted agent} ∪ {op(executor) if foreignTo(p, executor)}`. If `roots` isn't empty, the phase starts as `awaiting-approval`.
- **`effect.claim` (E3′).** Take the recorded `roots`, plus `op(P)` for each agent party P (proposer or executor) that is unrooted now, plus `op(executor)` if the proposer is outside {executor, op(executor)} and the executor's mode is guarded now. Each of these needs its own unconsumed, unexpired approval, signed with a key valid at claim, by a root who holds a role. Nobody stands in for a root. Root approvals don't count against the 16-approval cap. Approval rules still apply on top.
- **`session.input`.** Needs the steer right on an open session whose actor holds a role. `op(X)` has every right on X's sessions while it holds a role.

**Invariant (one sentence).** *An effect runs only if every agent party to it is rooted in its operator, through the operator's grant or its own approval, and the operator's grant never covers a foreign instruction it hasn't seen.*

### 1.4 The crux: who chooses the basis?

The tempting version lets every proposal carry `basis: EntryId[]`, the instructions it acts on, and has Policy check that the basis is rooted. That design is **unenforceable**. Policy can't see what a model read, so an agent steered by Eve can cite the operator's grant. This is the confused deputy, and round 8 already lost to it, because that round's guard depended on the agent tagging its own proposals. An honest host can't fix it either. Eve's text stays in the transcript and the workspace (a TODO file, a memory note), so it shapes later turns that the host would attribute "honestly" to the operator. Influence doesn't end with a turn. It ends only when a human reviews it.

So authority can't come from cited bases. It comes from a frontier that Policy computes from the log, and only the root can move that frontier. Cited bases keep one sound use: **narrowing**. Lying about a basis can only lose the agent authority, so Policy can trust it.

## 2. Concept count and interface

| Today's P-15 | Here |
|---|---|
| sticky guarded choice (agent's, ended by re-closing a session) | the operator's grant (one signed field) |
| owner-forced guarding through modes | `policy.steers` ceiling |
| effective mode per session, plus live/dead sessions | gone (sessions have no mode, so they are just open or closed) |
| holds (`held`), plus settling after every entry | derived: `rooted(X)`; settling happens only at the two tightening kinds |
| release with `seen`, stale-release denial | the grant's `seen`; a stale vouch simply doesn't cover |
| `cleared`/`outside` | `vouched`/`foreign` (the same two numbers, which are now the whole hold) |
| recorded guards plus derived guards | `roots`, recorded plus at claim (one rule, symmetric over both parties) |

That is 8 interacting concepts today against 4 here: **grant**, **foreign/vouched frontier → rooted**, **unrooted restrictions**, **effect roots**. The rule "attenuate freely, amplify only by the root" explains why the owners and the agent can tighten but not loosen, so it doesn't count as a separate concept.

han.ts sketch:

```ts
"agent.grant": { agent: ActorId; steers: SteerMode };      // + header seen = vouch frontier
"session.open": { task?: EntryId };                         // steerMode removed
"session.input": { session: EntryId };                      // release removed
PolicyDoc.sessions: { watch; read; steer };  PolicyDoc.steers: SteerMode;
SpaceState.agents: { [a: ActorId]: { steers: SteerMode; foreign: Seq; vouched: Seq } };
effects[p].roots?: ActorId[];  claims[i].rootApprovals?: EntryId[];
// remove: held, cleared, outside, guarded, sessions[s].steerMode/guard
Policy.rooted(state, agent): boolean;  Policy.mode(state, agent): SteerMode;   // replaces guardedNow
```

## 3. Every attack from rounds 8–17

| Round | Attack (from the review) | Here |
|---|---|---|
| 8→9 | The guard relied on the agent tagging its own proposals, so it could be dropped or laundered | **Closed.** Rootedness is computed, never cited (§1.4). |
| 9→10 | Taint laundered through tasks the assignee didn't name | **Closed.** Rootedness is per agent, not per task. |
| 9→10 | Agents without a guard | **Closed.** With no confirmed operator in a role, nobody can vouch or approve as root. Under a forced `guarded`, outside work leaves the agent unrooted and nothing of it runs. |
| 9→10 | Approvals by tainted agents | **Closed.** An unrooted agent can't approve. |
| 9→10 | Accidental clears | **Closed.** Only `op(X)`'s grant moves `vouched`. |
| 10→11 | Owners loosening open sessions | **Closed.** Owners only set a ceiling, which never overrides the operator's `guarded`. Loosening never lowers `foreign`. |
| 10→11 | Guard rights without a role | **Closed.** Vouching, root approval and session rights all need `op(X)` to hold a role. |
| 10→11 | Held agents can't cancel their own delegations | **Kept.** `task.cancel` is allowed. |
| 11→12 | Tightening to `suggest` loosened Policy | **Gone.** There is no `suggest`. |
| 11→12 | An auto side-session as a side door | **Closed.** Sessions have no mode, so authority is per agent. |
| 11→12 | Orphaned guarded state | **Same as today.** Effects that need roots wait for the operator, and owners may cancel. |
| 11→12 | Expired approvals | **Unchanged** (P-08). |
| 12→13 | An auto session opened before the operator confirmed | **Closed.** The mode isn't a session property. A later guarded grant applies the tightening rule to open outside work. |
| 12→13 | A held agent as executor | **Closed.** The unrooted executor party needs `op` at claim. |
| 12→13 | An owner evicts the guard and approves as stand-in | **Closed.** Nobody stands in, and an evicted operator can't be replaced by anyone. |
| 12→13 | Tasks as an instruction path | **Closed.** A task is an instruction, so `foreignTo` applies. |
| 13→14 | Un-guarding from outside (closing the guarded session, evicting the guard, toggling modes) | **Closed by construction.** Only the root's signature can loosen. |
| 13→14 | Naming the agent as executor is an unheld instruction | **Closed.** It counts as an instruction and is recorded in `roots`. |
| 13→14 | Returned approvals stay superseded | **Unchanged** (P-08). |
| 14→15 | Becoming guarded with outside instructions still open | **Closed** at both tightening kinds, which are the only routes left (a guard regaining its role no longer changes the mode). |
| 14→15 | A held proposer's older proposals | **Closed.** The claim adds the operator of any agent party that is unrooted now. |
| 14→15 | The agent can't undo its own choice | **Changed.** Loosening is now the operator's alone. The agent can tighten but not loosen (§6). |
| 15→16 | "Becoming guarded holds" worked only through the session route | **Closed.** One tightening rule covers both kinds. |
| 15→16 | An effect aimed at a held executor lost its guard on release | **Closed.** `roots` is recorded at propose, and a vouch doesn't approve effects. |
| 16→17 | Stale releases | **Closed.** `rooted` needs `foreign ≤ vouched`, so a vouch whose `seen` is older than the latest foreign instruction changes nothing. |
| 16→17 | Agents drawing on the space budget; E6 at claim | **Unchanged** (P-06). |
Most of the guard model's 27 mutations map onto the checks above. The five about session modes and liveness no longer apply. Nothing here has been model-checked.

## 4. Host and executor, and §7.5

Because the frontier is computed and not cited, **no authority moves to the host**. The host's duties shrink in wording, not in substance. R-24 becomes: run a turn in the stricter mode if `rooted(X)` is false, or if the turn is driven by an entry with `foreignTo` true. Treat streams, context, messages and outcomes as data. Run turns that read another session's context as if they were guarded, and treat this as a MUST while `mode(X) = guarded`. Never carry an instruction across spaces. The executor doesn't change: E3′ is Policy's job.

§7.5 rows. The prompt-injection, cross-space, stricter-mode and finished-history rows stay as they are, and nothing new is added to what the host is trusted with. One row is renamed: *an operator vouches without reading* is not prevented, the same as today's release, and the vouch is signed and attributable. The cited-basis variant would have needed a new row, *the host cites a false basis*, trusted. That is why it is rejected.

## 5. User requirements

- **Auto for my own turns, stricter for others'.** The operator's own turns are always rooted, and the harness keeps its auto mode. With `agent.grant {steers: guarded}`, an outside steer runs in the stricter mode (§8.4 is unchanged), and every effect it leads to waits for the operator. With `steers: auto`, the operator lets outside steers run under its authority. This is the user's two options, written as one signed choice.
- **Trust set per space by owners.** `policy.steers` sets the floor: a small team uses `auto`, a large network uses `guarded`. Watch, read and steer stay as today's `policy.sessions`.
- **Full agent-to-agent work.** Reading context, messages and subscriptions are data, so they need no authority. Requesting work is a `task`, which is an instruction and goes through `foreignTo`. A rooted agent can delegate. Its task carries its own authority, and to a guarded assignee it is foreign unless the delegating agent is the assignee's operator.
- **Rounds 8–17.** See §3.

## 6. Risks, open problems, verdict

**Risks and open problems**
1. **This is not fine-grained capability security.** The capability is coarse: one per agent per space, which is the same coarseness as today's hold. True per-instruction capabilities need cited bases, and §1.4 shows those can't be enforced. The gain here is a smaller vocabulary, not finer authority.
2. **Usability shift.** The agent can't loosen itself, so switching back to `auto` needs the operator's key. That is one signature per space, but it is a change from round 15's "an agent can always undo its own choice".
3. **Settling isn't fully gone.** The tightening rule still scans for open outside work. It runs at two kinds instead of after every entry.
4. **Orphaned guarded agents.** Steers into them are now accepted, and they just leave the agent unrooted. Today they are refused. Safety is the same, but the vectors change.
5. **Persistence.** `agents[X]` must survive X leaving and re-joining. Otherwise re-joining would clear `foreign`.
6. **Unverified.** `model_guard.py` needs a new ghost built from the §1.3 invariant. "At least as safe" is argued, not checked.

**Verdict.** It is **simpler**: 8 concepts become 4, four state maps become one, sessions lose their mode and their dead/live state, and two body fields go while one kind is added. The single "only the root amplifies" principle replaces four ad-hoc anti-loosening rules. On paper it is **as safe** against every listed attack, with one deliberate change (the agent can't self-loosen), and it is safer by construction on the "un-guard from outside" class. It is **not more expressive or finer-grained** than holds, and the honest answer to the crux is that no capability scheme makes Policy enforce *which* instruction a model acted on.

**Self-score.** The simplicity gain is real but moderate. I would expect C2 to move from about 68 to about 74–77. C4 should hold if the rebuilt guard model passes. It could slip by 2–3 until then, because a re-architecture this close to "before building" invites fresh review findings. Overall, for this direction: **7/10. Adopt the vocabulary and the derived hold; reject the cited basis.**
