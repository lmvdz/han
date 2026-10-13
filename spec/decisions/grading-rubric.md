# HAN v1 design — grading rubric (fixed across rounds)

Grade the design in this repository (goal.md, architecture.md, han.ts, coverage.md, plus any other files listed in architecture.md). The prior design (v0.4, not included in this repository) was the reference for C6.

Score each criterion 0–100, then the weighted total. Be a strict, adversarial reviewer: a 95 means "a stranger could implement this, there are no known contradictions, and I tried hard to break it and couldn't". Do not reward length or effort.

| # | Criterion | Weight | What 95+ looks like |
|---|---|---|---|
| C1 | **Goal fit** | 15 | Serves goal.md: open standard, public federated network, human⇄human/human⇄agent/agent⇄agent communication-first, agents acting in the world, the three v1 tests |
| C2 | **Simple interfaces** | 15 | Few calls, few parameters, general-purpose, hard to misuse; no special cases leaking into callers |
| C3 | **Deep modules** | 15 | Each module hides substantial complexity behind its interface; no pass-through or shallow modules; information hiding with no leakage |
| C4 | **Correctness & safety** | 20 | Rules are mutually consistent; failure paths (crash, lost reply, revocation race, concurrent claims, home death, equivocation, partitions) are handled; no rule contradicts another or the types |
| C5 | **Precision / implementability** | 15 | A stranger can build a conforming server from the docs alone: exact encodings, algorithms, state transitions, limits, error behavior; prose and han.ts agree |
| C6 | **Coverage of v0.4 guarantees** | 10 | Every v0.4 invariant/method/command/port is preserved or deliberately dropped with reason, and the mapping is actually true |
| C7 | **Verifiability** | 10 | Claims are checkable: formal model and/or conformance vectors exist or are concretely specified; any provided checks actually run and pass |

## Required output (exactly this JSON, then nothing else)

```json
{
  "scores": {"C1": 0, "C2": 0, "C3": 0, "C4": 0, "C5": 0, "C6": 0, "C7": 0},
  "weighted_total": 0.0,
  "defects": [
    {"id": "D1", "severity": "critical|major|minor", "criterion": "C4", "where": "architecture.md §6.3 / han.ts:LINE", "problem": "...", "fix": "..."}
  ],
  "strongest_points": ["..."]
}
```

List every defect you find, most severe first. Verify claims by reading files and, where possible, running things (e.g. `npx -y -p typescript@5.8.3 tsc -p tsconfig.json`, any scripts the design ships). Do not modify any files.
