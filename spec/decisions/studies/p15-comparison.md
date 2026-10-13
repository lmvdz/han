# Study: guarded steering (P-15), three designs compared

Status: independent adversarial review, written against round 17. Nothing in this file is normative. I wrote none of the three designs.

- **A**: today's P-15 (architecture.md §6.6, R-24, §7.5, §8.4; ref/policy.py `guarded_now`, `settle_holds`, `guards_of`).
- **B**: [p15-capabilities.md](p15-capabilities.md). Operator grant, derived rootedness, enforced by Policy.
- **C**: [p15-host-contract.md](p15-host-contract.md). A stateless claim check (E7) on a host-set `outside` tag, plus a verifier audit (V-1).

**Recommendation in one line:** adopt **B with four amendments (B′)**, listed in §6. Reject C. If there is no time to re-verify before building, keep A but patch the two holes shown in §2.1 (A1 and A2). Those patches turn A into most of B anyway.

---

## 1. Concepts, state and invariant

| | A (today) | B | C |
|---|---|---|---|
| **Policy implementer must understand** | steer modes and their caution order; effective mode; live/dead sessions; the session's guard; sticky `guarded[X]` ended by the agent re-closing a session; `guardedNow` (chosen or owner-forced); instruction (3 paths); hold on steer; `outside`/`cleared`; `settle_holds` after **every** entry (records guards and rearms ready → awaiting-approval); release with `seen` and stale denial; held-kind list; recorded + derived guards; guard approvals (cap-exempt, no stand-in, departed guard). **≈14** | grant (mode + vouch through `seen`); `policy.steers` floor; `mode(X)`; `foreignTo`; the tightening scan (only at the 2 tightening kinds); `rooted = foreign ≤ vouched`; unrooted-kind list; effect roots (recorded at propose + 3 clauses at claim); root approvals (cap-exempt, no stand-in); operator's session rights. **≈9** (B claims 4; that count folds most of these into "grant" and "roots") | session rights; `requireGuarded`; mode read from the pinned profile; E7 (2 clauses); operator approvals (cap-exempt); operator-only `policy.set` in the profile space; the `outside` field. **≈6** |
| **Host implementer must understand** | R-24: effective mode re-read at receipt; `guardedNow`; held ⇒ every turn stricter; outside instruction ⇒ stricter turn; data is never instruction; §8.4 sandbox. **≈6** | stricter turn if unrooted or the turn is driven by a `foreignTo` entry; data rule; sandbox. **≈4** | H-1 taint (instruction set, operator exclusion, reads); H-2 tag every proposal, **at least as coarse as V-1, i.e. the whole per-space hold predicate**; H-3 release UI with `seen`; H-4 forbidden-kind list; H-5 stricter mode; X-11 for executors; plus V-1 for verifier implementers. **≈9 + V-1** |
| **Entry fields / kinds** | `session.open.steerMode`, `session.input.release`, `policy.sessions.modes`; 21 kinds | + `agent.grant {agent, steers}` (header `seen` = vouch); − `steerMode`, − `release`; `policy.steers`; 22 kinds | + `effect.propose.outside` (required); `session.input.release` kept but ignored by Policy; profile `policy.set {steer}`; `policy.sessions.requireGuarded`; `ProfileView.steer` |
| **Policy state** | `held`, `guarded`, `cleared`, `outside`, `sessions[].steerMode/guard/human`, `effects[].guards`, `claims[].guardApprovals` | `agents[X] = {steers, foreign, vouched}`, `effects[].roots`, `claims[].rootApprovals` | `claims[].operatorApprovals` |
| **Invariant (one sentence)** | While X is guarded, any instruction to X from anyone but its guard g holds X; while held, nothing X proposes or is named executor of runs without g's own approval; only g ends a hold. | An effect runs only if every agent party to it is rooted in its operator, through the operator's vouch or its own approval, and a vouch never covers a foreign instruction newer than its `seen`. | Policy: an outside-tagged effect of a guarded agent, or one an outsider aims at a guarded executor, needs the operator's approval. Safety: an outside instruction to a guarded agent never reaches the world without its operator. |
| **Can Policy alone check it?** | **Yes.** model_guard, depth 5 | **Yes.** It needs a new model; none exists yet | **The E7 sentence, yes. The safety sentence, no.** Policy trusts the proposer's self-declared `outside`. Only V-1 checks it, and only after the fact |

C has the smallest Policy, but it does not remove the hold predicate. H-2 requires every host to tag at least as coarsely as V-1, and V-1 *is* the old hold predicate ("this is the old hold predicate, but it lives in an audit", C §2). So the predicate is now implemented in every host and every verifier, and it no longer gates anything. For the system as a whole, the concept count goes up.

---

## 2. Attacks the authors did not list

Each attack names the actors and the order of entries. "Reproduced" means I ran it against ref/policy.py (script in my scratchpad; it reuses `model_guard.py`'s `init`/`apply`).

### 2.1 Against A

- **A1. Owners can't force guarding on an agent that has no session.** Reproduced: BOT2 closes its session; O sets `modes: [guarded]`; P tasks BOT2; BOT2 proposes. Result: BOT2 is not held, and the proposal is `ready` and claimable without OP. §6.6 mentions this only in a parenthetical ("an agent with no session is guarded only by its own choice"). It contradicts requirement (2) and goal.md's "owners require it". Most agent work arrives as tasks, not sessions, so owner forcing is mostly advisory.
- **A2. The agent's own key un-guards it, so a model injected through *data* can too.** Reproduced: OP releases BOT; BOT closes s1 (its only guarded session); P steers BOT's auto session s3. No hold follows, and BOT's next proposal is `ready`. Steers are gated, but data (a message, a watched stream, a file) is not. A prompt-injected model only has to get its host to close a session, and nothing in R-24 or §8.4 forbids a host from exposing `session.close` to the model. Round 15's "an agent can always unguard" is exactly the opening. B and C both make loosening operator-only, which closes it.
- **A3. A redacted steer is released unread.** Eve steers guarded X, then redacts her own `session.input` (P-12 lets the author redact). X's model already received the text. Then g releases with `seen` ≥ the steer, reviewing an empty record. The release is valid. B and C share this hole. Fix in every design: a client MUST show "redacted; the agent received it" and SHOULD refuse to release on that basis.
- **A4. The stale-release rule is untested.** `model_guard.py` sets `seen = head` on every release (line 147), so the round-17 rule ("a release covers only what g had seen") is checked only by vectors, never by exhaustive search. Its correctness also depends on the guard's client setting `seen` to what it displayed, not to the head, and A states that nowhere as a client MUST.
- **A5. The ghost is not independent.** model_guard's `settle()` transliterates Policy's `settle_holds` (the same open-task, unfinished-effect and `seq > cleared` predicate). "Derived from the invariant sentence" overstates it: ExactHolds checks Policy against a copy of itself. Its scope is also narrow: BOT2 never unguards; operator confirmation is fixed; no service executor has an operator; there is one space and no rejoin.

### 2.2 Against B

- **B1. Every grant vouches, so setting the mode silently releases.** §1.3: when the operator authors a grant, either value, `vouched := max(vouched, seen)`. Eve steers guarded X at seq 20. The operator's settings screen re-asserts `steers: guarded` with `seen = 20` (or simply the head). Eve's steer is now vouched unreviewed, and X's proposals run with no operator approval. A's explicit `release: true` doesn't have this hazard. Fix: vouching only with an explicit `vouch: true`, denied when stale (as today).
- **B2. `policy.steers: guarded` hits executors.** "Forces every agent guarded", and E3′ adds `op(executor)` whenever the proposer is an outsider and the executor's mode is guarded. In the §1.1 worked example, `@git:c.example` is a service with operator `@ops`. Under owner forcing, every push Ada's fixer proposes then needs `@ops`'s own approval, and an operator-less service executor can never run anything. A avoids this only by accident (executors don't open sessions; see A1). Fix: owner forcing applies to `actorKind: "agent"` only. Services are guarded only by their operator's own grant.
- **B3. The operator must hold a role to grant, but the agent may self-guard.** An agent in a public space whose operator isn't a member can grant itself `guarded` (attenuation). Its first foreign steer then makes it unrooted for good: nobody can vouch, and only the operator can loosen. This fails closed and owners can remove it, but it's a trap. Fix: a self-grant needs the operator to hold a role (as A requires for guarded sessions), or the vectors document the trap.
- **B4. Phase drift.** B drops A's rearm row, so an effect can sit in `ready` while E3′ will deny every claim (its proposer turned unrooted after it became ready). Executors and UIs then poll a `ready` effect that can't run. Choose one: keep the phase computed from rules plus *recorded* roots and say "ready means rule-ready", or keep a rearm. Either is fine, but the spec must say which.
- **B5. Gaps the authors half-listed.** P-01 isn't updated to pin the operators of agent parties (needed for E3′ key checks). The tightening scan must include agents that currently hold no role (open tasks outlive membership). `agents[X]` must persist across leave and rejoin (B risk 5).

**Checking B's §3 table.** All 26 rows hold **under the amendments above**. "10→11: owners only set a ceiling" contradicts §5 ("sets the floor"); it is a floor of caution. "14→15: the tightening kinds are the only routes left" holds: in B, mode depends only on `agent.grant` and `policy.set`. "16→17 stale releases closed" holds, but B1 lets an unintended *current* vouch through, which is worse in practice. The "8 → 4 concepts" claim is about 11 → 9 when counted the way A is counted (§1). It is a real reduction, and the removed parts (session modes, liveness, per-entry settling, self-unguard) are exactly the ones every review round attacked.

### 2.3 Against C

- **C1. Auto mode loses delegation (requirements 1 and 3).** H-1 taints whatever the mode, and H-4 and V-1 forbid `task`, `effect.approve` and steering others while tainted, again whatever the mode. A teammate tasks an `auto` agent, which then delegates a subtask: V-1 reports a provenance fault, and a conforming host must refuse. The "or they just allow auto from external steers" option becomes "auto, but you can't delegate anything a teammate asked for". Gating H-4 and V-1 on the mode brings back "becoming guarded with outside work open", this time in the verifier.
- **C2. The steering mode becomes a stale cross-space fact.** C's mode lives in the profile space and E7 reads it from the pinned profile. R-10 freshness (300 s) is trusted, not checked. A space sequencer that keeps pinning an older profile seq (monotonic, so verifiers accept it) keeps the agent in `auto` indefinitely, and replay can't tell. C's threat-model claim "malicious sequencer: equal, E7 is replayable" is **false** for this case. In A and B the mode is in the space's own log, so a sequencer that ignores it is caught by replay. It also cuts against §7.2 and R-05 (no order across spaces, no authority by cross-space reference). n2 states only the honest-sequencer half of this problem.
- **C3. No per-space choice for the operator.** The profile's `steer` is global. "Auto in my team space, guarded in the public one" can't be said, unless the owners of every public space set `requireGuarded`. Requirement (1) frames the choice as one the owner of the agent makes per context, and A and B both allow per-space.
- **C4. Operator-less actors and services are undefined.** The default is `guarded` and only the operator may change it. With `allowAgents: "any"`, an operator-less agent stays guarded forever, and E7 demands an approval from an operator who doesn't exist. Either its outside-requested work can never run, or implementers guess. Services (executors) hit the same thing as B2, with no owner to blame.
- **C5. The buggy host does the damage before the audit.** Policy ignores `release`, so anyone with steer right can append `session.input {release: true}`. A host that checks only the flag (an easy bug) clears its taint on Eve's say-so. Likewise a host that misses tasks as an instruction path (round 9's exact bug, now in host code) relays a task to an auto agent Y, which runs it. V-1 produces a signed proof afterwards, but the push has happened. goal.md promises "nothing it proposes or executes runs without that human", and C turns that into "or a proof that it did".
- **C6. A self-contradiction.** The round-9 row says "N/A: the host knows its own context" (per-context taint), but H-2 requires per-*space* coarseness, because influence persists in workspace files (B §1.4 makes the same point). So the per-context argument doesn't remove anything.

**Checking C's §3 and §4 claims.** "About 12 closed by Policy" is overstated. Every **C** row (8, 9 ×3, 12-tasks, 14-stricter, 16-stale) is host-only, so Policy prevents it only for correct hosts. "§7.5 prompt injection through reads: better" is true only for honest hosts, and H-2's cross-space tagging is a MAY, so "held agent acts in another space: better" is not normative. "Malicious host or stolen key: no difference" holds (A doesn't stop an attacker who *is* X either). "Malicious executor: equal" holds. "Malicious sequencer: equal" fails (C2). "Today's design couldn't guarantee owners can't flip the choice" is half true: since round 14 owners can't loosen A's sticky choice, but the agent can (A2).

---

## 3. What each design protects against

| Adversary | A | B (B′) | C | Evidence |
|---|---|---|---|---|
| Outside steerer (member, guest) | prevented via steers, tasks and executor slot, **but** not when owner-forced on a session-less agent (A1) | prevented on every path; owner forcing is per agent | prevented only if the agent's host tags correctly | A1 reproduced; B §1.3; C E7 trusts `outside` |
| Owners | can't loosen the agent's choice or stand in; can DoS by evicting the guard | same; loosening is the operator's alone | can't touch the mode; can DoS by evicting the operator | A §6.6; B §1.3; C §1 |
| Other agents (A2A tasks, executor naming) | prevented (Policy) | prevented (Policy) | host-dependent (H-1, H-4) | C5 |
| Prompt-injected model, via a steer | prevented | prevented | prevented (honest host) | |
| Prompt-injected model, via data | **can self-unguard** (A2); data → stricter turn is host-only | can't loosen (only the operator); data is host-only | host may count reads (better, honest host only) | A2 reproduced |
| Honest-but-buggy host (wrong provenance, correct sandbox) | prevented: Policy holds whatever the host computes | prevented | **detected afterwards only** (V-1) | C §4.6 admits it; C5 |
| Malicious host / stolen agent key | not prevented (it *is* X) | not prevented | not prevented | all three |
| Stolen operator key | not prevented (it can release and approve) | not prevented | not prevented | |
| Malicious executor | claim denied at the sequencer; replay catches | same | same (E7) | |
| Malicious sequencer | replay catches a guard-less claim; 300 s profile freshness trusted for keys | same | replay catches E7 **given** the pinned profile, but a stale `steer: auto` pin is undetectable | C2 |

---

## 4. Verifiability

- **A.** The strongest today: 30 P-15 vectors, Policy mutations, and model_guard (depth 4 re-run here: 407,619 transitions, six properties hold; 27/27 mutations). Weaknesses: the ghost copies Policy's settle rule (A5), stale releases are never explored (A4), and its scope is narrow (A5). R-24 and §8.4 need implementation tests (procedure written, ✗).
- **B.** Fully checkable by Policy, as A is, and the state is smaller (two integers per agent), so exhaustive search goes deeper for the same cost. **Nothing exists yet.** The trap: a ghost that computes `foreign ≤ vouched` just restates Policy. The ghost must keep explicit *sets* of unreviewed foreign instruction ids per agent and model review as "the operator's explicit vouch whose `seen` ≥ each id". Then rootedness is checked against intent. Host tests are as in A.
- **C.** E7 is stateless, so vectors and mutations cover it well and cheaply. But the safety property moves to host code, which is checked only by C's proposed oracle test against **one** host implementation (`witnesses/impl-py/`). That says nothing about a stranger's host, which is the v1 test "built by strangers". Conformance of other hosts can be observed only by V-1 after the fact. It is the weakest of the three where it matters.

---

## 5. Scores (C2, C3, C4; rubric-anchored)

I score the P-15 part as it would move each whole-design criterion. A's baselines are my estimate for round 17.

| | C2 Simple interfaces (15) | C3 Deep modules (15) | C4 Correctness & safety (20) | Other | Δ weighted vs A |
|---|---|---|---|---|---|
| **A** | 66: about 14 Policy concepts; per-session modes, liveness and a phase rearm leak into hosts and UIs | 72: Policy hides it all, but `settle_holds` after every entry is wide | 70: A1 and A2 are real holes; the guard model is strong but self-referential | | 0 |
| **B as written** | 71: fewer concepts and one kind, but the grant conflates mode and vouch (B1, easy to misuse) | 74: rootedness derived; no per-entry settling | 67: unverified; B1 and B2 are live | C7 −4 until a model exists | ≈ +0.5 |
| **B′ (amended, model-checked)** | 73 | 75 | 73: closes A1 and A2 and B1–B5 | C7 back to A's level | **≈ +1.6** (+0.9 C2, +0.45 C3, +0.6 C4) |
| **C** | 67: Policy is tiny, but a required self-declared safety boolean is the textbook misuse-prone interface, and the host contract grows | 64: the hold predicate leaks into every host and verifier (V-1); M6 is no longer "the host's business" | 60: prevention becomes detection for buggy hosts (C5); C1, C2 and C4 | C1 −2 (goal.md must weaken its promise); C7 −3 | **≈ −3.2** |

---

## 6. Recommendation: B′

B's core insight is right. Only the operator can loosen, the mode is a per-agent, per-space fact in the space's own log, and the hold becomes two integers that Policy compares. That removes exactly the surface that rounds 12–16 kept attacking: session modes, liveness, re-closing to unguard, and settling after every entry. C's one good idea (an honest host can treat reads as outside) can be kept *without* trusting the host for safety: let the agent attenuate itself into unrooted (amendment 3). Then an honest host gains caution, and a buggy one loses nothing that Policy enforces. Don't take C's E7, the `outside` tag, the profile-space mode or V-1.

### What to change (in order)

1. **han.ts, architecture.md §5.** Add the kind `agent.grant {agent, steers: "auto"|"guarded", vouch?: true}`. Remove `session.open.steerMode` and `session.input.release`. Replace `policy.sessions.modes` with `policy.steers: "auto"|"guarded"` (a floor of caution). `SpaceState`: replace `held`, `guarded`, `cleared`, `outside` and the session `steerMode`/`guard`/`human` fields with `agents[X] = {steers, foreign, vouched}`, `effects[].roots` and `claims[].rootApprovals`. Bump to 22 kinds; update check_consistency.
2. **§6.6 P-15, rewritten from B §1.3 with these amendments:**
   a. **Explicit vouch** (fixes B1). Only an operator grant with `vouch: true` moves `vouched := seen`. It is denied if `foreign > seen` (stale), as today. Without `vouch`, a grant changes only `steers`.
   b. **Forcing scope** (fixes B2). `policy.steers: guarded` forces `actorKind: "agent"` only. A service is guarded only by its own operator's grant.
   c. **Self-attenuation** (fixes B3; keeps C's idea). X may grant itself `guarded` only while its operator holds a role. X may also append `agent.grant {agent: X, steers: guarded}` to set `foreign[X] := seq` (a self-hold), which an honest host SHOULD do after feeding another session's context into a guarded agent's turn. Nothing X authors ever lowers `foreign` or raises `vouched`.
   d. **Bookkeeping** (fixes B4, B5). `agents[X]` persists across leave and rejoin. The tightening scan covers agents without a role. The phase is computed from rules plus recorded roots; E3′ checks the dynamic roots at claim, and §6.4 says so (delete the "ready → awaiting-approval" rearm row). P-01 pins the operators of the agent parties (at most two).
   Keep A's "nobody stands in", the root-approval cap exemption and supersession unchanged.
3. **policy.py.** Delete `guarded_now`, `settle_holds`, `effective_mode`, `live`, `hold_on_instruction`, `note_outside` and the release branch. Add `mode(X)`, `foreign_to(e, X)`, `rooted(X)`, `roots_at_claim`. Keep `held_may` as `unrooted_may`. `sessionAccess`: the session actor, and its operator while holding a role, get all three rights; anyone else gets them by role level. A session is simply open or closed.
4. **R-24 and §8.4.** The stricter turn applies when `rooted(X)` is false or the turn is driven by a `foreignTo` entry. Add: a host MUST NOT let the model author `agent.grant`. A guard's client MUST set a vouch's `seen` to the latest instruction it displayed, and MUST show redacted instructions as "redacted; the agent received it" (A3, all designs).
5. **goal.md, §7.5, §11, HISTORY.** "Only the agent's operator loosens, and only the operator's explicit vouch ends a hold." Change the §11 row. Remove "an agent can always unguard" (round 15) and state the trade-off: switching back to auto needs the operator's key, once per space.
6. **coverage.md and the §10 P-15 row.** Rename the vectors and rules.

### The minimum verification before adopting

1. **A new `model_guard.py` with an intent-level ghost.** Per agent: the *set* of unreviewed foreign instruction ids; the mode history from grants and `policy.steers` only; review = an explicit operator vouch with nondeterministic `seen` (not always the head). Properties: **RootedClaimsNeedOperator**, **UnrootedInstructNoOne**, **OnlyOperatorLoosens** (no entry by anyone but `op(X)` makes X less cautious or raises `vouched`), **ExactRootedness**, **ServicesNotForced**, **OneOpenApprovalPerAuthor**. Scope: two agents with different operators, a variant where both share one operator, an agent as executor, a service executor *with* an operator, one operator-less agent (`allowAgents: any`), multi-assignee tasks, the agent leaving and rejoining, operator removal and re-add, operator key revocation, `policy.steers` toggles, self-attenuation, stale vouches. Depth ≥ 5, non-vacuity guard.
2. **Mutations.** Port A's 27 where they still apply, and add: grant-vouches-implicitly (B1), forcing-hits-services (B2), rejoin-resets-foreign, agent-can-loosen, stale-vouch-accepted, tightening-skips-roleless-agents, roots-ignored-at-claim. Each must be caught by a property.
3. **Vectors.** Rewrite the 30 P-15 vectors, and add `vouch-is-explicit`, `stale-vouch-denied`, `services-not-forced-by-owners`, `rejoin-keeps-foreign`, `agent-cannot-loosen`, `self-hold`, `owner-forcing-covers-session-less-agents` (A1 as a regression), and `self-unguard-impossible` (A2 as a regression). Every vector and mutation passes; fuzz_fold and check_consistency pass; `run_all.py` is green.
4. **Hosts.** Keep the written R-24 procedure (✗ until there is an implementation), and add one step: a data-injected model can't produce `agent.grant` or `session.close` that loosens anything.

Fallback if step 1 can't be done before building: stay on A, and patch A1 (owner forcing applies per agent, not through sessions) and A2 (only the guard ends the sticky choice). Both are small Policy changes with a vector each. That converges on B′ without the vocabulary change.
