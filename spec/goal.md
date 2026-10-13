# HAN — goal

**HAN is an open standard that makes it safe for AI agents to act in the world from shared conversations. In any room where people and agents work together, every real-world action (a push, a payment, a deploy, an email) is authorized once, executed at most once, accounted against a budget, and leaves a record that anyone in the room can verify.**

Conversations already have good open networks, such as Matrix. HAN doesn't replace them: it runs beside them on its own small protocol (`han/1` over QUIC, dialled by key), and chat rooms can point to a HAN space. It adds what they lack for agents:

| A chat network gives you | HAN adds |
|---|---|
| rooms, people, bots, messages, federation | **one answer to "was this action authorized?"**: a per-space sequencer orders every agent action into a signed log |
| eventually-consistent permissions | **at most one launch per approved action**, and an honest `unknown` after a crash |
| bots that are just users | **accountable agents**: every agent has a human operator who vouches for it, and can withdraw that |
| no notion of money | **budgets** reserved before an action and settled after it, delegated down task trees |
| coarse power levels | **approvals bound to the exact plan**, with expiry and separation of duties |
| a timeline of finished messages | **live sessions across silos**: watch, read and steer other people's and agents' work while it happens, with who-may-what set per space by its owners; every steer is signed; an agent can be `guarded`: then it can't approve, govern or steer anyone else (it may still leave), and nothing it proposes or executes runs without its own human's approval, nor anything a delegate proposes, executes or delegates for that task, or with no task named, after accepting a task it gave, until the task closes for it; that delegate can't approve, govern or steer anyone else either, until then. Guarding is recorded on the work when the work is created, so unguarding it later, or switching owners' forcing off, frees nothing already recorded; nothing is held. Only that human can unguard it, and owners may guard every non-human (§7.5 lists what this doesn't cover) |
| complex state resolution | **pure, replayable policy**: any participant can re-check every decision, and a cheating sequencer leaves a portable proof |

## v1 is done when all three hold

1. **Used daily.** The author runs their own agent work through HAN, in shared spaces, through a working sequencer and executor, and a small team watches and steers each other's live agent sessions through it.
2. **Airtight.** The safety core is consistent, including its failure paths. That means the effect protocol passes exhaustive bounded model checking against the reference Policy, and every rule has a conformance vector, a scenario, a model property, or a written implementation-test procedure ([ref/](../ref/)).
3. **Built by strangers.** Someone implements a conforming sequencer or executor from these documents alone, without asking the authors.

## Design principles (in priority order)

1. **Don't rebuild chat.** Chat stays in chat tools. HAN's binding carries only what agent safety and live work need (the log, signals, files, session streams), and HAN specifies only that.
2. **One primitive.** Every authority-bearing fact is a signed entry in a room's sequenced log. New features are new entry kinds, not new APIs.
3. **Simple interfaces, deep modules.** A module earns its place by hiding a lot behind a few calls. Pass-through layers are a defect.
4. **Honest outcomes.** `unknown` is a first-class result. Nothing claims more authority, durability or success than it can show.
5. **No vendor in the core.** The standard names behaviours. The binding names behaviours too (QUIC is required, no QUIC library is), and chat integrations and reference products are separate.

## What HAN deliberately is not (v1)

- Not a chat network: chat stays in chat tools, which integrate (a room can point to a space).
- Not a consensus system: each room has one sequencer at a time. A cheating sequencer is *detectable* (signed receipts), not *prevented*. If a sequencer dies, its log stays verifiable and work continues in a fork, without inherited authority.
- Not an agent framework or a model API: how an agent thinks is its host's business.
- Not exactly-once in the world: HAN guarantees *at most one launch per claim*, no second attempt after an uncertain one unless someone explicitly records that it failed (or the proposer declared the action `repeatable`, i.e. harmless to repeat), and an honest outcome. It can't make the outside world cooperate.
