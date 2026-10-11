// TS client driver: runs the shared scenario (scenario.json) through the clean-room HanClient, op by op.
//   node ts_driver.mjs --scenario F --keys DIR --url ws://127.0.0.1:P/han --tap http://127.0.0.1:T --out results.json [--mode scenario|exec]
import fs from "node:fs";
import { createHash } from "node:crypto";
import { HanClient, WireFault } from "../cleanroom-ts/dist/client.js";
import { importKey, signBytes, sigMessage } from "../cleanroom-ts/dist/keys.js";
import { entryId, jcs } from "../cleanroom-ts/dist/wire.js";

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i < 0 ? d : process.argv[i + 1]; };
const SC = JSON.parse(fs.readFileSync(arg("scenario"), "utf8"));
const FLAGS = arg("flag-dir", null);
const KEYS = arg("keys"), URL_ = arg("url"), TAP = arg("tap"), OUT = arg("out"), MODE = arg("mode", "scenario");
const SERVER = SC.server, SPACE = SC.space;
const sha = (b) => createHash("sha256").update(b).digest("hex");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const actorId = (n) => `@${n}:${SERVER}`;
const keyOf = (n) => { const j = JSON.parse(fs.readFileSync(`${KEYS}/${n}.json`, "utf8")); return importKey(j.keyId, j.public, j.private); };
const blobBytes = (name) => { const n = name === "b1" ? 2000 : 1700000; const b = Buffer.alloc(n); for (let i = 0; i < n; i++) b[i] = (i * 7 + 3) % 251; return b; };
const mark = async (name) => { try { await fetch(`${TAP}/__mark?name=${encodeURIComponent(name)}`); } catch { /* */ } };
async function until(f, ms = 6000, what = "condition") { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw new Error("timed out waiting for " + what); await sleep(20); } }

const conns = {}, ids = {}, sent = {}, heads = {}, blobs = {};
const sub = (v) => {
  if (typeof v === "string") {
    if (v.startsWith("$id:")) return ids[v.slice(4)];
    if (v.startsWith("$blob:")) return blobs[v.slice(6)];
    return v;
  }
  if (Array.isArray(v)) return v.map(sub);
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, sub(x)]));
  return v;
};
function build(name) {
  const d = SC.entries[name], space = d.space ?? SPACE, content = d.content === undefined ? undefined : sub(d.content);
  const e = { v: 1, space, kind: d.kind, author: actorId(d.actor), keyId: "k1", body: sub(d.body), contentHash: content === undefined ? "" : sha(jcs(content)),
    refs: d.refs.map(sub), seen: d.seen === "head" ? (heads[space] ?? 0) : d.seen, nonce: d.nonce, time: SC.time };
  e.sig = signBytes(keyOf(d.actor), sigMessage(entryId(e)));
  return { entry: e, content };
}
async function login(n) { return (conns[n] ??= await HanClient.connect(URL_, SERVER, actorId(n), keyOf(n))); }
async function send(name) {
  const d = SC.entries[name], { entry, content } = build(name);
  const c = await login(d.actor);
  const r = await c.append(entry, content);
  if (r.id !== entryId(entry)) throw new Error("receipt id is not the entry id");
  ids[name] = r.id; sent[name] = { entry, content }; heads[r.space] = Math.max(heads[r.space] ?? 0, r.seq);
  return r;
}
const ok = (c, m) => { if (!c) throw new Error("check failed: " + m); };
const results = [], status = {};
async function op(name, requires, f) {
  const blocked = requires.filter((r) => status[r] !== "pass");
  if (blocked.length) { status[name] = "skip"; results.push({ op: name, status: "skip", error: "blocked by " + blocked.join(",") }); return; }
  await mark("op:" + name);
  try { const detail = await f(); status[name] = "pass"; results.push({ op: name, status: "pass", detail: detail ?? null }); }
  catch (e) { status[name] = "fail"; results.push({ op: name, status: "fail", error: e instanceof WireFault ? `${e.code}: ${e.message}` : String(e?.message ?? e) }); }
}
async function readAll(c, space, after = 0, opts = {}) { const items = await c.read(space, after, opts); return items; }

async function scenario() {
  await op("hello", [], async () => {
    const c = await login("alice");
    ok(c.helloOk && c.helloOk.limits && typeof c.helloOk.time === "string" && Array.isArray(c.helloOk.knownHeads), "hello ok shape");
    return { limits: Object.keys(c.helloOk.limits).sort(), durability: c.helloOk.durability };
  });
  if (status.hello === "pass") { await mark("setup"); await login("bob"); }   // other connections are opened outside any op, so op frames stay pure
  await op("space.create", ["hello"], async () => { const r = await send("create"); ok(r.seq === 1 && r.prev === "", "seq 1"); return { receiptKeys: Object.keys(r).sort(), profiles: r.profiles }; });
  await op("member.set", ["space.create"], async () => { for (const n of ["m_bob", "m_vera", "m_x"]) await send(n); });
  await op("message", ["member.set"], async () => { const r = await send("msg1"); await send("msg_uni"); ok(r.seq === 5, "seq 5"); return { profiles: r.profiles }; });
  await op("read", ["message"], async () => {
    const c = await login("alice");
    const items = await readAll(c, SPACE, 0), recs = items.filter((i) => i.type === "record").map((i) => i.record);
    ok(recs.length === heads[SPACE], `records ${recs.length} vs ${heads[SPACE]}`);
    recs.forEach((r, i) => ok(r.receipt.seq === i + 1, "seq order"));
    for (const n of ["create", "m_bob", "msg1", "msg_uni"]) { const r = recs.find((x) => x.receipt.id === ids[n]); ok(r, n + " present"); ok(jcs(r.entry) === jcs(sent[n].entry), n + " entry round-trips"); ok(jcs(r.content ?? null) === jcs(sent[n].content ?? null), n + " content round-trips"); }
    return { n: recs.length, recordKeys: Object.keys(recs[0]).sort() };
  });
  await op("read.state", ["message"], async () => {
    const c = await login("alice");
    const items = await readAll(c, SPACE, 0, { state: true }), st = items[0];
    ok(st?.type === "state", "first item is state"); ok(st.state.head === heads[SPACE], "state head");
    ok(st.state.roles?.[actorId("bob")] === "member", "bob is a member in roles");
    return { stateKeys: Object.keys(st.state).sort() };
  });
  await op("follow", ["message"], async () => {
    const c = await login("alice"), got = [];
    const s = c.stream("read", { space: SPACE, after: heads[SPACE], follow: true }, (i) => got.push(i));
    await sleep(150); await send("msg2");
    await until(() => got.some((i) => i.type === "record" && i.record.receipt.id === ids.msg2), 4000, "live record");
    await s.cancel();
  });
  await op("subscribe", ["message"], async () => {
    const c = await login("alice"), got = [];
    const s = c.stream("subscribe", { space: SPACE, after: heads[SPACE] }, (i) => got.push(i));
    await sleep(150); await send("msg3");
    await until(() => got.some((i) => i.type === "record" && i.record.receipt.id === ids.msg3), 4000, "live record");
    await s.cancel();
  });
  await op("signal", ["message"], async () => {
    const c = await login("alice"), b = await login("bob"), got = [];
    const s = c.stream("subscribe", { space: SPACE }, (i) => got.push(i));
    await sleep(150); await b.call("signal", { space: SPACE, body: SC.signalBody });
    await until(() => got.some((i) => i.type === "signal"), 4000, "signal item");
    const sig = got.find((i) => i.type === "signal");
    ok(sig.from === actorId("bob") && jcs(sig.body) === jcs(SC.signalBody), "signal from/body");
    await s.cancel();
  });
  await op("propose", ["member.set"], async () => { await send("propose"); });
  await op("approve", ["propose"], async () => {
    await send("approve");
    const c = await login("alice"); const st = (await readAll(c, SPACE, 0, { state: true }))[0].state;
    ok(st.effects?.[ids.propose]?.phase === "ready", "effect phase ready after approval, got " + st.effects?.[ids.propose]?.phase);
  });
  await op("blobPut", ["member.set"], async () => { const c = await login("alice"); blobs.b1 = await c.blobPut(blobBytes("b1"), "application/octet-stream"); ok(blobs.b1.hash === sha(blobBytes("b1")), "ref hash"); });
  await op("blobGet", ["blobPut"], async () => {
    await send("msg_b1"); const b = await login("bob");
    const got = await b.blobGet(blobs.b1, SPACE); ok(got.equals(blobBytes("b1")), "bytes equal");
  });
  await op("blobPut.multipart", ["member.set"], async () => { const c = await login("alice"); blobs.b2 = await c.blobPut(blobBytes("b2"), "application/octet-stream"); });
  await op("blobGet.multipart", ["blobPut.multipart"], async () => {
    await send("msg_b2"); const b = await login("bob");
    const got = await b.blobGet(blobs.b2, SPACE); ok(got.equals(blobBytes("b2")), "bytes equal");
  });
  await op("session.open", ["member.set"], async () => { await send("sess_open"); });
  await op("session.input", ["session.open"], async () => { await send("sess_input"); });
  // `stream` and `watch` share one frame segment: the watcher is open while the actor publishes
  const sw = ["session.open"];
  if (sw.every((r) => status[r] === "pass")) {
    await mark("op:stream+watch");
    const bob = await login("bob"), alice = await login("alice"), got = [];
    let wErr = null, w = null;
    try { w = bob.stream("watch", { space: SPACE, session: ids.sess_open }, (i) => got.push(i)); w.done.catch((e) => { wErr = e; }); await sleep(150); } catch (e) { wErr = e; }
    const item = (it) => ({ space: SPACE, session: ids.sess_open, time: SC.time, ...it });
    try {
      for (const it of SC.streamItems) await alice.call("stream", { item: item(it) });
      await alice.call("stream", { item: item(SC.streamItems[2]) });   // a repeat of the last item is ok
      status["stream"] = "pass"; results.push({ op: "stream", status: "pass" });
    } catch (e) { status["stream"] = "fail"; results.push({ op: "stream", status: "fail", error: e instanceof WireFault ? `${e.code}: ${e.message}` : String(e.message) }); }
    try {
      if (wErr) throw wErr;
      await until(() => got.length >= 3, 3000, "3 watch items");
      ok(got.length === 3, "exactly 3 items (no duplicate for the repeat), got " + got.length);
      got.forEach((g, i) => ok(g.n === i + 1 && g.type === SC.streamItems[i].type, "item " + (i + 1)));
      await w.cancel(); status["watch"] = "pass"; results.push({ op: "watch", status: "pass" });
    } catch (e) { status["watch"] = "fail"; results.push({ op: "watch", status: "fail", error: e instanceof WireFault ? `${e.code}: ${e.message}` : String(e.message), detail: { got: got.length } }); }
  } else for (const n of ["stream", "watch"]) results.push({ op: n, status: "skip", error: "blocked by session.open" });
  await op("session.close", ["session.open"], async () => { await send("sess_close"); });
  await op("task", ["member.set"], async () => { await send("task"); await send("task_update"); });
  await op("redact", ["subscribe"], async () => {
    await send("redact"); const c = await login("alice");
    const recs = (await readAll(c, SPACE, 0)).filter((i) => i.type === "record").map((i) => i.record);
    const r = recs.find((x) => x.receipt.id === ids.msg2); ok(r && r.redacted === true && (r.content === null || r.content === undefined), "msg2 is redacted with no content");
    return { redactedRecordKeys: Object.keys(r).sort() };
  });
}

async function execSetup() {
  const result = { phase: "setup" };
  await mark("exec:setup");
  for (const n of ["create", "m_x", "m_bob"]) await send(n);
  if (FLAGS) {   // "late" order: the space exists before the executor starts; the harness starts it, then writes `go`
    fs.writeFileSync(`${FLAGS}/spaceready`, "1"); const t0 = Date.now();
    while (!fs.existsSync(`${FLAGS}/go`) && Date.now() - t0 < 40000) await sleep(100);
  }
  for (const n of ["propose", "approve"]) await send(n);
  result.proposal = ids.propose;
  const c = await login("alice");
  const t = Date.now(); let st;
  await mark("exec:wait");
  while (Date.now() - t < 60000) {
    st = (await readAll(c, SC.space, 0, { state: true }))[0].state;
    const ph = st.effects?.[ids.propose]?.phase;
    if (ph === "succeeded" || ph === "failed") break;
    await sleep(500);
  }
  const eff = st.effects?.[ids.propose];
  result.phase = eff?.phase; result.claims = eff?.claims;
  result.recordsAfter = (await readAll(c, SC.space, 0)).filter((i) => i.type === "record").map((i) => ({ seq: i.record.receipt.seq, kind: i.record.entry.kind, author: i.record.entry.author }));
  return result;
}

let out;
try {
  if (MODE === "exec") out = await execSetup(); else { await scenario(); out = { ops: results }; try { out.finalState = (await readAll(await login("alice"), SPACE, 0, { state: true }))[0].state; } catch (e) { out.finalStateError = String(e?.message ?? e); } }
} catch (e) { out = { fatal: String(e?.stack ?? e), ops: results }; }
fs.writeFileSync(OUT, JSON.stringify(out, null, 1));
for (const c of Object.values(conns)) try { c.close(); } catch { /* */ }
setTimeout(() => process.exit(0), 100);
