# HAN — goal

**HAN is an open standard that makes it safe for AI agents to act in the world from shared conversations. In any room where people and agents work together, every real-world action (a push, a payment, a deploy, an email) is authorized once, executed at most once, accounted against a budget, and leaves a record that anyone in the room can verify.**

Conversations already have good open networks; Matrix is the first target. HAN doesn't replace them. It adds what they lack for agents:

| A chat network gives you | HAN adds |
|---|---|
| rooms, people, bots, messages, federation | **one answer to "was this action authorized?"**: a per-room sequencer orders every agent action into a signed log |
| eventually-consistent permissions | **at most one launch per approved action**, and an honest `unknown` after a crash |
| bots that are just users | **accountable agents**: every agent has a human operator who vouches for it, and can withdraw that |
| no notion of money | **budgets** reserved before an action and settled after it, delegated down task trees |
| coarse power levels | **approvals bound to the exact plan**, with expiry and separation of duties |
| complex state resolution | **pure, replayable policy**: any participant can re-check every decision, and a cheating sequencer leaves a portable proof |

## v1 is done when all three hold

1. **Used daily.** The author runs their own agent work through HAN, in Matrix rooms, through a working sequencer and executor.
2. **Airtight.** The safety core is consistent, including its failure paths. That means the effect protocol passes exhaustive bounded model checking against the reference Policy, and every rule has a conformance vector, a scenario, a model property, or a written implementation-test procedure ([ref/](ref/)).
3. **Built by strangers.** Someone implements a conforming sequencer or executor from these documents alone, without asking the authors.

## Design principles (in priority order)

1. **Don't rebuild chat.** HAN uses the transport for talking, identity, presence and files, and specifies only what agent safety needs.
2. **One primitive.** Every authority-bearing fact is a signed entry in a room's sequenced log. New features are new entry kinds, not new APIs.
3. **Simple interfaces, deep modules.** A module earns its place by hiding a lot behind a few calls. Pass-through layers are a defect.
4. **Honest outcomes.** `unknown` is a first-class result. Nothing claims more authority, durability or success than it can show.
5. **No vendor in the core.** The standard names behaviours. Transport bindings (Matrix first) and reference products are separate.

## What HAN deliberately is not (v1)

- Not a chat network or a federation protocol: those are the transport's job.
- Not a consensus system: each room has one sequencer at a time. A cheating sequencer is *detectable* (signed receipts), not *prevented*. If a sequencer dies, its log stays verifiable and work continues in a fork, without inherited authority.
- Not an agent framework or a model API: how an agent thinks is its host's business.
- Not exactly-once in the world: HAN guarantees *at most one launch per claim*, no second attempt after an uncertain one unless someone explicitly records that it failed (or the proposer declared the action `repeatable`, i.e. harmless to repeat), and an honest outcome. It can't make the outside world cooperate.
