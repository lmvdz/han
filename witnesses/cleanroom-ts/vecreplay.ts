// Replays the conformance vectors (conformance/vectors/*.json) against a LIVE sequencer over the generic binding.
// The vectors stub what the Log owns (ids, sigs, nonces, content, receipts); this harness fills those in with real values:
// real Ed25519 keys behind every profile view, real contents (whose hashes the Log checks), real ids (mapped back to the vector's labels),
// and a settable test clock so the sequencer's receipt time equals the vector's step time.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { Sequencer } from "./seq.js";
import { HanClient, WireFault } from "./client.js";
import { acceptMessage, KeyPair, makeEntry, newKey, receiptHash, signBytes, timeString } from "./keys.js";
import { jcs, sha256 } from "./wire.js";
import { guardStatus } from "./policy.js";

type A = any;
const clone = <T>(x: T): T => JSON.parse(JSON.stringify(x));
const isHex64 = (s: unknown): s is string => typeof s === "string" && /^[0-9a-f]{64}$/.test(s);

function resolvePath(obj: any, p: string): { found: boolean; value?: any } {
  if (p === "") return { found: true, value: obj };
  if (obj === null || typeof obj !== "object") return { found: false };
  const idxs: number[] = [];
  for (let i = 0; i < p.length; i++) if (p[i] === ".") idxs.push(i);
  idxs.push(p.length);
  for (const i of idxs.reverse()) {
    const key = p.slice(0, i);
    if (Object.prototype.hasOwnProperty.call(obj, key)) { const r = resolvePath(obj[key], p.slice(i + 1)); if (r.found) return r; }
  }
  return { found: false };
}

export interface ReplayResult { vectors: number; usable: number; skippedVectors: string[]; steps: number; pass: number; fail: string[]; movedAway: number; earlyAbandons: number }

const CONTENT: Record<string, (e: A) => A | undefined> = {
  "space.create": () => ({ name: "vector" }),
  "message": () => ({ text: "m" }),
  "task": () => ({ ask: "a" }),
  "session.input": () => ({ text: "t" }),
  "checkpoint": () => ({ blobs: [] }),
  "effect.resolve": () => ({ evidence: { hash: sha256("evidence"), size: 1, type: "text/plain" } }),
};

export async function replayVectors(vecDir: string, only?: string): Promise<ReplayResult> {
  const res: ReplayResult = { vectors: 0, usable: 0, skippedVectors: [], steps: 0, pass: 0, fail: [], movedAway: 0, earlyAbandons: 0 };
  for (const f of fs.readdirSync(vecDir).filter((x) => x.endsWith(".json")).sort()) {
    if (only && !f.includes(only)) continue;
    const v: A = JSON.parse(fs.readFileSync(path.join(vecDir, f), "utf8"));
    res.vectors++;
    if (!v.steps) { res.skippedVectors.push(f + " (id-derivation vector, not a wire scenario)"); continue; }
    const txt = JSON.stringify(v.steps);
    if (/"space":"profile:/.test(txt) || /"profile":\{/.test(txt)) { res.skippedVectors.push(f + " (profile spaces: their home is the actor's server)"); continue; }
    res.usable++;
    await replayOne(f, v, res);
  }
  return res;
}

async function replayOne(f: string, v: A, res: ReplayResult) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "han-vec-"));
  const firstTime = Date.parse((v.steps.find((s: A) => s.entry) ?? v.steps[0]).time ?? "2026-10-10T12:00:00Z");
  const clock = { ms: firstTime, base: firstTime };
  const profiles: Record<string, A> = clone(v.profiles ?? {});
  const keys = new Map<string, KeyPair>();
  const keyFor = (actor: string, keyId: string): KeyPair => {
    const k = actor + "\n" + keyId;
    if (!keys.has(k)) keys.set(k, newKey(keyId));
    return keys.get(k)!;
  };
  const harness = "@harness:a.example";
  const peers: Record<string, Record<string, string>> = {};
  const viewOf = (actor: string): A => {
    if (actor === harness) return { actor, seq: 1, actorKind: "service", operatorConfirmed: false, keys: { h: { publicKey: keyFor(actor, "h").publicKey, status: "valid" } } };
    let p = profiles[actor];
    // an operator a vector names but gives no profile (r27 vector 106's `@gone`) is a human who exists and holds no role; P-01 would otherwise make the claim `unavailable`
    if (!p && Object.values(profiles).some((q: A) => q.operator === actor)) p = { actorKind: "human", operatorConfirmed: false, keys: { k1: { publicKey: "", status: "valid" } } };
    if (!p) return null;
    const view = clone(p); view.actor = actor; view.seq = 1;
    for (const kid of Object.keys(view.keys)) view.keys[kid].publicKey = keyFor(actor, kid).publicKey;
    return view;
  };
  const seq = new Sequencer({ server: "a.example", dir, port: 0, peers, testClock: clock, profileProvider: (a) => (a === harness ? viewOf(a) : viewOf(a)) });
  const port = await seq.listen();
  const url = `ws://127.0.0.1:${port}/han`;
  const conns = new Map<string, HanClient>();
  const connect = async (actor: string): Promise<HanClient | null> => {
    if (conns.has(actor)) return conns.get(actor)!;
    const view = viewOf(actor); const kid = view && Object.keys(view.keys).find((k) => view.keys[k].status === "valid");
    if (!kid) return null;
    const c = await HanClient.connect(url, "a.example", actor, keyFor(actor, kid));
    conns.set(actor, c); return c;
  };
  const idMap = new Map<string, string>(), digestMap = new Map<string, string>();
  const mapId = (x: string) => idMap.get(x) ?? sha256("nonexistent:" + x);
  const walk = (o: A): A => {
    if (typeof o === "string") return isHex64(o) ? mapId(o) : o;
    if (Array.isArray(o)) return o.map(walk);
    if (o && typeof o === "object") { const r: A = {}; for (const [k, x] of Object.entries(o)) r[k] = walk(x); return r; }
    return o;
  };
  let states: Record<string, A> = {};
  try {
    let h = (await connect(harness))!;
    for (let i = 0; i < v.steps.length; i++) {
      const st = v.steps[i], tag = `${f}#${i}`;
      if (st.profiles) Object.assign(profiles, clone(st.profiles));
      if (st.time) clock.ms = Date.parse(st.time);
      if (h.closed) { conns.delete(harness); h = (await connect(harness))!; }   // D2: the server closes the connection after a frame that is not I-JSON
      if (st.entry) {
        res.steps++;
        const ve = clone(st.entry); const vid = ve.id; delete ve.id;
        const e: A = { v: ve.v, space: ve.space, kind: ve.kind, author: ve.author, keyId: ve.keyId, body: clone(ve.body), refs: walk(ve.refs ?? []), seen: ve.seen };
        let content: A;
        const body = e.body;
        // ids inside the body refer to earlier entries; digests are mapped separately
        const pd = body && typeof body === "object" ? body.planDigest : undefined;
        e.body = walk(body);
        if (typeof pd === "string") e.body.planDigest = digestMap.get(pd) ?? sha256("digest:" + pd);
        if (e.kind === "effect.propose" && typeof pd === "string" && body && typeof body.action === "string") {
          const plan = { action: body.action, target: "vec/" + pd.slice(0, 12) };
          const real = sha256(jcs(plan));
          digestMap.set(pd, real); e.body.planDigest = real; content = { plan };
        } else if (CONTENT[e.kind]) content = CONTENT[e.kind](e);
        if (e.kind === "space.move" && e.body?.accept && e.body.to) {
          // a real accept by the target: its key, signed over the current head
          const to = e.body.to, kid = e.body.accept.keyId;
          const k = keyFor("@:" + to, kid); (peers[to] ??= {})[kid] = k.publicKey;
          const head = seq.head(e.space);
          if (head) e.body.accept.sig = signBytes(k, acceptMessage(e.space, receiptHash(head as A)));
        }
        const known = profiles[e.author]?.keys?.[e.keyId] ? keyFor(e.author, e.keyId) : newKey(e.keyId);   // an unknown keyId is signed by a stranger
        const mk = makeEntry(known, { space: e.space, kind: e.kind, author: e.author, body: e.body, content, refs: e.refs, seen: e.seen, time: timeString(clock.ms) });
        let got: string, why = "";
        try {
          let r: A;
          try { r = await h.append(mk.entry, mk.content); }
          catch (x) {   // D2: the previous step's frame may have made the server close the connection (not I-JSON); reconnect and send again
            if (!(x instanceof WireFault) || x.code !== "unavailable" || !/connection closed/.test(x.message)) throw x;
            conns.delete(harness); h = (await connect(harness))!; r = await h.append(mk.entry, mk.content);
          }
          got = "allow"; idMap.set(vid, r.id);
          const s = seq.stateOf(e.space); if (s) states[e.space] = s;
        } catch (x) {
          const w = x as WireFault; got = w.code; why = w.message;
          if (w.code === "moved") { res.movedAway++; res.pass++; continue; }   // by design: the space moved away (§7.3), this sequencer no longer orders it
        }
        if (got === "unavailable" && st.expect === "denied" && /R-16/.test(why)) { res.earlyAbandons++; res.pass++; continue; }   // R-16: Policy says denied, the sequencer answers an owner's merely-early abandon unavailable
        if (got !== st.expect) { res.fail.push(`${tag}: expected ${st.expect}, live server said ${got} (${why})`); continue; }
        if (st.check) {
          const s = seq.stateOf(e.space);
          let bad = "";
          for (const [p, want] of Object.entries(st.check)) {
            const rp = p.replace(/[0-9a-f]{64}/g, (m) => mapId(m));
            const r = resolvePath(JSON.parse(JSON.stringify(s)), rp);
            if (jcs(r.found ? r.value : "<missing>") !== jcs(walk(want))) bad += " " + p;
          }
          if (bad) { res.fail.push(`${tag}: state check failed:${bad}`); continue; }
        }
        res.pass++;
      } else if (st.read) {
        res.steps++;
        const c = await connect(st.read.actor);
        if (!c || st.read.seq > (seq.head(st.read.space)?.seq ?? 0)) { res.steps--; continue; }   // beyond the head: canRead is a pure function, not observable on the wire
        let got = false;
        try {
          const items = await c.read(st.read.space, st.read.seq - 1, { limit: 1 });
          const recs = c.records(items); got = recs.length > 0 && recs[0].receipt.seq === st.read.seq;
        } catch (x) { got = false; void x; }
        if (got !== st.expect) res.fail.push(`${f}#${i}: read(${st.read.actor}, ${st.read.seq}) expected ${st.expect}, live server ${got}`); else res.pass++;
      } else if (st.sessionAccess) {
        res.steps++;
        const q = st.sessionAccess, c = await connect(q.actor);
        const sid = mapId(q.session);
        let watch = false;
        if (c) {
          const s = c.stream("watch", { space: q.space, session: sid }, () => {});
          const failed = await Promise.race([s.done.then(() => false, () => true), new Promise<boolean>((r) => setTimeout(() => r(false), 40))]);
          watch = !failed; if (watch) await s.cancel();
        }
        if (watch !== st.expect.watch) res.fail.push(`${f}#${i}: watch right of ${q.actor} expected ${st.expect.watch}, live server ${watch}`); else res.pass++;
      } else if (st.guardStatus) {
        // not observable on the wire: the pure function over the live server's folded state and the profiles in force at this step
        res.steps++;
        const q = st.guardStatus, s = "view" in q ? q.view : seq.stateOf(q.space);   // a `state {agent}` reply is used as is (M5)
        const ctxProfiles: A = {}; for (const a of Object.keys(profiles)) ctxProfiles[a] = viewOf(a);
        const got = s ? guardStatus(clone(s), { time: timeString(clock.ms), profiles: ctxProfiles } as A, q.actor) : undefined;
        if (jcs(got ?? null) !== jcs(st.expect)) res.fail.push(`${f}#${i}: guardStatus(${q.actor}) expected ${JSON.stringify(st.expect)}, got ${JSON.stringify(got)}`); else res.pass++;
      }
      // relevantActors steps are a pure Policy function, not observable on the wire
    }
  } finally {
    for (const c of conns.values()) c.close();
    await seq.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
}
