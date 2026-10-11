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
  body: Bodies[K];       // permanent; the only part Policy reads
  contentHash: Hex | ""; // sha256(JCS(content)); "" iff the kind has no content
  refs: EntryId[];       // same-space only (R-05)
  seen: Seq;             // ≤ head at append (R-17)
  nonce: string;         // 26 chars [a-z2-7]; reused only when resending the same entry (R-06)
  time: Time;            // author's clock; informational, never a Policy input (R-11)
  sig: Sig;              // Ed25519(key, "han-sig-v1\n" ‖ id)
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

export interface BlobRef { hash: Hex; size: number; type: string }    // size ≤ 4 GiB; type: a media type, each half [a-z0-9][a-z0-9!#$&^_.+-]{0,63}
export interface Link { space: SpaceId; seq: Seq; id: EntryId }   // cross-space pointer; content only (R-05)
export interface Budget { unit: string; amount: number }
export type SignedInt = number;     // integer in (−2^53, 2^53); used only where stated (budgetLeft)
// Numbers anywhere else, including inside opaque Json (plan.args, extension bodies), are integers in [0, 2^53);
// fractional or negative numbers there are invalid (R-09), so JCS stays exact.          // integer minor units, e.g. { unit: "USD-cent", amount: 500 }

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
  "effect.claim": { proposal: EntryId; attempt: number; instance: KeyId };
  "effect.cancel": { proposal: EntryId };
  "effect.outcome": { claim: EntryId; result: EffectResult; started: Started; cost?: Budget };
  "effect.resolve": { proposal: EntryId; result: "succeeded" | "failed"; cost?: Budget };
  "checkpoint": { through: Seq };
  "redact": { target: EntryId };
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
}
export type KindName = keyof Bodies;
/** Extension kinds: `<prefix>.<name>`, prefix [a-z][a-z0-9-]{0,31} not a core prefix, name [a-z0-9][a-z0-9._-]{0,63}.
 *  Bodies and contents are opaque objects to the core; `content.blobs` (BlobRef[]) is the generic blob position (R-13). */
export type ExtensionKind = `${string}.${string}`;
export interface Plan { action: string; target: string; args?: Json; argsBlob?: BlobRef }   // target ≤ 1024 bytes; sha256(JCS(plan)) = planDigest
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
  servers: { deny: ServerName[] };
}

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
  profiles: { [actor: ActorId]: ProfileView };      // at the seqs pinned in the receipt (P-01)
}
export type Decision = { allow: true } | { allow: false; code: "denied" | "invalid"; reason: string };

export type EffectPhase =
  "awaiting-approval" | "ready" | "claimed" | "unknown" | "succeeded" | "failed" | "cancelled";

export interface SpaceState {
  space: SpaceId;
  head: Seq;
  home: ServerName;
  policy: PolicyDoc;
  roles: { [actor: ActorId]: Role };
  roleHistory: { [actor: ActorId]: { from: Seq; to?: Seq }[] };   // readable-role intervals in this space's seqs (P-02)
  tasks: {
    [id: EntryId]: { author: ActorId; to: ActorId[]; open: boolean; cancelRequested: boolean; depth: number;
                     parent?: EntryId; budget?: Budget; left?: Budget };
  };
  effects: {
    [proposal: EntryId]: {
      proposer: ActorId; proposerKey: KeyId; executor: ActorId; action: string; planDigest: Hex;
      cost?: Budget; repeatable: boolean; task?: EntryId; phase: EffectPhase; cancelRequested: boolean;
      expiresAt?: Time;                             // E5: no claim at or after this time
      approvals: { id: EntryId; author: ActorId; keyId: KeyId; expiresAt?: Time; consumedBy?: EntryId; dropped?: true }[];
      claims: {
        id: EntryId; attempt: number; instance: KeyId; time: Time;
        window: number;                             // the launchWindow in force when the claim was ordered (pinned for X-04, P-09)
        approval?: EntryId;                         // the approval this claim consumed (E3)
        reserved?: Budget; source?: ["task", EntryId] | ["space", null]; settled?: true;   // P-06 reservation
        outcome?: EffectResult; started?: Started;
      }[];
    };
  };
  budgetLeft: { unit: string; amount: SignedInt } | null;   // budget − committed; negative only after a cut (P-06)
  entries: { [id: EntryId]: { author: ActorId; kind: KindName } };   // R-05 resolution, P-04 redact
  profile?: {                                                          // profile spaces only (§4.3)
    actorKind: ActorKind; operator?: ActorId | null; operatorConfirmed: boolean;
    keys: { [keyId: KeyId]: { publicKey: string; status: "valid" | "revoked" } };
  };
}

// ───────────────────────── the wire: six calls ─────────────────────────
export type ErrorCode = "denied" | "invalid" | "not_found" | "limit" | "unavailable" | "moved";
export interface WireError { code: ErrorCode; message: string; retryAfterMs?: number; movedTo?: ServerName }

export interface Limits {
  maxFrameBytes: 1310720;      // 1.25 MiB of JSON text
  maxHeaderBytes: 16384; maxContentBytes: 65536;
  maxBlobBytes: 4294967296;
  maxBlobPartBytes: 786432;    // 768 KiB → 1,048,576 base64 chars, fits a frame with its envelope
  maxBlobRangeBytes: 786432;   // blobGet answers at most this many bytes per call
  maxFollows: 256;
}
export interface Challenge { challenge: string; server: ServerName; versions: number[]; time: Time }
export type StateView = Omit<SpaceState, "entries">;   // the wire state; too big for one frame → `limit`, replay records instead
export type ReadItem =
  | { type: "state"; state: StateView }                  // first item when `state: true` and the reader may read at head
  | { type: "heads"; heads: Receipt[] }                  // the server's latest receipts, when the reader sent knownHeads (R-15)
  | { type: "record"; record: LogRecord }
  | { type: "signal"; from: ActorId; body: Json };       // ephemeral, on follow streams only, unordered with records (R-22)

export interface Calls {
  hello: { args: { who: ActorId; keyId: KeyId; proof: Sig; version: 1; knownHeads?: Receipt[] };
           ok: { limits: Limits; durability: Durability; time: Time; knownHeads: Receipt[] } };
  append: { args: { entry: Entry; content?: Json }; ok: Receipt };
  read: { args: { space: SpaceId; after: Seq; follow?: boolean; state?: boolean; limit?: number /* 1–1000, default 1000 */; knownHeads?: Receipt[] /* ≤ 256 */ }; item: ReadItem };
  signal: { args: { space: SpaceId; body: Json }; ok: null };
  blobPut: { args: { ref: BlobRef; offset: number; bytes: string }; ok: { have: number; complete: boolean } };
  blobGet: { args: { ref: BlobRef; space: SpaceId; start?: number; end?: number /* half-open [start, end); default whole blob; > 768 KiB → limit */ }; ok: { bytes: string } };
}
export type CallName = keyof Calls;
export type RequestFrame<C extends CallName = CallName> = { id: number; call: C; args: Calls[C]["args"] };
export type ReplyFrame =
  | { id: number; ok: Json } | { id: number; error: WireError }
  | { id: number; item: ReadItem } | { id: number; end: true };

// ───────────────────────── modules ─────────────────────────
export interface Log {                                   // M1
  append(entry: Entry, content: Json | undefined): Promise<Receipt>;          // throws WireError
  read(space: SpaceId, after: Seq, opts: { follow?: boolean; state?: boolean; limit?: number }, as: ActorId): AsyncIterable<ReadItem>;
}
export interface Policy {                                // M2 — four pure functions (R-11)
  decide(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): Decision;
  fold(state: SpaceState | undefined, record: LogRecord, ctx: PolicyCtx): SpaceState;
  canRead(state: SpaceState, reader: ActorId, seq: Seq): boolean;                 // P-02
  relevantActors(state: SpaceState | undefined, entry: Entry): ActorId[];         // P-01
}
export interface Identity {                              // M3
  profile(actor: ActorId, minSeq?: Seq): Promise<ProfileView>;
  verify(entry: Entry, view: ProfileView): Decision;
}
export interface Blobs {                                 // M4
  put(ref: BlobRef, offset: number, bytes: Uint8Array, as: ActorId): Promise<{ have: number; complete: boolean }>;
  get(ref: BlobRef, space: SpaceId, as: ActorId, range?: { start: number; end: number }): Promise<Uint8Array>;
}
export interface Federation {                            // M5
  forward(entry: Entry, content: Json | undefined): Promise<Receipt>;
  mirror(space: SpaceId): Promise<MirrorStatus>;         // pull, verify (R-14), store, gossip (R-15); resolves when stopped or faulty
}
export type MirrorStatus =
  | { state: "stopped"; through: Seq }
  | { state: "faulty"; through: Seq; proof: { kind: "equivocation" | "wrong-home" | "forbidden-accept" | "bad-signature" | "chain" | "bad-redaction" | "bad-move"; receipts: Receipt[]; record?: LogRecord } }
  | { state: "moved"; through: Seq; to: ServerName };
export interface Signals {                               // M6
  publish(space: SpaceId, from: ActorId, body: Json): void;
  subscribe(space: SpaceId, as: ActorId): AsyncIterable<{ from: ActorId; body: Json }>;
}
/** The client an actor is built with: the six calls, authenticated as that actor. */
export interface Client {
  append(entry: Entry, content?: Json): Promise<Receipt>;              // outbox + R-06 retry are the client's job (R-07)
  read(space: SpaceId, after: Seq, opts?: { follow?: boolean; state?: boolean }): AsyncIterable<ReadItem>;
  signal(space: SpaceId, body: Json): Promise<void>;
  blobPut(ref: BlobRef, bytes: Uint8Array): Promise<void>;
  blobGet(ref: BlobRef, space: SpaceId): Promise<Uint8Array>;
}
/** M7 and M8 (and human apps) are actors: fed every record and signal of the spaces they follow (and the notices on their
 *  own profile), they answer only through their Client. */
export interface Actor {
  deliver(item: ReadItem): Promise<void>;
}
export type ActorFactory = (client: Client, self: ActorId) => Actor;
export type Executor = Actor;                            // M7: hides X-01…X-07, sandboxes, workspaces, the journal
export type AgentHost = Actor;                           // M8: hides models, prompts, memory, the agent framework

export type Json = null | boolean | number | string | Json[] | { [k: string]: Json };
