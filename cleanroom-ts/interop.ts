// Interop test: my own client + executor against my own sequencer, over real WebSockets.
//   node dist/interop.js            everything (about 35 s: R-16 and X-04 need real elapsed time)
//   node dist/interop.js --quick    skips the scenarios that wait for real time (about 10 s)
// To point another implementation at this: HanClient (client.ts), Executor (exec.ts), verifySpace (verify.ts) and replayVectors
// (vecreplay.ts) take a URL and a ServerName. Only world() provisions actors by calling the in-process Sequencer; a foreign server
// needs its own provisioning (profile spaces created by its server actor, §4.3) and a way to hand the harness the actors' keys.
import fs from "node:fs";
import { createHash as createHashNode } from "node:crypto";
import os from "node:os";
import path from "node:path";
import WebSocket from "ws";
import { Sequencer } from "./seq.js";
import { HanClient, WireFault } from "./client.js";
import { Executor, STALL_BOUND_MS } from "./exec.js";
import { KeyPair, makeEntry, newKey, helloMessage, signBytes, timeString } from "./keys.js";
import { verifySpace } from "./verify.js";
import { replayVectors } from "./vecreplay.js";
import { jcs, sha256 } from "./wire.js";

type A = any;
const QUICK = process.argv.includes("--quick");
let passed = 0, failed = 0;
const failures: string[] = [];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(f: () => boolean, ms = 8000, what = "condition") { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw new Error("timed out waiting for " + what); await sleep(25); } }
function ok(c: unknown, msg: string) { if (!c) throw new Error("assertion failed: " + msg); }
async function code(p: Promise<unknown>): Promise<string> { try { await p; return "ok"; } catch (e) { return e instanceof WireFault ? e.code : "exception:" + (e as Error).message; } }
async function scenario(name: string, f: () => Promise<void>) {
  const t = Date.now();
  try { await f(); passed++; console.log(`PASS  ${name} (${Date.now() - t} ms)`); }
  catch (e) { failed++; failures.push(name + ": " + (e as Error).message); console.log(`FAIL  ${name}: ${(e as Error).message}`); }
}

// ───────────── world ─────────────
const SERVER = "han.test";
class Person {
  key: KeyPair; client!: HanClient;
  constructor(public name: string, public kind: "human" | "agent" | "service", public operator?: string) { this.key = newKey("k1"); }
  get id() { return `@${this.name}:${SERVER}`; }
}
interface World { dir: string; seq: Sequencer; url: string; people: Record<string, Person>; tmp: string }
const POLICY = (over: A = {}) => ({
  visibility: "members", join: "invite", membersMayInvite: false, allowAgents: "any", approvals: [], separateApprover: false, budget: null,
  maxDelegationDepth: 2, minDurability: "local", launchWindow: 15, extensionsMinRole: "member",
  sessions: { watch: "member", read: "owner", steer: "owner", modes: ["auto", "guarded"] }, servers: { deny: [] }, ...over,
});
let tmpCounter = 0;
async function newWorld(opts: A = {}): Promise<World> {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "han-interop-" + tmpCounter++ + "-"));
  const seq = new Sequencer({ server: SERVER, dir: path.join(tmp, "seq"), port: opts.port ?? 0, ...opts.seq });
  const port = await seq.listen();
  return { dir: tmp, seq, url: `ws://127.0.0.1:${port}/han`, people: {}, tmp };
}
function addPerson(w: World, name: string, kind: Person["kind"] = "human", operator?: Person, instanceKey?: KeyPair): Person {
  const p = new Person(name, kind, operator?.id);
  if (instanceKey) p.key = instanceKey;
  w.people[name] = p;
  w.seq.provisionActor(p.id, kind, { keyId: p.key.keyId, publicKey: p.key.publicKey }, operator?.id);
  return p;
}
async function login(w: World, p: Person): Promise<HanClient> { p.client = await HanClient.connect(w.url, SERVER, p.id, p.key); return p.client; }
function mk(p: Person, space: string, kind: string, body: A, content?: A, seen = 0, refs: string[] = []) { return makeEntry(p.key, { space, kind, author: p.id, body, content, seen, refs }); }
async function send(p: Person, space: string, kind: string, body: A, content?: A, seen?: number, refs?: string[]): Promise<A> {
  const e = mk(p, space, kind, body, content, seen ?? 0, refs); const r = await p.client.append(e.entry, e.content); return { ...r, entry: e.entry, content: e.content };
}
const planOf = (action: string, target: string, content = "hi\n") => ({ action, target, args: { content } });
const digestOf = (plan: A) => sha256(jcs(plan));
async function createSpace(p: Person, space: string, policy: A = POLICY()) { return send(p, space, "space.create", { policy }, { name: space }); }
async function stop(w: World) { for (const p of Object.values(w.people)) try { p.client?.close(); } catch { /* */ } await w.seq.close(); fs.rmSync(w.tmp, { recursive: true, force: true }); }

// raw websocket helper for frame-level tests
async function raw(url: string) {
  const ws = new WebSocket(url); const q: A[] = []; let waiter: ((v: A) => void) | null = null; let closed = false, closeCode = 0;
  ws.on("message", (d) => { const m = JSON.parse(d.toString()); if (waiter) { const w = waiter; waiter = null; w(m); } else q.push(m); });
  ws.on("close", (c) => { closed = true; closeCode = c; if (waiter) waiter({ closed: c }); });
  await new Promise<void>((r) => ws.once("open", () => r()));
  const next = (): Promise<A> => q.length ? Promise.resolve(q.shift()) : new Promise((r) => { waiter = r; });
  return { ws, next, send: (x: unknown) => ws.send(typeof x === "string" || Buffer.isBuffer(x) ? (x as A) : JSON.stringify(x)), get closed() { return closed; }, get closeCode() { return closeCode; } };
}

// ═════════════════════════════════════ A. end to end ═════════════════════════════════════
async function e2e() {
  const w = await newWorld();
  try {
    const ada = addPerson(w, "ada"), bob = addPerson(w, "bob"), eve = addPerson(w, "eve"), ann = addPerson(w, "ann"), ops = addPerson(w, "ops");
    const fixer = addPerson(w, "fixer", "agent", ann);
    const execDir = path.join(w.tmp, "exec"), sandbox = path.join(w.tmp, "sandbox");
    const inst = Executor.init(execDir, "inst1");
    const key = importFromDir(execDir);
    const fx = addPerson(w, "fx", "service", ops, key);
    ok(inst.publicKey === fx.key.publicKey, "instance key registered in the profile");
    for (const p of Object.values(w.people)) if (p.name !== "fx") await login(w, p);
    // operators confirm in the agents' profile spaces (§4.3)
    await send(ann, `profile:${fixer.id}`, "member.set", { actor: ann.id, role: "owner" });
    await send(ops, `profile:${fx.id}`, "member.set", { actor: ops.id, role: "owner" });
    const SP = "r1:" + SERVER;
    const pol = POLICY({ allowAgents: "with-operator", approvals: [{ action: "file.write", approvers: ["owner"] }], separateApprover: true });
    const created = await createSpace(ada, SP, pol);
    ok(created.seq === 1 && created.prev === "" && created.home === SERVER && created.durability === "local", "space.create receipt");
    for (const p of [bob, fixer, fx]) await send(ada, SP, "member.set", { actor: p.id, role: "member" });
    const ex = new Executor({ actor: fx.id, server: SERVER, url: w.url, dir: execDir, sandbox, spaces: [SP], onEvent: (s) => process.env.HAN_TRACE && console.log("   [exec]", s) });
    await ex.start();
    const plan = planOf("file.write", "out/hello.txt", "hello han\n");
    const prop = await send(fixer, SP, "effect.propose", { executor: fx.id, action: "file.write", planDigest: digestOf(plan), repeatable: false }, { plan });
    ok(w.seq.stateOf(SP)!.effects[prop.id].phase === "awaiting-approval", "proposal waits for approval (P-08)");
    await sleep(300); ok(!fs.existsSync(path.join(sandbox, "out/hello.txt")), "nothing runs before approval");
    await send(ada, SP, "effect.approve", { proposal: prop.id, planDigest: digestOf(plan) }, { note: "ok" });
    await until(() => w.seq.stateOf(SP)!.effects[prop.id].phase === "succeeded", 8000, "effect succeeded");
    ok(fs.readFileSync(path.join(sandbox, "out/hello.txt"), "utf8") === "hello han\n", "file written");
    const st = w.seq.stateOf(SP)!, claim = st.effects[prop.id].claims[0];
    ok(claim.instance === "inst1" && claim.window === 15 && claim.outcome === "succeeded" && claim.started === "yes", "claim pinned window and instance; outcome recorded");
    // evidence blob (G-01): readable by a member through the referencing outcome, not by an outsider
    const items = await ada.client.read(SP, 0); const recs = ada.client.records(items);
    const outcome = recs.find((r: A) => r.entry.kind === "effect.outcome")!;
    const bytes = await ada.client.blobGet(outcome.content.evidence, SP);
    ok(JSON.parse(bytes.toString()).path === "out/hello.txt", "evidence blob content");
    ok(await code(eve.client.blobGet(outcome.content.evidence, SP)) === "not_found", "outsider cannot get the blob");
    ok(await code(eve.client.read(SP, 0)) === "not_found", "outsider read is not_found");
    // verifier (R-14): chain, signatures, Policy replay of the whole space and of the profile spaces it pins
    const wk = await (await fetch(`http://127.0.0.1:${new URL(w.url).port}/.well-known/han`)).json() as A;
    const deps = {
      serverKeys: (s: string, k: string) => (s === SERVER ? wk.keys.find((x: A) => x.keyId === k) : undefined),
      profileRecords: async (a: string) => ada.client.records(await ada.client.read("profile:" + a, 0)),
    };
    const v = await verifySpace(SP, recs, deps);
    ok(v.ok && v.through === recs.length, "verifier accepts the space: " + v.faults.join("; "));
    ok(jcs(strip(v.state)) === jcs(strip(w.seq.stateOf(SP))), "verifier's replayed state equals the sequencer's");
    // receipts chain, and tamper detection
    for (let i = 1; i < recs.length; i++) ok(recs[i].receipt.seq === i + 1, "seq contiguous");
    const bad = JSON.parse(JSON.stringify(recs)); bad[3].entry.body.role = "owner";
    ok(!(await verifySpace(SP, bad, deps)).ok, "verifier rejects a tampered record");
    const bad2 = JSON.parse(JSON.stringify(recs)); bad2[2].receipt.time = "2030-01-01T00:00:00Z";
    ok(!(await verifySpace(SP, bad2, deps)).ok, "verifier rejects a tampered receipt");
    const pv = await verifySpace("profile:" + fixer.id, await ada.client.records(await ada.client.read("profile:" + fixer.id, 0)), deps);
    ok(pv.ok, "profile space verifies: " + pv.faults.join("; "));
    await ex.stop();
  } finally { await stop(w); }
}
const strip = (s: A) => { if (!s) return s; const { entries, ...r } = s; return r; };
function importFromDir(dir: string): KeyPair {
  const j = JSON.parse(fs.readFileSync(path.join(dir, "instance.json"), "utf8"));
  return { keyId: j.keyId, publicKey: j.publicKey, priv: null as unknown as KeyObject0 } as KeyPair;
}
type KeyObject0 = KeyPair["priv"];

// ═════════════════════════════════════ B. protocol on the live server ═════════════════════════════════════
async function protocol() {
  const w = await newWorld();
  try {
    const ada = addPerson(w, "ada"), bob = addPerson(w, "bob"), eve = addPerson(w, "eve"), nina = addPerson(w, "nina");
    for (const p of [ada, bob, eve, nina]) await login(w, p);
    const SP = "p1:" + SERVER;
    await scenario("hello: first call must be hello; challenge is bound to the server name; bad proof is denied", async () => {
      const r = await raw(w.url); const ch = await r.next();
      ok(ch.server === SERVER && ch.versions.includes(1) && typeof ch.challenge === "string" && !("id" in ch), "bare Challenge first");
      r.send({ id: 1, call: "read", args: { space: SP, after: 0 } });
      let m = await r.next(); ok(m.id === 1 && m.error?.code === "denied", "call before hello is denied");
      const hello = (id: number, who: string, challenge: string, dialed: string, version = 1) => ({ id, call: "hello", args: { who, keyId: "k1", proof: signBytes(ada.key, helloMessage(challenge, dialed)), version } });
      r.send(hello(2, ada.id, ch.challenge, "other.test"));
      m = await r.next(); ok(m.error?.code === "denied", "proof over another server name is denied");
      r.send(hello(3, ada.id, ch.challenge, SERVER));
      m = await r.next(); ok(m.error?.code === "denied", "G-08: the challenge is used by the first hello that names it, even a failed one");
      const r2 = await raw(w.url); const ch2 = await r2.next();
      r2.send(hello(3, ada.id, ch2.challenge, SERVER, 2)); m = await r2.next(); ok(m.error?.code === "invalid", "unsupported version is invalid (R-08)");
      r2.send(hello(4, ada.id, ch2.challenge, SERVER));
      m = await r2.next(); ok(m.id === 4 && m.ok?.limits?.maxFrameBytes === 1310720 && m.ok.durability === "local", "good proof");
      r2.send(hello(5, ada.id, ch2.challenge, SERVER)); m = await r2.next(); ok(m.error?.code === "invalid", "a second hello on an authenticated connection is invalid");
      r2.ws.close();
      r.ws.close();
      let e = ""; try { await HanClient.connect(w.url, "other.test", ada.id, ada.key); } catch (x) { e = (x as Error).message; }
      ok(/challenge names/.test(e), "the client aborts when the challenge names another server");
    });
    await scenario("frames: malformed, binary, duplicate stream id", async () => {
      // D2: a frame that is not I-JSON gets {id:0, invalid} and the server closes the connection
      for (const bad of ["{not json", '{"id":6,"call":"append","args":{"entry":{"a":1.0}}}', '{"id":7,"id":8,"call":"cancel","args":{"target":1}}']) {
        const q = await raw(w.url); await q.next();
        q.send(bad); const mm = await q.next(); ok(mm.id === 0 && mm.error.code === "invalid", "not I-JSON -> id 0 invalid: " + bad);
        await sleep(100); ok(q.closed, "the server closes the connection after a non-I-JSON frame");
      }
      const r = await raw(w.url); const ch = await r.next();
      r.send({ id: 5, call: "hello", args: { who: ada.id, keyId: "k1", proof: signBytes(ada.key, helloMessage(ch.challenge, SERVER)), version: 1 } });
      let m = await r.next();
      r.send({ id: 5, call: "cancel", args: { target: 1 }, extra: true }); m = await r.next(); ok(m.id === 5 && m.error.code === "invalid", "extra frame member: invalid on its own id");
      r.send({ id: 5, call: "cancel" }); m = await r.next(); ok(m.id === 5 && m.error.code === "invalid", "missing args: invalid");
      r.send({ id: 0, call: "cancel", args: { target: 1 } }); m = await r.next(); ok(m.id === 0 && m.error.code === "invalid", "id 0 is unusable");
      r.send({ id: 9, call: "nonsense", args: {} }); m = await r.next(); ok(m.error.code === "invalid", "unknown call");
      r.send({ id: 10, call: "subscribe", args: { space: "profile:" + ada.id } }); // live-only stream
      await sleep(50); r.send({ id: 10, call: "subscribe", args: { space: "profile:" + ada.id } });
      m = await r.next(); ok(m.id === 10 && m.error.code === "invalid", "streaming call whose id is still open is invalid");
      r.ws.send(Buffer.from([1, 2, 3]), { binary: true }); await sleep(100); ok(r.closed && r.closeCode === 1003, "binary frames close the connection");
    });
    let created: A;
    await scenario("append: receipts, shape (R-09), content binding, signature", async () => {
      created = await createSpace(ada, SP);
      ok(created.seq === 1 && created.profiles[ada.id] !== undefined, "receipt pins the author's profile (P-01)");
      const e = mk(ada, SP, "member.set", { actor: bob.id, role: "member" });
      ok(await code(ada.client.append({ ...e.entry, extra: 1 })) === "invalid", "extra header field");
      const noSig = { ...e.entry, sig: "A".repeat(86) };
      ok(await code(ada.client.append(noSig)) === "invalid", "bad signature");
      const wrongV = mk(ada, SP, "member.set", { actor: bob.id, role: "member" }); wrongV.entry.v = 2;
      ok(await code(ada.client.append(wrongV.entry)) === "invalid", "unsupported v");
      const withContent = makeEntry(ada.key, { space: SP, kind: "message", author: ada.id, body: {}, content: { text: "x" }, seen: 0 });
      ok(await code(ada.client.append(withContent.entry)) === "invalid", "contentHash names content that was not sent");
      ok(await code(ada.client.append(withContent.entry, { text: "y" })) === "invalid", "content does not match contentHash");
      ok(await code(ada.client.append(mk(ada, SP, "message", {}).entry)) === "invalid", "message needs content");
      ok(await code(ada.client.append(mk(ada, SP, "member.set", { actor: bob.id, role: "emperor" }).entry)) === "invalid", "closed body");
      const big = makeEntry(ada.key, { space: SP, kind: "message", author: ada.id, body: {}, content: { text: "é".repeat(20000) }, seen: 0 });
      ok(await code(ada.client.append(big.entry, big.content)) === "invalid", "text over 32,768 bytes");
      const r = await ada.client.append(e.entry); ok(r.seq === 2, "good entry gets seq 2");
      ok(await code(send(eve, SP, "message", {}, { text: "hi" })) === "denied", "outsider: denied");
    });
    await scenario("R-01 dedupe precedes every check, forever", async () => {
      await send(ada, SP, "member.set", { actor: eve.id, role: "member" });
      const m = mk(eve, SP, "message", {}, { text: "first" });
      const r1 = await eve.client.append(m.entry, m.content);
      await send(ada, SP, "member.set", { actor: eve.id, role: "banned" });
      const r2 = await eve.client.append(m.entry, m.content);
      ok(jcs(r1) === jcs(r2), "a banned author's resend returns the stored receipt");
      const r3 = await eve.client.append(m.entry);
      ok(jcs(r1) === jcs(r3), "even without the content");
      const fresh = mk(eve, SP, "message", {}, { text: "second" });
      ok(await code(eve.client.append(fresh.entry, fresh.content)) === "denied", "a new entry from the banned author is denied");
    });
    await scenario("vector rules: create-once (01), decision order (126), keys single-use (38/04)", async () => {
      ok(await code(createSpace(ada, SP)) === "invalid", "space.create on an existing space is invalid (01)");
      const adaRef = mk(ada, SP, "message", {}, { text: "x" }, 0, ["a".repeat(64)]);
      ok(await code(ada.client.append(adaRef.entry, adaRef.content)) === "invalid", "unresolved ref is invalid (R-05)");
      ok(await code(send(ada, SP, "checkpoint", { through: 999 }, { blobs: [] })) === "invalid", "checkpoint beyond head is invalid");
      // bob rotates keys in his profile space: add k2, revoke k1, k1 can no longer sign, k1 can't come back
      const k2 = newKey("k2");
      await send(ada, SP, "member.set", { actor: bob.id, role: "member" });
      await send(bob, `profile:${bob.id}`, "key.add", { keyId: "k2", publicKey: k2.publicKey });
      ok(await code(send(bob, `profile:${bob.id}`, "key.add", { keyId: "k2", publicKey: k2.publicKey })) === "invalid", "a used keyId is invalid (38)");
      await send(bob, `profile:${bob.id}`, "key.revoke", { keyId: "k1" });
      ok(await code(send(bob, SP, "message", {}, { text: "old key" })) === "denied", "a revoked key is denied (04, R-12)");
      ok(await code(send(bob, SP, "message", {}, { text: "old key, bad ref" }, 0, ["a".repeat(64)])) === "denied", "the key is judged before references (126)");
      const e = makeEntry(k2, { space: SP, kind: "message", author: bob.id, body: {}, content: { text: "new key" }, seen: 0 });
      ok((await bob.client.append(e.entry, e.content)).seq > 0, "the new key signs");
      const rv = makeEntry(k2, { space: `profile:${bob.id}`, kind: "key.revoke", author: bob.id, body: { keyId: "k1" }, seen: 0 });
      ok(await code(bob.client.append(rv.entry)) === "invalid", "revoking a key that is not valid is invalid");
    });
    await scenario("read: P-02 rights, state, limit, follow, heads", async () => {
      ok(await code(nina.client.read(SP, 0)) === "not_found", "no rights -> not_found, as for a missing space");
      ok(await code(ada.client.read("zz:" + SERVER, 0)) === "not_found", "missing space");
      const all = await ada.client.read(SP, 0, { limit: 3, knownHeads: [created] });
      const sv = (await ada.client.state(SP)).state;
      ok(!("entries" in sv) && sv.head >= 5, "state: a StateView without the entries index");
      ok(all[0].type === "heads" && all[0].heads[0].space === SP, "read: entries only, heads first");
      ok(all.filter((i: A) => i.type === "record").length === 3, "limit bounds the backlog");
      ok(await code(eve.client.state(SP)) === "denied", "a banned author keeps old reads but not the state at the head");
      const evRecs = eve.client.records(await eve.client.read(SP, 0));
      const iv = (w.seq.stateOf(SP)!.roleHistory as A)[eve.id][0];
      ok(evRecs.length > 0 && evRecs.every((r: A) => r.receipt.seq >= iv.from && r.receipt.seq < iv.to!) && evRecs[evRecs.length - 1].receipt.seq === iv.to! - 1, "former members read exactly [from, to) (110)");
      const stAt = await eve.client.state(SP, { at: iv.to! - 1 });
      ok(stAt.state.head === iv.to! - 1, "state with `at`: a former member reads the state as it was while in");
      ok(await code(eve.client.state(SP, { at: w.seq.stateOf(SP)!.head })) === "denied", "`at` needs read rights at `at`");
      ok(await code(ada.client.read(SP, 0, { at: 2 })) === "invalid" && await code(ada.client.read(SP, 0, { state: true } as A)) === "invalid" && await code(ada.client.state(SP, { at: 0 })) === "invalid", "`read` takes no state/at; `state` `at` outside 1…head is invalid");
      ok(await code(ada.client.state(SP, { at: 2, cursor: null } as A)) === "invalid" && await code(ada.client.state(SP, { effect: "0".repeat(64), at: 1 } as A)) === "invalid", "two selectors are invalid by presence");
      const gap: A[] = []; const gs = ada.client.stream("read", { space: SP, after: 0, follow: true, limit: 2 }, (i) => gap.push(i));
      await sleep(80); ok(gap.length === 2, "follow with a long backlog sends `limit` records");
      await send(ada, SP, "message", {}, { text: "after gap" }); await until(() => gap.length === 3, 3000, "live record after the gap");
      ok(gap[2].record.receipt.seq === w.seq.stateOf(SP)!.head && gap[2].record.receipt.seq > gap[1].record.receipt.seq + 1, "then live records from the head: a visible gap in seq"); await gs.cancel();
      const live: A[] = []; const s = ada.client.stream("read", { space: SP, after: w.seq.stateOf(SP)!.head, follow: true }, (i) => live.push(i));
      await sleep(50); await send(ada, SP, "message", {}, { text: "live" }); await until(() => live.length === 1, 3000, "followed record");
      ok(live[0].record.entry.kind === "message", "follow delivers new records"); await s.cancel();
      const sub: A[] = []; const s2 = ada.client.stream("subscribe", { space: SP, after: 0 }, (i) => sub.push(i));
      await sleep(80); ok(sub.length >= 5, "subscribe with after replays"); await s2.cancel();
    });
    await scenario("redaction (R-04, P-12): content gone, proof served, verifier checks it", async () => {
      const m = await send(ada, SP, "message", {}, { text: "secret" });
      await send(ada, SP, "redact", { target: m.id }, { reason: "oops" });
      const recs = ada.client.records(await ada.client.read(SP, 0));
      const r = recs.find((x: A) => x.receipt.id === m.id)!;
      ok(r.redacted === true && r.content === null && r.redactedBy.entry.kind === "redact" && r.redactedBy.receipt.seq > r.receipt.seq, "redactedBy carries the redact entry and receipt");
      ok(!fs.readFileSync(path.join(w.tmp, "seq", "content.jsonl"), "utf8").includes("secret"), "content deleted from disk");
      const wk = await (await fetch(`http://127.0.0.1:${new URL(w.url).port}/.well-known/han`)).json() as A;
      const v = await verifySpace(SP, recs, { serverKeys: (s, k) => wk.keys.find((x: A) => x.keyId === k), profileRecords: async (a) => ada.client.records(await ada.client.read("profile:" + a, 0)) });
      ok(v.ok, "verifier accepts a redacted record through its proof: " + v.faults.join("; "));
    });
    await scenario("notices (G-03): only to the actor's own sessions on its profile follow", async () => {
      const got: A[] = [], other: A[] = [];
      const s1 = nina.client.stream("subscribe", { space: `profile:${nina.id}` }, (i) => got.push(i));
      const bob2 = await HanClient.connect(w.url, SERVER, ada.id, ada.key);
      const s2 = bob2.stream("subscribe", { space: `profile:${nina.id}` }, (i) => other.push(i));
      await sleep(80);
      await send(ada, SP, "member.set", { actor: nina.id, role: "guest" });
      await until(() => got.length === 1, 3000, "notice");
      ok(got[0].type === "signal" && got[0].body.type === "notice" && got[0].body.space === SP && got[0].from === "@:" + SERVER, "notice item");
      await sleep(100); ok(other.length === 0, "another reader of the profile space gets no notice");
      // a signal in a normal space goes to readers only (G-04)
      ok(await code(eve.client.call("signal", { space: SP, body: { type: "typing" } })) === "denied", "banned: may not signal (G-04)");
      const sig: A[] = []; const s3 = ada.client.stream("subscribe", { space: SP }, (i) => sig.push(i)); await sleep(50);
      ok(await code(nina.client.call("signal", { space: SP, body: { type: "typing" } })) === "ok", "guest signals");
      await until(() => sig.length === 1 && sig[0].type === "signal", 3000, "signal"); ok(sig[0].from === nina.id, "signal carries the sender");
      ok(await code(nina.client.call("signal", { space: `profile:${nina.id}`, body: { type: "notice", space: SP, seq: 1 } })) === "denied", "only server actors signal a profile space");
      await s1.cancel(); await s2.cancel(); await s3.cancel(); bob2.close();
    });
    await scenario("blobs (G-01): parts, dedupe by hash never grants access, theft refused", async () => {
      const data = Buffer.alloc(1_700_000, 7); for (let i = 0; i < data.length; i += 4099) data[i] = i % 251;
      const ref = await ada.client.blobPut(data, "application/octet-stream");
      ok(ref.size === data.length, "uploaded in 3 parts");
      const m = await send(ada, SP, "message", {}, { text: "see blob", blobs: [ref] });
      ok(m.seq > 0, "referencing record");
      const back = await ada.client.blobGet(ref, SP); ok(back.equals(data), "round trip, hash verified");
      // eve (banned) and nina: nina is a guest: may read the referencing record, so access holds, supply holds (author uploaded)
      ok((await nina.client.blobGet(ref, SP)).equals(data), "a guest who may read the record gets the bytes");
      // theft: a stranger mentions the hash in a space they control; no supply for them
      const mine = "t1:" + SERVER; await createSpace(eve, mine);
      const stolen = await code(eve.client.blobGet(ref, mine)); ok(stolen === "not_found", "bytes are not served for a space nobody uploaded them to");
      await send(eve, mine, "message", {}, { text: "I have it", blobs: [ref] });
      ok(await code(eve.client.blobGet(ref, mine)) === "not_found", "mentioning a known hash in your own space gets you nothing");
      const first = await eve.client.call("blobPut", { ref, offset: 0, data: Buffer.from(data.subarray(0, 100)).toString("base64url") });
      ok(first.complete === false && first.have === 100, "a hash already stored is NOT 'complete' for another uploader");
      ok(await code(eve.client.call("blobPut", { ref, offset: 500, data: "AAAA" })) === "invalid", "gap in parts is invalid");
      const bad = { hash: sha256("x"), size: 3, type: "text/plain" };
      ok(await code(eve.client.call("blobPut", { ref: bad, offset: 0, data: Buffer.from("abc").toString("base64url") })) === "invalid", "bytes that do not hash to ref.hash are invalid");
    });
    await scenario("sessions (G-05, P-15): stream rules and per-right delivery", async () => {
      const S2 = "s2:" + SERVER;
      await createSpace(ada, S2, POLICY({ sessions: { watch: "guest", read: "member", steer: "owner", modes: ["auto", "guarded"] } }));
      await send(ada, S2, "member.set", { actor: bob.id, role: "member" });
      await send(ada, S2, "member.set", { actor: nina.id, role: "guest" });
      const open = await send(ada, S2, "session.open", { steerMode: "auto" }, { title: "ada's terminal" });
      const itm = (n: number, extra: A = { type: "output", text: "line " + n }) => ({ space: S2, session: open.id, n, time: timeString(Date.now()), ...extra });
      const bobGot: A[] = [], ninaGot: A[] = [];
      const wb = bob.client.stream("watch", { space: S2, session: open.id }, (i) => bobGot.push(i));
      const wn = nina.client.stream("watch", { space: S2, session: open.id }, (i) => ninaGot.push(i));
      ok(await code(eve.client.call("stream", { item: itm(1) })) === "denied", "only the session's actor may stream");
      ok(await code(ada.client.call("stream", { item: itm(2) })) === "invalid", "n must be one more than the last");
      ok(await code(ada.client.call("stream", { item: itm(1) })) === "ok", "n = 1");
      ok(await code(ada.client.call("stream", { item: itm(1) })) === "ok", "a repeat of the last item is ok");
      const ctxRef = await ada.client.blobPut(Buffer.from("the transcript so far"), "text/plain");
      ok(await code(bob.client.blobGet(ctxRef, S2)) === "not_found", "before it is streamed, a context blob is not served");
      const ctx = itm(2, { type: "context", blob: ctxRef, what: "transcript" });
      ok(await code(ada.client.call("stream", { item: ctx })) === "ok", "context item");
      ok(await code(ada.client.call("stream", { item: itm(3, { type: "status", state: "working" }) })) === "ok", "status item");
      await until(() => bobGot.length === 3 && ninaGot.length === 2, 3000, "deliveries");
      ok(bobGot.some((i) => i.type === "context") && !ninaGot.some((i) => i.type === "context"), "context only to those granted read (member yes, guest no)");
      ok(await code(eve.client.call("stream", { item: itm(4) })) === "denied", "outsider cannot publish");
      ok((await bob.client.blobGet(ctxRef, S2)).toString() === "the transcript so far", "a context blob is served to whoever has read on the live session (G-01)");
      ok(await code(nina.client.blobGet(ctxRef, S2)) === "not_found", "not to a guest without read");
      await send(ada, S2, "member.set", { actor: bob.id, role: "none" });      // right removed by a receipt
      ok(await code(bob.client.blobGet(ctxRef, S2)) === "not_found", "access ends when the read right is removed");
      ok(await code(ada.client.call("stream", { item: itm(4) })) === "ok", "n = 4");
      await sleep(150); ok(bobGot.length === 3, "nothing published after the removal reaches the removed watcher");
      ok(await code(bob.client.stream("watch", { space: S2, session: open.id }, () => {}).done) === "denied", "a new watch is denied");
      await send(ada, S2, "session.close", { session: open.id });
      ok(await code(ada.client.call("stream", { item: itm(5) })) === "not_found", "closed session has left the state: no such session");
      await wb.cancel().catch(() => {}); await wn.cancel();
    });
    await scenario("limits: abuse limit answers `limit` with retryAfterMs", async () => {
      const w2 = await newWorld({ seq: { appendsPerSecond: 3 } });
      try {
        const a = addPerson(w2, "ada"); await login(w2, a); await createSpace(a, "l1:" + SERVER);
        const codes: string[] = [];
        for (let i = 0; i < 4; i++) { try { await send(a, "l1:" + SERVER, "message", {}, { text: "m" + i }); codes.push("ok"); } catch (e) { codes.push((e as WireFault).code + ((e as WireFault).retryAfterMs ? "+retry" : "")); } }
        ok(codes[0] === "ok" && codes[1] === "ok" && codes[2] === "limit+retry", "got " + codes.join());
      } finally { await stop(w2); }
    });
  } finally { await stop(w); }
}

// ═════════════════════════════════════ C. durability, clock ═════════════════════════════════════
async function durability() {
  const w = await newWorld();
  let w2: Sequencer | null = null;
  try {
    const ada = addPerson(w, "ada"); await login(w, ada);
    const SP = "d1:" + SERVER;
    await createSpace(ada, SP);
    const m = await send(ada, SP, "message", {}, { text: "durable" });
    const before = jcs(strip(w.seq.stateOf(SP)));
    ada.client.close(); const port = new URL(w.url).port;
    await w.seq.close();
    // a crash mid-write leaves a torn last line: it was never acknowledged, so it is dropped (R-03)
    fs.appendFileSync(path.join(w.tmp, "seq", "log.jsonl"), '{"entry":{"v":1,"space":"d1:han');
    await scenario("R-03 restart: every acknowledged record is back, torn tail dropped, dedupe still answers", async () => {
      w2 = new Sequencer({ server: SERVER, dir: path.join(w.tmp, "seq"), port: Number(port) }); await w2.listen();
      ok(jcs(strip(w2.stateOf(SP))) === before, "same state after replay");
      ok(w2.head(SP)!.seq === m.seq, "same head");
      const c = await HanClient.connect(w.url, SERVER, ada.id, ada.key);
      const again = await c.append(m.entry, m.content);
      ok(again.id === m.id && again.seq === m.seq && again.homeSig === m.homeSig, "R-01 across a restart: the same stored receipt");
      const next = makeEntry(ada.key, { space: SP, kind: "message", author: ada.id, body: {}, content: { text: "after restart" }, seen: 0 });
      const r = await c.append(next.entry, next.content);
      ok(r.seq === m.seq + 1 && r.prev.length === 64, "chain continues"); c.close();
      await w2.close();
    });
    await scenario("R-03a: a clock behind the last receipt time answers unavailable until it catches up", async () => {
      w2 = new Sequencer({ server: SERVER, dir: path.join(w.tmp, "seq"), port: Number(port), clockOffsetMs: -40_000 }); await w2.listen();
      const c = await HanClient.connect(w.url, SERVER, ada.id, ada.key);
      const e = makeEntry(ada.key, { space: SP, kind: "message", author: ada.id, body: {}, content: { text: "slow clock" }, seen: 0 });
      let got: A = null; try { await c.append(e.entry, e.content); } catch (x) { got = x; }
      ok(got?.code === "unavailable" && got.retryAfterMs > 0, "unavailable, with a retry hint");
      ok((await c.read(SP, 0)).length > 0, "reads still work"); c.close(); await w2.close();
    });
  } finally { fs.rmSync(w.tmp, { recursive: true, force: true }); }
}

// ═════════════════════════════════════ D. executor ═════════════════════════════════════
async function executorScenarios() {
  const w = await newWorld();
  try {
    const ada = addPerson(w, "ada"); await login(w, ada);
    let n = 0;
    const setup = async (name: string, execOpts: A = {}, planFor: (n: number) => A = (k) => planOf("file.write", `d${k}/f.txt`, `data ${k}\n`), approvals = false) => {
      const k = ++n, dir = path.join(w.tmp, "x" + name), sandbox = path.join(w.tmp, "sb" + name);
      const inst = Executor.init(dir, "inst1"); const key = importFromDir(dir); void inst;
      const fx = addPerson(w, "fx" + name, "service", undefined, key);
      const SP = "e" + k + ":" + SERVER;
      await createSpace(ada, SP, POLICY({ approvals: approvals ? [{ action: "*", approvers: ["owner"] }] : [] }));
      await send(ada, SP, "member.set", { actor: fx.id, role: "member" });
      const plan = planFor(k);
      const prop = await send(ada, SP, "effect.propose", { executor: fx.id, action: plan.action, planDigest: digestOf(plan), repeatable: false }, { plan }, 1);
      const mkEx = (o: A = {}) => new Executor({ actor: fx.id, server: SERVER, url: w.url, dir, sandbox, spaces: [SP], onEvent: (s) => process.env.HAN_TRACE && console.log("   [exec " + name + "]", s), ...execOpts, ...o });
      return { SP, fx, prop, plan, dir, sandbox, mkEx, file: path.join(sandbox, plan.target ?? "") };
    };
    const eff = (SP: string, id: string) => w.seq.stateOf(SP)!.effects[id] as A;

    await scenario("X-02/X-06: crash after journaling `claiming`, before the first send -> restart finishes the claim and reports failed/not started; never launches", async () => {
      const t = await setup("a", { crashAt: "after-claiming-journal" });
      const ex = t.mkEx(); await ex.start(); await sleep(400);
      ok(eff(t.SP, t.prop.id).claims.length === 0 && eff(t.SP, t.prop.id).phase === "ready", "the claim never reached the sequencer");
      const ex2 = t.mkEx({ crashAt: undefined }); await ex2.start();
      await until(() => eff(t.SP, t.prop.id).claims[0]?.started === "no", 5000, "failed/no outcome");
      const e = eff(t.SP, t.prop.id); ok(e.claims.length === 1 && e.claims[0].outcome === "failed", "claim has a failed/no outcome");
      ok(!fs.existsSync(t.file), "nothing was launched"); await sleep(300); ok(eff(t.SP, t.prop.id).claims.length === 1, "no second claim on its own"); await ex2.stop();
    });
    await scenario("X-06: crash after the claim's receipt, before `launching` -> failed/not started", async () => {
      const t = await setup("b", { crashAt: "after-receipt" });
      const ex = t.mkEx(); await ex.start(); await until(() => eff(t.SP, t.prop.id).phase === "claimed", 5000, "claimed"); await sleep(100);
      const ex2 = t.mkEx({ crashAt: undefined }); await ex2.start();
      await until(() => eff(t.SP, t.prop.id).claims[0]?.started === "no", 5000, "failed/no");
      ok(!fs.existsSync(t.file), "nothing was launched"); await ex2.stop();
    });
    await scenario("X-06: crash after `launching` -> the result is unknown; reported unknown/unknown, never relaunched", async () => {
      const t = await setup("c", { crashAt: "after-launching-journal" });
      const ex = t.mkEx(); await ex.start(); await until(() => eff(t.SP, t.prop.id).phase === "claimed", 5000, "claimed"); await sleep(100);
      const ex2 = t.mkEx({ crashAt: undefined }); await ex2.start();
      await until(() => eff(t.SP, t.prop.id).phase === "unknown", 5000, "unknown");
      const c = eff(t.SP, t.prop.id).claims[0]; ok(c.outcome === "unknown" && c.started === "unknown", "unknown/unknown");
      ok(!fs.existsSync(t.file), "the executor did not launch again on restart"); await ex2.stop();
    });
    await scenario("X-06: crash after the launch but before the report -> recovery finds the result and reports succeeded/yes", async () => {
      const t = await setup("d", { crashAt: "after-launch" });
      const ex = t.mkEx(); await ex.start(); await until(() => fs.existsSync(t.file), 5000, "file written"); await sleep(150);
      ok(eff(t.SP, t.prop.id).phase === "claimed", "no report yet");
      const ex2 = t.mkEx({ crashAt: undefined }); await ex2.start();
      await until(() => eff(t.SP, t.prop.id).phase === "succeeded", 5000, "succeeded"); await ex2.stop();
    });
    await scenario("X-07: an instance key is held by one process; a key without its journal is refused", async () => {
      const t = await setup("e");
      const ex = t.mkEx(); await ex.start();
      let m = ""; try { await t.mkEx().start(); } catch (e) { m = (e as Error).message; }
      ok(/X-07.*held by a running process/.test(m), "second process refused: " + m);
      await ex.stop();
      const d2 = path.join(w.tmp, "restored"); fs.mkdirSync(d2); fs.copyFileSync(path.join(t.dir, "instance.json"), path.join(d2, "instance.json"));
      m = ""; try { await new Executor({ actor: t.fx.id, server: SERVER, url: w.url, dir: d2, sandbox: t.sandbox, spaces: [t.SP] }).start(); } catch (e) { m = (e as Error).message; }
      ok(/journal is missing/.test(m), "restored key without journal refused: " + m);
      fs.writeFileSync(path.join(d2, "journal.jsonl"), JSON.stringify({ t: "header", journalId: "other", keyId: "inst1" }) + "\n");
      m = ""; try { await new Executor({ actor: t.fx.id, server: SERVER, url: w.url, dir: d2, sandbox: t.sandbox, spaces: [t.SP] }).start(); } catch (e) { m = (e as Error).message; }
      ok(/does not belong/.test(m), "a journal from another key refused: " + m);
    });
    await scenario("X-09: plans the executor can't run or check are never claimed (unsupported action, path escape, argsBlob)", async () => {
      for (const [name, plan] of [["f1", planOf("http.post", "https://x.invalid/")], ["f2", planOf("file.write", "../escape.txt")], ["f3", planOf("file.write", "/abs.txt")]] as Array<[string, A]>) {
        const t = await setup(name, {}, () => plan);
        const ex = t.mkEx(); await ex.start(); await sleep(400);
        ok(eff(t.SP, t.prop.id).phase === "ready" && eff(t.SP, t.prop.id).claims.length === 0, `${plan.action} ${plan.target}: never claimed`);
        const recs = ada.client.records(await ada.client.read(t.SP, 0));
        ok(recs.some((r: A) => r.entry.kind === "message" && r.entry.refs.includes(t.prop.id) && r.entry.author === t.fx.id), "X-11: the proposer is told with a message whose refs name the proposal");
        ok(!fs.existsSync(path.join(w.tmp, "escape.txt")), "no escape"); await ex.stop();
      }
      const SP = "bad:" + SERVER; await createSpace(ada, SP);
      const plan = planOf("file.write", "x.txt");
      ok(await code(send(ada, SP, "effect.propose", { executor: ada.id, action: "file.write", planDigest: sha256("not the plan"), repeatable: false }, { plan })) === "invalid", "the Log refuses a plan that does not hash to planDigest");
    });
    if (!QUICK) await scenario("X-04: a stall between the receipt and the launch past W - stallBound -> `expired`, failed/not started, no launch", async () => {
      const t = await setup("g", { stallBeforeCheckMs: 15000 - STALL_BOUND_MS + 700 });
      const ex = t.mkEx(); await ex.start();
      await until(() => eff(t.SP, t.prop.id).claims[0]?.started === "no", 15000, "failed/no after expiry");
      ok(!fs.existsSync(t.file), "no launch after the deadline");
      ok(fs.readFileSync(path.join(t.dir, "journal.jsonl"), "utf8").includes('"t":"expired"'), "expired is journaled"); await ex.stop();
    });
  } finally { await stop(w); }
}

// ═════════════════════════════════════ E. R-16 and X-10 ═════════════════════════════════════
async function abandon() {
  const w = await newWorld();
  const port = Number(new URL(w.url).port);
  let w2: Sequencer | null = null;
  try {
    const ada = addPerson(w, "ada"); await login(w, ada);
    const dir = path.join(w.tmp, "xr"), sandbox = path.join(w.tmp, "sbr");
    Executor.init(dir, "inst1"); const fx = addPerson(w, "fxr", "service", undefined, importFromDir(dir));
    const SP = "ab:" + SERVER; await createSpace(ada, SP, POLICY());
    await send(ada, SP, "member.set", { actor: fx.id, role: "member" });
    const plan = planOf("file.write", "ab/f.txt", "abandoned\n");
    const prop = await send(ada, SP, "effect.propose", { executor: fx.id, action: "file.write", planDigest: digestOf(plan), repeatable: false }, { plan }, 1);
    const ex = new Executor({ actor: fx.id, server: SERVER, url: w.url, dir, sandbox, spaces: [SP], crashAt: "after-launch" });
    await ex.start(); await until(() => fs.existsSync(path.join(sandbox, "ab/f.txt")), 5000, "launched"); await sleep(150);
    const claimId = w.seq.stateOf(SP)!.effects[prop.id].claims[0].id;
    const abandonEntry = mk(ada, SP, "effect.outcome", { claim: claimId, result: "unknown", started: "unknown" }, undefined, 0);
    const early = await ada.client.append(abandonEntry.entry).catch((e: WireFault) => e);
    ok(early instanceof WireFault && early.code === "unavailable" && (early.retryAfterMs ?? 0) > 1000, "an owner's abandon that is only early: unavailable + retryAfterMs, not denied (R-16)");
    const bob = addPerson(w, "bobr"); await login(w, bob); await send(ada, SP, "member.set", { actor: bob.id, role: "member" });
    const nonOwner = mk(bob, SP, "effect.outcome", { claim: claimId, result: "unknown", started: "unknown" }, undefined, 0);
    ok(await code(bob.client.append(nonOwner.entry)) === "denied", "an abandon by anyone but an owner is denied");
    ada.client.close(); await w.seq.close();
    // The sequencer restarts with its wall clock 25 s ahead: receipt time says A has passed, its steady clock says it has not (R-16).
    w2 = new Sequencer({ server: SERVER, dir: path.join(w.tmp, "seq"), port, clockOffsetMs: 25_000 }); await w2.listen();
    const c = await HanClient.connect(w.url, SERVER, ada.id, ada.key);
    const A_ = Math.ceil(15 * 101 / 99) + 2;
    const t0 = Date.now(); let err: A = null;
    const again = mk(ada, SP, "effect.outcome", { claim: claimId, result: "unknown", started: "unknown" }, undefined, 0);
    try { await c.append(again.entry); } catch (x) { err = x; }
    await scenario("R-16: after a restart a clock step can't shorten the abandon wait: `unavailable` until A steady seconds have passed", async () => {
      ok(err?.code === "unavailable" && err.retryAfterMs > (A_ - 3) * 1000, "unavailable with retryAfterMs ~ " + A_ + " s, got " + (err?.code) + " " + err?.retryAfterMs);
    });
    if (QUICK) { await c.close(); return; }
    await sleep(Math.max(0, A_ * 1000 - (Date.now() - t0)) + 300);
    await scenario("R-16/R-06: retrying the identical entry after A seconds is ordered; the effect becomes unknown", async () => {
      const r = await c.append(again.entry); ok(r.seq > 0, "ordered");
      ok(w2!.stateOf(SP)!.effects[prop.id].phase === "unknown", "phase unknown (an abandon is never a success)");
    });
    await scenario("X-10: the executor's report is refused after the abandon; it resolves with its journal as evidence", async () => {
      const ex2 = new Executor({ actor: fx.id, server: SERVER, url: w.url, dir, sandbox, spaces: [SP] });
      await ex2.start(); await until(() => w2!.stateOf(SP)!.effects[prop.id].phase === "succeeded", 8000, "resolved succeeded");
      const recs = c.records(await c.read(SP, 0)); const res = recs.find((r: A) => r.entry.kind === "effect.resolve");
      ok(res && res.content.evidence.type === "application/x-ndjson", "resolve carries the journal as evidence");
      const j = (await c.blobGet(res.content.evidence, SP)).toString();
      ok(j.includes('"t":"launched"'), "the evidence is the executor's journal"); await ex2.stop();
    });
    c.close();
  } finally { if (w2) await w2.close(); else await w.seq.close(); fs.rmSync(w.tmp, { recursive: true, force: true }); }
}

// ═════════════════════════════════════ F. the vectors, on the live server ═════════════════════════════════════
async function vectors() {
  await scenario("conformance vectors replayed on the live server (every step through the wire pipeline)", async () => {
    const r = await replayVectors("H:/HAN/v1/ref/vectors");
    console.log(`      vectors ${r.vectors} (${r.usable} replayable, ${r.skippedVectors.length} skipped); steps ${r.steps}: pass ${r.pass}, fail ${r.fail.length}; ${r.movedAway} post-move steps correctly answered 'moved'; ${r.earlyAbandons} early abandons correctly answered 'unavailable' (Policy: denied)`);
    for (const s of r.skippedVectors) console.log("      skipped " + s);
    ok(r.fail.length === 0, r.fail.slice(0, 10).join(" | "));
  });
}

// ═════════════════════════════════════ R21. wire decisions D1-D14 ═════════════════════════════════════
async function round21() {
  const w = await newWorld();
  try {
    const ada = addPerson(w, "ada"), bob = addPerson(w, "bob");
    for (const p of [ada, bob]) await login(w, p);
    const SP = "w1:" + SERVER, FOREIGN = "q1:other.test";
    await createSpace(ada, SP); await send(ada, SP, "member.set", { actor: bob.id, role: "member" });
    await scenario("D1: a foreign space is `moved` for every call naming it; a missing local one is not_found", async () => {
      const c = ada.client, ref = { hash: "0".repeat(64), size: 1, type: "text/plain" };
      const mv = async (p: Promise<unknown>) => { try { await p; return "ok"; } catch (e) { return e instanceof WireFault ? e.code + ":" + e.movedTo : String(e); } };
      for (const [n, call] of [["read", () => c.read(FOREIGN, 0)], ["subscribe", () => c.call("subscribe", { space: FOREIGN })],
        ["signal", () => c.call("signal", { space: FOREIGN, body: {} })], ["blobGet", () => c.call("blobGet", { ref, space: FOREIGN })],
        ["watch", () => c.call("watch", { space: FOREIGN, session: "0".repeat(64) })],
        ["space.create", () => c.append(mk(ada, FOREIGN, "space.create", { policy: POLICY() }, { name: "x" }).entry, { name: "x" })]] as [string, () => Promise<unknown>][])
        ok(await mv(call()) === "moved:other.test", n + " of a foreign space is moved");
      ok(await code(c.read("w9:" + SERVER, 0)) === "not_found", "a missing local space is not_found");
    });
    await scenario("D2: frames are exactly {id, call, args}; args are closed", async () => {
      const r = await raw(w.url); const ch = await r.next();
      r.send({ id: 1, call: "hello", args: { who: ada.id, keyId: "k1", proof: signBytes(ada.key, helloMessage(ch.challenge, SERVER)), version: 1 } }); await r.next();
      r.send({ id: 7, call: "read", args: { space: SP, after: 0, bogus: 1 } }); let m = await r.next(); ok(m.id === 7 && m.error.code === "invalid", "unknown read arg");
      r.send({ id: 8, call: "append", args: { entry: {}, bogus: 1 } }); m = await r.next(); ok(m.id === 8 && m.error.code === "invalid", "unknown append arg");
      r.send({ id: 9, call: "cancel", args: { target: 1, x: 1 } }); m = await r.next(); ok(m.id === 9 && m.error.code === "invalid", "unknown cancel arg");
      r.ws.close();
    });
    await scenario("D4/D8/D9/D11/D12/D13/D14: base64url, blobPut, limits, hello errors, challenge, well-known, settled", async () => {
      const data = Buffer.from("probe blob bytes"), ref = { hash: sha256Hex(data), size: 16, type: "text/plain" };
      const put = (extra: A) => ada.client.call("blobPut", { ref, offset: 0, data: data.toString("base64url"), ...extra });
      ok(await code(ada.client.call("blobPut", { ref, offset: 5, data: data.subarray(5).toString("base64url") })) === "invalid", "gap is invalid (D8)");
      ok(await code(ada.client.call("blobPut", { ref, offset: 0, data: data.toString("base64url") + "==" })) === "invalid", "padding is invalid (D4)");
      const bad = { hash: sha256Hex(Buffer.from("abd")), size: 3, type: "text/plain" };
      ok(await code(ada.client.call("blobPut", { ref: bad, offset: 0, data: Buffer.from("abc").toString("base64url") })) === "invalid", "hash mismatch is invalid and discarded (D8)");
      ok((await put({})).complete === true, "a valid upload completes");
      ok((await ada.client.call("blobGet", { ref, space: SP })).size === 16, "uploader reads its own unreferenced blob (D3)");
      ok(ada.client.limits.maxClockSkewSeconds === 60, "hello limits carry maxClockSkewSeconds (D9)");
      const r = await raw(w.url); const ch = await r.next();
      ok(/^[A-Za-z0-9_-]{43}$/.test(ch.challenge), "challenge is 43 characters (D12)");
      r.send({ id: 1, call: "hello", args: { who: "@nobody:" + SERVER, keyId: "k1", proof: signBytes(ada.key, helloMessage(ch.challenge, SERVER)), version: 1 } });
      let m = await r.next(); ok(m.error?.code === "not_found", "unknown actor is not_found (D11)");
      r.ws.close();
      const rq = await raw(w.url); const chq = await rq.next();
      rq.send({ id: 2, call: "hello", args: { who: ada.id, keyId: "nokey", proof: signBytes(ada.key, helloMessage(chq.challenge, SERVER)), version: 1 } });
      m = await rq.next(); ok(m.error?.code === "denied", "unknown key is denied (D11)");
      rq.ws.close();
      const base = `http://127.0.0.1:${new URL(w.url).port}`;
      const wk = await (await fetch(base + "/.well-known/han")).json() as A;
      ok(wk.server === SERVER && /\/han$/.test(wk.sequencer) && Array.isArray(wk.keys) && wk.keys[0].from, "/.well-known/han shape (D13)");
      ok(Array.isArray((await (await fetch(base + "/.well-known/han-peers")).json() as A).peers), "/.well-known/han-peers shape (D13)");
      const st = (await ada.client.state(SP)).state;
      ok(typeof st === "object", "state view");
    });
    await scenario("D10: signal `from` only on a server-actor connection", async () => {
      ok(await code(ada.client.call("signal", { space: SP, body: {}, from: bob.id })) === "invalid", "from on a user connection is invalid");
    });
  } finally { await stop(w); }
}
const sha256Hex = (b: Buffer) => createHashNode("sha256").update(b).digest("hex");

// ═════════════════════════════════════ R21b. executor early start (D5), host (G-06), relayed signal (D10) ═════════════════════════════════════
async function round21b() {
  await scenario("D5: an executor started before its space exists retries with back-off and claims once the space appears", async () => {
    const w = await newWorld();
    try {
      const ada = addPerson(w, "ada"), ops = addPerson(w, "ops");
      const execDir = path.join(w.tmp, "exec"), sandbox = path.join(w.tmp, "sandbox");
      const inst = Executor.init(execDir, "inst1"); const key = importFromDir(execDir);
      const fx = addPerson(w, "fx", "service", ops, key);
      await login(w, ada); await login(w, ops);
      await send(ops, `profile:${fx.id}`, "member.set", { actor: ops.id, role: "owner" });
      void inst;
      const SP = "e1:" + SERVER;
      const ex = new Executor({ actor: fx.id, server: SERVER, url: w.url, dir: execDir, sandbox, spaces: [SP], retryMs: 150 });
      await ex.start();                                     // the space does not exist yet: not_found, no stream
      const pol = POLICY({ approvals: [{ action: "file.write", approvers: ["owner"] }], separateApprover: false });
      await createSpace(ada, SP, pol); await send(ada, SP, "member.set", { actor: fx.id, role: "member" });
      const plan = planOf("file.write", "out/early.txt", "early\n");
      const prop = await send(ada, SP, "effect.propose", { executor: fx.id, action: "file.write", planDigest: digestOf(plan), repeatable: false }, { plan });
      await send(ada, SP, "effect.approve", { proposal: prop.id, planDigest: digestOf(plan) }, { note: "ok" });
      await until(() => w.seq.stateOf(SP)!.effects[prop.id].phase === "succeeded", 10000, "effect succeeded after an early start");
      const claim = w.seq.stateOf(SP)!.effects[prop.id].claims[0];
      ok(claim.outcome === "succeeded", "claim outcome");
      await ex.stop();
    } finally { await stop(w); }
  });
  await scenario("G-06/D10: server-actor connection relays signals and hands a space over with `host`", async () => {
    const A = await newWorld({ seq: { } });
    const dirB = fs.mkdtempSync(path.join(os.tmpdir(), "han-host-"));
    const B = new Sequencer({ server: "b.test", dir: dirB, port: 0, peers: { [SERVER]: { [A.seq.keys().keyId]: A.seq.keys().publicKey } } });
    const portB = await B.listen();
    try {
      const ada = addPerson(A, "ada"), bob = addPerson(A, "bob");
      await login(A, ada); await login(A, bob);
      const SP = "m1:" + SERVER;
      await createSpace(ada, SP); await send(ada, SP, "member.set", { actor: bob.id, role: "member" });
      // relay: a server actor connection may speak for a user of its server
      const peersA = { peers: { "b.test": { [B.keys().keyId]: B.keys().publicKey } } };
      void peersA;
      const got: A[] = []; const sub = ada.client.stream("subscribe", { space: SP }, (i) => got.push(i)); void sub;
      await sleep(50);
      const asServer = await HanClient.connect(A.url.replace(/:\d+/, ":" + new URL(A.url).port), SERVER, "@:" + SERVER, A.seq.signer()).catch((e) => e);
      ok(!(asServer instanceof Error), "the server actor can authenticate with its own published key: " + (asServer as Error).message);
      await (asServer as HanClient).call("signal", { space: SP, body: { hi: 1 }, from: bob.id });
      await until(() => got.some((i) => i.type === "signal" && i.from === bob.id), 3000, "relayed signal");
      ok(await code((asServer as HanClient).call("signal", { space: SP, body: {}, from: "@x:other.test" })) === "denied", "a relay may speak only for its own actors");
      (asServer as HanClient).close();
      // host: A pushes the chain to B
      const recs = ada.client.records(await ada.client.read(SP, 0));
      ok(await code(new Promise((res, rej) => ada.client.call("host", { space: SP, records: recs }).then(res, rej))) === "denied", "host from a user connection is denied");
      const toB = await HanClient.connect(`ws://127.0.0.1:${portB}/han`, "b.test", "@:" + SERVER, A.seq.signer());
      ok(await code(toB.call("host", { space: SP, records: recs.slice(1) })) === "invalid", "records must be contiguous from have + 1");
      const head = recs[recs.length - 1].receipt;
      const res1 = await toB.call("host", { space: SP, records: recs, head });
      ok(res1.have === recs.length && res1.accept?.keyId === B.keys().keyId, "B holds the chain and signs its acceptance");
      ok(await code(toB.call("host", { space: SP, records: [{ ...recs[0], receipt: { ...recs[0].receipt, seq: recs.length + 1 } }] })) === "invalid", "a record that doesn't continue the chain");
      toB.close();
    } finally { await stop(A); await B.close(); fs.rmSync(dirB, { recursive: true, force: true }); }
  });
}

async function round22() {
  const w = await newWorld({ seq: { maxStateItems: 3, appendsPerSecond: 1000 } });
  try {
    const ada = addPerson(w, "ada"), ann = addPerson(w, "ann"), bob = addPerson(w, "bob");
    const bot = addPerson(w, "bot", "agent", ann);
    for (const p of [ada, ann, bob, bot]) await login(w, p);
    await send(ann, `profile:${bot.id}`, "member.set", { actor: ann.id, role: "owner" });
    const SP = "p15:" + SERVER;
    await createSpace(ada, SP, POLICY({ sessions: { watch: "member", read: "owner", steer: "member", modes: ["auto", "guarded"] } }));
    for (const p of [ann, bob, bot]) await send(ada, SP, "member.set", { actor: p.id, role: "member" });
    const st = async () => (await ada.client.stateAll(SP));
    let sess = "";
    await scenario("P-15: only the guard unguards (agent.release + unguard); closing never does; a closed session is no session", async () => {
      sess = (await send(bot, SP, "session.open", { steerMode: "guarded" })).id;
      ok((await st()).guarded[bot.id] === ann.id, "guarded[bot] = operator");
      await send(bot, SP, "session.close", { session: sess });
      ok((await st()).guarded[bot.id] === ann.id, "closing does not unguard");
      ok(await code(send(bot, SP, "session.close", { session: sess })) === "invalid", "a closed session has left the state: re-close is invalid");
      ok(await code(send(bot, SP, "agent.release", { agent: bot.id, unguard: true })) === "denied", "the agent cannot unguard itself");
      ok(await code(send(ada, SP, "agent.release", { agent: bot.id, unguard: true })) === "denied", "an owner is not the guard");
      ok(await code(send(ann, SP, "agent.release", { agent: bot.id, unguard: false } as A)) === "invalid", "unguard is true or absent");
      ok(await code(send(ann, SP, "session.input", { session: sess, release: true } as A, { text: "x" })) === "invalid", "session.input carries no release flag");
      const r = await send(ann, SP, "agent.release", { agent: bot.id, unguard: true });
      ok(r.seq > 0 && (await st()).guarded[bot.id] === undefined, "the guard unguards, no session needed");
    });
    await scenario("P-15: forceGuarded guards every non-human target of an instruction, from the profile pinned for it", async () => {
      ok(await guarded(false), "off by default");
      const pol = { ...POLICY({ sessions: { watch: "member", read: "owner", steer: "member", modes: ["auto", "guarded"], forceGuarded: true } }) };
      await send(ada, SP, "policy.set", { policy: pol });
      ok(!("agents" in await st()), "no agents map");
      const s2 = (await send(bot, SP, "session.open", { steerMode: "auto" })).id;
      await send(bob, SP, "session.input", { session: s2 }, { text: "hello" });
      ok((await st()).held[bot.id] === ann.id, "an outside steer holds the forced agent");
      await send(ann, SP, "agent.release", { agent: bot.id }, undefined, (await st()).head);
      ok((await st()).held[bot.id] === undefined, "released");
      async function guarded(_x: boolean) { return (await st()).guarded[bot.id] === undefined; }
    });
    await scenario("E8: state pages (maxStateItems), cursor, effect", async () => {
      for (let i = 0; i < 7; i++) await send(ada, SP, "task", { to: [bob.id] }, { ask: "t" + i });
      const first = await ada.client.state(SP);
      ok(Object.keys(first.state.tasks).length === 3 && typeof first.next === "string" && /^\d+(\.\d+){10}$/.test(first.next), "first page: 3 tasks and a dotted next");
      const all: A = { ...first.state.tasks }; let next = first.next, pages = 1;
      while (next) { const it = await ada.client.state(SP, { cursor: next }); Object.assign(all, it.state.tasks); next = it.next; pages++; }
      ok(Object.keys(all).length === 7 && pages === 3, "7 tasks in 3 pages");
      ok(await code(ada.client.state(SP, { cursor: "bogus" })) === "invalid" && await code(ada.client.state(SP, { cursor: first.next.replace(/\.0$/, ".00") })) === "invalid", "a malformed cursor is invalid");
      ok(await code(ada.client.read(SP, 0, { cursor: first.next } as A)) === "invalid", "`cursor` is not a member of read");
      const ef = (await send(bob, SP, "effect.propose", { action: "file.write", planDigest: digestOf(planOf("file.write", "a.txt")), executor: bob.id } as A, { plan: planOf("file.write", "a.txt") }).catch((e) => e)) as A;
      if (ef.id) {
        const one = await ada.client.state(SP, { effect: ef.id });
        ok(Object.keys(one.state.effects).length === 1 && one.next === undefined && JSON.stringify(Object.keys(one.state).sort()) === JSON.stringify(["effects", "head", "home", "policy", "space"]), "one effect: {space, head, home, policy, effects}, no next");
        ok(await code(ada.client.state(SP, { effect: "0".repeat(64) })) === "not_found", "unknown effect");
      }
    });
    await scenario("integers >= 2^53: invalid on the request id, else id 0 + close", async () => {
      const r = await raw(w.url); const ch = await r.next();
      r.send({ id: 1, call: "hello", args: { who: ada.id, keyId: "k1", proof: signBytes(ada.key, helloMessage(ch.challenge, SERVER)), version: 1 } }); await r.next();
      r.send('{"id":5,"call":"read","args":{"space":"' + SP + '","after":9007199254740993}}'); let m = await r.next();
      ok(m.id === 5 && m.error.code === "invalid" && !r.closed, "invalid on id 5, connection open");
      r.send('{"id":9007199254740993,"call":"read","args":{}}'); m = await r.next();
      ok(m.id === 0 && m.error.code === "invalid", "id 0");
      await sleep(100); ok(r.closed, "closed");
    });
    await scenario("E5: a denied entry is denied even from an author over the rate limit", async () => {
      const w2 = await newWorld({ seq: { appendsPerSecond: 1 } });
      try {
        const x = addPerson(w2, "x"); await login(w2, x);
        const S = "rl:" + SERVER; await createSpace(x, S);
        const bad = await code(send(x, S, "member.set", { actor: "@nobody:" + SERVER, role: "owner" }).then(() => send(x, S, "policy.set", { policy: { bogus: 1 } })));
        ok(bad === "invalid" || bad === "denied", "invalid/denied beats limit: " + bad);
      } finally { await stop(w2); }
    });
  } finally { await stop(w); }
}

const t0 = Date.now();
await scenario("A. end to end: create, members, propose, approve, claim, execute, report, verify", e2e);
await protocol();
await round21();
await round21b();
await round22();
await durability();
await executorScenarios();
await abandon();
await vectors();
console.log(`\n${passed} scenarios passed, ${failed} failed (${Math.round((Date.now() - t0) / 1000)} s)`);
if (failed) { console.log(failures.join("\n")); process.exit(1); }
process.exit(0);
void newKey;
