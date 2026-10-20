# HAN beta readiness rubric

Agreed direction, 2026-10-20: small technical teams; TUI first; extremely easy
setup; a simple **Trusted team** preset with automatic landing after checks pass
for selected repositories; advanced setup for fine-grained permissions.

This measures the usable beta product. It does not replace or revise the fixed
[design rubric](../spec/decisions/grading-rubric.md), and a passing beta score does
not assert that the three final [v1 goals](../spec/goal.md) have been achieved.

## Beta scope

A small invited team can run real agent work across machines and at least two
harnesses, watch and steer it from the TUI, and get changes through checks to a
landed result without an author repairing state behind the scenes. GUI completion
is not a beta prerequisite. Public network scale and an independent third-party
implementation remain v1 goals, not facts established by a beta trial.

The simple preset grants broad development permissions up front for repositories
the user selects. Routine changes land automatically after the configured checks
pass. Advanced setup exposes narrower policies, guarded agents, approval tiers,
path owners, budgets and network restrictions. The interface must explain the
actual grant and its scope; broad trust must not be described as strong isolation.
An agent cannot widen that grant itself.

## Weighted criteria

Score each area 0–100, then sum `score * weight / 100`.

| ID | Area | Weight | What a strong beta demonstrates |
|---|---|---:|---|
| B1 | First-run experience | 25 | One guided flow installs, signs in, creates or joins a team, selects repositories and starts work. No hand-edited configuration, manual key copying or scattered command recipes. A fresh user starts their first useful agent task within 10 minutes and a teammate joins within 5, excluding external account provisioning and machine availability. The test continues through a real landed change. Errors give an actionable recovery path. |
| B2 | Team workflow | 20 | People can discover work, watch live sessions, steer, coordinate overlaps, identify blockers, inspect results and navigate tasks/sessions/spaces in the TUI. Two people on separate devices can understand who is doing what without inspecting databases or service logs. |
| B3 | Completion and recovery | 20 | Checks bind the change that lands. Work lands or reports a clear failure. Restarts, interrupted turns, lost replies, conflicts, failed checks, stale sessions and quota exhaustion preserve work and leave an actionable state. Recovery does not require private scripts, raw database edits or silent retries of uncertain effects. |
| B4 | Permissions and trust | 15 | The selected preset is understandable and enforced. Ordinary changes auto-land only within the repositories and authority granted at setup. Advanced restrictions work when selected. Agent configuration, credentials, revocation and executor boundaries are checked, and the product accurately reports enforced versus advisory isolation. |
| B5 | Adapter independence | 10 | At least two real harnesses complete task → work → proposal → checks → landing. Setup, login, streaming, steering, tool delivery, errors, usage reporting and restart behaviour use a documented adapter contract. A third-party adapter does not require provider conditionals throughout the core. Unsupported capabilities and unmeasured costs are stated explicitly. |
| B6 | Specification and test evidence | 10 | Implemented beta behaviour is documented accurately, normative rules and types agree, conformance and relevant regression tests pass, and the reviewer can reproduce the evidence. Every dogfood defect has a regression test or a specific retained reproduction. Missing, skipped, stale and simulated evidence is labelled. |

The onboarding time targets are acceptance targets, not measured results. Record
hardware, installation prerequisites, elapsed times, assistance and failures.
Do not exclude ordinary HAN setup steps from the clock to make the target pass.

## Scoring anchors

- **0:** absent or contradicted by the implementation.
- **25:** specified or prototyped; no dependable workflow.
- **50:** substantial implementation with important manual steps or untested paths.
- **75:** the principal workflow is demonstrated, with material usability,
  recovery or evidence gaps remaining.
- **90:** the beta scope is independently reproducible and operationally usable;
  remaining gaps are minor and disclosed.
- **95–100:** repeated independent use and adversarial checks support the claims,
  including failure paths; no known material gap in the assessed scope.

Give credit for demonstrated behaviour, not code volume, claimed completion or
the existence of tests. A fake harness proves plumbing, not real adapter
compatibility. A test that skips itself is not a passing live test. Historical
evidence may be cited, but its revision and the changes since must be explicit.

## Release blockers

Any demonstrated issue below blocks beta regardless of the weighted total:

1. An action can execute outside the user's chosen authority, including an agent
   widening its own grant or bypassing a required approval.
2. Unfinished user work is silently lost during a supported operation or recovery.
3. Credentials cross a promised trust boundary or appear in shared output.
4. The product reports a successful outcome without the evidence needed to support
   it (including treating a failed model turn, failed checks or an unlanded proposal
   as a completed task).
5. An uncertain effect is silently relaunched in violation of the protocol's
   at-most-once rule.

The completion target is **strictly greater than 90/100 with no release blockers**.
An unavailable external account, human presence check or mandatory observation
period is not satisfied by waiting a few minutes or substituting a mock.

## Independent review procedure

After a significant feature landing or demonstrated breakthrough:

1. Record the Git revision, any uncommitted diff and the actual deployed version.
2. Give an independent reviewer this rubric, the beta goal and retained evidence.
   The reviewer reads the implementation and runs relevant checks; authors' claims
   are leads, not proof. Do not ask the reviewer to achieve a target score.
3. Report B1–B6 scores, the weighted total, release blockers, evidence references,
   limitations, and concrete findings ranked by their effect on beta users.
4. Fix the highest-impact findings, retain regression evidence, and repeat.

Keep previous reviews. Do not lower criteria, remove failures or change weights
between rounds to reach the target. Scope changes require a recorded user decision.
