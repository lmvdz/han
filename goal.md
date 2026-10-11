# HAN — goal

**HAN is an open standard that lets anyone's people and AI agents find each other, talk, and work together — the way email lets anyone's mail server reach anyone else's. No single company owns it.**

Three kinds of conversation are equal citizens: human ⇄ human, human ⇄ agent, agent ⇄ agent. Agents may do anything agents can do today — write code, write documents, run for days, delegate to other agents, and act in the world (send, buy, deploy) — but every action that touches the world is recorded before it happens and is never silently done twice.

## v1 is done when all three hold

1. **Used daily.** The author runs their own human/agent work through HAN.
2. **Airtight.** The core is proven consistent, including its failure paths: a model of the log and the effect protocol passes exhaustive bounded checking against the reference Policy, and every rule in [architecture.md](architecture.md) has a conformance vector or a model property ([ref/](ref/)).
3. **Built by strangers.** Someone implements a conforming server or client from these documents alone, without asking the authors.

## Design principles (in priority order)

1. **One primitive.** Everything durable is a signed entry in a space's log. New features are new entry kinds, not new APIs.
2. **Simple interfaces, deep modules.** A module earns its place by hiding a lot behind a few calls. Pass-through layers are a defect.
3. **Server-to-server is client-to-server.** Federation reuses the same six calls, so there is one protocol to learn, implement and test.
4. **Honest outcomes.** "Unknown" is a first-class result. Nothing claims more durability, authority or success than it can show.
5. **No vendor in the core.** The standard names behaviours, not products. Reference choices live in a separate companion.

## What HAN deliberately is not (v1)

- Not a consensus system: each space has one ordering home at a time. A misbehaving home is *detectable* (signed receipts), not *prevented*. If a home dies, its spaces stay readable and work continues in a fork, without inherited authority.
- Not automatic trust: anyone may run a server; every server and space decides whom it accepts.
- Not end-to-end encrypted in v1: entry content may carry ciphertext, but key distribution is an extension.
- Not a model API, not an agent framework: how an agent thinks is its host's business; the standard only defines what it says and does.
- Not exactly-once in the world: HAN guarantees *at most one launch per claim*, no second attempt after an uncertain one unless someone explicitly records that it failed (or the proposer declared the action `repeatable`, i.e. harmless to repeat), and an honest outcome. It cannot guarantee that the outside world cooperates.
