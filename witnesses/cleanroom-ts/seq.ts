// Clean-room HAN v1 sequencer over the generic binding (§3, §4.1, §8.3). One process = one ServerName.
// Storage: append-only JSON lines with fsync (log.jsonl, content.jsonl, supply.jsonl) plus blobs/<hash>.
import http from "node:http";
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { WebSocketServer, WebSocket } from "ws";
import type { Limits, LogRecord, PolicyCtx, ProfileView, Receipt, SpaceState } from "./han.js";
import {
  isActorId, isHex, isInt, isKeyId, isNonce, isServerName, isSig, isCanonB64u, isSpaceId, isTime, isProfileSpace, profileActorOf, serverOf, spaceServer,
  jcs, sha256, entryId, parseStrict, validateContent, validBlobRef, utf8len, JsonError, isKind, jsonInts,
} from "./wire.js";
import { badBody, canRead, decide, fold, profileViewOf, relevantActors, sessionAccess } from "./policy.js";
import { acceptMessage, b64u, helloMessage, homeSigMessage, importKey, KeyPair, newKey, exportPrivate, receiptHash, signBytes, timeString, unb64u, verifyBytes, sigMessage, parseTimeMs } from "./keys.js";

type A = any;
export type ErrorCode = "denied" | "invalid" | "not_found" | "limit" | "unavailable" | "moved";
export class HanError extends Error {
  constructor(public code: ErrorCode, message: string, public extra: { retryAfterMs?: number; movedTo?: string } = {}) { super(message); }
  wire() { return { code: this.code, message: this.message, ...this.extra }; }
}
const err = (code: ErrorCode, m: string, extra?: { retryAfterMs?: number; movedTo?: string }) => new HanError(code, m, extra);

export const LIMITS: Limits = {
  maxFrameBytes: 1310720, maxHeaderBytes: 16384, maxContentBytes: 65536, maxBlobBytes: 4294967296, maxBlobPartBytes: 786432,
  maxBlobRangeBytes: 786432, maxStreamItemBytes: 65536, maxFollows: 256, maxClockSkewSeconds: 60, maxStateItems: 1000,
};
const CALLS = ["hello", "append", "read", "state", "subscribe", "signal", "host", "blobPut", "blobGet", "stream", "watch", "cancel"];
const READABLE = ["owner", "member", "guest"];
const HEADER_FIELDS = ["v", "space", "kind", "author", "keyId", "body", "contentHash", "refs", "seen", "nonce", "time", "sig"];

export interface SeqOptions {
  server: string;                       // ServerName this sequencer is
  dir: string;                          // data directory
  port?: number;                        // 0 = ephemeral
  clockOffsetMs?: number;               // test hook: starts the steady clock this far from the wall clock
  peers?: Record<string, Record<string, string>>;   // other servers' keys: {server: {keyId: publicKey}} (stands in for /.well-known/han)
  hostSpace?: (space: string) => boolean;           // §4.1 step 2: "decides by its own rules whether to host it"
  appendsPerSecond?: number;            // abuse limit per connection (step 3)
  outboundSignal?: (server: string, space: string, body: A) => void;   // G-03: notices to other servers
  remoteProfile?: (actor: string) => ProfileView | null;
  /** TEST ONLY: answers for every actor, overriding profile spaces (null = no such actor). Used to replay conformance vectors on a live server. */
  profileProvider?: (actor: string) => ProfileView | null | undefined;
  /** TEST ONLY: replaces the steady clock with a settable one (vector replay needs the vectors' receipt times). */
  testClock?: { ms: number; base: number };
  peerUrls?: Record<string, string>;                // served in /.well-known/han-peers; default https://<server>/.well-known/han
  /** G-06: does this server take over hosting this space (a move handover)? Default yes. */
  maxFrameBytes?: number;                           // test mode (G-11 test.limits): the frame limit hello reports and state pages are cut to (default 1,310,720; the receive limit stays)
  maxStateItems?: number;                           // test hook: `effects` and `tasks` per state page (default 1000)
  acceptHosting?: (space: string, from: string) => boolean;
}

interface Space {
  id: string; records: LogRecord[]; state: SpaceState | undefined; lastTime: string;
  views: ProfileView[];                     // profile spaces only: views[seq]
  streams: Map<string, { n: number; last: string }>;
  contextBlobs: Map<string, Map<string, A>>;      // session -> hash -> BlobRef of `context` StreamItems (G-01)
}
interface Sub { conn: Conn; reqId: number; space: string; who: string; signals: boolean }
interface Watch { conn: Conn; reqId: number; space: string; session: string; who: string }

class Conn {
  who?: string; challenge: string; challengeAt = Date.now(); challengeUsed = false; open = new Map<number, () => void>(); follows = 0;
  bucket = { t: Date.now(), n: 0 };
  constructor(public ws: WebSocket) { this.challenge = b64u(randomBytes(32)); }
  send(f: unknown) { if (this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(f)); }
}

export class Sequencer {
  readonly server: string;
  private dir: string;
  private opts: SeqOptions;
  private spaces = new Map<string, Space>();
  private ids = new Map<string, { space: string; seq: number }>();
  private key!: KeyPair;
  private keyFrom!: string;
  private logFd = -1; private contentFd = -1; private supplyFd = -1;
  private subs = new Set<Sub>(); private watches = new Set<Watch>();
  private conns = new Set<Conn>();
  private hosting = new Map<string, Space>();            // G-06: chains received by `host` for spaces this server is not (yet) the home of

  private claimSteady = new Map<string, number>();     // R-16: steady-clock ms at which this process ordered a claim
  private wall0: number; private h0 = process.hrtime.bigint();
  private blobs: Blobs;
  private http!: http.Server; private wss!: WebSocketServer;
  port = 0;
  public keys() { return { keyId: this.key.keyId, publicKey: this.key.publicKey }; }
  public signer() { return this.key; }   // the server actor `@:server` signs profile-space creation (§4.3); provisioning code in the same process uses this

  constructor(opts: SeqOptions) {
    this.opts = opts; this.server = opts.server; this.dir = opts.dir;
    this.wall0 = Date.now() + (opts.clockOffsetMs ?? 0);
    fs.mkdirSync(path.join(this.dir, "blobs"), { recursive: true });
    this.loadKey();
    this.blobs = new Blobs(this.dir);
    this.loadLog();
    this.logFd = fs.openSync(path.join(this.dir, "log.jsonl"), "a");
    this.contentFd = fs.openSync(path.join(this.dir, "content.jsonl"), "a");
  }

  // ───────────── clock (R-03a, R-16) ─────────────
  /** Steady milliseconds since this process started (monotonic; unaffected by wall-clock steps). */
  steadyMs(): number { if (this.opts.testClock) return this.opts.testClock.ms - this.opts.testClock.base; return Number((process.hrtime.bigint() - this.h0) / 1_000_000n); }
  nowMs(): number { return this.opts.testClock ? this.opts.testClock.ms : this.wall0 + this.steadyMs(); }
  now(): string { return timeString(this.nowMs()); }

  // ───────────── keys / storage ─────────────
  private loadKey() {
    const f = path.join(this.dir, "serverkey.json");
    if (fs.existsSync(f)) {
      const j = JSON.parse(fs.readFileSync(f, "utf8"));
      this.key = importKey(j.keyId, j.publicKey, j.d); this.keyFrom = j.from;
    } else {
      this.key = newKey("srv1"); this.keyFrom = this.now();
      fs.writeFileSync(f, JSON.stringify({ keyId: this.key.keyId, publicKey: this.key.publicKey, d: exportPrivate(this.key), from: this.keyFrom }));
    }
  }
  private writeSync(fd: number, line: string) { fs.writeSync(fd, line + "\n"); fs.fsyncSync(fd); }

  private loadLog() {
    const cf = path.join(this.dir, "content.jsonl");
    const contents = new Map<string, A>();
    if (fs.existsSync(cf)) {
      for (const l of fs.readFileSync(cf, "utf8").split("\n")) { if (!l) continue; try { const j = JSON.parse(l); contents.set(j.id, j.content); } catch { /* torn tail */ } }
    }
    const lf = path.join(this.dir, "log.jsonl");
    if (!fs.existsSync(lf)) return;
    const text = fs.readFileSync(lf, "utf8");
    const lines = text.split("\n");
    let good = 0;
    for (let i = 0; i < lines.length; i++) {
      const l = lines[i]; if (!l) continue;
      let j: A;
      try { j = JSON.parse(l); } catch { break; }   // a torn last line was never acknowledged (R-03): drop it
      this.replayRecord(j.entry, j.receipt, contents);
      good += Buffer.byteLength(l) + 1;
    }
    if (good < Buffer.byteLength(text)) fs.truncateSync(lf, good);
  }
  private replayRecord(entry: A, receipt: Receipt, contents: Map<string, A>) {
    let sp = this.spaces.get(entry.space);
    if (!sp) { sp = { id: entry.space, records: [], state: undefined, lastTime: "", views: [], streams: new Map(), contextBlobs: new Map() }; this.spaces.set(entry.space, sp); }
    const head = sp.state?.head ?? 0;
    if (receipt.seq !== head + 1) throw new Error(`log corrupt: ${entry.space} seq ${receipt.seq} after ${head}`);
    const prevHash = head ? receiptHash(sp.records[head - 1].receipt as A) : "";
    if (receipt.prev !== prevHash) throw new Error("log corrupt: prev mismatch");
    if (!verifyBytes(this.key.publicKey, homeSigMessage(receiptHash(receipt as A)), receipt.homeSig) && receipt.home === this.server) throw new Error("log corrupt: home signature");
    const rec: LogRecord = { entry, content: contents.get(receipt.id) ?? null, redacted: false, receipt };
    this.commitMemory(sp, rec, this.ctxFromReceipt(receipt), false);
  }
  /** profile views at the seqs pinned in a receipt, for fold (P-01) */
  private ctxFromReceipt(r: Receipt): PolicyCtx {
    const profiles: A = {};
    for (const [actor, seq] of Object.entries(r.profiles)) { const v = this.viewAt(actor, seq as number); if (v) profiles[actor] = v; }
    return { time: r.time, profiles };
  }
  private viewAt(actor: string, seq: number): ProfileView | null {
    if (actor.startsWith("@:")) return this.serverView(actor);
    const sp = this.spaces.get("profile:" + actor);
    return sp?.views[seq] ?? null;
  }
  private serverView(actor: string): ProfileView | null {
    const S = serverOf(actor);
    const keys: A = {};
    if (S === this.server) keys[this.key.keyId] = { publicKey: this.key.publicKey, status: "valid" };
    else if (this.opts.peers?.[S]) for (const [k, pk] of Object.entries(this.opts.peers[S])) keys[k] = { publicKey: pk, status: "valid" };
    else return null;
    return { actor: actor as A, seq: 0, actorKind: "service", operatorConfirmed: false, keys };
  }

  /** Apply a stored/accepted record to memory. Returns the new state. */
  private commitMemory(sp: Space, rec: LogRecord, ctx: PolicyCtx, live: boolean) {
    const ns = fold(sp.state, rec, ctx);
    sp.state = ns; sp.records.push(rec); sp.lastTime = rec.receipt.time;
    this.ids.set(rec.receipt.id, { space: sp.id, seq: rec.receipt.seq });
    if (ns.profile) sp.views[rec.receipt.seq] = profileViewOf(profileActorOf(sp.id), rec.receipt.seq, ns);
    if ((rec.entry as A).kind === "redact") {
      const tgt = this.ids.get((rec.entry as A).body.target);
      if (tgt && tgt.space === sp.id) {
        const tr = sp.records[tgt.seq - 1];
        tr.redacted = true; tr.content = null; (tr as A).redactedBy = { entry: rec.entry, receipt: rec.receipt };
        if (live) this.rewriteContent();
      }
    }
    if (rec.entry.kind === "effect.claim") this.claimSteady.set(rec.receipt.id, this.steadyMs());
  }
  /** R-04 / P-12: content is deleted "wherever honoured": rewrite content.jsonl without redacted records (atomic rename). */
  private rewriteContent() {
    const lines: string[] = [];
    for (const sp of this.spaces.values()) for (const r of sp.records) if (r.content !== null && !r.redacted) lines.push(JSON.stringify({ id: r.receipt.id, content: r.content }));
    const f = path.join(this.dir, "content.jsonl"), t = f + ".tmp";
    const fd = fs.openSync(t, "w"); fs.writeSync(fd, lines.map((l) => l + "\n").join("")); fs.fsyncSync(fd); fs.closeSync(fd);
    fs.closeSync(this.contentFd); fs.renameSync(t, f); this.contentFd = fs.openSync(f, "a");
  }

  // ───────────── profiles (Identity, §4.3) ─────────────
  /** Identity.profile(actor): the current view and the seq to pin. */
  private profile(actor: string): ProfileView {
    const forced = this.opts.profileProvider?.(actor);
    if (forced === null) throw err("invalid", "unknown actor " + actor);
    if (forced) return forced;
    if (actor.startsWith("@:")) {
      const v = this.serverView(actor);
      if (!v) throw err("unavailable", "no keys known for server " + serverOf(actor));
      return v;
    }
    if (serverOf(actor) === this.server) {
      const sp = this.spaces.get("profile:" + actor);
      if (!sp || !sp.state) throw err("invalid", "unknown actor " + actor);
      return sp.views[sp.state.head];
    }
    const v = this.opts.remoteProfile?.(actor);
    if (!v) throw err("unavailable", "profile of " + actor + " is not reachable");
    return v;
  }

  // ───────────── the append pipeline (§4.1) ─────────────
  append(conn: Conn | null, entryIn: unknown, contentIn: unknown): Receipt {
    if (typeof entryIn !== "object" || entryIn === null || Array.isArray(entryIn)) throw err("invalid", "entry is not an object");
    const entry = entryIn as A;
    // 1. dedupe (R-01), before any other check
    let id: string;
    try { id = entryId(entry); } catch { throw err("invalid", "entry is not canonicalizable"); }
    const known = this.ids.get(id);
    if (known) return this.spaces.get(known.space)!.records[known.seq - 1].receipt;
    // 2. not home?
    if (!isSpaceId(entry.space)) throw err("invalid", "space");
    let sp = this.spaces.get(entry.space);
    if (!sp) {
      const home = spaceServer(entry.space);
      if (home !== this.server) throw err("moved", "this server does not host the space", { movedTo: home });
    } else if (sp.state && sp.state.home !== this.server) throw err("moved", "the space moved", { movedTo: sp.state.home });
    if (!sp && entry.kind === "space.create" && this.opts.hostSpace && !this.opts.hostSpace(entry.space)) throw err("denied", "this sequencer does not host that space");
    // 3. shape
    this.checkShape(entry, contentIn, id);
    const state = sp?.state;
    // `seen ≤ head` and R-05 reference resolution are decided by Policy.decide (P-16 steps 5 and 1), in its order: after the author's key
    if (entry.kind === "space.move") this.checkAccept(sp, entry);
    // 4. the author's profile and the signature, then the other relevant actors (the order of verdicts, E5)
    const time = this.now();
    // `time` is read once: the pin loop, Policy's ctx.time (step 5) and the receipt time are the same moment (P-01, §4.1 step 4(c))
    const fetched: Record<string, A> = {};             // every view fetched (an unreachable actor is tried, not fetched)
    const failed: Record<string, unknown> = {};        // actor -> why it could not be fetched
    const fetch = (actor: string) => {
      if (actor in fetched || actor in failed) return;
      try { fetched[actor] = this.profile(actor); } catch (x) { failed[actor] = x; }
    };
    const isOwn = (actor: string) => state?.profile !== undefined && actor === profileActorOf(state.space) && actor === entry.author;
    fetch(entry.author);
    if (entry.author in failed) throw failed[entry.author];
    const authorKeys0 = state?.profile !== undefined && entry.author === profileActorOf(state.space) ? state.profile.keys : fetched[entry.author]?.keys;
    const k0 = authorKeys0?.[entry.keyId];
    if (k0 && !verifyBytes(k0.publicKey, sigMessage(id), entry.sig)) throw err("invalid", "bad signature");
    let need: string[] = [entry.author];
    if (!state && isProfileSpace(entry.space)) { /* only the author */ }
    else {
      // P-01: Policy's relevantActors is the whole pin rule; this only fetches. Call it with the views fetched so far, fetch what it names
      // that was not tried yet, and call again until it names nothing new: three calls at most. The receipt pins exactly the last list; an
      // actor in it that could not be fetched makes the append `unavailable` (one tried earlier and dropped from the list, e.g. a candidate
      // after the first valid key, does not)
      const viewsOf = () => { const o: A = {}; for (const a in fetched) if (!isOwn(a)) o[a] = fetched[a]; return o; };
      for (let call = 1; ; call++) {
        need = relevantActors(state, entry, { time, profiles: viewsOf() });
        const fresh = need.filter((a) => !(a in fetched) && !(a in failed));
        if (fresh.length === 0) break;
        if (call === 3) throw err("unavailable", "relevantActors still names new actors after three calls");
        for (const a of fresh) fetch(a);
      }
      for (const a of need) if (a in failed) throw failed[a];
    }
    const pins: Record<string, number> = {};
    const profiles: A = {};
    for (const a of need) { pins[a] = fetched[a].seq; if (!isOwn(a)) profiles[a] = fetched[a]; }
    // own-profile-space author: its view is the space's own state; record its seq as the head before this entry
    if (state?.profile !== undefined && entry.author === profileActorOf(state.space)) pins[entry.author] = state.head;
    // 6. policy, with the steady clock rounded down (R-03a)
    const ctx: PolicyCtx = { time, profiles };
    const d = decide(state, entry, ctx);
    let earlyWait = 0;
    if (!d.allow) {
      earlyWait = d.code === "denied" && entry.kind === "effect.outcome" && state ? this.earlyAbandonWait(state, entry, ctx) : 0;
      if (earlyWait <= 0) throw err(d.code, d.reason);
    }
    // 6. rate limits, after Policy
    this.rateLimit(conn);
    // 7. the moment-dependent `unavailable`, in this order
    if (earlyWait > 0) throw err("unavailable", "an owner's abandon that is only early (R-16)", { retryAfterMs: earlyWait });
    // R-16: an owner's abandon needs A seconds of this process's steady clock since it ordered the claim
    if (entry.kind === "effect.outcome" && state) this.checkAbandonElapsed(state, entry);
    // R-03a: receipt time never decreases
    const lastTime = sp?.lastTime ?? "";
    if (lastTime && time < lastTime) throw err("unavailable", "clock is behind the last receipt time; retry", { retryAfterMs: 1000 });
    // R-08: never state a durability level we have not achieved; refuse what we can never provide
    if (state && state.policy.minDurability === "replicated" && entry.kind !== "policy.set" && entry.kind !== "space.move") throw err("unavailable", "cannot reach the space's minDurability");
    if (entry.kind === "space.create" || entry.kind === "policy.set") {
      if (entry.body.policy.minDurability === "replicated") throw err("unavailable", "this sequencer cannot provide minDurability replicated (R-08)");
    }
    // 7. seq, prev, time, durability, profiles; sign
    const seq = (state?.head ?? 0) + 1;
    const prev = state ? receiptHash(sp!.records[state.head - 1].receipt as A) : "";
    const receipt: A = { space: entry.space, seq, id, prev, home: this.server, homeKey: this.key.keyId, time, durability: "local", profiles: pins };
    receipt.homeSig = signBytes(this.key, homeSigMessage(receiptHash(receipt)));
    const rec: LogRecord = { entry, content: entry.contentHash === "" ? null : contentIn as A, redacted: false, receipt };
    // 8. fold first (pure; "never fails for an allowed entry"), then make durable, then commit to memory (R-03)
    fold(state, rec, ctx);
    if (rec.content !== null) this.writeSync(this.contentFd, JSON.stringify({ id, content: rec.content }));
    this.writeSync(this.logFd, JSON.stringify({ entry, receipt }));
    if (!sp) { sp = { id: entry.space, records: [], state: undefined, lastTime: "", views: [], streams: new Map(), contextBlobs: new Map() }; this.spaces.set(entry.space, sp); }
    this.commitMemory(sp, rec, ctx, true);
    // 9. publish
    this.publish(sp, rec);
    return receipt;
  }

  private checkShape(entry: A, content: unknown, id: string) {
    const keys = Object.keys(entry);
    if (keys.length !== HEADER_FIELDS.length || !HEADER_FIELDS.every((k) => k in entry)) throw err("invalid", "header must have exactly the Entry fields");
    if (entry.v !== 1) throw err("invalid", "unsupported version (R-08)");
    if (!isKind(entry.kind)) throw err("invalid", "kind");
    if (!isActorId(entry.author)) throw err("invalid", "author");
    if (!isKeyId(entry.keyId)) throw err("invalid", "keyId");
    if (!(entry.contentHash === "" || isHex(entry.contentHash))) throw err("invalid", "contentHash");
    if (!Array.isArray(entry.refs) || entry.refs.length > 64 || !entry.refs.every(isHex)) throw err("invalid", "refs");
    if (!isInt(entry.seen)) throw err("invalid", "seen");
    if (!isNonce(entry.nonce)) throw err("invalid", "nonce");
    if (!isTime(entry.time)) throw err("invalid", "time");
    if (!isSig(entry.sig)) throw err("invalid", "sig");
    if (typeof entry.body !== "object" || entry.body === null || Array.isArray(entry.body)) throw err("invalid", "body");
    if (utf8len(jcs(entry)) > LIMITS.maxHeaderBytes) throw err("invalid", "header over 16 KiB");
    const bb = badBody(entry.kind, entry.body); if (bb) throw err("invalid", "body: " + bb);
    // D4: public keys are canonical base64url too (Policy sees only the grammar)
    const pks = entry.kind === "key.add" ? [entry.body.publicKey] : entry.kind === "space.create" && entry.body.profile ? [entry.body.profile.key?.publicKey] : [];
    if (!pks.every((k) => k === undefined || isCanonB64u(k))) throw err("invalid", "publicKey is not canonical base64url");
    // content present iff contentHash ≠ "", matches it, fits the kind's schema
    const none = content === undefined || content === null;
    if (entry.contentHash === "") { if (!none) throw err("invalid", "content sent but contentHash is empty"); }
    else {
      if (none) throw err("invalid", "contentHash names content that was not sent");
      let h: string; try { h = sha256(jcs(content)); } catch { throw err("invalid", "content not canonicalizable"); }
      if (h !== entry.contentHash) throw err("invalid", "content does not match contentHash");
    }
    const ce = validateContent(entry.kind, entry.contentHash === "" ? undefined : content, entry.body); if (ce) throw err("invalid", "content: " + ce);
    void id;
  }

  /** §7.3 step 3: the target's accept must verify against the current head. */
  private checkAccept(sp: Space | undefined, entry: A) {
    if (!sp?.state) return;   // decide will reject
    const a = entry.body.accept;
    const pk = this.opts.peers?.[entry.body.to]?.[a.keyId] ?? (entry.body.to === this.server && a.keyId === this.key.keyId ? this.key.publicKey : undefined);
    if (!pk) throw err("invalid", "no known key for the move target");
    const head = sp.records[sp.state.head - 1].receipt;
    if (!verifyBytes(pk, acceptMessage(sp.id, receiptHash(head as A)), a.sig)) throw err("invalid", "accept does not verify against the current head");
  }

  private rateLimit(conn: Conn | null) {
    const cap = this.opts.appendsPerSecond; if (!cap || !conn) return;
    const now = Date.now();
    if (now - conn.bucket.t >= 1000) conn.bucket = { t: now, n: 0 };
    if (++conn.bucket.n > cap) throw err("limit", "rate limit", { retryAfterMs: Math.max(1, 1000 - (now - conn.bucket.t)) });
  }

  /** R-16: an owner's abandon that Policy refuses only because it is early. Returns the remaining wait in ms (0 = not that case). */
  private earlyAbandonWait(state: SpaceState, entry: A, ctx: PolicyCtx): number {
    const f = Object.values(state.effects).flatMap((ef: A) => ef.claims.map((c: A) => ({ ef, c }))).find((x: A) => x.c.id === entry.body.claim);
    if (!f) return 0;
    const A_ = Math.floor((f.c.window * 101 + 98) / 99) + 2;
    const dueMs = Date.parse(f.c.time) + A_ * 1000;
    if (!decide(state, entry, { time: timeString(dueMs), profiles: ctx.profiles }).allow) return 0;   // refused for another reason too: plain denied
    const byReceipt = dueMs - Date.parse(ctx.time);
    const bySteady = A_ * 1000 - (this.steadyMs() - (this.claimSteady.get(f.c.id) ?? 0));
    return Math.max(byReceipt, bySteady, 1);
  }
  /** R-16 */
  private checkAbandonElapsed(state: SpaceState, entry: A) {
    const f = Object.values(state.effects).flatMap((ef: A) => ef.claims.map((c: A) => ({ ef, c }))).find((x: A) => x.c.id === entry.body.claim);
    if (!f) return;
    const own = entry.author === f.ef.executor && entry.keyId === f.c.instance;
    if (own) return;
    const A_ = Math.floor((f.c.window * 101 + 98) / 99) + 2;
    const orderedAt = this.claimSteady.get(f.c.id) ?? 0;        // not ordered by this process: wait a full A from our start
    const waitMs = A_ * 1000 - (this.steadyMs() - orderedAt);
    if (waitMs > 0) throw err("unavailable", "abandon needs " + A_ + " s of this sequencer's steady clock (R-16)", { retryAfterMs: waitMs });
  }

  // ───────────── publish / notices ─────────────
  private publish(sp: Space, rec: LogRecord) {
    const state = sp.state!;
    for (const s of this.subs) {
      if (s.space !== sp.id) continue;
      if (canRead(state, s.who, rec.receipt.seq)) s.conn.send({ id: s.reqId, item: { type: "record", record: rec } });
    }
    const e = rec.entry as A;
    // G-03 notices
    const targets = new Set<string>();
    if (e.kind === "member.set" && READABLE.includes(e.body.role)) targets.add(e.body.actor);
    if ((e.kind === "message" || e.kind === "task") && Array.isArray(e.body.to)) for (const a of e.body.to) if (READABLE.includes(state.roles[a])) targets.add(a);
    for (const a of targets) this.notice(a, sp.id, rec.receipt.seq);
    // G-05: a lost right stops delivery; nothing to do here because every item re-evaluates sessionAccess at the head
  }
  private notice(actor: string, space: string, seq: number) {
    const body = { type: "notice", space, seq };
    const S = serverOf(actor);
    const psp = "profile:" + actor;
    if (S !== this.server) { this.opts.outboundSignal?.(S, psp, body); return; }
    this.deliverNotice(actor, "@:" + this.server, body);
  }
  /** S verifies a notice by reading the space as @:S (P-02), drops what it can't verify, and delivers only to the actor's own sessions. */
  private deliverNotice(actor: string, from: string, body: A) {
    if (body?.type !== "notice" || !isSpaceId(body.space) || !isInt(body.seq)) return;
    const sp = this.spaces.get(body.space);
    if (sp?.state && body.seq <= sp.state.head && sp.state.home === serverOf(from) && canRead(sp.state, "@:" + serverOf(actor), body.seq) && canRead(sp.state, actor, body.seq)) {
      for (const s of this.subs) if (s.signals && s.space === "profile:" + actor && s.who === actor) s.conn.send({ id: s.reqId, item: { type: "signal", from, body } });
    }
  }

  // ───────────── read / subscribe (§3, §8.3) ─────────────
  private readableUpTo(sp: Space, who: string, after: number, limit: number): LogRecord[] {
    const out: LogRecord[] = [];
    for (let seq = after + 1; seq <= sp.records.length && out.length < limit; seq++) if (canRead(sp.state!, who, seq)) out.push(sp.records[seq - 1]);
    return out;
  }
  private headsFor(who: string, known: unknown): Receipt[] {
    if (known === undefined) return [];
    if (!Array.isArray(known) || known.length > 256) throw err("invalid", "knownHeads must be a list of at most 256 receipts");
    const out: Receipt[] = []; const seen = new Set<string>();
    for (const r of known as A[]) {
      if (typeof r?.space !== "string" || !isSpaceId(r.space)) throw err("invalid", "knownHeads receipt");
      if (seen.has(r.space)) continue; seen.add(r.space);
      const sp = this.spaces.get(r.space);
      if (sp?.state && canRead(sp.state, who, sp.state.head)) out.push(sp.records[sp.state.head - 1].receipt);
    }
    return out;
  }

  // ───────────── connections ─────────────
  listen(): Promise<number> {
    return new Promise((resolve) => {
      this.http = http.createServer((req, res) => this.httpHandler(req, res));
      this.wss = new WebSocketServer({ server: this.http, path: "/han", maxPayload: LIMITS.maxFrameBytes });
      this.wss.on("connection", (ws) => this.onConnection(ws));
      this.http.listen(this.opts.port ?? 0, "127.0.0.1", () => { this.port = (this.http.address() as A).port; resolve(this.port); });
    });
  }
  async close() {
    for (const c of this.conns) c.ws.terminate();
    await new Promise<void>((r) => this.wss.close(() => r()));
    await new Promise<void>((r) => this.http.close(() => r()));
    for (const fd of [this.logFd, this.contentFd]) try { fs.closeSync(fd); } catch { /* */ }
  }
  private httpHandler(req: http.IncomingMessage, res: http.ServerResponse) {
    const url = req.url ?? "";
    const out = (j: unknown) => { res.writeHead(200, { "content-type": "application/json; charset=utf-8" }); res.end(JSON.stringify(j)); };
    if (url === "/.well-known/han") return out({ server: this.server, sequencer: `ws://127.0.0.1:${this.port}/han`, keys: [{ keyId: this.key.keyId, publicKey: this.key.publicKey, from: this.keyFrom }] });
    if (url === "/.well-known/han-peers") {
      return out({ peers: Object.keys(this.opts.peers ?? {}).filter((server) => server !== this.server).map((server) => ({ server, url: this.opts.peerUrls?.[server] ?? `https://${server}/.well-known/han` })) });
    }
    res.writeHead(404); res.end();
  }
  private onConnection(ws: WebSocket) {
    const conn = new Conn(ws); this.conns.add(conn);
    conn.send({ challenge: conn.challenge, server: this.server, versions: [1], time: this.now() });
    ws.on("message", (data, isBinary) => {
      if (isBinary) { ws.close(1003, "binary frames are not allowed"); return; }
      this.onFrame(conn, data.toString("utf8"));
    });
    ws.on("close", () => { this.conns.delete(conn); for (const c of [...conn.open.values()]) c(); });
    ws.on("error", () => { /* closed by the close handler */ });
  }
  private reply(conn: Conn, id: number, f: () => unknown) {
    try { conn.send({ id, ok: f() }); } catch (e) { conn.send({ id, error: this.toWire(e) }); }
  }
  private toWire(e: unknown) {
    if (e instanceof HanError) return e.wire();
    return { code: "unavailable", message: "internal error: " + String((e as A)?.message ?? e) };
  }

  private onFrame(conn: Conn, text: string) {
    let f: A;
    const big = { found: false };
    try { f = parseStrict(text, big); } catch (e) {
      // D2: a frame that is not I-JSON: error with id 0, and the connection is closed
      conn.send({ id: 0, error: { code: "invalid", message: "frame does not parse as I-JSON" } });
      conn.ws.close(1007, "not I-JSON");
      return;
    }
    // G-07 (1): text that is not an I-JSON object (an array, a scalar) is treated like unparseable text
    if (typeof f !== "object" || f === null || Array.isArray(f)) {
      conn.send({ id: 0, error: { code: "invalid", message: "frame is not an object" } });
      conn.ws.close(1007, "not an object");
      return;
    }
    // an integer of 2^53 or more anywhere: `invalid` on the request's own id if that is usable (the connection stays open), else id 0 and a close
    if (big.found) {
      if (isInt(f.id) && f.id >= 1) return conn.send({ id: f.id, error: { code: "invalid", message: "integer of 2^53 or more" } });
      conn.send({ id: 0, error: { code: "invalid", message: "integer of 2^53 or more" } });
      conn.ws.close(1007, "integer out of range");
      return;
    }
    // D2: exactly {id, call, args}; id a positive integer, args an object. Otherwise an error on the request's own id (0 if unusable)
    const obj = true;
    const usableId = obj && isInt(f.id) && f.id >= 1;
    const bad = !obj || !usableId || typeof f.call !== "string" || typeof f.args !== "object" || f.args === null || Array.isArray(f.args) || Object.keys(f).length !== 3;
    if (bad) return conn.send({ id: usableId ? f.id : 0, error: { code: "invalid", message: "frame needs exactly id, call, args" } });
    const { id, call, args } = f as { id: number; call: string; args: A };
    if (!CALLS.includes(call)) return conn.send({ id, error: { code: "invalid", message: "unknown call " + call } });   // G-07 (2): before authentication
    const streaming = call === "read" || call === "subscribe" || call === "watch";
    if (streaming && conn.open.has(id)) return conn.send({ id, error: { code: "invalid", message: "request id is still open" } });
    if (call === "hello") return this.reply(conn, id, () => this.hello(conn, args));
    if (!conn.who) return conn.send({ id, error: { code: "denied", message: "hello first" } });
    switch (call) {
      case "append": return this.reply(conn, id, () => {
        if (!("entry" in args) || !Object.keys(args).every((k) => k === "entry" || k === "content")) throw err("invalid", "append args");
        return this.append(conn, args.entry, args.content);
      });
      case "host": return this.reply(conn, id, () => this.doHost(conn, args));
      case "read": return this.doRead(conn, id, args);
      case "state": return this.reply(conn, id, () => this.doState(conn.who!, args));
      case "subscribe": return this.doSubscribe(conn, id, args);
      case "signal": return this.reply(conn, id, () => this.doSignal(conn, args));
      case "blobPut": return this.reply(conn, id, () => this.blobs.put(conn.who!, args));
      case "blobGet": return this.reply(conn, id, () => this.doBlobGet(conn, args));
      case "stream": return this.reply(conn, id, () => this.doStream(conn, args));
      case "watch": return this.doWatch(conn, id, args);
      case "cancel": return this.reply(conn, id, () => {
        if (!("target" in args) || Object.keys(args).length !== 1 || !isInt(args.target)) throw err("invalid", "target");
        const c = conn.open.get(args.target); if (c) c();
        return null;
      });
      default: return conn.send({ id, error: { code: "invalid", message: "unknown call " + call } });
    }
  }

  private hello(conn: Conn, a: A) {
    if (conn.who) throw err("invalid", "hello already done");
    const keys = Object.keys(a);
    if (!["who", "keyId", "proof", "version"].every((k) => k in a) || !keys.every((k) => ["who", "keyId", "proof", "version", "knownHeads"].includes(k))) throw err("invalid", "hello args");
    if (a.version !== 1) throw err("invalid", "unsupported version");
    if (!isActorId(a.who) || !isKeyId(a.keyId) || !isSig(a.proof)) throw err("invalid", "hello args");
    // G-08: the challenge is single use (consumed by the first hello that names it, successful or not) and valid for 60 s
    if (conn.challengeUsed || Date.now() - conn.challengeAt > 60_000) throw err("denied", "challenge used or expired");
    conn.challengeUsed = true;
    // D11: unknown actor is not_found; a bad proof or an unknown/revoked key is denied
    let view: ProfileView;
    try { view = this.profile(a.who); } catch (e) { if (e instanceof HanError && e.code === "invalid" && e.message.startsWith("unknown actor")) throw err("not_found", e.message); throw e; }
    const k = view.keys[a.keyId];
    if (!k || k.status !== "valid") throw err("denied", "key is not valid");
    if (!verifyBytes(k.publicKey, helloMessage(conn.challenge, this.server), a.proof)) throw err("denied", "bad proof");
    const heads = this.headsFor(a.who, a.knownHeads);
    conn.who = a.who;
    return { limits: { ...LIMITS, maxFrameBytes: this.frameLimit(), maxStateItems: this.opts.maxStateItems ?? LIMITS.maxStateItems }, durability: "local", time: this.now(), knownHeads: heads };
  }

  /** D1: a server with no record of a space answers `moved` (to the server in the space id) for every call naming it, unless it is that server (`not_found`). */
  private route(id: unknown): Space {
    if (!isSpaceId(id)) throw err("invalid", "space");
    const sp = this.spaces.get(id);
    if (!sp?.state) {
      const home = spaceServer(id);
      if (home !== this.server) throw err("moved", "this server does not host the space", { movedTo: home });
      throw err("not_found", "no such space");
    }
    if (sp.state.home !== this.server) throw err("moved", "the space moved", { movedTo: sp.state.home });
    return sp;
  }
  private spaceFor(who: string, id: unknown): Space { void who; return this.route(id); }

  private doRead(conn: Conn, id: number, a: A) {
    try {
      const keys = Object.keys(a);
      if (!("space" in a) || !("after" in a) || !keys.every((k) => ["space", "after", "follow", "limit", "knownHeads"].includes(k))) throw err("invalid", "read args");
      if (!isInt(a.after)) throw err("invalid", "after");
      if (a.follow !== undefined && typeof a.follow !== "boolean") throw err("invalid", "follow");
      let limit = 1000;
      if (a.limit !== undefined) { if (!isInt(a.limit) || a.limit < 1 || a.limit > 1000) throw err("invalid", "limit"); limit = a.limit; }
      const who = conn.who!;
      const sp = this.spaceFor(who, a.space);
      this.requireReadable(sp, who);
      const items: A[] = [];
      const heads = this.headsFor(who, a.knownHeads);
      if (a.knownHeads !== undefined) items.push({ type: "heads", heads });
      const probe = this.readableUpTo(sp, who, a.after, limit + 1);
      const recs = probe.slice(0, limit);
      for (const it of items) {
        const text = JSON.stringify({ id, item: it });
        if (text.length > LIMITS.maxFrameBytes) throw err("limit", "item too large for one frame");
        conn.send({ id, item: it });
      }
      for (const r of recs) conn.send({ id, item: { type: "record", record: r } });
      // without follow the stream ends here; with follow a longer backlog leaves a visible gap in seq (the client re-reads it)
      if (!a.follow) { conn.send({ id, end: true }); return; }
      this.addSub(conn, id, sp.id, who, false);       // G-09: `read` with `follow` never carries signals
    } catch (e) { conn.send({ id, error: this.toWire(e) }); }
  }
  /** "A reader who may read no seq gets not_found, the same as for a missing space." */
  private requireReadable(sp: Space, who: string) {
    const st = sp.state!;
    for (let s = 1; s <= st.head; s++) if (canRead(st, who, s)) return;
    throw err("not_found", "no such space");
  }
  /** `state {space, at? | cursor? | effect?}` (§3, G-09): one page of the StateView, or one effect. */
  private doState(who: string, a: A): A {
    const keys = Object.keys(a);
    if (!("space" in a) || !keys.every((k) => ["space", "at", "cursor", "effect", "agent"].includes(k))) throw err("invalid", "state args");
    const sp = this.spaceFor(who, a.space);
    const st = sp.state!;
    this.requireReadable(sp, who);
    // two or more selectors: `invalid` by presence, whatever the values (null included)
    if (["cursor", "effect", "agent"].filter((k) => k in a).length > 1 || ("at" in a && ("cursor" in a || "effect" in a))) throw err("invalid", "cursor, effect and agent exclude each other; at combines with agent only");
    const view = (head: number): A => { const { entries, ...v } = (head === st.head ? st : this.stateAt(sp, head)) as A; void entries; return v; };
    if ("effect" in a) {
      if (typeof a.effect !== "string" || !isHex(a.effect)) throw err("invalid", "`effect` must be an effect id");
      if (!canRead(st, who, st.head)) throw err("denied", "state needs read rights at the head");
      const full = view(st.head);
      if (!(a.effect in full.effects)) throw err("not_found", "no such effect");
      return { state: withSettled({ space: full.space, head: full.head, home: full.home, policy: full.policy, effects: { [a.effect]: full.effects[a.effect] } }) };
    }
    if ("agent" in a) {
      if (typeof a.agent !== "string" || !isActorId(a.agent)) throw err("invalid", "`agent` must be an ActorId");
      let head = st.head;
      if ("at" in a) {
        if (!isInt(a.at) || a.at < 1 || a.at > st.head) throw err("invalid", "`at` must be a seq in 1…head");
        head = a.at;
      }
      if (!canRead(st, who, head)) throw err("denied", "state needs read rights at " + ("at" in a ? "`at`" : "the head"));
      const full = view(head);
      const cut = (m: A) => (a.agent in m ? { [a.agent]: m[a.agent] } : {});
      const tasks = Object.fromEntries(Object.entries(full.tasks as A).filter(([, t]) => (t as A).accepted.includes(a.agent) && (t as A).guardedFor.length > 0));   // round 32 (Z1): what carried reads
      return { state: { space: full.space, head: full.head, home: full.home, policy: full.policy,
        guarded: cut(full.guarded), tasks } };
    }
    if ("cursor" in a) {
      if (typeof a.cursor !== "string" || !/^(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)){7}$/.test(a.cursor)) throw err("invalid", "malformed cursor");
      const nums = a.cursor.split(".").map(Number);
      if (!nums.every(Number.isSafeInteger) || nums[0] < 1 || nums[0] > st.head) throw err("invalid", "malformed cursor");
      if (!canRead(st, who, nums[0])) throw err("denied", "state needs read rights at the cursor's head");
      const full = view(nums[0]);
      const offs = nums.slice(1);
      PAGED.forEach((m, k) => { if (offs[k] > pagedItems(full, m).length) throw err("invalid", "cursor offset beyond the map"); });
      return this.statePage(full, offs);
    }
    let head = st.head;
    if ("at" in a) {
      if (!isInt(a.at) || a.at < 1 || a.at > st.head) throw err("invalid", "`at` must be a seq in 1…head");
      head = a.at;
    }
    if (!canRead(st, who, head)) throw err("denied", "state needs read rights at " + ("at" in a ? "`at`" : "the head"));
    return this.statePage(view(head), PAGED.map(() => 0));
  }
  /** One page of a StateView (§3 state, H3): the PagedMap members are cut (taken in PAGED order, one item at a time while the map has added fewer than maxStateItems and the frame
   *  `{"id": <16 digits>, "ok": {"state": …}}` stays within maxFrameBytes less 256; an item costs its key's JCS bytes plus its value's plus 2; the first item that does not fit closes the page for
   *  every later map). `next` is `<head>.<offsets>` while anything remains. */
  private statePage(full: A, offs: number[]): A {
    const n = this.opts.maxStateItems ?? LIMITS.maxStateItems;
    const v = withSettled(full);
    delete v.entries;
    const targets: Record<string, A>[] = [];
    for (const m of PAGED) {
      if (m === "profile.keys") { if (v.profile !== undefined) { v.profile.keys = {}; targets.push(v.profile.keys); } else targets.push({}); }
      else { v[m] = {}; targets.push(v[m]); }
    }
    const src = PAGED.map((m) => pagedItems(full, m));
    let room = this.frameLimit() - 256 - utf8len(jcs({ id: 1000000000000000, ok: { state: v } }));
    const taken = PAGED.map(() => 0);
    let closed = false;
    for (let k = 0; k < PAGED.length && !closed; k++) {
      for (let q = offs[k]; q < src[k].length && taken[k] < n; q++) {
        const [key, val] = src[k][q];
        const cost = utf8len(jcs(key)) + utf8len(jcs(val)) + 2;
        if (cost > room) { closed = true; break; }
        room -= cost; targets[k][key] = val; taken[k]++;
      }
    }
    const more = PAGED.some((_, k) => offs[k] + taken[k] < src[k].length);
    if (more && taken.every((t) => t === 0)) throw new Error("assert: an item of the state does not fit one page by itself");
    const out: A = { state: v };
    if (more) out.next = [full.head, ...PAGED.map((_, k) => offs[k] + taken[k])].join(".");
    return out;
  }
  private frameLimit(): number { return this.opts.maxFrameBytes ?? LIMITS.maxFrameBytes; }
  /** the SpaceState after record `at`, by replay */
  private stateAt(sp: Space, at: number): SpaceState {
    let st: SpaceState | undefined;
    for (let i = 0; i < at; i++) st = fold(st, sp.records[i], this.ctxFromReceipt(sp.records[i].receipt));
    return st!;
  }
  private addSub(conn: Conn, id: number, space: string, who: string, signals: boolean) {
    if (conn.follows >= LIMITS.maxFollows) throw err("limit", "too many follows");
    const sub: Sub = { conn, reqId: id, space, who, signals };
    this.subs.add(sub); conn.follows++;
    conn.open.set(id, () => { if (this.subs.delete(sub)) conn.follows--; conn.open.delete(id); });
  }
  private doSubscribe(conn: Conn, id: number, a: A) {
    try {
      if (!("space" in a) || !Object.keys(a).every((k) => k === "space" || k === "after")) throw err("invalid", "subscribe args");
      if (a.after !== undefined && !isInt(a.after)) throw err("invalid", "after");
      const who = conn.who!;
      const sp = this.spaceFor(who, a.space);
      // G-09: a reader who may not read the space at its head is denied; anyone may subscribe to a profile space, but only its actor gets signals (G-04)
      const prof = isProfileSpace(sp.id);
      if (!prof && !canRead(sp.state!, who, sp.state!.head)) throw err("denied", "you may not read this space");
      if (a.after !== undefined) for (const r of this.readableUpTo(sp, who, a.after, Infinity)) conn.send({ id, item: { type: "record", record: r } });
      this.addSub(conn, id, sp.id, who, !prof || who === profileActorOf(sp.id));   // registered in the same synchronous turn as the replay: no gap
    } catch (e) { conn.send({ id, error: this.toWire(e) }); }
  }

  // ───────────── move handover, receiving side (§7.3, G-06) ─────────────
  /** `host {space, records, head?}`: only from a server actor `@:A` that is the home of `space` at the last record we hold. */
  private doHost(conn: Conn, a: A): A {
    if (!("space" in a) || !("records" in a) || !Object.keys(a).every((k) => k === "space" || k === "records" || k === "head")) throw err("invalid", "host args");
    if (!isSpaceId(a.space) || !Array.isArray(a.records)) throw err("invalid", "host args");
    if (!conn.who!.startsWith("@:")) throw err("denied", "host is for server actors");
    const from = serverOf(conn.who!);
    this.rateLimit(conn);
    const own = this.spaces.get(a.space);
    if (own?.state) {
      if (own.state.home === this.server) return { have: own.state.head };         // already the home: nothing to take
      throw err("denied", "this server holds the space as a former home");
    }
    let sp = this.hosting.get(a.space);
    const have = sp?.state?.head ?? 0;
    const homeNow = sp?.state ? sp.state.home : spaceServer(a.space);
    if (homeNow !== from) throw err("denied", "the caller is not the home of the space at the last record held");
    if (this.opts.acceptHosting && !this.opts.acceptHosting(a.space, from)) throw err("denied", "this server will not host that space");
    const recs = a.records as A[];
    if (a.head !== undefined && (typeof a.head !== "object" || a.head === null)) throw err("invalid", "head");
    // verify the whole batch against a scratch copy; adopt it only if every record checks (R-14)
    const work: Space = sp ? { ...sp, records: [...sp.records], views: [...sp.views] } : { id: a.space, records: [], state: undefined, lastTime: "", views: [], streams: new Map(), contextBlobs: new Map() };
    for (let i = 0; i < recs.length; i++) {
      const r = recs[i];
      if (typeof r !== "object" || r === null || typeof r.entry !== "object" || r.entry === null || typeof r.receipt !== "object" || r.receipt === null) throw err("invalid", "record");
      const e = r.entry as A, rc = r.receipt as A;
      const expectSeq = (work.state?.head ?? 0) + 1;
      if (rc.seq !== expectSeq) throw err("invalid", `records must be contiguous from ${expectSeq}`);
      if (e.space !== a.space || rc.space !== a.space) throw err("invalid", "record of another space");
      let id: string; try { id = entryId(e); } catch { throw err("invalid", "entry is not canonicalizable"); }
      if (rc.id !== id) throw err("invalid", "receipt id is not the entry id");
      const prevHash = work.state ? receiptHash(work.records[work.state.head - 1].receipt as A) : "";
      if (rc.prev !== prevHash) throw err("invalid", "prev does not link");
      const rcHome = work.state ? work.state.home : spaceServer(a.space);
      if (rc.home !== rcHome) throw err("invalid", "receipt by a server that was not the home");
      const pk = rc.home === this.server ? this.key.publicKey : this.opts.peers?.[rc.home]?.[rc.homeKey];
      if (!pk) throw err("invalid", "no known key for " + rc.home);
      if (!verifyBytes(pk, homeSigMessage(receiptHash(rc)), rc.homeSig)) throw err("invalid", "homeSig does not verify");
      if (work.lastTime && rc.time < work.lastTime) throw err("invalid", "receipt time decreases");
      const content = r.content ?? null;
      if (!r.redacted && (e.contentHash === "" ? content !== null : (() => { try { return sha256(jcs(content)) !== e.contentHash; } catch { return true; } })())) throw err("invalid", "content does not match contentHash");
      const profiles: A = {};
      for (const [actor, seq] of Object.entries(rc.profiles ?? {})) {
        const v = this.viewAt(actor, seq as number) ?? (actor.startsWith("@:") ? null : this.opts.remoteProfile?.(actor) ?? null);
        if (v && v.seq === seq) profiles[actor] = v;
      }
      const authorKey = (work.state?.profile !== undefined && e.author === profileActorOf(a.space) ? work.state.profile.keys : profiles[e.author]?.keys)?.[e.keyId];
      if (authorKey && !verifyBytes(authorKey.publicKey, sigMessage(id), e.sig)) throw err("invalid", "entry signature does not verify");
      const rec: LogRecord = { entry: e, content: r.redacted ? null : content, redacted: !!r.redacted, receipt: rc };
      try {
        const ctx: PolicyCtx = { time: rc.time, profiles };
        if (!work.state) work.state = fold(undefined, rec, ctx); else work.state = fold(work.state, rec, ctx);
      } catch (x) { throw err("invalid", "record does not fold: " + String((x as A)?.message ?? x)); }
      work.records.push(rec); work.lastTime = rc.time;
    }
    const held = work.state?.head ?? 0;
    if (recs.length) this.hosting.set(a.space, work); sp = this.hosting.get(a.space);
    const out: A = { have: held };
    if (a.head !== undefined) {
      const hd = a.head as A;
      if (!sp?.state || hd.space !== a.space || hd.seq !== held || receiptHash(hd) !== receiptHash(sp.records[held - 1].receipt as A)) throw err("invalid", "head is not the last record held");
      out.accept = { keyId: this.key.keyId, sig: signBytes(this.key, acceptMessage(a.space, receiptHash(hd))) };
    }
    // a chain whose last record moves the space to us makes us the home (§7.3 step 4)
    if (sp?.state && sp.state.home === this.server) this.adoptHosted(sp);
    return out;
  }
  private adoptHosted(sp: Space) {
    this.hosting.delete(sp.id);
    for (const r of sp.records) {
      if (r.content !== null && !r.redacted) this.writeSync(this.contentFd, JSON.stringify({ id: r.receipt.id, content: r.content }));
      this.writeSync(this.logFd, JSON.stringify({ entry: r.entry, receipt: r.receipt }));
      this.ids.set(r.receipt.id, { space: sp.id, seq: r.receipt.seq });
    }
    this.spaces.set(sp.id, sp);
  }

  // ───────────── signals (T-05, G-03, G-04) ─────────────
  private doSignal(conn: Conn, a: A): null {
    if (!("space" in a) || !("body" in a) || !Object.keys(a).every((k) => k === "space" || k === "body" || k === "from")) throw err("invalid", "signal args");
    if (!isSpaceId(a.space)) throw err("invalid", "space");
    // D10: `from` only on a connection authenticated as a server actor, for an actor on that server
    let who = conn.who!;
    if ("from" in a) {
      if (!conn.who!.startsWith("@:")) throw err("invalid", "`from` is accepted only from a server actor");
      if (!isActorId(a.from) || a.from.startsWith("@:")) throw err("invalid", "from");
      if (serverOf(a.from) !== serverOf(conn.who!)) throw err("denied", "a server may relay signals only from its own actors");
      who = a.from;
    }
    if (!jsonInts(a.body)) throw err("invalid", "signal body: Json numbers are integers in [0, 2^53)");
    try { if (utf8len(jcs(a.body)) > 65536) throw err("invalid", "signal body over 64 KiB"); } catch (e) { if (e instanceof HanError) throw e; throw err("invalid", "signal body is not canonicalizable"); }
    const sp = this.spaces.get(a.space);
    if (!sp?.state && spaceServer(a.space) !== this.server) throw err("moved", "this server does not host the space", { movedTo: spaceServer(a.space) });
    if (isProfileSpace(a.space)) {
      // only G-03 notices from server actors are accepted into a profile space
      if (!conn.who!.startsWith("@:") || "from" in a) throw err("denied", "only server actors may signal a profile space");
      if (a.body?.type !== "notice") throw err("denied", "profile spaces accept only notices");
      this.deliverNotice(profileActorOf(a.space), conn.who!, a.body);
      return null;
    }
    if (!sp?.state) throw err("not_found", "no such space");
    if (sp.state.home !== this.server) throw err("moved", "the space moved", { movedTo: sp.state.home });
    if (!canRead(sp.state, who, sp.state.head)) throw err("denied", "only actors who may read the space at its head may signal it");
    this.rateLimit(conn);
    for (const s of this.subs) if (s.signals && s.space === sp.id && s.conn !== conn && canRead(sp.state, s.who, sp.state.head)) s.conn.send({ id: s.reqId, item: { type: "signal", from: who, body: a.body } });
    return null;
  }

  // ───────────── blobs (G-01, G-02) ─────────────
  private referencedBlobs(r: LogRecord): A[] {
    if (r.redacted || !r.content) return [];
    const c: A = r.content, out: A[] = [];
    if (Array.isArray(c.blobs)) out.push(...c.blobs);
    if (c.plan?.argsBlob) out.push(c.plan.argsBlob);
    if (c.evidence && (r.entry.kind === "effect.approve" || r.entry.kind === "effect.cancel" ||r.entry.kind === "effect.outcome" || r.entry.kind === "effect.resolve")) out.push(c.evidence);
    return out;
  }
  private doBlobGet(conn: Conn, a: A) {
    if (!("ref" in a) || !("space" in a) || !Object.keys(a).every((k) => ["ref", "space", "offset", "length"].includes(k))) throw err("invalid", "blobGet args");
    const e = validBlobRef(a.ref); if (e) throw err("invalid", e);
    if (!isSpaceId(a.space)) throw err("invalid", "space");
    if (a.offset !== undefined && !isInt(a.offset)) throw err("invalid", "offset");
    if (a.length !== undefined && (!isInt(a.length) || a.length < 1)) throw err("invalid", "length");
    const who = conn.who!;
    const sp = this.route(a.space);
    let allowed = false;
    if (sp.state) {
      // 1. access: a record the caller may read references the blob. 2. supply: the server holds the bytes for that space.
      for (const r of sp.records) {
        if (!this.referencedBlobs(r).some((b: A) => b.hash === a.ref.hash && b.size === a.ref.size)) continue;
        if (!canRead(sp.state, who, r.receipt.seq)) continue;
        if (this.blobs.hasSupply(r.entry.author, a.ref.hash, sp.id)) { allowed = true; break; }
      }
    }
    if (!allowed && sp.state) {
      // a context blob of a live session on which the caller has `read` (G-01 access 1); supply is the session's actor's own upload
      for (const [sid, m] of sp.contextBlobs) {
        const ref = m.get(a.ref.hash); const sess = sp.state.sessions[sid] as A;
        if (ref && ref.size === a.ref.size && sess && sessionAccess(sp.state, who, sid).read && this.blobs.hasSupply(sess.actor, a.ref.hash, sp.id)) { allowed = true; break; }
      }
    }
    if (!allowed && sp.state && canRead(sp.state, who, sp.state.head) && this.blobs.ownRecent(who, a.ref.hash, a.ref.size)) allowed = true;   // D3: uploaders read their own blobs for 24 h, via a space they can read
    if (!allowed) throw err("not_found", "no such blob for this reader and space");
    return this.blobs.get(a.ref.hash, a.offset ?? 0, a.length);
  }

  // ───────────── session streams (G-05) ─────────────
  private doStream(conn: Conn, a: A): null {
    if (!("item" in a) || Object.keys(a).length !== 1) throw err("invalid", "stream args");
    const it = a.item;
    const base = ["space", "session", "n", "time", "type"];
    if (typeof it !== "object" || it === null || !isSpaceId(it.space) || !isHex(it.session) || !isInt(it.n) || it.n < 1 || !isTime(it.time)) throw err("invalid", "StreamItem");
    let extra: string[];
    if (it.type === "output") { if (typeof it.text !== "string") throw err("invalid", "output.text"); extra = ["text"]; }
    else if (it.type === "status") {
      if (!["idle", "working", "blocked", "done", "error"].includes(it.state) || ("detail" in it && typeof it.detail !== "string")) throw err("invalid", "status");
      extra = "detail" in it ? ["state", "detail"] : ["state"];
    } else if (it.type === "context") {
      const e = validBlobRef(it.blob); if (e) throw err("invalid", e);
      if (!["transcript", "files", "plan"].includes(it.what)) throw err("invalid", "context.what");
      extra = ["blob", "what"];
    } else throw err("invalid", "StreamItem.type");
    if (!Object.keys(it).every((k) => base.includes(k) || extra.includes(k))) throw err("invalid", "unknown StreamItem field");
    if (utf8len(jcs(it)) > LIMITS.maxStreamItemBytes) throw err("invalid", "StreamItem over 64 KiB");
    const sp = this.route(it.space);
    if (!(it.session in sp.state!.sessions)) throw err("not_found", "no such session");
    const sess = sp.state!.sessions[it.session] as A;
    if (sess.actor !== conn.who) throw err("denied", "only the session's actor may stream");
    if (!sessionAccess(sp.state!, conn.who!, it.session).steer) throw err("denied", "the session is not live");
    const st = sp.streams.get(it.session) ?? { n: 0, last: "" };
    const text = jcs(it);
    if (it.n === st.n && text === st.last) return null;       // a repeat of the last item is ok
    if (it.n !== st.n + 1) throw err("invalid", `n must be ${st.n + 1}`);
    sp.streams.set(it.session, { n: it.n, last: text });
    if (it.type === "context") { const m = sp.contextBlobs.get(it.session) ?? new Map(); m.set(it.blob.hash, it.blob); sp.contextBlobs.set(it.session, m); }
    for (const w of this.watches) {
      if (w.space !== it.space || w.session !== it.session) continue;
      const acc = sessionAccess(sp.state!, w.who, it.session);     // evaluated at the head when each item is delivered
      if (it.type === "context" ? acc.read : acc.watch) w.conn.send({ id: w.reqId, item: it });
    }
    return null;
  }
  private doWatch(conn: Conn, id: number, a: A) {
    try {
      if (!("space" in a) || !("session" in a) || Object.keys(a).length !== 2) throw err("invalid", "watch args");
      if (!isHex(a.session)) throw err("invalid", "session");
      const sp = this.spaceFor(conn.who!, a.space);
      if (!(a.session in sp.state!.sessions)) throw err("not_found", "no such session");
      if (!sessionAccess(sp.state!, conn.who!, a.session).watch) throw err("denied", "no watch right");
      if (conn.follows >= LIMITS.maxFollows) throw err("limit", "too many follows");
      const w: Watch = { conn, reqId: id, space: a.space, session: a.session, who: conn.who! };
      this.watches.add(w); conn.follows++;
      conn.open.set(id, () => { if (this.watches.delete(w)) conn.follows--; conn.open.delete(id); });
    } catch (e) { conn.send({ id, error: this.toWire(e) }); }
  }

  // ───────────── test/inspection helpers ─────────────
  head(space: string): Receipt | undefined { const sp = this.spaces.get(space); return sp?.state ? sp.records[sp.state.head - 1].receipt : undefined; }
  stateOf(space: string): SpaceState | undefined { return this.spaces.get(space)?.state; }
  /** Provisioning: the server actor `@:server` creates the actor's profile space (§4.3). Goes through the same pipeline as any append. */
  provisionActor(actor: string, kind: "human" | "agent" | "service", key: { keyId: string; publicKey: string }, operator?: string): Receipt {
    const server = "@:" + this.server;
    if (serverOf(actor) !== this.server) throw new Error("not our actor");
    const policy = {
      visibility: "public", join: "closed", membersMayInvite: false, allowAgents: "any", approvals: [], separateApprover: false, budget: null,
      maxDelegationDepth: 0, minDurability: "local", launchWindow: 30, extensionsMinRole: "owner",
      sessions: { watch: "nobody", read: "nobody", steer: "nobody", modes: ["auto"] }, servers: { deny: [] },
    };
    const profile: A = { actorKind: kind, key }; if (operator) profile.operator = operator;
    return this.appendAs(this.key, server, "profile:" + actor, "space.create", { policy, profile }, { name: actor });
  }
  /** Append an entry authored by an actor whose key this process holds (server actor only). */
  appendAs(key: KeyPair, author: string, space: string, kind: string, body: unknown, content?: unknown, extra: { seen?: number; refs?: string[] } = {}): Receipt {
    const state = this.spaces.get(space)?.state;
    const e: A = { v: 1, space, kind, author, keyId: key.keyId, body, contentHash: content === undefined ? "" : sha256(jcs(content)), refs: extra.refs ?? [], seen: extra.seen ?? state?.head ?? 0, nonce: randomNonce(), time: this.now() };
    e.sig = signBytes(key, sigMessage(entryId(e)));
    return this.append(null, e, content);
  }
  appendProfileEntry(actor: string, kind: "key.add" | "key.revoke" | "member.set", body: unknown): Receipt {
    return this.appendAs(this.key, "@:" + this.server, "profile:" + actor, kind, body);
  }
}

import { newNonce as randomNonce } from "./keys.js";

// ───────────── blob store (G-01) ─────────────
const UPLOAD_INCOMPLETE_MAX = 2 * 1024 ** 3, UPLOAD_RECENT_MAX = 16 * 1024 ** 3;   // per uploader (G-01)
class Blobs {
  private uploads = new Map<string, { size: number; type: string; chunks: Buffer[]; have: number }>();
  private complete = new Map<string, { size: number; type: string; at: number }>();   // uploader\nhash
  private fd: number;
  constructor(private dir: string) {
    const f = path.join(dir, "supply.jsonl");
    if (fs.existsSync(f)) for (const l of fs.readFileSync(f, "utf8").split("\n")) { if (!l) continue; try { const j = JSON.parse(l); this.complete.set(j.uploader + "\n" + j.hash, { size: j.size, type: j.type, at: j.at }); } catch { /* torn */ } }
    this.fd = fs.openSync(f, "a");
  }
  put(who: string, a: A) {
    if (!("ref" in a) || !("offset" in a) || !("data" in a) || Object.keys(a).length !== 3) throw err("invalid", "blobPut args");
    const e = validBlobRef(a.ref); if (e) throw err("invalid", e);
    if (!isInt(a.offset)) throw err("invalid", "offset");
    if (typeof a.data !== "string" || !/^[A-Za-z0-9_-]*$/.test(a.data)) throw err("invalid", "data must be base64url without padding");
    const bytes = unb64u(a.data);
    if (b64u(bytes) !== a.data) throw err("invalid", "data is not canonical base64url");
    if (bytes.length > LIMITS.maxBlobPartBytes) throw err("invalid", "part over maxBlobPartBytes");
    const ref = a.ref, key = who + "\n" + ref.hash;
    const done = this.complete.get(key);
    if (done) { if (done.size !== ref.size) throw err("invalid", "size differs from the earlier upload"); return { have: done.size, complete: true }; }
    // G-01: bound, per uploader, the bytes held in incomplete uploads and the completed bytes of the last 24 h; over the bound `limit`
    let incomplete = 0, recent = 0;
    for (const [k, v] of this.uploads) if (k.startsWith(who + "\n")) incomplete += v.have;
    for (const [k, v] of this.complete) if (k.startsWith(who + "\n") && Date.now() - v.at < 24 * 3600 * 1000) recent += v.size;
    if (incomplete + bytes.length > UPLOAD_INCOMPLETE_MAX || (!this.uploads.has(key) && recent + ref.size > UPLOAD_RECENT_MAX)) throw err("limit", "too many upload bytes held for this uploader", { retryAfterMs: 1000 });
    let u = this.uploads.get(key);
    if (!u) { u = { size: ref.size, type: ref.type, chunks: [], have: 0 }; this.uploads.set(key, u); }
    if (u.size !== ref.size || u.type !== ref.type) throw err("invalid", "ref differs from the first part's ref");
    if (a.offset > u.have) throw err("invalid", "gap: part starts after the bytes held (" + u.have + ")");
    if (a.offset + bytes.length > u.size) throw err("invalid", "part runs past ref.size");
    const all = Buffer.concat(u.chunks);
    const overlap = Math.min(u.have - a.offset, bytes.length);
    if (overlap > 0 && !all.subarray(a.offset, a.offset + overlap).equals(bytes.subarray(0, overlap))) throw err("invalid", "overlapping bytes differ from those already held");
    const next = Buffer.concat([all.subarray(0, a.offset), bytes]);
    const keep = next.length >= u.have ? next : Buffer.concat([next, all.subarray(next.length)]);
    u.chunks = [keep]; u.have = keep.length;
    if (u.have === u.size) {
      const h = sha256Bytes(keep);
      if (h !== ref.hash) { this.uploads.delete(key); throw err("invalid", "bytes do not hash to ref.hash"); }
      const bf = path.join(this.dir, "blobs", ref.hash);
      if (!fs.existsSync(bf)) { const fd = fs.openSync(bf + ".tmp", "w"); fs.writeSync(fd, keep); fs.fsyncSync(fd); fs.closeSync(fd); fs.renameSync(bf + ".tmp", bf); }
      const rec = { uploader: who, hash: ref.hash, size: ref.size, type: ref.type, at: Date.now() };
      fs.writeSync(this.fd, JSON.stringify(rec) + "\n"); fs.fsyncSync(this.fd);
      this.complete.set(key, { size: ref.size, type: ref.type, at: rec.at });
      this.uploads.delete(key);
      return { have: u.have, complete: true };
    }
    return { have: u.have, complete: false };
  }
  hasSupply(uploader: string, hash: string, _space: string): boolean { return this.complete.has(uploader + "\n" + hash); }
  ownRecent(who: string, hash: string, size: number): boolean { const c = this.complete.get(who + "\n" + hash); return !!c && c.size === size && Date.now() - c.at < 24 * 3600 * 1000; }
  get(hash: string, offset: number, length: number | undefined) {
    const f = path.join(this.dir, "blobs", hash);
    const size = fs.statSync(f).size;
    if (offset > size) throw err("invalid", "offset past the end");
    const n = Math.min(length ?? LIMITS.maxBlobRangeBytes, LIMITS.maxBlobRangeBytes, size - offset);
    const buf = Buffer.alloc(n);
    const fd = fs.openSync(f, "r"); try { fs.readSync(fd, buf, 0, n, offset); } finally { fs.closeSync(fd); }
    return { data: b64u(buf), size, offset };
  }
}
/** D14: every claim in a StateView carries `settled` (true once its outcome or a resolve settled it, else false). */
function withSettled(view: A): A {
  const v = JSON.parse(JSON.stringify(view)) as A;
  for (const ef of Object.values(v.effects ?? {}) as A[]) for (const c of ef.claims ?? []) c.settled = c.settled === true;
  return v;
}
/** han.ts PagedMap, in the order of the cursor and of the fill */
const PAGED = ["roles", "roleHistory", "tasks", "effects", "sessions", "guarded", "profile.keys"];
const pagedItems = (st: A, m: string): [string, unknown][] => Object.entries(m === "profile.keys" ? (st.profile?.keys ?? {}) : (st[m] ?? {}));
const sha256Bytes = (b: Buffer) => createHash("sha256").update(b).digest("hex");
void parseTimeMs; void isServerName; void JsonError;

// ───────────── CLI ─────────────
if (process.argv[1] && /seq\.(js|ts)$/.test(process.argv[1])) {
  const server = process.argv[2] ?? "han.test", dir = process.argv[3] ?? "./seq-data", port = Number(process.argv[4] ?? 8787);
  const s = new Sequencer({ server, dir, port });
  s.listen().then((p) => console.log(`HAN sequencer ${server} listening on ws://127.0.0.1:${p}/han (data: ${dir})`));
}
