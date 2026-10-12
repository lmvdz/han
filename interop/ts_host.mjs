// Thin adapter: runs the clean-room TypeScript sequencer with actors provisioned from a JSON list.
//   node ts_host.mjs --server a.localhost --dir DIR --port P --actors actors.json [--freeze 2026-01-01T00:00:00Z]
// (The TS tree has no CLI for provisioning: profile spaces are created in-process by Sequencer.provisionActor.)
//
// Test mode (architecture.md G-11), with --freeze: the clock starts there and moves only by the control call
//   test.clock {wall?, steadyAdvance?}   -> ok {wall, steady}
// which this host adds in process; the stock server does not have it (outside test mode an unknown call is `invalid`).
import fs from "node:fs";
import { Sequencer } from "../cleanroom-ts/dist/seq.js";

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i < 0 ? d : process.argv[i + 1]; };
const server = arg("server"), dir = arg("dir"), port = Number(arg("port")), freeze = arg("freeze", null);
const actors = JSON.parse(fs.readFileSync(arg("actors"), "utf8"));
const opts = { server, dir, port };
if (freeze) { const ms = Date.parse(freeze); opts.testClock = { ms, base: ms }; }
const seq = new Sequencer(opts);
for (const a of actors) {
  if (!seq.stateOf("profile:" + a.actor)) seq.provisionActor(a.actor, a.kind, { keyId: a.keyId, publicKey: a.public }, a.operator);
}

if (freeze) {
  // two readings the test controls and nothing else moves: wall (ms since the epoch) and steady (ms since start)
  const clock = { wallMs: Date.parse(freeze), steadyMs: 0 };
  seq.nowMs = () => clock.wallMs;
  seq.steadyMs = () => clock.steadyMs;
  const fmt = (ms) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
  const parseTime = (s) => {
    if (typeof s !== "string" || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(s)) return null;
    const ms = Date.parse(s);
    return Number.isNaN(ms) || fmt(ms) !== s ? null : ms;      // an impossible date (February 30) does not round-trip
  };
  const onFrame = seq.onFrame.bind(seq);
  seq.onFrame = (conn, text) => {
    let f;
    try { f = JSON.parse(text); } catch { return onFrame(conn, text); }
    if (!f || typeof f !== "object" || Array.isArray(f) || f.call !== "test.clock"
        || Object.keys(f).length !== 3 || !Number.isSafeInteger(f.id) || f.id < 1 || !f.args || typeof f.args !== "object" || Array.isArray(f.args)) return onFrame(conn, text);
    const bad = (message) => conn.send({ id: f.id, error: { code: "invalid", message } });
    if (!conn.who) return conn.send({ id: f.id, error: { code: "denied", message: "hello first" } });
    const a = f.args;
    if (Object.keys(a).some((k) => k !== "wall" && k !== "steadyAdvance")) return bad("unknown args for test.clock");
    const wall = "wall" in a ? parseTime(a.wall) : null;
    if ("wall" in a && wall === null) return bad("wall is not a Time");
    const adv = "steadyAdvance" in a ? a.steadyAdvance : 0;
    if (!Number.isSafeInteger(adv) || adv < 0) return bad("steadyAdvance is a non-negative integer (seconds)");
    if (wall !== null) clock.wallMs = wall;
    clock.steadyMs += adv * 1000;
    conn.send({ id: f.id, ok: { wall: fmt(clock.wallMs), steady: Math.floor(clock.steadyMs / 1000) } });
  };
}

const p = await seq.listen();
console.log(`READY ts ${server} ws://127.0.0.1:${p}/han`);
process.on("SIGTERM", async () => { await seq.close(); process.exit(0); });
process.on("SIGINT", async () => { await seq.close(); process.exit(0); });
