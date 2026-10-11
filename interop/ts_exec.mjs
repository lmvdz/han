// Adapter: runs the clean-room TS executor (exec.ts) as a process.
//   node ts_exec.mjs --url ws://.../han --actor @x:S --server S --dir STATEDIR --sandbox DIR --spaces sp1,sp2
// STATEDIR holds instance.json (key) and journal.jsonl, created by the harness (Executor.init writes the same files with a random key).
import { Executor } from "../cleanroom-ts/dist/exec.js";

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i < 0 ? d : process.argv[i + 1]; };
const ex = new Executor({ actor: arg("actor"), server: arg("server"), url: arg("url"), dir: arg("dir"), sandbox: arg("sandbox"), spaces: arg("spaces").split(","),
  onEvent: (s) => console.log("[exec]", s) });
try { await ex.start(); console.log("ts-exec ready"); }
catch (e) { console.error("ts-exec: refusing to start:", String(e?.message ?? e)); process.exit(3); }
process.on("SIGTERM", async () => { try { await ex.stop(); } catch { /* */ } process.exit(0); });
setInterval(() => {}, 1 << 30);
