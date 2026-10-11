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
    profile?: { actorKind: ActorKind; operator?: ActorId; key: { keyId: KeyId; publicKey: string } };
  };
  "member.set": { actor: ActorId; role: Role | "none" };
  "policy.set": { policy: PolicyDoc };
  "key.add": { keyId: KeyId; publicKey: string };
  "key.revoke": { keyId: KeyId };
  "message": { to?: ActorId[] };
  "task": { to: ActorId[]; budget?: Budget; parent?: EntryId; deadline?: Time };
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
  "session.input": { session: EntryId; release?: true };      // steering, accountable and sequenced; release: the guard ends a hold (P-15)
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
  "effect.cancel": null;
  "effect.outcome": { evidence?: BlobRef };
  "effect.resolve": { evidence: BlobRef };
  "checkpoint": { blobs: BlobRef[] };
  "redact": { reason?: string };
  "space.move": null;
  "session.open": { title?: string };
  "session.close": null;
  "session.input": { text: string; blobs?: BlobRef[] };
}
export type KindName = keyof Bodies;
/** How a session's host treats input from someone other than the session's own actor (P-15, R-24):
 *  auto: like its own input · guarded: the harness's stricter mode, and an outside steer holds the agent (P-15). */
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
  sessions: { watch: SessionLevel; read: SessionLevel; steer: SessionLevel; modes: SteerMode[] };   // P-15: minimum roles (read, steer no wider than watch); allowed modes
  servers: { deny: ServerName[] };
}

export type SessionLevel = "owner" | "member" | "guest" | "nobody";

export interface ProfileView {                          // fold of a profile space through `seq` (§4.3)
  actor: ActorId;
  seq: Seq;
  actorKind: ActorKind;
  operator?: ActorId;
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
  "awaiting-approval" | "ready" | "claimed" | "unknown" | "succeeded" | "failed" | "cancelled";

export interface SpaceState {
  space: SpaceId;
  head: Seq;
  home: ServerName;
  policy: PolicyDoc;
  roles: { [actor: ActorId]: Role };
  roleHistory: { [actor: ActorId]: { from: Seq; to?: Seq }[] };   // readable-role intervals in this space's seqs (P-02): held at seq k iff from ≤ k < to (no `to` while it lasts)
  tasks: {
    [id: EntryId]: { author: ActorId; to: ActorId[]; open: boolean; state: TaskState | "pending"; cancelRequested: boolean; depth: number; seq: Seq;
                     parent?: EntryId; budget?: Budget; left?: Budget };
  };
  effects: {
    [proposal: EntryId]: {
      proposer: ActorId; proposerKey: KeyId; executor: ActorId; action: string; planDigest: Hex; seq: Seq;
      cost?: Budget; repeatable: boolean; task?: EntryId; phase: EffectPhase; cancelRequested: boolean;
      expiresAt?: Time;                             // E5: no claim at or after this time
      guards?: ActorId[];                           // the proposer's guard if it was held when proposing (P-15). The executor's
                                                    // guard is recorded here too when P-15 settles a hold (§6.6)
      approvals: { id: EntryId; author: ActorId; keyId: KeyId; expiresAt?: Time; consumedBy?: EntryId; dropped?: true }[];
      claims: {
        id: EntryId; attempt: number; instance: KeyId; time: Time;
        window: number;                             // the launchWindow in force when the claim was ordered (pinned for X-04, P-09)
        approval?: EntryId;                         // the approval this claim consumed (E3)
        guardApprovals?: EntryId[];                 // the guards' approvals it consumed (P-15)
        reserved?: Budget; source?: ["task", EntryId] | ["space", null]; settled?: true;   // P-06 reservation
        outcome?: EffectResult; started?: Started;
      }[];
    };
  };
  budgetLeft: { unit: string; amount: SignedInt } | null;   // budget − committed; negative only after a cut (P-06)
  entries: { [id: EntryId]: { author: ActorId; kind: KindName } };   // R-05 resolution, P-04 redact
  sessions: { [id: EntryId]: { actor: ActorId; open: boolean; opened: Time; steerMode: SteerMode; steers: number; task?: EntryId;
                                human: boolean; guard?: ActorId } };   // the guard is an agent's confirmed operator at open
  held: { [agent: ActorId]: ActorId };                          // P-15: agent → the human it waits for
  cleared: { [agent: ActorId]: Seq };                           // P-15: `seen` of the guard's last release (instructions up to it are reviewed)
  outside: { [agent: ActorId]: Seq };                           // P-15: seq of the latest outside instruction while guarded (stale releases)
  guarded: { [agent: ActorId]: ActorId };                       // P-15: agent → its guard; the agent's own sticky choice
                                                                // (guardedNow also counts owner-forced guarding, derived)
  profile?: {                                                          // profile spaces only (§4.3)
    actorKind: ActorKind; operator?: ActorId | null; operatorConfirmed: boolean;
    keys: { [keyId: KeyId]: { publicKey: string; status: "valid" | "revoked" } };
  };
}

// ───────────────────────── the wire: three calls ─────────────────────────
export type ErrorCode = "denied" | "invalid" | "not_found" | "limit" | "unavailable" | "moved";
export interface WireError { code: ErrorCode; message: string; retryAfterMs?: number /* `limit`, and `unavailable` when waiting helps (R-03a, R-16) */; movedTo?: ServerName }

export interface Limits {
  maxFrameBytes: 1310720;      // 1.25 MiB of JSON text
  maxHeaderBytes: 16384; maxContentBytes: 65536;
  maxBlobBytes: 4294967296;
  maxBlobPartBytes: 786432;    // generic binding only (§8.3): blobPut part, 768 KiB → 1,048,576 base64 chars, fits a frame
  maxBlobRangeBytes: 786432;   // generic binding only: blobGet answers at most this many bytes per call
  maxStreamItemBytes: 65536;   // T-07: one StreamItem; larger context goes in a blob
  maxFollows: 256;
  maxClockSkewSeconds: 60;     // R-03a: the one future-time tolerance; a receipt dated further ahead of a verifier's clock is a `future-time` fault
}
export interface Challenge { challenge: string; server: ServerName; versions: number[]; time: Time }
export type StateView = Omit<SpaceState, "entries">;   // the wire state; too big for one frame → `limit`, replay records instead
export type ReadItem =
  | { type: "state"; state: StateView }                  // first item when `state: true` and the reader may read at head (or at `at`, then StateView.head = at)
  | { type: "heads"; heads: Receipt[] }                  // the server's latest receipts, when the reader sent knownHeads (R-15)
  | { type: "record"; record: LogRecord }
  | { type: "signal"; from: ActorId; body: Json };       // from the transport (T-05): ephemeral, unordered with records, never authority

export interface Calls {
  hello: { args: { who: ActorId; keyId: KeyId; proof: Sig; version: 1; knownHeads?: Receipt[] };
           ok: { limits: Limits; durability: Durability; time: Time; knownHeads: Receipt[] } };
  append: { args: { entry: Entry; content?: Json }; ok: Receipt };
  read: { args: { space: SpaceId; after: Seq; follow?: boolean; state?: boolean;
                  at?: Seq /* with state: the StateView after record `at`; needs read rights at `at` (P-02); 1…head */;
                  limit?: number /* 1–1000, default 1000; with follow a longer backlog leaves a seq gap the client MUST re-read */; knownHeads?: Receipt[] /* ≤ 256 */ }; item: ReadItem };
}   // the three sequencer calls (§3); signals, files, invites and session streams are the transport's (§8, T-01…T-07)
export type CallName = keyof Calls;
export type RequestFrame<C extends CallName = CallName> = { id: number; call: C; args: Calls[C]["args"] };
export type ReplyFrame =
  | { id: number; ok: Json } | { id: number; error: WireError }
  | { id: number; item: ReadItem | StreamItem } | { id: number; end: true };

// ───────────────────────── the generic binding's frames (§8.3) ─────────────────────────
// One WebSocket, text frames of I-JSON. The server speaks first with a bare `Challenge` (no id). The client's first call
// is `hello`; any other call before it is `denied`. Request ids are integers in [0, 2^53), chosen by the client; a streaming
// call whose id is still open is `invalid`. A frame that doesn't parse, or has no valid id/call/args, is answered with
// `{ id: 0, error: invalid }`. Binary frames close the connection. Streaming calls (`read`, `subscribe`, `watch`) answer with
// `item` frames: `read` without `follow` then ends with `{ id, end: true }`; the others, and `read` with `follow`, run until
// `cancel`, the connection closing, or `{ id, error: unavailable }` (the subscriber fell too far behind: read again).
// `data` is base64url without padding. Every call below is a RequestFrame with the same envelope.
export interface BindingCalls {
  subscribe: { args: { space: SpaceId; after?: Seq };            // without `after`: live only; with it, replays records after `after`, atomically with registration (no gap)
               item: ReadItem };                                  // records and signals; a reader who may not read the space gets `denied`
  signal:    { args: { space: SpaceId; body: Json; from?: ActorId }; ok: null };   // T-05, G-04. `from` only on a server's relay: the user it speaks for, who must live on the relaying server; otherwise the connection's actor is the sender
  host:      { args: { space: SpaceId; records: LogRecord[]; head?: Receipt };      // §7.3, G-06: server to server, authenticated as `@:A`, the home: the next contiguous records of the space to move; `head` asks B to sign its acceptance once it holds the chain through it
               ok: { have: Seq; accept?: { keyId: KeyId; sig: Sig } } };
  blobPut:   { args: { ref: BlobRef; offset: number; data: string };   // one part of at most maxBlobPartBytes, at `offset`
               ok: { have: number; complete: boolean } };         // bytes held so far from this uploader; `complete` once all of ref.size hash-match (G-01)
  blobGet:   { args: { ref: BlobRef; space: SpaceId; offset?: number; length?: number };
               ok: { data: string; size: number; offset: number } };   // at most maxBlobRangeBytes per call; `size` is the whole blob
  stream:    { args: { item: StreamItem }; ok: null };            // G-05: only the session's actor, `n` one more than its last (a repeat of the last item is ok)
  watch:     { args: { space: SpaceId; session: EntryId }; item: StreamItem };   // live items only, while sessionAccess grants watch
  cancel:    { args: { target: number }; ok: null };              // ends the streaming call whose request id is `target`
}
export type BindingCallName = keyof BindingCalls;
/** §7.3: signals in the space being moved. The owner asks; the current home A answers with the target's acceptance of its head. */
export type MoveSignal = { type: "host-request"; to: ServerName } | { type: "host-accept"; to: ServerName; head: Receipt; keyId: KeyId; sig: Sig };
export type BindingRequestFrame<C extends BindingCallName = BindingCallName> = { id: number; call: C; args: BindingCalls[C]["args"] };
export type AnyRequestFrame = RequestFrame | BindingRequestFrame;

// ───────────────────────── modules ─────────────────────────
export interface Log {                                   // M1
  append(entry: Entry, content: Json | undefined): Promise<Receipt>;          // throws WireError
  read(space: SpaceId, after: Seq, opts: { follow?: boolean; state?: boolean; at?: Seq; limit?: number; knownHeads?: Receipt[] }, as: ActorId): AsyncIterable<ReadItem>;
}
export interface Policy {                                // M2 — five pure functions (R-11)
  decide(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): Decision;   // after the Log's checks only (R-25); order and codes: P-16
  fold(state: SpaceState | undefined, record: LogRecord, ctx: PolicyCtx): SpaceState;   // the entry's id is record.receipt.id (R-25)
  canRead(state: SpaceState, reader: ActorId, seq: Seq): boolean;                 // P-02
  sessionAccess(state: SpaceState, actor: ActorId, session: EntryId): { watch: boolean; read: boolean; steer: boolean };   // P-15
  // P-15 hosts also need guardedNow(state, agent) → guard | null; it is derived from `guarded` and the sessions (§6.6).
  relevantActors(state: SpaceState | undefined, entry: Entry, time: Time): ActorId[];   // P-01: author, member.set target, claim: proposer, every approval candidate in order, guards; each once
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
  | { type: "context"; blob: BlobRef; what: "transcript" | "files" | "plan" }  // a snapshot: read only; its blob is served by blobGet to anyone with read on the live session (G-01)
);
// ───────────────────────── the Matrix binding's shapes (§8.2) ─────────────────────────
/** `GET https://<server>/_han/v1/challenge` answers a `Challenge`. `GET .../read` carries `Authorization: HAN <actor> <keyId> <proof>`
 *  and `HAN-Challenge: <challenge>` (the challenge that was signed; single-use, 120 s), with proof = Ed25519(key,
 *  "han-hello-v1
" ‖ challenge ‖ "
" ‖ <server>). It answers `HttpReadResponse`, with HTTP status 400 invalid, 401 denied,
 *  404 not_found, 421 moved, 429 limit, 503 unavailable. No `follow`, no `knownHeads`. */
export interface HttpReadRequest { space: SpaceId; after: Seq; state?: boolean; at?: Seq; limit?: number }   // the query string
export type HttpReadResponse = { items: ReadItem[] } | { error: WireError };
/** Room events. All carry no authority (T-05, R-12); a client honours `org.han.space`, receipts, rejects and `org.han.session` only
 *  from the room's sequencer user (§8.2, trust anchor). */
export interface MatrixEvents {
  "org.han.space": { spaceId: SpaceId; sequencer: string /* the Matrix user @han-seq:<server of the space id> */ };   // state event, state key ""; a room keeps its first binding
  "org.han.entry": { entry: Entry; content?: Json }                        // sent by a member (append)
                 | { id: EntryId; kind: string; author: ActorId };         // the sequencer's header-only copy: nothing P-02 might hide
  "org.han.receipt": { receipt: Receipt };                                   // m.relates_to (m.reference) the entry's event
  "org.han.reject": { id: EntryId; code: "denied" | "invalid" | "not_found" | "moved"; reason: string };   // m.relates_to the entry's event; `unavailable` and `limit` get none
  "org.han.signal": { body: Json };                                          // room-wide, T-05
  "org.han.session": { session: EntryId; watch: string; read: string };      // state event in the space room, state key = the session id; room ids of the two stream rooms
  "org.han.stream": StreamItem & { file?: { url: string; key: Json; iv: string; hashes: { [alg: string]: string }; v: string } };   // in a stream room; `file` = Matrix EncryptedFile info of a context item's blob
}
/** M7: what HAN needs from a transport (T-01…T-07). The Matrix binding implements it with org.han.* room events. */
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
  read(space: SpaceId, after: Seq, opts?: { follow?: boolean; state?: boolean; at?: Seq; limit?: number; knownHeads?: Receipt[] }): AsyncIterable<ReadItem>;
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
