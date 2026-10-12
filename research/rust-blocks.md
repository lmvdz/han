# Rust building blocks for HAN v1 (protocol, device runtime, faces)

Research date: **2026-10-12**. Crate versions, dates, licenses and downloads come from the crates.io API (`/api/v1/crates/<name>`). Repository activity (pushed_at, stars, last commits) comes from the GitHub API (`gh api`), queried the same day. "Verified" means I read the source, docs or registry. "Asserted" means a project or third party says it and I did not check it. "Unverified" means I could not settle it.

Context read: H:\HAN\v1\PLAN.md §3–§8 and H:\HAN\v1\architecture.md §2–§3 and §8. a reference CRDT model kept outside this repo (1,772 lines of TS; no signing or hashing of deltas).

---

## 0. Bottom line

| HAN crate | Adopt (dependency) | Build, borrowing patterns from |
|---|---|---|
| han-core | `ed25519-dalek` **3.x** (not 2.x: iroh 1.x requires 3.x), `sha2`. Optional: `serde_json_canonicalizer` as a test oracle | A strict I-JSON parser and an integer-only JCS writer (small, wasm-friendly) |
| han-net | `iroh` 1.3 (Endpoint, Router, ProtocolHandler, EndpointHooks/IncomingFilter); `iroh-relay` 1.3 self-hosted; `iroh-gossip` 0.101 for presence only, signed and encrypted at app level | Blob parts on `han/1` frames (G-01/G-02). iroh-blobs is BLAKE3-only and serves by hash. |
| han-seq | `rusqlite` 0.40 (bundled) + `rusqlite_migration` 2.6 | Everything else. Nothing found replaces a single-writer receipt chain with exclusive claims. Learn from tlog checkpoints and witnesses, Hypercore, and NIP-29. |
| han-node | `interprocess` 2.4 (named pipe + Unix socket, Windows security descriptor); `tokio` | Views as hand-rolled incremental folds (Snapshot then Diff). JSON-RPC 2.0 framing, hand-rolled or jsonrpsee via the reth-ipc pattern. Spacedrive's CQRS action/query registry. |
| faces | `ratatui` 0.30; later `uniffi` 0.32 and Tauri 2 | tokio-console's watch-stream TUI; the ratatui component template |
| testing | `turmoil` 0.7 over the injected `Transport`/`Clock` | n0 `patchbay` (Linux netns) for real-iroh tests in WSL; skip madsim |
| shared code context (reference-model-like) | **not in MVP**; if needed later, **Loro** 1.16 | Overlap detection from git diff hunks; keep the reference model as the design reference and don't port it |

---

## 1. The iroh ecosystem

### 1.1 iroh (core)
- **What:** P2P QUIC connections dialed by Ed25519 public key (`EndpointId`), with hole punching and relay fallback. The QUIC stack is now `noq`, n0's fork of Quinn ([docs.rs/iroh](https://docs.rs/iroh/latest/iroh/), [0.97 post](https://www.iroh.computer/blog/iroh-0-97-0-custom-transports-and-noq)).
- **Version, license and activity:** **1.3.0** (2026-09-28). 1.0.0 shipped 2026-06-15, then 1.0.1–1.0.3, 1.1.0 (2026-08-25, security fixes) and 1.2.0 (2026-09-09). MIT OR Apache-2.0. Repo pushed 2026-10-12, 12.7k stars ([crates.io](https://crates.io/crates/iroh), [blog index](https://www.iroh.computer/blog)).
- **Stability (asserted by n0, [v1 post](https://iroh.computer/blog/v1)):**
  - wire format and API are stable; "any change that affects the wire stability of iroh will always coincide with a major release";
  - Ed25519 is the only key algorithm; optional PQ key exchange;
  - official bindings for Rust, Python, Node, Swift and Kotlin;
  - relay support: v0.35 public relays until 2026-12-31; 0.9x and rc relays ended 2026-09-30;
  - adoption figures are n0's own and inconsistent (">200M endpoints in 30 days"; "millions" vs "hundreds of thousands" of devices).
- **API (verified, docs.rs 1.3.0):**
  - `Endpoint::builder(presets::N0).alpns(..).bind()`, `ep.connect(addr, ALPN)`, `conn.open_bi()/accept_bi()`;
  - `iroh::protocol::{Router, RouterBuilder, ProtocolHandler (async fn accept(&self, Connection)), IncomingFilter, IncomingFilterOutcome}`;
  - `iroh::endpoint::{EndpointHooks, BeforeConnectOutcome, AfterHandshakeOutcome, Incoming, Connection, SendStream, RecvStream}`;
  - discovery is now `address_lookup` (`DnsAddressLookup` publishes relay and direct addresses as DNS records);
  - custom transports are behind `unstable-custom-transports`, outside the 1.0 guarantees ([rc.1 post](https://iroh.computer/blog/iroh-1-0-0-rc-1)).
- **Fit:** han-net exactly.
  - ALPN `han/1` = one `ProtocolHandler` on a `Router`.
  - `IncomingFilter`/`EndpointHooks` drop unknown or abusive peers before the handshake; HAN `hello` still authenticates the actor.
  - Self-host `iroh-relay` 1.3.0 (MIT/Apache) per PLAN §8.
- **Privacy note:** default DNS address lookup publicly publishes "EndpointId reachable via relay X". A team may prefer static or self-hosted lookup. I did not inspect the exact published records.
- **Python binding** (M2 interop): PyPI `iroh` 1.1.0 (2026-07-16), built from [iroh-ffi](https://github.com/n0-computer/iroh-ffi) with uniffi 0.31.1. It is **core only**: endpoint, connections, relay, tickets. Verified from src/: no gossip, blobs or docs. Python interop is fine for frames on streams but not for gossip.
- **Verdict: INTEGRATE.**

### 1.2 iroh-gossip
- **What:** topic-based epidemic broadcast (HyParView + PlumTree). It has an I/O-free `proto` state machine and a `simulator` feature.
- **Version and activity:** **0.101.0** (2026-06-15), still 0.x. Depends on `iroh = "1"` (verified Cargo.toml). Commits through 2026-10-09. MIT/Apache-2.0 ([repo](https://github.com/n0-computer/iroh-gossip)).
- **Privacy model (verified from source and docs):**
  - **No access control of any kind** in the crate; a code search for authoriz/allowlist/access control found nothing.
  - Joining requires only the 32-byte `TopicId` and one reachable bootstrap peer already in the topic ([docs](https://docs.iroh.computer/connecting/gossip.md)). The docs suggest hashing a name into the TopicId only to avoid collisions.
  - `Message.delivered_from` is documented as *"The endpoint that delivered the message. This is not the same as the original author"* (src/api.rs), so the origin is unauthenticated.
  - Encryption is hop-by-hop QUIC only, so all topic members see plaintext.
  - `DEFAULT_MAX_MESSAGE_SIZE = 4096` bytes (min 512), configurable (src/proto.rs).
  - **Whoever learns the TopicId and can reach one member can join and read everything.**
- **Fit:** presence and who-is-on hints only, matching PLAN §7's round-30 default. If used:
  - derive the TopicId from a reader-only space secret (e.g. HMAC(space read key, "han-gossip-v1")), as Delta Chat does with a random topic id sent over E2EE;
  - sign every payload with the HAN device key;
  - encrypt to the space if it carries more than presence;
  - keep items under 4 KiB or raise the limit.
- **Verdict: INTEGRATE (presence only), never authority.**

### 1.3 iroh-blobs
- **What:** BLAKE3/bao verified streaming of content-addressed blobs.
- **Version:** **0.103.1** (2026-10-06). The same day 0.97.1–0.102.1 were republished and 0.102.0/0.103.0 yanked, matching a security fix on the [blog index](https://www.iroh.computer/blog) (2026-10-08): a bug that "let any peer push blobs into a provider's store". The post URL I guessed returned 404, so details are unverified. MIT OR Apache-2.0.
- **Conflicts with HAN (verified):**
  - (1) "as of now you can only use the BLAKE3 hash function" ([0.95 post](https://www.iroh.computer/blog/iroh-blobs-0-95-new-features)), while HAN `BlobRef.hash` is **sha256** (T-03);
  - (2) it serves by hash by default, but HAN G-01 says "access follows references". Provider events (0.93+) can intercept `GetRequest` and abort with Permission, so G-01 could be enforced with a sha256→BLAKE3 map and per-space checks;
  - (3) 0.x with a fresh advisory.
- **Verdict: LEARN-FROM / defer.** Implement blobPut/blobGet parts as `han/1` frames, as the spec already defines.

### 1.4 iroh-docs
- **What:** a multi-writer KV, `(namespace, author, key) → (BLAKE3, size, timestamp)`, synced by range-based set reconciliation and stored in redb. The namespace key is the write capability; there is no read access control ([README](https://github.com/n0-computer/iroh-docs)).
- **Version and activity:** **0.101.0** (2026-06-15); last commits 2026-08-19; 75★.
- **Verdict: SKIP.** Its model conflicts with the sequenced log. Set reconciliation is noted for later head anti-entropy.

### 1.5 Apps built on iroh
| App | Facts | Pattern for HAN | Verdict |
|---|---|---|---|
| **Delta Chat core** ([chatmail/core](https://github.com/chatmail/core) `src/peer_channels.rs`, MPL-2.0) | Webxdc realtime: a random 32-byte TopicId is sent **inside the E2EE message**. Peers announce relay + key with no direct IPs ("as this information can be persisted by email providers"). Ephemeral keys; lazy start. **Still on iroh 0.35** (verified Cargo.toml); public relays for 0.35 end 2026-12-31. | Topic id as a capability, distributed over an authenticated channel | LEARN-FROM |
| **dumbpipe** 0.39 (MIT/Apache, iroh 1.0) and **sendme** 0.36.1 (iroh 1.0 + blobs 0.103.1) | n0 reference apps | Idiomatic 1.x usage | LEARN-FROM (copyable) |
| **cross.stream / xs** ([cablehead/xs](https://github.com/cablehead/xs), MIT, 230★, pushed 2026-09-29) | Local-first append-only event store: fjall index + cacache CAS; **HTTP/1.1 API over a local Unix socket**; iroh 1.1 to share streams | Closest small analogue of han-node (ordered frames, follow subscriptions, CAS side store) | LEARN-FROM |
| **irpc** ([n0-computer/irpc](https://github.com/n0-computer/irpc), Apache-2.0) | Streaming RPC over memory, quinn or iroh | Same enum messages in-process and remote; no named-pipe transport | LEARN-FROM |
| Codux (GPL-3), Teamtype (AGPL-3), iroh-rings (MIT) | Listed in [awesome-iroh](https://github.com/n0-computer/awesome-iroh). Codux is an agent terminal controlled over iroh across devices. | n/a | SKIP code |
---

## 2. Signed append-only and hash-chained logs

HAN's log is one chain per space. The home sequencer signs receipts (`prev` = receiptHash of seq−1, R-02). Authors sign entries. Policy decides over a **total order**, which exclusive effect claims, the budget ledger and exactly-once execution depend on.

### 2.1 Transparency logs (tlog-style)
| Item | Facts | Verdict |
|---|---|---|
| **tlog_tiles / signed_note** ([cloudflare/azul](https://github.com/cloudflare/azul)) | BSD-3-Clause. On crates.io only **0.2.0 (2025-04-18)**. The repo is active (pushed 2026-10-12) and also holds unpublished `tlog_checkpoint`, `tlog_cosignature`, `tlog_witness` and others. Implements C2SP tlog-tiles, checkpoint and signed-note. | LEARN-FROM: checkpoint (signed head) and witness cosignature formats are a ready design for R-15 head gossip and portable equivocation evidence |
| **ct-merkle** ([rozbb/ct-merkle](https://github.com/rozbb/ct-merkle)) | MIT/Apache, 0.3.0 (2026-05-23). RFC 6962 Merkle tree with inclusion and consistency proofs. | LEARN-FROM; ADOPT only if HAN later adds Merkle receipts (O(log n) inclusion instead of replay from seq 1) |
| **sigstore** 0.14.0 / **sigstore-rekor** 0.14.0 | Apache-2.0. The sigstore-rs repo describes itself as "experimental"; sigstore-rekor lives in [sigstore/sigstore-rust](https://github.com/sigstore/sigstore-rust). | SKIP (clients of public supply-chain logs) |
| **siglog** (prefix-dev) | BSD-3, 0.1.0, 2★, a tlog-tiles server over OpenDAL + sea-orm/SQLite | SKIP (shows tiles on SQLite are feasible) |
| **Trillian / Tessera** | Go, Apache-2.0, active | LEARN-FROM only |

Takeaway: the linear `prev` chain is right for v1, since verifiers replay from seq 1 anyway (R-04). The transferable idea is **signed checkpoints plus independent witnesses** against split views.

### 2.2 p2panda (special attention)
- **What:** a modular local-first p2p stack ([p2panda/p2panda](https://github.com/p2panda/p2panda)). Crates: core, net, sync, store, stream, auth, encryption, spaces, discovery and blobs; FFI via uniffi.
- **License and maturity:**
  - MIT OR Apache-2.0, per the README and Cargo.toml (GitHub shows "none" because the files are under LICENSES/).
  - **0.7.1** (2026-08-21); 0.7.0 2026-07-07; 0.6.x 2026-05. Pushed 2026-10-12, 579★, NGI-funded.
  - The README warns: *"APIs are not yet considered stable for production use… Stability guarantees will improve with the release of v1.0.0."*
- **Stack:**
  - `p2panda-net` uses **iroh 1.3.0 + iroh-gossip 0.101 + iroh-mdns-address-lookup**, adds confidential topic discovery (private set intersection) and Erlang-style supervision trees;
  - `p2panda-store` uses sqlx 0.9 SQLite;
  - `p2panda-core` uses **ed25519-dalek 2.2** and CBOR.
- **Operation and log model (verified, p2panda-core/src/operation/header.rs):**
  - `Header { version, verifying_key, signature, payload_size, payload_hash, seq_num, backlink, extensions }`; op id = BLAKE3(header CBOR);
  - logs are **single-writer per author** (seq_num from 0; `backlink` = hash of the author's previous op), and an author may have many logs;
  - "fork-tolerant", prunable;
  - the body is off-chain, so it is deletable while the header still verifies;
  - multi-writer comes from *combining* per-author logs, with causal links and timestamps carried in extensions.
- **Authority model (verified, p2panda-auth docs):**
  - group membership is a **DAG of signed ops, made eventually consistent** by a resolver;
  - default "strong removal": removing a manager invalidates their concurrent actions; mutual removals both apply; re-adds keep membership but invalidate concurrent actions;
  - access levels Pull/Read/Write/Manage, with conditions.

| Aspect | p2panda | HAN |
|---|---|---|
| Ordering | per-author chains + causal DAG; no total order | one total order per space (home sequencer) |
| Who signs the chain | each author, over their own log | authors sign entries; the **sequencer** signs the receipt chain |
| Header / payload split | `payload_hash` in the header; body deletable | the same: permanent header, redactable content via `contentHash` (§2, R-04), which independently confirms HAN's design |
| Authority conflicts | resolved after the fact by invalidating concurrent ops | prevented: Policy decides at append against the state at seq−1 |
| Exclusive claim, exactly-once effect, budget | **not expressible** without a coordinator: two concurrent claims are both valid until one is invalidated later, possibly after the world was touched | the sequencer orders claims; the first wins before anyone acts |
| Offline writes | first-class | outbox (R-07); authority waits for the home |
| Transport | iroh 1.3 | iroh 1.x |

- **Verdict: LEARN-FROM, not a replacement.** Eventually consistent authority cannot provide HAN's exclusive-claim, budget or exactly-once guarantees, which need a linearization point.
- **Patterns to borrow:**
  - (a) header/body split with deletable bodies;
  - (b) PSI-based confidential topic discovery, if gossip discovery must not leak space ids;
  - (c) supervision trees for han-node's net subsystems;
  - (d) the strong-removal rules, as a counter-example worth recording in the spec's decisions.
- **As a dependency:** unstable pre-1.0, and it would add ed25519-dalek 2 next to iroh's 3.

### 2.3 Hypercore (Rust)
- **What:** [datrs/hypercore](https://github.com/datrs/hypercore) **0.16.0** (2026-08-07), MIT OR Apache-2.0, 356★. A single-writer append-only log whose Merkle roots are signed by the writer, with sparse replication via proofs.
- **Maturity:** its README calls itself a *"limited Rust port"*. The v11 manifest and v1.0.0 are open to-dos.
- **Fit:** a HAN space log is conceptually "a hypercore whose writer is the sequencer". Merkle roots plus sparse proofs are the upgrade path from a linear chain.
- **Verdict: LEARN-FROM; SKIP as a dependency.**

### 2.4 Willow / Earthstar
- **What:** `willow25` 0.7.9 (2026-08-27) and `willow-data-model` 0.8.0, MIT OR Apache-2.0. The GitHub repo is **archived** and moved to [codeberg worm-blossom/willow_rs](https://codeberg.org/worm-blossom/willow_rs).
- **Model:** multi-writer, path-keyed, prefix-pruning destructive edits, Meadowcap capabilities, private set intersection. Earthstar is TypeScript.
- **Verdict: LEARN-FROM** (Meadowcap delegation, prefix deletion). **SKIP** as a dependency: no total order.

### 2.5 Nostr
- **Libraries:** [rust-nostr](https://github.com/rust-nostr/nostr), MIT: `nostr-sdk` 0.45.4 (2026-09-19) and `nostr-relay-builder` 0.44.1, very active. Relays: nostr-rs-relay (MIT) and strfry (C++, GPL-3).
- **NIP-29 relay-based groups** ([spec](https://github.com/nostr-protocol/nips/blob/master/29.md)) are the closest prior art to "the home server is the space's authority":
  - group state (kinds 39000–39003) "MUST be created by the relay master key only";
  - moderation via kinds 9000/9001;
  - `previous` tags (8-hex prefixes, at least 3) stop out-of-context rebroadcast;
  - late-publication rejection;
  - groups can move or fork to other relays.
  - There are **no sequence numbers or total order**; ties are left to "the relay currently serving the group".
- **Signatures:** secp256k1 Schnorr.
- **Verdict: LEARN-FROM** (HAN ≈ NIP-29 plus a receipt chain that closes its ordering gap). **SKIP** as a dependency.

### 2.6 Conclusion for han-seq
**Build it.** No library offers "single writer per space, signed receipt chain, Policy decided at append, idempotent append (R-01), durable before receipt (R-03), steady-clock receipt time, moves".
- Storage: `rusqlite` with WAL and `synchronous=FULL` for `durability:"local"`, plus `rusqlite_migration` (format version via `user_version`).
- Upgrade path later: tlog checkpoint and witness formats (azul) or Merkle receipts (ct-merkle).

---

## 3. Local-first sync and incremental views

### 3.1 Views engine options
| Option | Facts | Fit to han-node views | Verdict |
|---|---|---|---|
| **Hand-rolled projections** | n/a | Views are folds over (a) log records (Policy.fold already exists) and (b) namespaced stream items and gossip. Board, who-is-on and overlaps are small keyed maps. `view(q)` = snapshot, then per-key upsert/remove diffs. Overlaps = an index `path → {sessions}` updated per StatusItem. Deterministic and easy under turmoil. | **BUILD (recommended)** |
| **differential-dataflow** ([repo](https://github.com/TimelyDataflow/differential-dataflow)) | MIT, 0.25.1 (2026-07-15), active, 3k★; needs the timely worker runtime | Correct incremental joins but heavy, with its own threads. Overkill for 3–5 views over thousands of items. | LEARN-FROM ((data, time, diff) as the `ViewEvent::Diff` shape) |
| **DBSP / Feldera** ([feldera](https://github.com/feldera/feldera)) | `dbsp` 0.364.0 (2026-10-09; near-weekly), MIT OR Apache-2.0 (crates.io); about 170 dependency lines in its Cargo.toml | Heavier still | LEARN-FROM (Z-sets: changes as ±1-weighted rows) |
| **SQLite reactive** (rusqlite `hooks`/`preupdate_hook`/`session`) | rusqlite 0.40.2, MIT | update_hook → invalidate → requery works as a cache-invalidation fallback; hooks fire on the writer connection only | optional helper, not the engine |
| **cr-sqlite** ([vlcn-io/cr-sqlite](https://github.com/vlcn-io/cr-sqlite)) | MIT; last release **v0.16.3 (2024-01-17)**; 2026 commits are build fixes | Multi-writer CRR tables conflict with the sequenced log | SKIP |
| "materialize-lite" | No project by that name could be identified | n/a | unverified |

**Recommendation:** a han-node `View` trait with `apply(&mut self, &Input) -> Vec<Diff>` and `snapshot()`. One ordered input channel carries records by (space, seq), stream items and gossip; each subscription filters the diffs.

Spacedrive's Oct 2025 sync redesign ([LSYNC-000](https://github.com/spacedriveapp/spacedrive/blob/main/.tasks/core/LSYNC-000-library-sync.md)) states the key insight: *"Device ownership eliminates 90% of conflicts. Only truly shared resources need ordered logs."* In HAN, status and presence are session- or device-owned, so the last value per session wins and no merge is needed. Only authority needs the sequencer.

### 3.2 CRDTs
See §6. The MVP views need no CRDT: the log is totally ordered and stream items have a single owner.
---

## 4. Smaller crates

| Crate | Version / date / license | Verified facts | Fit | Verdict |
|---|---|---|---|---|
| **serde_json_canonicalizer** ([repo](https://github.com/evik42/serde-json-canonicalizer)) | 0.3.2 (2026-02-03), MIT, 8.2M recent downloads, 22★ | Sorts keys by **UTF-16 code units** (`encode_utf16`). Numbers via `ryu-js`. **Every integer type is cast to f64** (`write_u64` → `value as f64`), so ≥2^53 silently loses precision. Its README calls serde_jcs "abandoned" and RFC-divergent. | HAN numbers are integers in [0, 2^53), so f64 output is exact. But the crate does **no I-JSON validation**: serde_json `Value` collapses duplicate keys and accepts `1.0`, `1e3` and `-0`. | INTEGRATE as output or test oracle; **BUILD** the strict I-JSON parser (duplicates, lone surrogates, non-integer forms, ≥2^53 → `invalid`). An integer-only JCS writer is roughly 100 lines and keeps han-core lean for wasm/FFI. |
| **serde_jcs** ([repo](https://github.com/l1h3r/serde_jcs)) | 0.2.0 (2026-03-25), 8★ | Revived March 2026; open issue #3 "Encoding numbers does not follow specification" | n/a | SKIP |
| **json-canon** | 0.1.3 (2023-05) | stale | n/a | SKIP |
| **ed25519-dalek** | **3.0.0 (2026-07-06)**, BSD-3. MSRV 1.85, edition 2024, rand_core 0.10, signature 3, sha2 0.11 ([CHANGELOG](https://github.com/dalek-cryptography/curve25519-dalek/blob/main/ed25519-dalek/CHANGELOG.md)). The last v2 is 2.2.0 (2025-07). | **iroh-base 1.3 requires `ed25519-dalek >=3.0.0-rc.0, <4`** (crates.io dependency list), so v2 in han-core would mean two copies and two key types | han-core. Use `verify_strict` and pin edge cases (small-order keys, non-canonical S/R) in the wire vectors so ref/ (Python) and cleanroom-ts agree. Background: "Taming the many EdDSAs", https://eprint.iacr.org/2020/1244 (not re-fetched). | **INTEGRATE v3**, which supersedes "v2" in the brief |
| **rusqlite** | 0.40.2 (2026-08-08), MIT | `bundled`, `hooks` features | han-seq, han-node | INTEGRATE (`bundled` on Windows) |
| **rusqlite_migration** | 2.6.0 (2026-05-28), Apache-2.0, requires `rusqlite ^0.40` | uses `user_version` | PLAN §6.4 "migrations from the first commit" | INTEGRATE |
| **jsonrpsee** | 0.26.1 (2026-09-30), MIT | server dependencies are hyper and soketto (HTTP/WS); **no IPC transport** | node API | only through the reth-ipc pattern; otherwise SKIP |
| **reth-ipc** ([reth crates/rpc/ipc](https://github.com/paradigmxyz/reth/tree/main/crates/rpc/ipc)) | MIT OR Apache-2.0; **not on crates.io** | jsonrpsee 0.26 server and client over `interprocess` (Unix sockets and Windows named pipes) | reference | LEARN-FROM / copy with attribution |
| **interprocess** ([repo](https://github.com/kotauskas/interprocess)) | 2.4.5 (2026-10-10), 0BSD OR Apache-2.0, 604★ | `local_socket` gives a named pipe on Windows and a Unix socket elsewhere; tokio support; **`ListenerOptionsExt::security_descriptor`** on Windows (verified in source) lets the pipe be restricted to the current user, which matters for "keys never leave han-node" | han-node local channel | **INTEGRATE** |
| **parity-tokio-ipc** | 0.9.0 (**2021-07-06**) | unmaintained | n/a | SKIP |
| AF_UNIX on Windows (`uds_windows`) | used by OpenAI Codex `codex-rs/uds` (Apache-2.0) | Windows 10+ AF_UNIX allows a single code path | alternative | LEARN-FROM (fallback) |
| **uniffi** | 0.32.2 (2026-09-23), MPL-2.0 (file-level copyleft; fine as a dependency), 5k★ | iroh-ffi pins 0.31.1, so align versions if you ship both | mobile face | INTEGRATE later. Expose only `call(json)` and `view(json, callback)`; Spacedrive does exactly this: "same JSON-RPC protocol over FFI". |
| **turmoil** ([tokio-rs/turmoil](https://github.com/tokio-rs/turmoil)) | 0.7.2 (2026-04-24), MIT, pushed 2026-10-05 | deterministic hosts, TCP/UDP, partitions, seeded RNG; code must use its net types or injected traits | M3 three-node simulation over HAN's `Transport` and `Clock`. **Real iroh will not run on turmoil sockets**: custom transports are unstable, no in-memory sim transport is documented, and no turmoil use was found in n0 or p2panda repos. | **INTEGRATE** (simulate HAN's Transport, not iroh) |
| **madsim** | 0.2.34 (2025-10-11), Apache-2.0, last push 2026-02 | swaps tokio via `cfg(madsim)` | intrusive, slowing | SKIP |
| **patchbay** ([n0-computer/patchbay](https://github.com/n0-computer/patchbay)) | pushed 2026-10-08 | "realistic network simulation via Linux namespaces" (non-deterministic) | real-iroh NAT and relay tests in WSL | optional |
| **ratatui** | 0.30.2 (2026-06-19), MIT, 23k★; split into ratatui-core, ratatui-widgets and backend crates | n/a | han-tui | INTEGRATE |

**Ratatui apps with live dashboards worth studying:**
- **tokio-console** ([tokio-rs/console](https://github.com/tokio-rs/console), MIT, on ratatui 0.30.2): a TUI attached to a live process via a streaming `WatchUpdates` RPC plus `Pause`/`Resume`, whose updates carry what is "new since the last update". **This is HAN's `view(q)` (snapshot, then diffs).** Best single reference.
- **bottom** (MIT): live time series.
- **trippy** (Apache-2.0): live tables.
- **gitui** (MIT): async git and diff panes, a good model for drill-down.
- **kdash** (MIT): list, detail and logs dashboard.
- **radicle-tui** (Apache-2.0): a face over a node daemon.
- **OpenAI Codex tui** (Apache-2.0).
- [ratatui/templates](https://github.com/ratatui/templates) (MIT): the component and action-channel template.

---

## 5. P2P team and collaboration apps resembling han-node

| Project | Facts | Patterns for HAN | Verdict |
|---|---|---|---|
| **Spacedrive v2 core** ([repo](https://github.com/spacedriveapp/spacedrive), Apache-2.0, 39k★, v2.0.0-alpha.2 2026-02-07, nightly 2026-10-08) | From core/README (verified):<br>- daemon + CLI over **"Unix socket JSON-RPC 2.0"** with method strings like `query:vdfs.list_entries`;<br>- **CQRS actions and queries registered at compile time via `inventory`, "shared across daemon RPC, iOS FFI, and WASM extensions"**;<br>- iOS uses the "same JSON-RPC protocol over FFI";<br>- iroh P2P and mDNS pairing; SQLite via SeaORM; durable resumable jobs.<br>Sync moved from a single leader assigning sequences to leaderless device-owned state plus HLC logs (Oct 2025). | Node API identical in-process, over a socket and over FFI; a command/query registry; device pairing. The leaderless move fits its data, not HAN authority. | **LEARN-FROM**; Apache-2.0 code is borrowable |
| **Zed collaboration** ([zed](https://github.com/zed-industries/zed)) | The collab, rpc, proto, text and clock crates are **GPL-3.0-or-later** (verified Cargo.toml); gpui is Apache-2.0. Central server, protobuf RPC, buffer CRDT. A test comment says collab is "going away with the reference model soon". | Lamport-clocked fragments and anchors, the ancestry of the reference model | LEARN-FROM only (GPL) |
| **any-sync / Anytype** ([any-sync](https://github.com/anyproto/any-sync), Go, MIT, active) | E2EE spaces made of signed CRDT DAGs. A **consensus node is "responsible for ACL changes monitoring and validation"**: a "strictly ordered list" for ACLs only, with CRDT content. Separate sync, file, coordinator and consensus nodes. | Independent confirmation of HAN's split: authority ordered, content not | LEARN-FROM |
| **Radicle heartwood** ([repo](https://github.com/radicle-dev/heartwood), GitHub-detected Apache-2.0) | `radicle-node` daemon with a control socket; Windows uses the `winpipe` named-pipe crate (CHANGELOG). `rad` CLI and radicle-tui faces. **COBs: "graphs of CRDTs"**, i.e. issues and patches as signed op histories in git. | Daemon + control socket + several faces; signed issue and task objects | LEARN-FROM |
| **OpenAI Codex** ([codex-rs](https://github.com/openai/codex), Apache-2.0) | app-server JSON-RPC protocol, app-server-daemon, transport, `uds` (AF_UNIX on Windows too), a ratatui tui, and windows-sandbox-rs (DACL-restricted named pipes) | Agent daemon + faces + Windows IPC hardening; a future han-host driver | LEARN-FROM |
| **Ditto** | `dittolive-ditto` 5.2.0-preview.5; crates.io license "non-standard"; proprietary Rust core with mesh CRDT ([docs](https://docs.ditto.live/home/about-ditto)) | n/a | SKIP |
| **cross.stream (xs)** | see §1.5 | local-socket event-store daemon | LEARN-FROM |
---

## 6. CRDT and history engines vs a reference history model

**Reference model.** An operation-history design kept outside this repository: a causal DAG of operations with Lamport ids, versions as frontiers, named timelines that fork and merge inside one history, time travel, registers, maps and a text CRDT, two-message anti-entropy sync, and SQLite storage, with no signing of operations. In it, an agent session is a timeline over a worktree and review is a DAG difference. Details derived from the closed product it models are deliberately not reproduced here.

### 6.1 Feature by feature

| Feature | reference model | **Loro** | **diamond-types (Eg-walker)** | **Automerge** | **Yrs** |
|---|---|---|---|---|---|
| Op id / clock | (actor, Lamport ts) | `OpId(peer u64, counter)`; a Lamport per change ("lamport of the Change is the lamport of its first op") | (agent, seq) and local versions | (actor, counter); changes are **hash-linked** (`ChangeHash`, deps by hash) | (client id, clock) |
| **Frontier versions** | yes | **yes**: `oplog_frontiers`/`state_frontiers`, plus a VersionVector | **yes** (the frontier idea; Loro's README credits Eg-walker) | **yes**: `get_heads()` = change hashes | **no**: `StateVector` |
| **Fork / checkout / merge** | named timelines in one history | `checkout(frontiers)` detaches; **`set_detached_editing(true)`** allows edits at a checkout ("uses a different PeerID for each checkout"); `fork()`/`fork_at(frontiers)` create a new doc; merging = `import`, or checking out the union of frontiers. Named timelines must be app-level (name → frontiers). | `Branch` checked out at any version; `branch.merge(oplog, version)` | `fork()`/`fork_at(heads)` + `merge()`; `transaction_at(heads)`; no named branches | none (clone the doc and apply updates) |
| **Time travel** | `stateAt` | `checkout(frontiers)` ("fast time travel", README) | yes | `*_at(heads)` getters, `diff(before, after)` | `Snapshot`, only with `skip_gc: true`; a snapshot update "can't be merged into a document that has since diverged" |
| **Review = DAG difference** | `difference(agent, meet(main, agent))` | `find_id_spans_between(from, to)`, `diff(a, b)`, `get_changed_containers_in` | `encode_from(version)` | `get_changes_added`, `diff(heads, heads)` | `encode_diff(state_vector)` |
| **Text** | fragment CRDT, UTF-16, O(n) per edit (lite) | Fugue-based rich text | **fastest plain text**; JSON types only on the `more_types` branch | rich text with marks | rich text |
| **Map** | LWW + concurrent values + add-wins remove | LWW map | not on main | map exposing conflicts | LWW map |
| **Tree** | file tree (the reference model only) | **movable tree** (`LoroTree`) | none | none | XML tree (not movable) |
| **Incremental sync over iroh** | hello/respond/receive | `export(ExportMode::updates(&vv))` → bytes → `import`; `subscribe_local_update` for live pushes; `loro-protocol` 0.3 (MIT) | `encode_from(version)` / decode | **`generate_sync_message`/`receive_sync_message`**, transport-agnostic; `samod` 0.15 (MIT), an automerge-repo-compatible Rust layer | y-sync `Protocol` + `Awareness` |
| **SQLite / own storage** | SQLite (lite: rows + packs + checkpoints; real: CoW B-trees) | bytes in and out (snapshot, updates, shallow snapshot); keep them in SQLite yourself | bytes | `save`/`save_after(heads)`/`load_incremental`; bring your own | bytes |
| **Signing / authenticating ops** | none | none; PeerID is a random u64, and the docs warn duplicates "corrupt the document" | none | none in core; the hash chain gives integrity, not authorship. **keyhive_core** 0.6.0 (Apache-2.0, Ink & Switch, pre-1.0) adds signed capabilities. | none |
| **Rust maturity / license** | a TS prototype of a closed original | **1.16.2 (2026-09-21), MIT, 6.2k★, very active, 1.0 announced** | crates.io **1.0.0 dates from 2022**; the README says it is "quite out of date" and "WIP"; repo active 2026-09; ISC | 0.12.0 (2026-09-16), MIT, 6.7k★, very active | 0.28.0 (2026-09-17), MIT, 2.2k★, active |

### 6.2 Closest production-grade Rust equivalent of the reference model: **Loro**
Loro alone combines:
- a Lamport-stamped op DAG with **frontier versions**;
- checkout at any frontier **with detached editing**, so a timeline is a named frontier plus the edits made from it;
- fork/fork_at, and DAG-difference queries (`find_id_spans_between`, `diff`);
- a **movable tree** for file trees, beside text and maps;
- version-vector incremental export that fits a two-message anti-entropy over iroh;
- the MIT license, with roughly monthly 1.x releases.

Gaps vs the reference model:
- no first-class named timelines (needs a small timeline → frontiers table);
- no SQLite-native storage;
- **no per-author authentication.** HAN would wrap each exported update in a HAN-signed envelope and check that the update's PeerIDs belong to the signer. Whether Loro exposes an update's peer ids before import is **unverified**; I did not check `decode_import_blob_meta`.

The others:
- **Automerge** is the mature runner-up. Its hash-linked changes match HAN's integrity style and its sync protocol is strong, but it has no tree, and branches are separate forks.
- **diamond-types** has the best algorithm but is text-only and its crates.io release is stale.
- **Yrs** is editor-oriented: no frontiers, no forks, limited time travel.

### 6.3 Port the reference model or adopt a library?
**For the MVP: neither. Keep the reference model as the design reference. When character-precise shared timelines are needed, adopt Loro instead of porting.**

Reasoning, against the MVP use case:
1. **Live visibility of agent code changes.** Each session is single-writer in its own git worktree. The host can derive `files[]`, the step and a diff summary from `git diff` against the session base and publish them in `han.status/1` (PLAN §5). Observing one writer needs no CRDT.
2. **Precise overlap detection.**
   - File level is a set intersection over `files[]`.
   - Range level: `git diff -U0 base..worktree` gives hunks `(path, start, len)` relative to the **base** commit, and sessions on the same base overlap when hunks intersect.
   - Sessions on different bases need the hunks mapped through `git diff base1..base2`, which is bounded and well understood.
   - CRDT anchors would survive rebases more gracefully. That is a refinement, not an MVP requirement.
3. **Shared team context** (board, notes, summaries) is either authority (the HAN log) or single-owner status items. Neither needs merge semantics.
4. **A Rust port of the reference model** (about 1.8k lines of TS) would give an unsigned, O(n)-per-edit text CRDT with replay-from-checkpoint materialization, which is strictly weaker than Loro.
5. **Mirroring whole worktrees into a CRDT** (the reference model's real capability) is a large commitment: file-system semantics and git round-trips. It should wait for evidence from M6 dogfooding.

**Plan:**
- **MVP:** git-diff-derived `files` and `hunks` in `han.status/1`, a non-authority item that can evolve without a spec round. The overlaps view does interval intersection per path.
- **Later:** a Loro-backed extension (e.g. `han.timeline/1`): an agent session is a Loro checkout over a worktree doc, review is `find_id_spans_between`, and every update travels in a HAN-signed envelope. the reference model and EVIDENCE.md serve as the semantic reference (merge rules, seals, timelines as frontier pointers).

---

## 7. Gaps and unverified points
- **iroh 1.0 adoption figures:** n0's own claims, internally inconsistent.
- **DNS address lookup:** exact published records not inspected.
- **iroh-gossip:** no n0 security statement found either way. "No access control" rests on source search plus the API.
- **iroh-blobs advisory:** details come only from the blog index summary; the post URL I tried returned 404.
- **Loro:** pre-import peer-id inspection unverified; movable-tree performance at repo scale untested.
- **Automerge:** the 0.12.0 crate version is taken from crates.io, not cross-checked against release notes.
- **Ed25519 parity:** cross-implementation verification behaviour of ref/ (Python) and cleanroom-ts not checked.
- **"materialize-lite":** not identifiable.
- **Licences:** Ditto's is inferred from crates.io "non-standard"; Radicle's is GitHub-detected only.

## 8. Sources
- iroh: https://iroh.computer/blog/v1 · https://www.iroh.computer/blog · https://docs.rs/iroh/latest/iroh/ · https://iroh.computer/blog/iroh-1-0-0-rc-1 · https://www.iroh.computer/blog/iroh-0-97-0-custom-transports-and-noq · https://crates.io/crates/iroh
- gossip/blobs/docs: https://github.com/n0-computer/iroh-gossip · https://docs.iroh.computer/connecting/gossip.md · https://www.iroh.computer/blog/iroh-blobs-0-95-new-features · https://github.com/n0-computer/iroh-docs
- iroh apps: https://github.com/chatmail/core · https://webxdc.org/docs/spec/joinRealtimeChannel.html · https://github.com/n0-computer/awesome-iroh · https://github.com/cablehead/xs · https://github.com/n0-computer/iroh-ffi · https://github.com/n0-computer/irpc
- logs: https://github.com/p2panda/p2panda · https://github.com/cloudflare/azul · https://github.com/rozbb/ct-merkle · https://github.com/sigstore/sigstore-rs · https://github.com/datrs/hypercore · https://codeberg.org/worm-blossom/willow_rs · https://github.com/nostr-protocol/nips/blob/master/29.md · https://github.com/rust-nostr/nostr
- views: https://github.com/TimelyDataflow/differential-dataflow · https://github.com/feldera/feldera · https://github.com/vlcn-io/cr-sqlite · https://github.com/spacedriveapp/spacedrive/blob/main/.tasks/core/LSYNC-000-library-sync.md
- crates: https://github.com/evik42/serde-json-canonicalizer · https://github.com/l1h3r/serde_jcs · https://github.com/dalek-cryptography/curve25519-dalek/blob/main/ed25519-dalek/CHANGELOG.md · https://github.com/kotauskas/interprocess · https://github.com/paradigmxyz/reth/tree/main/crates/rpc/ipc · https://github.com/mozilla/uniffi-rs · https://github.com/tokio-rs/turmoil · https://github.com/madsim-rs/madsim · https://github.com/n0-computer/patchbay · https://github.com/tokio-rs/console · https://github.com/ratatui/ratatui
- daemon + faces apps: https://github.com/spacedriveapp/spacedrive/blob/main/core/README.md · https://github.com/anyproto/any-sync · https://github.com/radicle-dev/heartwood · https://github.com/openai/codex · https://docs.ditto.live/home/about-ditto
- CRDTs: https://docs.rs/loro/latest/loro/struct.LoroDoc.html · https://github.com/loro-dev/loro · https://github.com/josephg/diamond-types · https://docs.rs/automerge/latest/automerge/struct.Automerge.html · https://docs.rs/yrs/latest/yrs/ · https://github.com/inkandswitch/keyhive · https://github.com/alexjg/samod
