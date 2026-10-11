# HAN — Human–Agent Network

**An open standard that lets anyone's people and AI agents find each other, talk, and work together, the way email lets anyone's mail server reach anyone else's. No single company owns it.**

Human ⇄ human, human ⇄ agent and agent ⇄ agent conversations are equal citizens. Agents may act in the world (edit code, send, buy, deploy), but every action that touches the world is recorded before it happens and is never silently done twice.

> **Status: design draft (v1, round 6).** This is a specification with an executable reference and checkers. There is no production implementation yet. The latest independent adversarial review scored it **76.6 / 100** against a fixed rubric ([grading-rubric.md](grading-rubric.md)); see [HISTORY.md](HISTORY.md) for every round. Expect breaking changes.

## The idea in one screen

- **One primitive.** Everything durable is a signed **entry** in a **space** (a DM, a room, a task thread, an agent's workbench). Each space has one **home** server that orders entries into a hash-chained log. Other servers **mirror** it and re-verify every decision.
- **Six calls.** `hello`, `append`, `read`, `signal`, `blobPut`, `blobGet`. Clients and servers use the same six calls, so federation is just a server acting as a client.
- **18 entry kinds**, covering talking (`message`), work (`task`…), touching the world (`effect.propose` → `approve` → `claim` → `outcome`), and governance.
- **Policy is four pure functions.** Any mirror can replay any decision, so a misbehaving home is *detectable*: equivocation and wrong-home proofs.
- **Honest outcomes.** `unknown` is a first-class result. An effect with an uncertain past runs again only after someone explicitly records that it failed.

## Read

| File | What it is |
|---|---|
| [goal.md](goal.md) | why HAN exists, what "v1 done" means, and what HAN deliberately is not |
| [architecture.md](architecture.md) | the specification: wire, modules, rules (`R-`, `P-`, `X-`), trust model, and a rule-by-rule verification table (§10) |
| [han.ts](han.ts) | exact types (normative for shapes) |
| [coverage.md](coverage.md) | how every guarantee of the earlier v0.4 design was kept, moved, weakened or dropped |
| [ref/](ref/) | executable reference Policy and wire layer (real Ed25519), conformance vectors, mutation suites, a fuzzer and a model checker |

## Run the checks

```sh
pip install cryptography          # the wire layer uses real Ed25519
python ref/run_all.py --quick     # ~3 min: tsc, consistency, vectors, mutations, fuzz, wire
python ref/run_all.py             # adds the exhaustive model checker (much longer)
```

## Licenses

- **Specification text** (`*.md`): [CC BY 4.0](LICENSE-SPEC). Implement it freely; credit the authors.
- **Code** (`han.ts`, `ref/`): [Apache-2.0](LICENSE), which includes a patent grant.

See [NOTICE](NOTICE).
