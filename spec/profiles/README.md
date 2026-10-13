# Profiles

Normative application profiles for interoperability between HAN apps. A profile carries **no authority**: Policy never reads it, and nothing that matters (an approval, a steer, a claim) depends on it (architecture.md, "Tiers"). Profiles evolve faster than the core and are versioned by name.

| Profile | Since | What |
|---|---|---|
| [`han.status/1`](han.status-1.md) | round 30 | where a live session is: state, step, files, tests, cost |
| [`han.timeline/1`](han.timeline-1.md) | draft | work timelines: op ids, versions, timelines and merges, registers, maps, text, seals, worktree mirroring, signed envelopes, sync ([vectors](../../conformance/timeline-vectors/README.md)) |

Planned: the team tools (`team.*`). See [PLAN.md](../../PLAN.md) §4 and §5.
