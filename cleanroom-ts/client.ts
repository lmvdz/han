// Clean-room HAN v1 generic-binding client (§3, §8.3): one authenticated WebSocket, request/reply and streaming calls.
import WebSocket from "ws";
import { createHash } from "node:crypto";
import { b64u, helloMessage, KeyPair, signBytes, unb64u } from "./keys.js";
import { parseStrict } from "./wire.js";

type A = any;
export class WireFault extends Error {
  constructor(public code: string, message: string, public retryAfterMs?: number, public movedTo?: string) { super(`${code}: ${message}`); }
}
const sha = (b: Buffer) => createHash("sha256").update(b).digest("hex");

export class HanClient {
  private ws!: WebSocket;
  private nextId = 1;
  private pending = new Map<number, { ok: (v: A) => void; fail: (e: Error) => void }>();
  private streams = new Map<number, { item: (i: A) => void; end: () => void; fail: (e: Error) => void }>();
  closed = false;
  limits: A; who!: string;
  private challengeWaiter!: (c: A) => void;

  /** `dialed` is the ServerName the client means to talk to; the client aborts if the challenge names another (§3 hello). */
  static async connect(url: string, dialed: string, who: string, key: KeyPair, knownHeads?: A[]): Promise<HanClient> {
    const c = new HanClient();
    c.who = who;
    c.ws = new WebSocket(url, { maxPayload: 1310720 });
    const challenge = new Promise<A>((res) => { c.challengeWaiter = res; });
    c.ws.on("message", (data, bin) => c.onMessage(bin ? "" : data.toString("utf8"), bin));
    c.ws.on("close", () => c.onClose());
    c.ws.on("error", () => { /* surfaces through close */ });
    await new Promise<void>((res, rej) => { c.ws.once("open", () => res()); c.ws.once("error", rej); });
    const ch = await challenge;
    if (ch.server !== dialed) { c.ws.close(); throw new WireFault("denied", `challenge names ${ch.server}, dialed ${dialed}`); }
    if (!Array.isArray(ch.versions) || !ch.versions.includes(1)) { c.ws.close(); throw new WireFault("invalid", "server does not speak version 1"); }
    const args: A = { who, keyId: key.keyId, proof: signBytes(key, helloMessage(ch.challenge, dialed)), version: 1 };
    if (knownHeads) args.knownHeads = knownHeads;
    const ok = await c.call("hello", args);
    c.limits = ok.limits;
    c.helloOk = ok;
    return c;
  }
  helloOk: A;

  private onMessage(text: string, bin: boolean) {
    if (bin) return;
    let f: A; try { f = parseStrict(text); } catch { return; }
    if (f.challenge !== undefined && f.id === undefined) { this.challengeWaiter(f); return; }
    const id = f.id as number;
    if (id === 0 && "error" in f) {   // the server could not tell which request this was about: fail everything in flight
      const e = new WireFault(f.error.code, f.error.message);
      for (const p of this.pending.values()) p.fail(e); this.pending.clear();
      for (const s of this.streams.values()) s.fail(e); this.streams.clear();
      return;
    }
    const s = this.streams.get(id);
    if ("item" in f) { s?.item(f.item); return; }
    if ("end" in f) { this.streams.delete(id); s?.end(); return; }
    if ("error" in f) {
      const e = new WireFault(f.error.code, f.error.message, f.error.retryAfterMs, f.error.movedTo);
      if (s) { this.streams.delete(id); s.fail(e); return; }
      const p = this.pending.get(id); if (p) { this.pending.delete(id); p.fail(e); } return;
    }
    if ("ok" in f) { const p = this.pending.get(id); if (p) { this.pending.delete(id); p.ok(f.ok); } }
  }
  private onClose() {
    this.closed = true;
    const e = new WireFault("unavailable", "connection closed");
    for (const p of this.pending.values()) p.fail(e); this.pending.clear();
    for (const s of this.streams.values()) s.fail(e); this.streams.clear();
  }
  close() { this.ws.close(); }
  /** hard drop for crash tests */
  terminate() { this.ws.terminate(); }

  call(call: string, args: A): Promise<A> {
    const id = this.nextId++;
    return new Promise((ok, fail) => {
      if (this.closed) return fail(new WireFault("unavailable", "connection closed"));
      this.pending.set(id, { ok, fail });
      this.ws.send(JSON.stringify({ id, call, args }));
    });
  }
  /** Streaming call; resolves when the stream ends. `cancel()` sends the `cancel` call. */
  stream(call: string, args: A, onItem: (i: A) => void): { id: number; done: Promise<void>; cancel: () => Promise<void> } {
    const id = this.nextId++;
    const done = new Promise<void>((end, fail) => {
      this.streams.set(id, { item: onItem, end: () => end(), fail });
      if (this.closed) return fail(new WireFault("unavailable", "connection closed"));
      this.ws.send(JSON.stringify({ id, call, args }));
    });
    done.catch(() => { /* a caller that never awaits `done` must not crash the process on close */ });
    return { id, done, cancel: async () => { await this.call("cancel", { target: id }); this.streams.delete(id); } };
  }

  // ── the three calls ──
  append(entry: A, content?: A): Promise<A> { return this.call("append", content === undefined ? { entry } : { entry, content }); }
  /** R-06: retry the identical entry until a receipt or a definitive error. */
  async appendReliable(entry: A, content?: A, tries = 20): Promise<A> {
    for (let i = 0; ; i++) {
      try { return await this.append(entry, content); }
      catch (e) {
        if (e instanceof WireFault && (e.code === "unavailable" || e.code === "limit") && i < tries) { await new Promise((r) => setTimeout(r, Math.min(e.retryAfterMs ?? 100, 1000))); continue; }
        throw e;
      }
    }
  }
  async read(space: string, after = 0, opts: A = {}): Promise<A[]> {
    const items: A[] = [];
    await this.stream("read", { space, after, ...opts }, (i) => items.push(i)).done;
    return items;
  }
  records(items: A[]): A[] { return items.filter((i) => i.type === "record").map((i) => i.record); }

  // ── blobs (G-01): parts of at most maxBlobPartBytes ──
  async blobPut(bytes: Buffer, type = "application/octet-stream"): Promise<A> {
    const ref = { hash: sha(bytes), size: bytes.length, type };
    const part = this.limits?.maxBlobPartBytes ?? 786432;
    let off = 0, last: A = { have: 0, complete: false };
    do {
      const chunk = bytes.subarray(off, off + part);
      last = await this.call("blobPut", { ref, offset: off, data: b64u(chunk) });
      off += chunk.length;
    } while (off < bytes.length);
    if (!last.complete) throw new WireFault("invalid", "upload not complete");
    return ref;
  }
  async blobGet(ref: A, space: string): Promise<Buffer> {
    const out: Buffer[] = []; let off = 0;
    for (;;) {
      const r = await this.call("blobGet", { ref, space, offset: off });
      const part = unb64u(r.data); out.push(part); off += part.length;
      if (off >= r.size || part.length === 0) break;
    }
    const all = Buffer.concat(out);
    if (sha(all) !== ref.hash) throw new WireFault("invalid", "blob does not hash to ref.hash");   // T-03: every reader verifies
    return all;
  }
}
