// Clean-room HAN v1 executor (§4.5 M5): X-01 … X-10, one action: file.write.
// An executor is an actor: it is fed ReadItems (`deliver`) and answers only through its client.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import type { LogRecord, ReadItem, SpaceState } from "./han.js";
import { HanClient, WireFault } from "./client.js";
import { exportPrivate, importKey, KeyPair, makeEntry, newKey } from "./keys.js";
import { fold } from "./policy.js";
import { jcs, sha256 } from "./wire.js";

type A = any;
export const STALL_BOUND_MS = 10_000;   // stallBound = 10 s (§4.5)

/** Thrown by a crash-injection point: the executor stops dead, leaving its journal and lock as a killed process would. */
export class Crash extends Error {}
const alive = new Set<string>();       // lock tokens of executors in this process that have not crashed (stands in for pid liveness in in-process tests)

/** X-02: the elapsed-time clock must keep counting through suspend, sleep and VM pause (CLOCK_BOOTTIME). Node has no direct API;
 *  os.uptime() is boot-based on Linux (/proc/uptime) and counts sleep on Windows (GetTickCount64). Other platforms are refused. */
export function elapsedMs(): number {
  if (process.platform !== "linux" && process.platform !== "win32") throw new Error("X-02: no suspend-aware elapsed clock on " + process.platform);
  return os.uptime() * 1000;
}

export interface ExecOptions {
  actor: string;                 // the executor's ActorId
  server: string;                // ServerName it dials
  url: string;                   // ws URL of the sequencer
  dir: string;                   // private state: instance.json, journal.jsonl, lock
  sandbox: string;               // file.write root
  spaces: string[];
  crashAt?: "after-claiming-journal" | "after-receipt" | "after-launching-journal" | "after-launch" | "after-reporting-journal";
  backoffBaseMs?: number; backoffCapMs?: number;   // X-11: exponential back-off before claiming the same effect again after failed/not started (default 1 s, cap 60 s)
  stallBeforeCheckMs?: number;   // test hook: simulate a stalled platform between the receipt and the deadline check
  onEvent?: (e: string) => void;
}
type JEntry = { t: string; claimId?: string; [k: string]: A };

export class Executor {
  private key!: KeyPair;
  private journalFd = -1;
  private lockPath: string; private lockToken = "";
  private client!: HanClient;
  private states = new Map<string, SpaceState>();
  private plans = new Map<string, A>();              // proposal id -> plan (null if redacted)
  private claimed = new Set<string>();               // proposals this instance has ever journaled a claim for
  private queue: Promise<void> = Promise.resolve();
  private dead = false;
  private backoff = new Map<string, number>(); private timers = new Set<ReturnType<typeof setTimeout>>();
  private journal: JEntry[] = [];
  private streamsToCancel: Array<() => Promise<void>> = [];

  constructor(private o: ExecOptions) { this.lockPath = path.join(o.dir, "lock"); }

  /** Create the instance key and an empty journal bound to it. Returns the key to register in the actor's profile (§4.3). */
  static init(dir: string, keyId: string): { keyId: string; publicKey: string } {
    fs.mkdirSync(dir, { recursive: true });
    const k = newKey(keyId), journalId = randomBytes(8).toString("hex");
    fs.writeFileSync(path.join(dir, "instance.json"), JSON.stringify({ keyId, publicKey: k.publicKey, d: exportPrivate(k), journalId }));
    fs.writeFileSync(path.join(dir, "journal.jsonl"), JSON.stringify({ t: "header", journalId, keyId }) + "\n");
    return { keyId, publicKey: k.publicKey };
  }

  // ───────────── X-07: one process per instance key, key never without its journal ─────────────
  private acquire() {
    const kf = path.join(this.o.dir, "instance.json"), jf = path.join(this.o.dir, "journal.jsonl");
    if (!fs.existsSync(kf)) throw new Error("X-07: no instance key; run Executor.init");
    const k = JSON.parse(fs.readFileSync(kf, "utf8"));
    if (!fs.existsSync(jf)) throw new Error("X-07: the instance key is present but its journal is missing: retire the key (key.revoke) and use a new one");
    const first = JSON.parse(fs.readFileSync(jf, "utf8").split("\n")[0] || "null");
    if (!first || first.t !== "header" || first.journalId !== k.journalId || first.keyId !== k.keyId) throw new Error("X-07: the journal does not belong to this instance key");
    this.key = importKey(k.keyId, k.publicKey, k.d);
    this.lockToken = randomBytes(8).toString("hex");
    for (let tries = 0; ; tries++) {
      try {
        const fd = fs.openSync(this.lockPath, "wx");
        fs.writeSync(fd, JSON.stringify({ pid: process.pid, token: this.lockToken })); fs.fsyncSync(fd); fs.closeSync(fd);
        break;
      } catch (e: A) {
        if (e.code !== "EEXIST" || tries > 3) throw e;
        const l = JSON.parse(fs.readFileSync(this.lockPath, "utf8") || "{}");
        if (pidAlive(l.pid) && (l.pid !== process.pid || alive.has(l.token))) throw new Error("X-07: the instance key is held by a running process (pid " + l.pid + ")");
        fs.unlinkSync(this.lockPath);       // stale lock of a dead process
      }
    }
    alive.add(this.lockToken);
    this.journalFd = fs.openSync(jf, "a");
  }
  private releaseLock() { alive.delete(this.lockToken); try { fs.unlinkSync(this.lockPath); } catch { /* */ } }

  private j(e: JEntry) {
    this.journal.push(e);
    fs.writeSync(this.journalFd, JSON.stringify(e) + "\n"); fs.fsyncSync(this.journalFd);      // durable before the step it protects
  }
  private loadJournal() {
    const jf = path.join(this.o.dir, "journal.jsonl");
    const text = fs.readFileSync(jf, "utf8"); let good = 0;
    for (const l of text.split("\n")) {
      if (!l) continue;
      try { const e = JSON.parse(l); if (e.t !== "header") this.journal.push(e); } catch { break; }
      good += Buffer.byteLength(l) + 1;
    }
    if (good < Buffer.byteLength(text)) fs.truncateSync(jf, good);
    for (const e of this.journal) if (e.t === "claiming") this.claimed.add(e.proposal);
  }
  private ev(s: string) { this.o.onEvent?.(s); }
  private crashPoint(p: NonNullable<ExecOptions["crashAt"]>) { if (this.o.crashAt === p) throw new Crash(p); }

  // ───────────── lifecycle ─────────────
  async start() {
    this.acquire();
    try {
      this.loadJournal();
      this.client = await HanClient.connect(this.o.url, this.o.server, this.o.actor, this.key);
      await this.recover();                           // X-06, before anything new
      for (const sp of this.o.spaces) {
        const s = this.client.stream("read", { space: sp, after: 0, follow: true }, (item) => this.enqueue(item));
        this.streamsToCancel.push(s.cancel);
        s.done.catch(() => { /* connection lost: the executor is down; restart recovers */ });
      }
      await this.idle();
    } catch (e) { this.releaseLock(); throw e; }
  }
  async idle() { let q; do { q = this.queue; await q; } while (q !== this.queue); }
  async stop() { this.dead = true; for (const tm of this.timers) clearTimeout(tm); this.timers.clear(); this.client?.close(); this.releaseLock(); try { fs.closeSync(this.journalFd); } catch { /* */ } }
  /** Simulated process death: no cleanup, the lock file stays (its owner is no longer alive). */
  crash() { this.dead = true; for (const tm of this.timers) clearTimeout(tm); this.timers.clear(); alive.delete(this.lockToken); this.client?.terminate(); try { fs.closeSync(this.journalFd); } catch { /* */ } }

  // M5 `deliver(item)`: the executor is fed every record of the spaces it follows.
  async deliver(item: ReadItem): Promise<void> {
    if (this.dead || item.type !== "record") return;
    const rec = item.record as LogRecord;
    const space = rec.entry.space;
    this.ingest(rec);
    await this.scan(space);
  }
  private async scan(space: string) {
    const st = this.states.get(space);
    if (!st) return;
    for (const [pid, ef] of Object.entries(st.effects) as Array<[string, A]>) {
      if (this.dead) return;
      if (ef.executor === this.o.actor && ef.phase === "ready" && !this.claimed.has(pid)) {
        this.claimed.add(pid);
        try { await this.claim(space, pid); } catch (x) { if (x instanceof Crash) { this.crash(); this.ev("crashed at " + x.message); return; } throw x; }
      }
    }
  }
  /** X-11: after a failed / not started outcome, back off exponentially (capped) before claiming the same effect again */
  private scheduleRetry(space: string, pid: string) {
    if (this.dead) return;
    const n = (this.backoff.get(pid) ?? 0) + 1; this.backoff.set(pid, n);
    const delay = Math.min(this.o.backoffCapMs ?? 60_000, (this.o.backoffBaseMs ?? 1000) * 2 ** (n - 1));
    const tm = setTimeout(() => { this.timers.delete(tm); this.claimed.delete(pid); this.queue = this.queue.then(() => this.scan(space)).catch((e) => this.ev("retry failed: " + e)); }, delay);
    this.timers.add(tm);
  }
  /** X-11: an effect the executor won't run is never claimed; the proposer is told with a message whose refs name the proposal */
  private async refuse(space: string, pid: string, why: string) {
    const st = this.states.get(space), ef = st?.effects[pid] as A;
    if (!st || !ef) return;
    const m = makeEntry(this.key, { space, kind: "message", author: this.o.actor, body: { to: [ef.proposer] }, content: { text: `cannot run ${pid.slice(0, 8)}: ${why}` }, refs: [pid], seen: st.head });
    this.j({ t: "refusal", proposal: pid, entry: m.entry, content: m.content });
    try { await this.client.appendReliable(m.entry, m.content); } catch (e) { this.ev("refusal message failed: " + e); }
  }
  /** fold a record into the executor's view of the space (and remember plans); idempotent per seq */
  private ingest(rec: LogRecord) {
    const space = rec.entry.space, prev = this.states.get(space);
    if (prev && rec.receipt.seq <= prev.head) return;
    this.states.set(space, fold(prev, rec, { time: rec.receipt.time, profiles: {} }));
    if (rec.entry.kind === "effect.propose") this.plans.set(rec.receipt.id, rec.redacted ? null : (rec.content as A)?.plan ?? null);
  }
  private enqueue(item: ReadItem) { this.queue = this.queue.then(() => this.deliver(item)).catch((e) => this.ev("deliver failed: " + e)); }

  // ───────────── what the executor can run (X-09) ─────────────
  /** Returns the checked plan or a reason for refusing it. An executor refuses (never claims) a plan it can't run or check. */
  private checkPlan(pid: string, ef: A): { ok: true; plan: A; abs: string; content: string } | { ok: false; why: string } {
    const plan = this.plans.get(pid);
    if (!plan) return { ok: false, why: "plan unavailable (redacted or never seen)" };
    if (sha256(jcs(plan)) !== ef.planDigest) return { ok: false, why: "plan does not hash to planDigest" };       // X-09
    if (plan.action !== "file.write") return { ok: false, why: "unsupported action " + plan.action };
    if (plan.argsBlob !== undefined) return { ok: false, why: "argsBlob not supported" };
    if (typeof plan.args?.content !== "string" || Object.keys(plan.args).length !== 1) return { ok: false, why: "args must be {content: string}" };
    const t: string = plan.target;
    if (t === "" || t.includes("\0") || t.includes("\\") || t.startsWith("/") || /^[A-Za-z]:/.test(t) || t.split("/").some((s) => s === ".." || s === "." || s === "")) return { ok: false, why: "target must be a plain relative path inside the sandbox" };
    const root = path.resolve(this.o.sandbox), abs = path.resolve(root, t);
    if (!abs.startsWith(root + path.sep)) return { ok: false, why: "target escapes the sandbox" };
    return { ok: true, plan, abs, content: plan.args.content };
  }

  // ───────────── the claim protocol (X-01 … X-05) ─────────────
  private async claim(space: string, pid: string) {
    const st = this.states.get(space)!, ef = st.effects[pid] as A;
    const chk = this.checkPlan(pid, ef);
    if (!chk.ok) { this.ev(`refuse ${pid.slice(0, 8)}: ${chk.why}`); await this.refuse(space, pid, chk.why); return; }
    const W = st.policy.launchWindow;
    const { entry, id } = makeEntry(this.key, { space, kind: "effect.claim", author: this.o.actor, body: { proposal: pid, attempt: 1 + ef.claims.length, instance: this.key.keyId, window: W }, seen: st.head });
    const firstSend = elapsedMs();
    // X-02: durably journal claiming(claimId) with the signed claim and the elapsed-clock reading, before the first send (R-07)
    this.j({ t: "claiming", claimId: id, space, proposal: pid, entry, firstSend, boot: os.uptime() });
    this.crashPoint("after-claiming-journal");
    try { await this.client.appendReliable(entry); }
    catch (e) {
      if (e instanceof WireFault && (e.code === "denied" || e.code === "invalid")) { this.j({ t: "refused", claimId: id, code: e.code }); this.ev("claim refused: " + e.message); return; }
      throw e;
    }
    this.crashPoint("after-receipt");
    if (this.dead) return;           // stopped: the journal holds `claiming`, a restart finishes it (X-06)
    // X-03: act only after the claim's receipt is in hand
    // X-09: the plan must hash to the proposal's planDigest (checked above, and again here on the bytes we are about to run)
    const again = this.checkPlan(pid, ef);
    if (!again.ok) { await this.report(space, pid, id, "failed", "no", "plan check failed: " + again.why); return; }
    this.j({ t: "launching", claimId: id });                                           // X-04
    this.crashPoint("after-launching-journal");
    if (this.o.stallBeforeCheckMs) await new Promise((r) => setTimeout(r, this.o.stallBeforeCheckMs));
    if (this.dead) return;
    // X-04: as the last step before launching, less than W − stallBound must have passed on the elapsed clock since the first send
    if (elapsedMs() - firstSend >= W * 1000 - STALL_BOUND_MS) {
      this.j({ t: "expired", claimId: id });
      await this.report(space, pid, id, "failed", "no", "deadline passed before launch");
      return;
    }
    let result: "succeeded" | "failed" = "succeeded", detail = "";
    try { writeFileDurably(again.abs, again.content); } catch (e: A) { result = "failed"; detail = String(e?.message ?? e); }       // the launch
    this.j({ t: "launched", claimId: id, result, detail });
    this.crashPoint("after-launch");
    await this.report(space, pid, id, result, "yes", detail, again);
  }

  // ───────────── reporting (X-05, X-10) ─────────────
  private async evidence(obj: unknown): Promise<A> { return this.client.blobPut(Buffer.from(JSON.stringify(obj), "utf8"), "application/json"); }

  private async report(space: string, pid: string, claimId: string, result: "succeeded" | "failed" | "unknown", started: "yes" | "no" | "unknown", note: string, chk?: A) {
    const st = this.states.get(space);
    const ev = await this.evidence({ action: "file.write", claim: claimId, result, started, note, path: chk?.plan?.target, sha256: chk ? sha256(chk.content) : undefined, bytes: chk ? Buffer.byteLength(chk.content) : undefined });
    const { entry, content } = makeEntry(this.key, { space, kind: "effect.outcome", author: this.o.actor, body: { claim: claimId, result, started }, content: { evidence: ev }, seen: st?.head ?? 0 });
    // R-07: the signed report is stored before its first send
    this.j({ t: "reporting", claimId, entry, content });
    this.crashPoint("after-reporting-journal");
    await this.send(space, pid, claimId, entry, content, result, started);
  }
  private async send(space: string, pid: string, claimId: string, entry: A, content: A, result: string, started: string) {
    try {
      await this.client.appendReliable(entry, content); this.j({ t: "reported", claimId }); this.ev(`reported ${result}/${started}`);
      if (result === "failed" && started === "no") this.scheduleRetry(space, pid);
    }
    catch (e) {
      if (!(e instanceof WireFault) || (e.code !== "denied" && e.code !== "invalid")) throw e;
      // X-10: the outcome was denied (an owner abandoned the claim): report what really happened with effect.resolve + evidence
      const ef = await this.freshEffect(space, pid);
      if (ef?.phase === "unknown") {
        const ran = this.journal.some((x) => x.t === "launched" && x.claimId === claimId);
        const ok = this.journal.some((x) => x.t === "launched" && x.claimId === claimId && x.result === "succeeded");
        const jtext = fs.readFileSync(path.join(this.o.dir, "journal.jsonl"), "utf8");
        const evid = await this.client.blobPut(Buffer.from(jtext, "utf8"), "application/x-ndjson");   // the journal itself is the evidence
        const st = this.states.get(space);
        const r = makeEntry(this.key, { space, kind: "effect.resolve", author: this.o.actor, body: { proposal: pid, result: ok ? "succeeded" : "failed" }, content: { evidence: evid }, seen: st?.head ?? 0 });
        this.j({ t: "resolving", claimId, entry: r.entry, content: r.content, ran });
        await this.client.appendReliable(r.entry, r.content);
        this.j({ t: "resolved", claimId });
        this.ev("outcome refused after abandon; resolved " + (ok ? "succeeded" : "failed"));
      } else { this.j({ t: "reported", claimId, refused: e.code }); this.ev("outcome refused: " + e.message); }
    }
  }
  private async freshEffect(space: string, pid: string): Promise<A> {
    const items = await this.client.read(space, this.states.get(space)?.head ?? 0);
    for (const r of this.client.records(items)) this.ingest(r);
    return this.states.get(space)?.effects[pid];
  }

  // ───────────── X-06: no launch after restart ─────────────
  private async recover() {
    const by = new Map<string, A>();
    for (const e of this.journal) {
      if (!e.claimId) continue;
      const c = by.get(e.claimId) ?? { claimId: e.claimId };
      by.set(e.claimId, c);
      if (e.t === "claiming") Object.assign(c, { claiming: e });
      else c[e.t] = e;
    }
    for (const c of by.values()) {
      if (!c.claiming || c.refused || c.reported || c.resolved) continue;
      const { space, proposal: pid, entry } = c.claiming;
      this.ev(`recover claim ${c.claimId.slice(0, 8)}`);
      if (c.reporting) { await this.send(space, pid, c.claimId, c.reporting.entry, c.reporting.content, "recovered", "recovered"); continue; }
      if (c.resolving) { try { await this.client.appendReliable(c.resolving.entry, c.resolving.content); this.j({ t: "resolved", claimId: c.claimId }); } catch { /* definitive */ } continue; }
      if (!c.launching) {
        // claiming without launching: finish the claim's append (R-06); if it has a receipt, report failed / not started
        try { await this.client.appendReliable(entry); }
        catch (e) { if (e instanceof WireFault && (e.code === "denied" || e.code === "invalid")) { this.j({ t: "refused", claimId: c.claimId, code: e.code }); continue; } throw e; }
        await this.syncState(space);
        await this.report(space, pid, c.claimId, "failed", "no", "restart before launch (X-06)");
      } else if (c.expired) {
        await this.syncState(space);
        await this.report(space, pid, c.claimId, "failed", "no", "deadline expired before launch (X-06)");
      } else if (c.launched) {
        await this.syncState(space);
        await this.report(space, pid, c.claimId, c.launched.result, "yes", c.launched.detail || "recovered from journal");
      } else {
        // launching with neither expired nor an outcome: look the result up in the outside system if possible, else unknown/unknown
        await this.syncState(space);
        const found = await this.lookup(space, pid);
        if (found) await this.report(space, pid, c.claimId, "succeeded", "yes", "found the planned file in the sandbox (X-06 lookup)");
        else await this.report(space, pid, c.claimId, "unknown", "unknown", "launching was journaled but no result: unknown (X-06)");
      }
    }
  }
  private async syncState(space: string) { await this.freshEffect(space, ""); }
  private async lookup(space: string, pid: string): Promise<boolean> {
    // plans come from the log: re-read the proposal record
    const items = await this.client.read(space, 0);
    for (const r of this.client.records(items)) if (r.receipt.id === pid && r.content?.plan) this.plans.set(pid, r.content.plan);
    const ef = this.states.get(space)?.effects[pid];
    const chk = ef ? this.checkPlan(pid, ef) : { ok: false as const, why: "" };
    if (!chk.ok) return false;
    try { return fs.readFileSync(chk.abs, "utf8") === chk.content; } catch { return false; }
  }
}

function writeFileDurably(abs: string, content: string) {
  fs.mkdirSync(path.dirname(abs), { recursive: true });
  const tmp = abs + ".han-tmp-" + randomBytes(4).toString("hex");
  const fd = fs.openSync(tmp, "w"); fs.writeSync(fd, content); fs.fsyncSync(fd); fs.closeSync(fd);
  fs.renameSync(tmp, abs);
}
function pidAlive(pid: number): boolean { try { process.kill(pid, 0); return true; } catch (e: A) { return e.code === "EPERM"; } }
