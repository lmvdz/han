// Thin adapter: runs the clean-room TypeScript sequencer with actors provisioned from a JSON list.
//   node ts_host.mjs --server a.localhost --dir DIR --port P --actors actors.json [--freeze 2026-01-01T00:00:00Z]
// (The TS tree has no CLI for provisioning: profile spaces are created in-process by Sequencer.provisionActor.)
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
const p = await seq.listen();
console.log(`READY ts ${server} ws://127.0.0.1:${p}/han`);
process.on("SIGTERM", async () => { await seq.close(); process.exit(0); });
process.on("SIGINT", async () => { await seq.close(); process.exit(0); });
