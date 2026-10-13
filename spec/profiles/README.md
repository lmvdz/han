# Profiles

Normative application profiles for interoperability between HAN apps. A profile carries **no authority**: Policy never reads it, and nothing that matters (an approval, a steer, a claim) depends on it (architecture.md, "Tiers"). Profiles evolve faster than the core and are versioned by name (`<name>/<version>`). A profile's live data travels as `profile` stream items `{name, body}` under the watch right (G-05), whose body the server never validates (faces do); its bulk data, if any, travels on a **typed stream** of the type it names (G-07): the core frames it and nothing more, so the profile defines its frames, signs them with its own envelope (domain-separated, with its own replay context) and says who may send and receive them.

| Profile | Since | What |
|---|---|---|
| [`han.status/1`](han.status-1.md) | round 30 | where a live session is: state, step, files, tests, cost |
| [`han.timeline/1`](han.timeline-1.md) | draft | work timelines: op ids, versions, timelines and merges, registers, maps, text, seals, worktree mirroring, signed envelopes, sync ([vectors](../../conformance/timeline-vectors/README.md)) |

Planned: the team tools (`team.*`). See [PLAN.md](../../PLAN.md) §4 and §5.

## Stream types

A stream type is named like a profile (`<prefix>.<name…>/<version>`, at most 64 bytes). Registering one here is how a profile claims it; a receiver that doesn't know a type stops reading the stream.

| Stream type | Profile | Frames | Signed by |
|---|---|---|---|
| `han.timeline/1` | [`han.timeline/1`](han.timeline-1.md) | TL-110 sync messages (hello, answer), one per frame, whose envelopes are TL-93's | each batch's TL-93 envelope; who may sync: TL-113 |
