// HAN v1 — exact types (normative for shapes). Rules live in architecture.md (R-, P-, X-).
// JSON; JCS (RFC 8785) for hashing; SHA-256 lowercase hex; Ed25519 base64url (no padding); integers in [0, 2^53).

// ───────────────────────── scalars ─────────────────────────
export type Hex = string;
export type Sig = string;
export type Time = string;            // RFC 3339, UTC, e.g. "2026-10-10T12:00:00Z"
export type ServerName = string;      // DNS name
export type ActorId = `@${string}:${string}`;   // "@name:server"; "@:server" = the server actor
export type SpaceId = `${string}:${string}`;    // "<1–26 chars [a-z0-9]>:<home server>" | "profile:@name:server"; the server part names the FIRST home
export type EntryId = Hex;            // sha256("han-entry-v1\n" ‖ JCS(entry minus sig))
export type KeyId = string;           // 1–64 chars [A-Za-z0-9_-]
export type Seq = number;             // per space: 1, 2, 3, … ; 0 = before the first record

// ───────────────────────── the primitive ─────────────────────────
export interface Entry<K extends KindName = KindName> {
  v: 1;
  space: SpaceId;
  kind: K;
  author: ActorId;
  keyId: KeyId;          // which of the author's keys signed
  body: Bodies[K];       // permanent; with kind, author, keyId, refs and seen, all Policy reads (R-25)
  contentHash: Hex | ""; // sha256(JCS(content)); "" iff no content is sent (allowed when the kind's content fields are all optional). Log-owned (R-25)
  refs: EntryId[];       // same-space only (R-05)
  seen: Seq;             // ≤ head at append (R-17)
  nonce: string;         // 26 chars [a-z2-7]; reused only when resending the same entry (R-06). Log-owned: Policy never reads it (R-25)
  time: Time;            // author's clock; informational, never a Policy input (R-11, R-25)
  sig: Sig;              // Ed25519(key, "han-sig-v1\n" ‖ id); checked by the Log, never by Policy (R-25)
}

export type Durability = "local" | "replicated";
export interface Receipt {
  space: SpaceId;
  seq: Seq;
  id: EntryId;
  prev: Hex | "";                         // receiptHash(seq − 1); "" for seq 1
  home: ServerName;
  homeKey: KeyId;                          // key in https://<home>/.well-known/han
  time: Time;                              // home clock; the only time Policy uses (ctx.time)
  durability: Durability;                  // achieved (R-03)
  profiles: { [actor: ActorId]: Seq };     // pinned profile seqs for relevantActors (P-01, R-10)
  homeSig: Sig;                            // Ed25519(homeKey, "han-home-sig-v1\n" ‖ receiptHash)
}

export interface LogRecord<K extends KindName = KindName> {
  redacted: boolean;                                  // true → content is null and was erased (P-12)
  redactedBy?: { entry: Entry<"redact">; receipt: Receipt };   // required when redacted: the signed proof (R-04, R-14)
  entry: Entry<K>;
  content: Contents[K] | null;                        // null = kind has no content, or redacted
  receipt: Receipt;
}

export interface BlobRef { hash: Hex; size: number; type: string; uri?: string }    // size ≤ 4 GiB; type: a media type, each half [a-z0-9][a-z0-9!#$&^_.+-]{0,63}; uri: transport locator (T-03), e.g. mxc://…
export interface Link { space: SpaceId; seq: Seq; id: EntryId }   // cross-space pointer; content only (R-05)
export interface Budget { unit: string; amount: number }
export type SignedInt = number;     // integer in (−2^53, 2^53); used only where stated (budgetLeft)
// Numbers anywhere else, including inside opaque Json (plan.args, extension bodies), are integers in [0, 2^53);
// fractional or negative numbers there are invalid (R-09), so JCS stays exact. Budget amounts are integer minor units.

// ───────────────────────── entry kinds ─────────────────────────
export type Role = "owner" | "member" | "guest" | "banned";
export type ActorKind = "human" | "agent" | "service";
export type TaskState = "accepted" | "progress" | "done" | "failed" | "declined" | "cancelled";
export type EffectResult = "succeeded" | "failed" | "cancelled" | "unknown";
export type Started = "yes" | "no" | "unknown";

export interface Bodies {
  "space.create": {
    policy: PolicyDoc;
    forkOf?: { space: SpaceId; through: Seq };
    profile?: { actorKind: ActorKind; operator?: ActorId; key: { keyId: KeyId; publicKey: string } };   // an agent or service MUST name its operator here (invalid otherwise, §4.3); a human names none. Fixed for the life of the profile; it only confirms and withdraws
  };
  "member.set": { actor: ActorId; role: Role | "none" };
  "policy.set": { policy: PolicyDoc };
  "key.add": { keyId: KeyId; publicKey: string };
  "key.revoke": { keyId: KeyId };
  "message": { to?: ActorId[] };
  "task": { to: ActorId[]; budget?: Budget; parent?: EntryId; deadline?: Time; external?: string };   // external: a reference outside HAN, such as an issue's address; printable ASCII without spaces, 1–1024 chars; opaque to Policy (§5)
  "task.update": { task: EntryId; state: TaskState };
  "task.cancel": { task: EntryId };
  "effect.propose": { executor: ActorId; action: string; planDigest: Hex; cost?: Budget; repeatable: boolean; task?: EntryId; expiresAt?: Time };
  "effect.approve": { proposal: EntryId; planDigest: Hex; expiresAt?: Time };
  "effect.claim": { proposal: EntryId; attempt: number; instance: KeyId; window: number };   // window must equal policy.launchWindow (E5, X-03)
  "effect.cancel": { proposal: EntryId };
  "effect.outcome": { claim: EntryId; result: EffectResult; started: Started; cost?: Budget };
  "effect.resolve": { proposal: EntryId; result: "succeeded" | "failed"; cost?: Budget };
  "checkpoint": { through: Seq };
  "redact": { target: EntryId };
  "session.open": { steerMode: SteerMode; task?: EntryId };   // a live session by its author (P-15); the stream itself travels on the transport (T-07)
  "session.close": { session: EntryId };
  "session.input": { session: EntryId };      // steering, accountable and sequenced (P-15)
  "agent.unguard": { agent: ActorId };   // by the agent's guard (the operator in its pinned profile), holding a role: ends the agent's own guarded choice; frees no recorded work (P-15)
  "space.move": { to: ServerName; accept: { keyId: KeyId; sig: Sig } };   // sig = Ed25519(to's key, "han-accept-move-v1\n" ‖ space ‖ "\n" ‖ receiptHash(head))
}

export interface Contents {
  "space.create": { name: string };
  "member.set": null;
  "policy.set": null;
  "key.add": null;
  "key.revoke": null;
  "message": { text: string; blobs?: BlobRef[]; links?: Link[] };
  "task": { ask: string; blobs?: BlobRef[]; links?: Link[] };
  "task.update": { text?: string; blobs?: BlobRef[] };
  "task.cancel": null;
  "effect.propose": { plan: Plan };
  "effect.approve": { note?: string };
  "effect.claim": null;
  "effect.cancel": { reason?: string; evidence?: BlobRef };   // round 33: the executor's refusal says why (reason ≤ 1024 bytes) and may point at the proof (G-01)
  "effect.outcome": { evidence?: BlobRef };
  "effect.resolve": { evidence: BlobRef };
  "checkpoint": { blobs: BlobRef[] };
  "redact": { reason?: string };
  "space.move": null;
  "session.open": { title?: string };
  "session.close": null;
  "session.input": { text: string; blobs?: BlobRef[] };
  "agent.unguard": null;
}
export type KindName = keyof Bodies;
/** How a session's host treats input from someone other than the session's own actor (P-15, R-24):
 *  auto: like its own input · guarded: the agent chooses guarding (P-15), and the host runs outside input in the harness's stricter mode (R-24). Opening a `guarded` session is one of the two ways an agent becomes guarded;
 *  `policy.sessions.modes` only limits which modes `session.open` may use. */
export type SteerMode = "auto" | "guarded";
/** Extension kinds: `<prefix>.<name>`, prefix [a-z][a-z0-9-]{0,31} not a core prefix, name [a-z0-9][a-z0-9._-]{0,63}.
 *  Bodies and contents are opaque objects to the core; `content.blobs` (BlobRef[]) is the generic blob position (G-01). */
export type ExtensionKind = `${string}.${string}`;
export interface Plan { action: string; target: string; args?: Json; argsBlob?: BlobRef }   // target ≤ 1024 bytes; sha256(JCS(plan)) = planDigest
// Only this shape is normative. What target and args mean belongs to the action and its executor. Informative, from the reference executor:
//   file.write  target = path relative to the executor's sandbox; args = { content: string }
//   http.post   target = URL; args = the JSON body
// An executor refuses (never claims) a plan it can't run or check (X-09).
// Content string limits (UTF-8 bytes): name 256 · text/ask 32768 · note 4096 · reason 1024; blobs/links ≤ 64 each.

// ───────────────────────── policy ─────────────────────────
export interface PolicyDoc {
  visibility: "members" | "public";                     // P-02
  join: "invite" | "open" | "closed";                   // P-03
  membersMayInvite: boolean;
  allowAgents: "any" | "with-operator" | "none";
  approvals: { action: string; approvers: ("owner" | "member" | "guest" | ActorId)[] }[];   // P-08; first match wins; "*" = any action
  separateApprover: boolean;
  budget: Budget | null;                                 // one budget, one unit, per space (P-06); null = costs unlimited
  maxDelegationDepth: number;                            // P-05; default 4
  minDurability: Durability;                             // R-08
  launchWindow: number;                                  // seconds, 15–3600: abandon threshold (P-09) and executor deadline (X-04)
  extensionsMinRole: "guest" | "member" | "owner";       // lowest role that may append extension kinds (§5)
  sessions: { watch: SessionLevel; read: SessionLevel; steer: SessionLevel; modes: SteerMode[]; forceGuarded?: boolean };   // P-15: minimum roles (read, steer no wider than watch); modes limit what session.open may use, nothing else; forceGuarded (default false): every non-human actor (whatever actorKind other than human it has) is guarded for the operator in the profile pinned for each entry, session or not
  servers: { deny: ServerName[] };
}

export type SessionLevel = "owner" | "member" | "guest" | "nobody";

export interface ProfileView {                          // fold of a profile space through `seq` (§4.3)
  actor: ActorId;
  seq: Seq;
  actorKind: ActorKind;
  operator?: ActorId | null;                         // an agent's or service's: always an ActorId, named by the profile's space.create, never changed (§4.3); absent or null only for a human
  operatorConfirmed: boolean;
  keys: { [keyId: KeyId]: { publicKey: string; status: "valid" | "revoked" } };
}
export interface PolicyCtx {
  time: Time;                                       // receipt time
  profiles: { [actor: ActorId]: ProfileView };      // at the seqs pinned in the receipt (P-01). Missing a required actor: decide answers invalid (P-16)
}   // Policy reads from a view only actorKind, operator, operatorConfirmed and keys[keyId].status. fold reads from the receipt only seq and time, and the entry id from receipt.id (R-25).
// `invalid` and `denied` are defined by P-16; `reason` is free text, not part of the contract.
export type Decision = { allow: true } | { allow: false; code: "denied" | "invalid"; reason: string };

export type EffectPhase =
  "open" | "claimed" | "unknown" | "succeeded" | "failed" | "cancelled";

export interface SpaceState {
  space: SpaceId;
  head: Seq;
  home: ServerName;
  policy: PolicyDoc;
  roles: { [actor: ActorId]: Role };
  roleHistory: { [actor: ActorId]: { from: Seq; to?: Seq }[] };   // readable-role intervals in this space's seqs (P-02): held at seq k iff a kept interval has from ≤ k < to (no `to` while it lasts); at most the 64 most recent per actor, older ones are dropped (seqs before the oldest kept `from` are then not readable for a former member)
  tasks: {
    [id: EntryId]: { author: ActorId; to: ActorId[]; open: boolean; state: TaskState | "pending"; cancelRequested: boolean; depth: number; seq: Seq;
                     accepted: ActorId[];           // P-05, P-15: the assignees that accepted it (task.update "accepted"; the author if it assigned itself, from creation), in order; an assignee leaves with its own final update (a closed task still takes it) or when it loses its role (member.set to none or banned); a final update by an owner who isn't an assignee (a force-close, on a closed task too) empties it; a task.cancel changes nothing
                     guardedFor: ActorId[];         // P-15: the parent's guardedFor, then guards(author) when the task was created (Z1); fixed then, never shrinks; at most MAX_GUARDED_FOR − 2 (17) agents, else the task is denied
                     parent?: EntryId; budget?: Budget; left?: Budget };
  };
  effects: {
    [proposal: EntryId]: {
      proposer: ActorId; proposerKey: KeyId; executor: ActorId; action: string; planDigest: Hex; seq: Seq;
      cost?: Budget; repeatable: boolean; task?: EntryId; phase: EffectPhase; cancelRequested: boolean;
      guardedFor: ActorId[];                        // P-15: the named task's guardedFor, then guards(proposer), then guards(executor) at the proposal (Z1); fixed then, never shrinks; with the proposer and executor at most MAX_GUARDED_FOR agents
      expiresAt?: Time;                             // E5: no claim at or after this time
      approvals: { id: EntryId; author: ActorId; keyId: KeyId; actorKind: ActorKind /* the author's, from the profile pinned for the approval; it never changes (P-08) */; expiresAt?: Time; consumedBy?: EntryId }[];   // superseded and expired approvals are removed whenever an entry touches the effect; unconsumed ones are dropped when their author loses its role or becomes guarded, and by an approval's fold when they don't count (P-08); a consumed one is removed once its claim settles; at most MAX_APPROVALS
      claims: {
        id: EntryId; attempt: number; instance: KeyId; time: Time;
        window: number;                             // the launchWindow in force when the claim was ordered (pinned for X-04, P-09)
        approval?: EntryId;                         // the approval this claim consumed (E3): the first usable candidate, never one whose author is guarded at the claim
        guardApprovals?: EntryId[];                 // the guards' approvals it consumed (P-15)
        reserved?: Budget; source?: ["task", EntryId] | ["space", null]; settled: boolean;   // P-06 reservation; settled on every claim (§6.1)
        outcome?: EffectResult; started?: Started;
      }[];
    };
  };
  budgetLeft: { unit: string; amount: SignedInt } | null;   // budget − committed; negative only after a cut (P-06)
  entries: { [id: EntryId]: { author: ActorId; kind: KindName } };   // R-05 resolution, P-04 redact
  sessions: { [id: EntryId]: { actor: ActorId; opened: Time; steerMode: SteerMode; steers: number; task?: EntryId } };   // open sessions only, a closed one leaves the state
  // P-15. A set of actors is a map whose every value is `true`. Besides the guardedFor of tasks and effects there is no other agent-guard state; the guard is never stored, it is the operator in the profile pinned for the entry (guardStatus)
  guarded: { [agent: ActorId]: true };                          // agents that chose guarding (a `guarded` session); sticky, ended only by their guard's agent.unguard (owners' forceGuarded is not stored)
  profile?: {                                                          // profile spaces only (§4.3)
    actorKind: ActorKind; operator?: ActorId | null; operatorConfirmed: boolean;   // operator as in ProfileView (an ActorId for an agent or service, absent or null only for a human)
    keys: { [keyId: KeyId]: { publicKey: string; status: "valid" | "revoked" } };
  };
}

// ───────────────────────── the wire: four calls ─────────────────────────
export type ErrorCode = "denied" | "invalid" | "not_found" | "limit" | "unavailable" | "moved";
export interface WireError { code: ErrorCode; message: string; retryAfterMs?: number /* `limit`, and `unavailable` when waiting helps (R-03a, R-16) */; movedTo?: ServerName }

export interface Limits {
  maxFrameBytes: 1310720;      // 1.25 MiB of JSON text: one frame (G-07)
  maxHeaderBytes: 16384; maxContentBytes: 65536;
  maxBlobBytes: 4294967296;
  maxBlobPartBytes: 786432;    // the binding (§8.2): blobPut part, 768 KiB → 1,048,576 base64 chars, fits a frame
  maxBlobRangeBytes: 786432;   // the binding (§8.2): blobGet answers at most this many bytes per call
  maxStreamItemBytes: 65536;   // T-07: one StreamItem; larger context goes in a blob
  maxFollows: 256;
  maxClockSkewSeconds: 60;     // R-03a: the one future-time tolerance; a receipt dated further ahead of a verifier's clock is a `future-time` fault
  maxStateItems: 1000;         // §3 state: items of each paged map (PagedMap) per page of a state read (a page is also bounded by maxFrameBytes); the rest follows by `cursor`
}
export interface Challenge { challenge: string /* 43 chars: 32 random bytes, base64url; a client accepts 22–128 of [A-Za-z0-9_-] (G-08) */; server: ServerName; versions: number[]; time: Time }
/** G-08: the bytes a `han/1` hello `proof` signs: "han-hello-v2\n" ‖ UTF-8(challenge as sent) ‖ "\n" ‖ UTF-8(dialed) ‖ "\n" ‖ UTF-8(node), `dialed` being the ServerName the client intends (never a node id, address or port)
 *  and `node` the server's node id as the client reached it (round 32, Z4). The key of `keyId` is the connection's node id for a user actor; a server actor's node is in its `sequencer` (SR-13).
 *  The legacy WebSocket carrier keeps "han-hello-v1\n" ‖ challenge ‖ "\n" ‖ dialed. */
/** Policy's numeric constants (P-07 E5, P-08, P-15, P-02): fixed by the protocol, not negotiated by `hello`. */
export interface PolicyConstants {
  MAX_OPEN_APPROVALS: 16;      // countable open approvals by other authors a proposal may hold when a new one by a non-guard arrives: an approval counts while it is an E3 candidate (unconsumed, unexpired, matching the current rule and separateApprover, its author holding a role) and isn't by one of its guards; a guard's is never capped and never counts (P-08)
  MAX_OPEN_SESSIONS: 64;       // open sessions per actor per space (P-15)
  MAX_ATTEMPTS: 64;            // claims per effect: the 65th effect.claim is `denied`, the executor proposes again (P-07 E5)
  MAX_GUARDED_FOR: 19;         // P-15, checked: a proposal whose guardedFor with its proposer and executor would hold more agents is denied (each counted whether guarded or not), so a claim never needs more than 19 guards; a task's guardedFor may hold at most MAX_GUARDED_FOR − 2 (17), so a proposer and an executor always fit
  MAX_UNCONSUMED_APPROVALS: 36;   // derived, never checked (P-08): MAX_OPEN_APPROVALS + 1 by authors that are never guards (one more returned by a not-started outcome; an approval's fold keeps only countable ones besides the guards'), plus one per possible guard (MAX_GUARDED_FOR; operators never change)
  MAX_APPROVALS: 55;           // derived, never checked (P-08): MAX_OPEN_APPROVALS + 1 + 2 × MAX_GUARDED_FOR, each guard holding one unconsumed and one consumed approval
  ROLE_INTERVALS: 64;          // role intervals kept per actor (P-02)
}
/** The maps of a state page, in the order of the cursor and of the fill (§3 state, H3). Every one is cut: a page holds at most Limits.maxStateItems items of each, and the reply as a frame
 *  at most Limits.maxFrameBytes. Only the scalar members and `policy` repeat on every page; every item of a paged map is sent on exactly one page. "profile.keys" is the `keys` map of `profile`. */
export type PagedMap = "roles" | "roleHistory" | "tasks" | "effects" | "sessions" | "guarded" | "profile.keys";   // `guarded` is a set, sent as a map whose values are all `true`; the others are maps
export type StateView = Omit<SpaceState, "entries">;   // the wire state, one page: the PagedMap members are cut as above, each present (possibly `{}`) on every page; the others are repeated; never `limit`
/** `state {space, effect}`: that one effect and nothing else that grows. Never cut, never paged: one effect is bounded (at most 55 approvals, 64 claims, 19 guardedFor agents) and `policy` is bounded by the header limit. */
export type EffectStateView = Pick<StateView, "space" | "head" | "home" | "policy" | "effects">;
/** `state {space, agent}` (K7, P8, Z1): the state's `guarded` cut to that one actor (`{}` when it is not in it) and its `tasks` cut to those whose `accepted` lists the
 *  actor and whose `guardedFor` isn't empty (what carried reads), and nothing else. Never cut: one that would not fit a frame is `limit`, and the host pages the state.
 *  It is a GuardInput: a host passes it to guardStatus as is, with the profiles it pinned, instead of paging the whole state (R-24). */
export type AgentStateView = Pick<StateView, "space" | "head" | "home" | "policy" | "guarded" | "tasks">;
/** What guardStatus reads of a state. SpaceState and AgentStateView both satisfy it. */
export type GuardInput = Pick<SpaceState, "policy" | "guarded" | "tasks">;
/** The answer to `state`: one page. `next` is the cursor of the following page, absent on the last. A cursor is `<head>.<o1>.….<oN>` (decimal, no leading zeros), N = the number of PagedMap members in order, each
 *  the count of that map's items the earlier pages sent (§3). A client treats it as opaque; a server rejects anything else as `invalid`. */
export type StateReply = { state: StateView | EffectStateView | AgentStateView; next?: string };
export type ReadItem =
  | { type: "heads"; heads: Receipt[] }                  // the server's latest receipts, when the reader sent knownHeads (R-15)
  | { type: "record"; record: LogRecord }
  | { type: "signal"; from: ActorId; body: Json };       // T-05, G-09: only on `subscribe` (never on `read`); ephemeral, unordered with records, never authority

export interface Calls {
  hello: { args: { who: ActorId; keyId: KeyId; proof: Sig; version: 1; knownHeads?: Receipt[] };
           ok: { limits: Limits; durability: Durability; time: Time; knownHeads: Receipt[] } };
  append: { args: { entry: Entry; content?: Json }; ok: Receipt };
  read: { args: { space: SpaceId; after: Seq; follow?: boolean;
                  limit?: number /* 1–1000, default 1000; bounds the record backlog; with follow a longer backlog leaves a seq gap the client MUST re-read */; knownHeads?: Receipt[] /* ≤ 256 */ };
          item: ReadItem };                                            // entries only (records, heads, signals on subscribe); the state is the call `state`
  state: { args: { space: SpaceId } & ({ at?: Seq /* the StateView after record `at`; needs read rights at `at` (P-02); 1…head */ }
                                        | { cursor: string /* the `next` of an earlier page; its own head */ }
                                        | { effect: EntryId /* that one effect, EffectStateView */ }
                                        | { agent: ActorId /* that actor's guard facts, AgentStateView; may combine with `at` */; at?: Seq } );   // cursor, effect and agent exclude each other, `at` combines with `agent` only: any other two present is `invalid`, whatever the values
           ok: StateReply };
}   // the four sequencer calls (§3); signals, files, invites and session streams are the transport's (§8, T-01…T-07)
export type CallName = keyof Calls;
export type RequestFrame<C extends CallName = CallName> = { id: number; call: C; args: Calls[C]["args"] };
export type ReplyFrame =
  | { id: number; ok: Json } | { id: number; error: WireError }
  | { id: number; item: ReadItem | StreamItem } | { id: number; end: true };

// ───────────────────────── the QUIC binding's frames (§8.2, `han/1`) ─────────────────────────
// QUIC + TLS 1.3, ALPN "han/1", dialled by node id (G-07). A frame is one I-JSON object, sent as a 4-byte big-endian length then its UTF-8 bytes
// (≤ maxFrameBytes). The server opens a unidirectional control stream and sends a bare `Challenge` (no id) on it. Each call is one bidirectional stream
// opened by the client: one request frame, then the reply (`ok`/`error`, or `read`'s items and `end`). The deliveries of `read` with `follow`, `subscribe`
// and `watch` go on a unidirectional stream the server opens for that call. The client's first call is `hello`; any other call before it, `join` included,
// is `denied` (G-07 (3), X12); after it, admit (N-02) decides which calls reach their rules. Request ids are integers in [1, 2^53), chosen by the client; a streaming call whose id is still open is `invalid`.
// A request frame is exactly {id, call, args} (args an object, closed per call). A frame that is not an I-JSON object is answered `{ id: 0, error: invalid }`
// and the server closes the connection; an integer of 2^53 or more anywhere is answered `invalid` on the request's own id when that is usable (the connection
// stays open), else `{ id: 0 }` and a close; an object that is not a request frame is answered `invalid` with its own id when that is usable, else 0 (G-07).
// Streaming calls run until `cancel` (or a reset call stream), the connection closing, or `{ id, error: unavailable }` (the subscriber fell too far behind).
// Call streams of one connection are handled in stream-id order. `data` is base64url without padding. Every call below is a RequestFrame with the same envelope.
// (Round 29's legacy WebSocket carrier sent the same frames as text messages on one socket, §8.2.)
export interface BindingCalls {
  subscribe: { args: { space: SpaceId; after?: Seq };            // without `after`: live only; with it, replays records after `after`, atomically with registration (no gap)
               item: ReadItem };                                  // records and signals (G-09); a reader who may not read the space gets `denied` (a profile space: records only)
  signal:    { args: { space: SpaceId; body: Json; from?: ActorId }; ok: null };   // T-05, G-04. `from` only on a server's relay: the user it speaks for, who must live on the relaying server; otherwise the connection's actor is the sender
  host:      { args: { space: SpaceId; records: LogRecord[]; head?: Receipt };      // §7.3, G-06: server to server, authenticated as `@:A`, the home: the next contiguous records of the space to move; `head` asks B to sign its acceptance once it holds the chain through it
               ok: { have: Seq; accept?: { keyId: KeyId; sig: Sig } } };
  blobPut:   { args: { ref: BlobRef; offset: number; data: string };   // one part of at most maxBlobPartBytes, at `offset`; parts contiguous, overlaps must agree, a gap or a hash mismatch is `invalid` (G-01, D8)
               ok: { have: number; complete: boolean } };         // bytes held so far from this uploader; `complete` once all of ref.size hash-match (G-01)
  blobGet:   { args: { ref: BlobRef; space: SpaceId; offset?: number; length?: number };
               ok: { data: string; size: number; offset: number } };   // at most maxBlobRangeBytes per call (longer `length` is clamped, 0 is `invalid`, `offset` > size is `invalid`); `size` is the whole blob; no access is `not_found` (G-01)
  stream:    { args: { item: StreamItem }; ok: null };            // G-05: only the session's actor, `n` one more than its last (an identical repeat of the last item is ok)
  watch:     { args: { space: SpaceId; session: EntryId }; item: StreamItem };   // live items only, while sessionAccess grants watch; stays open and silent when the right goes (G-05)
  cancel:    { args: { target: number }; ok: null };              // ends the streaming call whose request id is `target`
  invite:    { args: { space: SpaceId; hash: Hex /* sha256 of the ticket's secret */; expiresAt: Time /* at most 7 days ahead */ }; ok: null };   // N-03: an owner (a member when membersMayInvite) registers a ticket at the home; never in the log
  join:      { args: { space: SpaceId; secret: string }; ok: null };   // N-03: after hello, on a joining node's connection (N-02 c); a known, unused, unexpired secret is used up; anything else `denied` and the connection closes
}
/** G-11: the control call of test mode, and of nothing else. Outside test mode it is not a call: a server answers `invalid` (G-07 (2)) and a production server MUST NOT offer it.
 *  `wall` sets the wall reading (any instant); `steadyAdvance` (integer seconds, ≥ 0) advances the steady reading. Both are independent. Answers the readings after the call. */
export interface TestCalls {
  "test.clock": { args: { wall?: Time; steadyAdvance?: number }; ok: { wall: Time; steady: number } };
  /** Lowers (or restores) the maxFrameBytes the server reports in `hello` and cuts state pages to, so the byte cut of a page can be tested without a 1.25 MiB state. Test mode only (G-11).
   *  `maxFrameBytes`: an integer from 1024 to 1310720. Answers the value in force. The size limit of an incoming frame (G-07) is not lowered. */
  "test.limits": { args: { maxFrameBytes?: number }; ok: { maxFrameBytes: number } };
}
export type BindingCallName = keyof BindingCalls;
/** §7.3: signals in the space being moved. The owner asks; the current home A answers with the target's acceptance of its head. */
export type MoveSignal = { type: "host-request"; to: ServerName } | { type: "host-accept"; to: ServerName; head: Receipt; keyId: KeyId; sig: Sig };
export type BindingRequestFrame<C extends BindingCallName = BindingCallName> = { id: number; call: C; args: BindingCalls[C]["args"] };
export type AnyRequestFrame = RequestFrame | BindingRequestFrame;

// ───────────────────────── modules ─────────────────────────
export interface Log {                                   // M1
  append(entry: Entry, content: Json | undefined): Promise<Receipt>;          // throws WireError
  read(space: SpaceId, after: Seq, opts: { follow?: boolean; limit?: number; knownHeads?: Receipt[] }, as: ActorId): AsyncIterable<ReadItem>;
  state(space: SpaceId, sel: { at?: Seq } | { cursor: string } | { effect: EntryId } | { agent: ActorId; at?: Seq }, as: ActorId): Promise<StateReply>;
}
export interface Policy {                                // M2 — six pure functions (R-11)
  decide(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): Decision;   // after the Log's checks only (R-25); order and codes: P-16
  fold(state: SpaceState | undefined, record: LogRecord, ctx: PolicyCtx): SpaceState;   // the entry's id is record.receipt.id (R-25)
  canRead(state: SpaceState, reader: ActorId, seq: Seq): boolean;                 // P-02
  sessionAccess(state: SpaceState, actor: ActorId, session: EntryId): { watch: boolean; read: boolean; steer: boolean };   // P-15
  /** P-01, the whole pin rule: author, member.set target, the targets of an instruction (session actor, assignees, executor), an unguard's agent, approve/cancel/claim: proposer, executor and guardedFor;
   *  no approver is pinned for an approve (forcing is judged from the actorKind each approval recorded, P-08);
   *  for a claim then the guards from ctx.profiles (P-15: of guardedFor, and of the proposer and executor where guards(proposer) or guards(executor) holds them) and the approval candidates in E3 order up to and including the first usable one by ctx.profiles: a valid key (no candidate's author is guarded: the fold drops such approvals).
   *  The sequencer calls it with the profiles fetched so far until it names none it has not tried: three calls at most. */
  relevantActors(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): ActorId[];
  /** P-15, the one guard function hosts call (R-24) instead of re-deriving it. `guard`: the `operator` of ctx.profiles[agent] if it is confirmed and the agent is not human, else absent.
   *  `forced`: policy.sessions.forceGuarded is on and ctx.profiles[agent] is not human. `guards` (Z1): the agents whose guard it answers to, the agent itself if `forced` or in the
   *  state's set `guarded` (its own choice), then carried(agent), each once. `guarded`: `guards` is not empty. */
  guardStatus(state: GuardInput, ctx: PolicyCtx, agent: ActorId): { guard?: ActorId; guarded: boolean; forced: boolean; guards: ActorId[] };
}
export interface Identity {                              // M3
  profile(actor: ActorId, minSeq?: Seq): Promise<ProfileView>;
  verify(entry: Entry, view: ProfileView): Decision;
}
export interface Verifier {                              // M4
  verify(space: SpaceId): Promise<VerifierStatus>;       // pull, check (R-14), retain profiles and keys, gossip heads (R-15)
}
export type VerifierStatus =
  | { state: "ok"; through: Seq }                        // every record through `through` checked (R-14)
  | { state: "stopped"; through: Seq }                   // could not continue (connection, a seq gap, a profile chain unavailable); never a fault, carries no proof
  | { state: "faulty"; through: Seq; proof: FaultProof }
  | { state: "moved"; through: Seq; to: ServerName };
export type FaultKind = "equivocation" | "wrong-home" | "forbidden-accept" | "bad-signature" | "chain" | "bad-redaction" | "bad-move" | "future-time";
/** A portable misbehaviour proof (R-02, R-14): checkable offline against server keys the checker trusts.
 *  equivocation: `receipts` = the two conflicting receipts. future-time: `receipts` = [the receipt] and `observedAt` (the checker
 *  trusts the prover's clock; the receipt is a fault when dated more than Limits.maxClockSkewSeconds after it). wrong-home found by gossip:
 *  `receipts` = [the receipt] and `chain`. Every other kind: `chain` (the verified records before the fault), `profiles` (the
 *  profile-space chains they pin, by space) and `record` (the offending record); the checker replays them from nothing and must
 *  hit the same kind of fault at `record`. `serverKeys` are what the prover used; a checker that doesn't trust them passes its own.
 *  The proof carries the space's content, so a private space's proof is private. */
export interface FaultProof {
  v: 1; kind: FaultKind; space: SpaceId;
  through: Seq;                                            // the last seq still trusted
  reason: string;                                          // free text
  receipts: Receipt[];
  record?: LogRecord; chain?: LogRecord[]; profiles?: { [space: SpaceId]: LogRecord[] };
  serverKeys: { [server: ServerName]: { [keyId: KeyId]: { publicKey: string; from: Time; until?: Time; compromised?: boolean } } };
  observedAt?: Time;
}
/** R-15, T-05: verifier gossip as in-space signals (ReadItem `signal` bodies). Heads are compared with the receiver's own records;
 *  a fault is adopted only if its proof checks against the receiver's own server keys. Signals carry no authority. */
export type GossipSignal = { type: "heads"; receipts: Receipt[] } | { type: "fault"; proof: FaultProof };
/** T-07: one item of a live session's stream. Published only by the session's actor; never authority (a steer is a
 *  session.input). `n` counts from 1 per session, so gaps are visible; `status` mirrors OSC 7501 program status. */
export type StreamItem = { space: SpaceId; session: EntryId; n: number; time: Time } & (
  | { type: "output"; text: string }                                         // what the session shows: watch
  | { type: "status"; state: "idle" | "working" | "blocked" | "done" | "error"; detail?: string }   // presence: watch
  | { type: "context"; blob: BlobRef; what: "transcript" | "files" | "plan" | "summary" }  // a snapshot: read only; its blob is served by blobGet to anyone with read on the live session (G-01); a summary of the context is context (SR-66)
  | { type: "profile"; name: string /* a profile's name, e.g. "han.status/1" */; body: Json /* defined by that profile; opaque to the server, checked by faces (Z6); never authority */ }   // watch, like status (X11)
);
// ───────────────────────── nodes, invites, well-known (§4.3, §8.2) ─────────────────────────
/** N-01: a node id is a device's Ed25519 public key, in the same base64url form as a profile key's `publicKey` (43 characters, canonical).
 *  A registered device is a `valid` key of its actor's profile; its node id is that key. Every `han/1` connection is dialled by node id (G-07). */
export type NodeId = string;
/** N-03: what an inviter hands a joining person: the home's ServerName, a node of it with its addresses, the space, and a one-time secret (32 random bytes, base64url).
 *  Passed on as the text "han1" + base64url(JCS(ticket)). The home keeps only sha256(secret), registered with `invite`. `node` is a dial hint only (round 32, Z4): before `hello`
 *  the client confirms that `node.node` is listed for `server` in the /.well-known/han it fetches over HTTPS. Whether its issuing owner's device auto-admits is that device's per-ticket choice, off by default (SR-17). */
export interface InviteTicket { server: ServerName /* the ServerName to put in `dialed` (G-08): the space's home when the ticket was made */; node: NodeAddr; space: SpaceId; secret: string }
/** §4.3, N-01 (X10): how to dial a node from the docs alone: its node id, its direct addresses ("ip:port"; IPv6 in brackets) and its relay URLs
 *  (https), at most 16 of each and at least one of the two. Every implementation MUST dial from these; it MAY also use other discovery. */
export interface NodeAddr { node: NodeId; addrs: string[]; relays: string[] }
/** §4.3, D13: `GET /.well-known/han` and `GET /.well-known/han-peers` over HTTPS, both `200 application/json`; an absent optional member is omitted, never null. */
export interface ServerKey { keyId: KeyId; publicKey: string; from: Time; until?: Time; compromised?: true }
export interface WellKnownHan { server: ServerName; sequencer: NodeAddr[] /* the server's HAN nodes, 1–16, with their addresses (X10; round 29: the WebSocket URL, D7) */; keys: ServerKey[] }
export interface WellKnownPeers { peers: { server: ServerName; url: string /* that peer's /.well-known/han */ }[] }
// §8.3 (informative, not part of the standard): a Matrix room may carry the state event "org.han.space" { spaceId: SpaceId; node: NodeId } (state key ""),
// a hint a client verifies against the server the space id names; a notifier bot is a plain HAN actor. The round-29 Matrix binding's shapes are gone.
/** N-02 (X9): the peer filter is one pure function, judged per call before the call's own rules, testable by vectors (conformance/carrier-vectors).
 *  `hello` is present after a successful `hello`: the actor and the node ids that vouch for it (the valid keys of its pinned profile, N-01; for `@:S`
 *  the node ids of S's well-known `sequencer`). `space` is the space the call names (`args.space`, the appended entry's `space`, or the streamed item's `space`); `kind` is the appended entry's `kind` (appends only). */
export interface AdmitConn { node: NodeId; call: string; space?: SpaceId; kind?: string; hello?: { who: ActorId; nodes: NodeId[] } }
/** The spaces the node serves, each as much of its SpaceState as admit reads. A profile space's roles name its actor and `@:server`; an agent's or a service's
 *  profile space also carries the `operator` its profile names (§4.3), whose `member.set` appends there are a member's (N-02, P-03). */
export type AdmitSpaces = { [space: SpaceId]: Pick<SpaceState, "policy" | "roles"> & { operator?: ActorId } };
/** `server`: a node its server's well-known lists, for every call. `member` (round 32, Z7: per space): a registered device of an actor holding a role in the space the call names,
 *  or, for a call naming no space (blobPut), an owner or member of a served space that isn't open-join, or the operator an agent's or a service's profile names, for a
 *  `member.set` append into that profile space (N-02, P-03); the call then judges itself. `open`: anyone, for `hello`, `join`, `cancel`,
 *  any call on a space not held here (§4.1 step 2 answers), reads of public and profile spaces, and appends to `join: "open"` spaces; rate-limited per node. */
export type AdmitVerdict = { allow: true; tier: "member" | "server" | "open" } | { allow: false; reason: string };
export interface PeerFilter {
  admit(conn: AdmitConn, spaces: AdmitSpaces): AdmitVerdict;
}
/** G-07 (round 32, Z5): a typed one-way stream, opened by either end after `hello`. Its first frame is `type` (UTF-8, a profile name, at most 64 bytes); every further frame is
 *  a 4-byte big-endian length (at most maxFrameBytes) and that many bytes, defined, encoded and signed by the profile that registers the type (spec/profiles/README.md;
 *  han.timeline/1: TL-93 envelopes in TL-110 messages). The core signs and checks nothing inside the frames. */
export interface TypedStream { type: string; frames: Uint8Array[] }
/** M7: what HAN needs from a transport (T-01…T-07). The QUIC binding `han/1` (§8.2) implements it. */
export interface TransportBinding {
  publish(space: SpaceId, record: LogRecord): Promise<void>;        // T-01: deliver to the space's members, any order
  subscribe(space: SpaceId, as: ActorId): AsyncIterable<ReadItem>;  // records and signals (T-05) for this actor
  signal(space: SpaceId, from: ActorId, body: Json): Promise<void>;  // T-05: ephemeral, never authority
  blobPut(bytes: Uint8Array, as: ActorId): Promise<BlobRef>;        // T-03
  blobGet(ref: BlobRef, space: SpaceId, as: ActorId): Promise<Uint8Array>;   // T-03, G-01: access via a referencing record, or a context blob of a live session the caller may read
  stream(item: StreamItem): Promise<void>;                          // T-07: by the session's actor's host
  watch(space: SpaceId, session: EntryId, as: ActorId): AsyncIterable<StreamItem>;   // T-07: only what sessionAccess grants `as`
}
/** The client an actor is built with: the sequencer calls plus the transport, authenticated as that actor. */
export interface Client {
  append(entry: Entry, content?: Json): Promise<Receipt>;              // outbox + R-06 retry are the client's job (R-07)
  read(space: SpaceId, after: Seq, opts?: { follow?: boolean; limit?: number; knownHeads?: Receipt[] }): AsyncIterable<ReadItem>;
  state(space: SpaceId, sel?: { at?: Seq } | { cursor: string } | { effect: EntryId } | { agent: ActorId; at?: Seq }): Promise<StateReply>;   // one page; the client follows `next` for the whole state
  signal(space: SpaceId, body: Json): Promise<void>;                    // via the transport (T-05)
  blobPut(bytes: Uint8Array): Promise<BlobRef>;                         // via the transport (T-03)
  blobGet(ref: BlobRef, space: SpaceId): Promise<Uint8Array>;          // via the transport (G-01: a referencing record, or a context blob of a live session you may read); verify the hash
  stream(item: StreamItem): Promise<void>;                              // T-07, for the client's own sessions
  watch(space: SpaceId, session: EntryId): AsyncIterable<StreamItem>;  // T-07: data, never instruction (R-24)
}
/** M5 and M6 (and human apps) are actors: fed every record and signal of the spaces they follow (and the notices on their
 *  own profile), they answer only through their Client. */
export interface Actor {
  deliver(item: ReadItem): Promise<void>;
}
export type ActorFactory = (client: Client, self: ActorId) => Actor;
export type Executor = Actor;                            // M5: hides X-01…X-11, sandboxes, workspaces, the journal
export type AgentHost = Actor;                           // M6: hides models, prompts, memory, the agent framework

export type Json = null | boolean | number | string | Json[] | { [k: string]: Json };
