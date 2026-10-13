// Wire probes: raw frames at a server, to compare error codes and edge behaviour (the places the 47 W-xx gaps predict).
//   node probe.mjs --port P --server S --keys DIR --space s1:S --out probes.json
// Each probe records what the spec text says (expect) and what came back; report.py compares the two servers.
import fs from "node:fs";
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { importKey, signBytes, helloMessage, sigMessage } from "../../witnesses/cleanroom-ts/dist/keys.js";
import { entryId, jcs } from "../../witnesses/cleanroom-ts/dist/wire.js";
const require = createRequire(import.meta.url);
const WebSocket = require("../../witnesses/cleanroom-ts/node_modules/ws");

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i < 0 ? d : process.argv[i + 1]; };
const PORT = arg("port"), SERVER = arg("server"), KEYS = arg("keys"), SPACE = arg("space"), OUT = arg("out");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const keyOf = (n) => { const j = JSON.parse(fs.readFileSync(`${KEYS}/${n}.json`, "utf8")); return importKey(j.keyId, j.public, j.private); };
const actor = (n) => `@${n}:${SERVER}`;
const sha = (b) => createHash("sha256").update(b).digest("hex");
const mark = async (n) => { try { await fetch(`http://127.0.0.1:${PORT}/__mark?name=${encodeURIComponent(n)}`); } catch { /* */ } };

class Raw {
  constructor(ws) { this.ws = ws; this.frames = []; this.closed = null; this.nextId = 100;
    ws.on("message", (d, bin) => { if (bin) this.frames.push({ binary: d.length }); else { try { this.frames.push(JSON.parse(d.toString("utf8"))); } catch { this.frames.push({ unparseable: d.toString("utf8").slice(0, 200) }); } } });
    ws.on("close", (c) => { this.closed = c; }); ws.on("error", () => {}); }
  static async open(path = "/han") {
    const ws = new WebSocket(`ws://127.0.0.1:${PORT}${path}`);
    const r = new Raw(ws);
    await new Promise((res, rej) => { ws.once("open", res); ws.once("error", rej); ws.once("unexpected-response", (_q, resp) => rej(new Error("HTTP " + resp.statusCode))); });
    await r.until(() => r.frames.length > 0 || r.closed !== null, 3000);
    return r;
  }
  get challenge() { return this.frames[0]; }
  send(x) { this.ws.send(typeof x === "string" ? x : JSON.stringify(x)); }
  async until(pred, ms = 3000) { const t = Date.now(); while (!pred()) { if (Date.now() - t > ms) return false; await sleep(15); } return true; }
  /** send a request and gather its frames until ok / error / end (or an id-0 error, or the connection closing) */
  async ask(call, args, { id = this.nextId++, raw, wait = 3000, settle = 0 } = {}) {
    const from = this.frames.length;
    this.send(raw ?? { id, call, args });
    const done = () => this.frames.slice(from).some((f) => f.id === id && ("ok" in f || "error" in f || "end" in f)) || this.frames.slice(from).some((f) => f.id === 0 && f.error) || this.closed !== null;
    await this.until(done, wait); if (settle) await sleep(settle);
    return this.frames.slice(from);
  }
  async hello(who, { key = keyOf(who), keyId = "k1", dialed = SERVER, version = 1, badProof = false } = {}) {
    const ch = this.challenge;
    let proof = signBytes(key, helloMessage(ch.challenge, dialed));
    if (badProof) proof = signBytes(key, helloMessage(ch.challenge + "x", dialed));
    return this.ask("hello", { who: actor(who), keyId, proof, version });
  }
  close() { try { this.ws.close(); } catch { /* */ } }
}
const code = (fr) => { const e = fr.find((f) => f.error); return e ? e.error.code : fr.find((f) => "ok" in f) ? "ok" : fr.find((f) => "end" in f) ? "end" : "none"; };
const results = [];
async function probe(id, title, expect, f) {
  await mark("probe:" + id);
  const rec = { id, title, expect };
  try { const r = await f(); rec.got = r.got; rec.summary = r.summary; rec.pass = r.pass; if (r.note) rec.note = r.note; }
  catch (e) { rec.error = String(e?.message ?? e); rec.pass = false; }
  results.push(rec);
}
async function login(who, path) { const r = await Raw.open(path); const h = await r.hello(who); if (code(h) !== "ok") throw new Error("hello failed: " + JSON.stringify(h)); return r; }
let n = 0;
const B32 = "abcdefghijklmnopqrstuvwxyz234567";
function b32nonce(seed) { const h = createHash("sha256").update(seed).digest(); let s = ""; for (let i = 0; i < 26; i++) s += B32[h[i] & 31]; return s; }
function entry(who, space, kind, body, content, seen = 0) {
  const k = keyOf(who);
  const e = { v: 1, space, kind, author: actor(who), keyId: "k1", body, contentHash: content === undefined ? "" : sha(jcs(content)), refs: [], seen: 0,
    nonce: b32nonce("probe" + n++), time: "2026-01-01T00:00:00Z" };
  e.sig = signBytes(k, sigMessage(entryId(e)));
  return { e, content };
}
const expectCode = (fr, c) => ({ got: fr, summary: code(fr), pass: code(fr) === c });

async function main() {
  // find the head and a session id of s1
  const alice = await login("alice");
  const rd = await alice.ask("read", { space: SPACE, after: 0 }, { wait: 5000 });
  const recs = rd.filter((f) => f.item?.type === "record").map((f) => f.item.record);
  const head = recs.length;
  const closedSess = recs.find((r) => r.entry.kind === "session.open")?.receipt.id;

  await probe("P01", "hello with a proof over another challenge", "denied", async () => { const r = await Raw.open(); const fr = await r.hello("alice", { badProof: true }); r.close(); return expectCode(fr, "denied"); });
  await probe("P02", "read before hello", "denied", async () => { const r = await Raw.open(); const fr = await r.ask("read", { space: SPACE, after: 0 }); r.close(); return expectCode(fr, "denied"); });
  await probe("P03", "second hello on an authenticated connection", "invalid", async () => { const r = await login("bob"); const fr = await r.hello("bob"); r.close(); return expectCode(fr, "invalid"); });
  await probe("P04", "hello version 2", "invalid", async () => { const r = await Raw.open(); const fr = await r.hello("alice", { version: 2 }); r.close(); return expectCode(fr, "invalid"); });
  await probe("P05", "hello for an actor with no profile (W-03)", "invalid or denied (unspecified)", async () => {
    const r = await Raw.open(); const ch = r.challenge; const k = keyOf("alice");
    const fr = await r.ask("hello", { who: actor("nobody"), keyId: "k1", proof: signBytes(k, helloMessage(ch.challenge, SERVER)), version: 1 }); r.close();
    return { got: fr, summary: code(fr), pass: ["invalid", "denied"].includes(code(fr)) };
  });
  await probe("P06", "hello with an unknown keyId (W-03)", "denied", async () => { const r = await Raw.open(); const fr = await r.hello("alice", { keyId: "nokey" }); r.close(); return expectCode(fr, "denied"); });
  await probe("P07", "hello naming another server in the proof (dialed != server)", "denied", async () => { const r = await Raw.open(); const fr = await r.hello("alice", { dialed: "other.localhost" }); r.close(); return expectCode(fr, "denied"); });
  await probe("P08", "a frame that is not JSON", "{id:0, error:invalid}", async () => { const r = await Raw.open(); const fr = await r.ask(null, null, { raw: "not json {", id: 0 }); r.close(); return { got: fr, summary: JSON.stringify(fr.map((f) => [f.id, f.error?.code])), pass: fr[0]?.id === 0 && fr[0]?.error?.code === "invalid" }; });
  await probe("P09", "valid JSON with id 7 but a fraction (not I-JSON) (W-04)", "error invalid on id 7 (the id the client can wait on)", async () => {
    const r = await login("alice"); const fr = await r.ask(null, null, { raw: '{"id":7,"call":"read","args":{"space":"' + SPACE + '","after":0.5}}', id: 7 }); r.close();
    return { got: fr, summary: JSON.stringify(fr.map((f) => [f.id, f.error?.code])), pass: fr[0]?.id === 7 && fr[0]?.error?.code === "invalid" };
  });
  await probe("P10", "frame with an extra member (W-04)", "invalid", async () => { const r = await login("alice"); const fr = await r.ask(null, null, { raw: { id: 5, call: "cancel", args: { target: 1 }, extra: true }, id: 5 }); r.close(); return { got: fr, summary: JSON.stringify(fr.map((f) => [f.id, f.error?.code ?? ("ok" in f ? "ok" : "?")])), pass: !!fr[0]?.error }; });
  await probe("P11", "frame without args (W-04)", "invalid (args must be an object)", async () => { const r = await login("alice"); const fr = await r.ask(null, null, { raw: { id: 5, call: "cancel" }, id: 5 }); r.close(); return { got: fr, summary: JSON.stringify(fr.map((f) => [f.id, f.error?.code ?? ("ok" in f ? "ok" : "?")])), pass: fr[0]?.error?.code === "invalid" }; });
  await probe("P12", "unknown call", "invalid", async () => { const r = await login("alice"); const fr = await r.ask("frobnicate", {}); r.close(); return expectCode(fr, "invalid"); });
  await probe("P13", "binary frame", "connection closes", async () => { const r = await login("alice"); r.ws.send(Buffer.from([1, 2, 3])); const ok = await r.until(() => r.closed !== null, 2000); return { got: [{ closed: r.closed }], summary: "closed " + r.closed, pass: ok }; });
  await probe("P14", "re-append of a stored entry (R-01) returns the same receipt", "same receipt", async () => {
    const m = recs.find((r) => r.entry.kind === "message"); const bob = await login("bob");
    const fr = await bob.ask("append", { entry: m.entry, content: m.content }); bob.close();
    return { got: fr, summary: code(fr), pass: code(fr) === "ok" && jcs(fr[0].ok) === jcs(m.receipt) };
  });
  await probe("P15", "re-append of a stored entry without its content (W-16d)", "same receipt", async () => {
    const m = recs.find((r) => r.entry.kind === "message"); const bob = await login("bob");
    const fr = await bob.ask("append", { entry: m.entry }); bob.close();
    return { got: fr, summary: code(fr), pass: code(fr) === "ok" };
  });
  await probe("P16", "append by a non-member", "denied", async () => { const c = await login("carol"); const { e, content } = entry("carol", SPACE, "message", {}, { text: "let me in" }, head); const fr = await c.ask("append", { entry: e, content }); c.close(); return expectCode(fr, "denied"); });
  await probe("P17", "non-create first entry in an unknown local space (W-13b)", "invalid", async () => { const { e, content } = entry("alice", "nospace:" + SERVER, "message", {}, { text: "x" }); const fr = await alice.ask("append", { entry: e, content }); return expectCode(fr, "invalid"); });
  await probe("P18", "space.create for a space homed on another server (W-13d)", "moved", async () => {
    const pol = JSON.parse(JSON.stringify(recs[0].entry.body.policy)); const { e, content } = entry("alice", "q1:other.localhost", "space.create", { policy: pol }, { name: "q" });
    const fr = await alice.ask("append", { entry: e, content }); return { got: fr, summary: code(fr) + (fr[0]?.error?.movedTo ? " movedTo=" + fr[0].error.movedTo : ""), pass: code(fr) === "moved" };
  });
  await probe("P19", "read of an unknown local space (W-13)", "not_found", async () => { const fr = await alice.ask("read", { space: "nospace:" + SERVER, after: 0 }); return expectCode(fr, "not_found"); });
  await probe("P20", "read of a members-only space by a non-member", "denied or not_found", async () => { const c = await login("carol"); const fr = await c.ask("read", { space: SPACE, after: 0 }); c.close(); return { got: fr, summary: code(fr), pass: ["denied", "not_found"].includes(code(fr)) }; });
  await probe("P21", "read with after >= head", "no records, then end", async () => { const fr = await alice.ask("read", { space: SPACE, after: head + 50 }); return { got: fr, summary: fr.map((f) => "item" in f ? "item:" + f.item.type : Object.keys(f).filter((k) => k !== "id").join()).join(","), pass: fr.length === 1 && fr[0].end === true }; });
  await probe("P22", "read with an unknown argument member (W-21a)", "invalid", async () => { const fr = await alice.ask("read", { space: SPACE, after: 0, bogus: 1 }); return expectCode(fr, "invalid"); });
  await probe("P23", "cancel of an unknown target (W-06)", "ok null", async () => { const fr = await alice.ask("cancel", { target: 999999 }); return { got: fr, summary: code(fr), pass: code(fr) === "ok" }; });
  await probe("P24", "streaming call reusing an open request id", "invalid", async () => {
    const r = await login("alice"); r.send({ id: 50, call: "subscribe", args: { space: SPACE } }); await sleep(150);
    const from = r.frames.length; r.send({ id: 50, call: "subscribe", args: { space: SPACE } }); await sleep(400);
    const fr = r.frames.slice(from); r.close(); return { got: fr, summary: code(fr), pass: code(fr) === "invalid" };
  });
  await probe("P25", "subscribe by a non-reader", "denied", async () => { const c = await login("carol"); const fr = await c.ask("subscribe", { space: SPACE }); c.close(); return expectCode(fr, "denied"); });
  await probe("P26", "signal into an unknown space (W-24)", "not_found", async () => { const fr = await alice.ask("signal", { space: "nospace:" + SERVER, body: { a: 1 } }); return expectCode(fr, "not_found"); });
  await probe("P27", "signal carrying a relayed `from` by a non-server connection (W-23)", "denied", async () => { const fr = await alice.ask("signal", { space: SPACE, body: { a: 1 }, from: actor("bob") }); return expectCode(fr, "denied"); });
  await probe("P28", "append with a flipped bit in the middle of the signature", "invalid", async () => {
    const { e, content } = entry("alice", SPACE, "message", {}, { text: "bad sig" }, head); e.sig = e.sig.slice(0, 10) + (e.sig[10] === "A" ? "B" : "A") + e.sig.slice(11); const fr = await alice.ask("append", { entry: e, content }); return expectCode(fr, "invalid");
  });
  await probe("P28b", "append whose signature has non-zero padding bits in its last character (non-canonical base64url) (W-17)", "invalid (Sig is canonical base64url)", async () => {
    const { e, content } = entry("alice", SPACE, "message", {}, { text: "noncanonical sig" }, head); const last = e.sig.at(-1);
    const alt = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"; const i = alt.indexOf(last); e.sig = e.sig.slice(0, -1) + alt[(i & ~3) | ((i & 3) ^ 1)];
    const fr = await alice.ask("append", { entry: e, content }); return expectCode(fr, "invalid");
  });
  await probe("P29", "append with content but contentHash \"\" (W-16b)", "invalid", async () => {
    const { e } = entry("alice", SPACE, "message", {}, undefined, head); const fr = await alice.ask("append", { entry: e, content: { text: "x" } }); return expectCode(fr, "invalid");
  });
  await probe("P30", "member.set with content (W-16c)", "invalid", async () => {
    const { e, content } = entry("alice", SPACE, "member.set", { actor: actor("carol"), role: "none" }, { text: "x" }, head); const fr = await alice.ask("append", { entry: e, content }); return expectCode(fr, "invalid");
  });
  await probe("P31", "two appends pipelined without waiting (W-05)", "two receipts, seq consecutive, in send order", async () => {
    const r = await login("bob"); const a = entry("bob", SPACE, "message", {}, { text: "p1" }, head), b = entry("bob", SPACE, "message", {}, { text: "p2" }, head);
    r.send({ id: 61, call: "append", args: { entry: a.e, content: a.content } }); r.send({ id: 62, call: "append", args: { entry: b.e, content: b.content } });
    await r.until(() => r.frames.filter((f) => f.id === 61 || f.id === 62).length >= 2, 3000);
    const fr = r.frames.filter((f) => f.id === 61 || f.id === 62); r.close();
    return { got: fr, summary: "ids " + fr.map((f) => f.id).join(",") + (fr.length === 2 && fr[1].ok?.seq === fr[0].ok?.seq + 1 ? "; consecutive seqs" : "; NOT consecutive"), pass: fr.length === 2 && fr[0].id === 61 && fr[1].ok?.seq === fr[0].ok?.seq + 1 };
  });
  // blobs
  const bytes = Buffer.from("probe blob bytes"), ref = { hash: sha(bytes), size: bytes.length, type: "text/plain" };
  await probe("P32", "blobPut with a gap (offset 5 before any part) (W-27)", "invalid", async () => { const fr = await alice.ask("blobPut", { ref, offset: 5, data: bytes.subarray(5).toString("base64url") }); return expectCode(fr, "invalid"); });
  await probe("P33", "blobPut whose bytes do not match ref.hash (W-27)", "invalid", async () => { const bad = { hash: sha("zzz"), size: 3, type: "text/plain" }; const fr = await alice.ask("blobPut", { ref: bad, offset: 0, data: Buffer.from("abc").toString("base64url") }); return expectCode(fr, "invalid"); });
  await probe("P34", "blobPut with padded base64 (data must be base64url without padding)", "invalid", async () => { const fr = await alice.ask("blobPut", { ref, offset: 0, data: bytes.toString("base64") + "=" }); return expectCode(fr, "invalid"); });
  await probe("P35", "blobPut complete, then repeat (idempotent) (W-27)", "ok complete both times", async () => {
    const a1 = await alice.ask("blobPut", { ref, offset: 0, data: bytes.toString("base64url") }); const a2 = await alice.ask("blobPut", { ref, offset: 0, data: bytes.toString("base64url") });
    return { got: [...a1, ...a2], summary: JSON.stringify([a1[0]?.ok, a2[0]?.ok ?? a2[0]?.error?.code]), pass: a1[0]?.ok?.complete === true && a2[0]?.ok?.complete === true };
  });
  await probe("P36", "blobGet of a blob nothing references, by a non-uploader (W-29a)", "denied or not_found", async () => { const c = await login("bob"); const fr = await c.ask("blobGet", { ref, space: SPACE }); c.close(); return { got: fr, summary: code(fr), pass: ["denied", "not_found"].includes(code(fr)) }; });
  await probe("P37", "blobGet by the uploader within 24 h (own-blob rule)", "ok", async () => { const fr = await alice.ask("blobGet", { ref, space: SPACE }); return { got: fr, summary: code(fr), pass: code(fr) === "ok" }; });
  await probe("P38", "blobGet with offset past the end (W-29c)", "invalid", async () => { const fr = await alice.ask("blobGet", { ref, space: SPACE, offset: 9999 }); return expectCode(fr, "invalid"); });
  await probe("P39", "blobGet with length 0 / huge length (W-29c)", "ok (clamped) for huge length", async () => { const fr = await alice.ask("blobGet", { ref, space: SPACE, offset: 0, length: 99999999 }); return { got: fr, summary: code(fr), pass: code(fr) === "ok" }; });
  // sessions
  await probe("P40", "stream item for a closed session (G-05)", "denied", async () => {
    const it = { space: SPACE, session: closedSess, n: 99, time: "2026-01-01T00:00:00Z", type: "output", text: "late" }; const fr = await alice.ask("stream", { item: it }); return expectCode(fr, "denied");
  });
  const so = entry("alice", SPACE, "session.open", { steerMode: "auto" }, { title: "probe" }, head + 3);
  const sor = await alice.ask("append", { entry: so.e, content: so.content });
  const sid = sor[0]?.ok?.id;
  const item = (nn, who = {}) => ({ space: SPACE, session: sid, n: nn, time: "2026-01-01T00:00:00Z", type: "output", text: "t" + nn, ...who });
  await probe("P41", "stream n=1 by the session actor", "ok", async () => { const fr = await alice.ask("stream", { item: item(1) }); return expectCode(fr, "ok"); });
  await probe("P42", "stream n=1 repeated (identical)", "ok", async () => { const fr = await alice.ask("stream", { item: item(1) }); return expectCode(fr, "ok"); });
  await probe("P43", "stream n=1 repeated with different text (W-31)", "invalid", async () => { const it = item(1); it.text = "different"; const fr = await alice.ask("stream", { item: it }); return expectCode(fr, "invalid"); });
  await probe("P44", "stream n=5 (gap)", "invalid", async () => { const fr = await alice.ask("stream", { item: item(5) }); return expectCode(fr, "invalid"); });
  await probe("P45", "stream by a member who is not the session's actor", "denied", async () => { const b = await login("bob"); const fr = await b.ask("stream", { item: item(2) }); b.close(); return expectCode(fr, "denied"); });
  await probe("P46", "stream for an unknown session (W-31)", "not_found or denied", async () => { const it = item(1); it.session = "0".repeat(64); const fr = await alice.ask("stream", { item: it }); return { got: fr, summary: code(fr), pass: ["not_found", "denied"].includes(code(fr)) }; });
  await probe("P47", "watch of an unknown session (W-32)", "not_found or denied", async () => { const b = await login("bob"); const fr = await b.ask("watch", { space: SPACE, session: "0".repeat(64) }); b.close(); return { got: fr, summary: code(fr), pass: ["not_found", "denied"].includes(code(fr)) }; });
  await probe("P48", "watch by a non-reader", "denied", async () => { const c = await login("carol"); const fr = await c.ask("watch", { space: SPACE, session: sid }); c.close(); return expectCode(fr, "denied"); });

  await probe("P52", "message whose content is over maxContentBytes (65536) (W-15/R-09)", "invalid", async () => {
    const { e, content } = entry("alice", SPACE, "message", {}, { text: "x".repeat(70000) }, head); const fr = await alice.ask("append", { entry: e, content }, { wait: 5000 }); return expectCode(fr, "invalid");
  });
  await probe("P53", "read with limit 2 from the start (no follow)", "2 records then end", async () => {
    const fr = await alice.ask("read", { space: SPACE, after: 0, limit: 2 }); const n = fr.filter((f) => f.item?.type === "record").length;
    return { got: fr, summary: `${n} records, last=${Object.keys(fr.at(-1) ?? {}).filter((k) => k !== "id")}`, pass: n === 2 && fr.at(-1)?.end === true };
  });
  await probe("P54", "read with limit 0 (outside 1-1000)", "invalid", async () => { const fr = await alice.ask("read", { space: SPACE, after: 0, limit: 0 }); return expectCode(fr, "invalid"); });
  await probe("P55", "read with knownHeads naming s1 (R-15, W-21c): item order", "heads item first, then records (unspecified order)", async () => {
    const fr = await alice.ask("read", { space: SPACE, after: head - 1, knownHeads: [recs[0].receipt] });
    const types = fr.map((f) => f.item ? f.item.type : Object.keys(f).filter((k) => k !== "id").join());
    return { got: fr.map((f) => f.item?.type === "heads" ? { id: f.id, item: { type: "heads", heads: f.item.heads.map((h) => ({ space: h.space, seq: h.seq })) } } : f.item?.type === "record" ? { id: f.id, item: { type: "record", seq: f.item.record.receipt.seq } } : f), summary: [...new Set(types)].join(","), pass: types.includes("heads") };
  });
  await probe("P56", "read of a space homed on another server (W-13a)", "moved", async () => { const fr = await alice.ask("read", { space: "q1:other.localhost", after: 0 }); return { got: fr, summary: code(fr) + (fr[0]?.error?.movedTo ? " movedTo=" + fr[0].error.movedTo : ""), pass: code(fr) === "moved" }; });
  await probe("P57", "watch ends when its session is closed (W-32)", "unspecified: error or end, or stays open", async () => {
    const b = await login("bob"); const from = b.frames.length; b.send({ id: 70, call: "watch", args: { space: SPACE, session: sid } }); await sleep(200);
    const cl = entry("alice", SPACE, "session.close", { session: sid }); await alice.ask("append", { entry: cl.e }); await sleep(700);
    const fr = b.frames.slice(from); b.close();
    return { got: fr, summary: fr.length ? JSON.stringify(fr.map((f) => f.end ? "end" : f.error ? "error:" + f.error.code : f.item ? "item" : "?")) : "stays open, no frame", pass: true };
  });
  await probe("P58", "`host` (G-06) from a non-server connection", "denied (host needs a server actor @:A)", async () => { const fr = await alice.ask("host", { space: SPACE, records: [] }); return expectCode(fr, "denied"); });
  // the connection itself
  await probe("P49", "challenge frame shape (W-01)", "{challenge, server, versions:[1], time}", async () => {
    const r = await Raw.open(); const ch = r.challenge; r.close();
    return { got: [ch], summary: `keys=${Object.keys(ch).sort()} len=${ch.challenge?.length} alphabet=${/^[A-Za-z0-9_-]+$/.test(ch.challenge) ? "b64url" : "other"}`, pass: /^[A-Za-z0-9_-]{22,128}$/.test(ch.challenge) && ch.server === SERVER };
  });
  await probe("P50", "hello ok shape (Limits keys)", "limits has every han.ts Limits key, incl. maxClockSkewSeconds", async () => {
    const r = await Raw.open(); const fr = await r.hello("bob"); r.close(); const lim = fr[0]?.ok?.limits ?? {};
    const want = ["maxFrameBytes", "maxHeaderBytes", "maxContentBytes", "maxBlobBytes", "maxBlobPartBytes", "maxBlobRangeBytes", "maxStreamItemBytes", "maxFollows", "maxClockSkewSeconds"];
    const missing = want.filter((k) => !(k in lim));
    return { got: fr, summary: `limits keys missing: [${missing}]; hello ok keys: ${Object.keys(fr[0]?.ok ?? {}).sort()}`, pass: missing.length === 0 };
  });
  await probe("P51", "WebSocket upgrade on path / (W-07)", "unspecified; README shows ws://host:port", async () => {
    try { const r = await Raw.open("/"); r.close(); return { got: [], summary: "upgrade accepted", pass: true }; } catch (e) { return { got: [], summary: "upgrade refused: " + e.message, pass: false }; }
  });
  alice.close();
}
await main().catch((e) => results.push({ id: "FATAL", error: String(e?.stack ?? e), pass: false }));
fs.writeFileSync(OUT, JSON.stringify(results, null, 1));
setTimeout(() => process.exit(0), 50);
