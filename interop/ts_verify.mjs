// Adapter: the clean-room TS verifier (verify.ts) following a space on any server.
//   node ts_verify.mjs --url ws://.../han --server a.localhost --actor vera --keys DIR --space S --server-key PUB --server-key-id h1 --out F
// verify.ts is a library (verifySpace over records + a profile fetcher); this script is the follower: it reads the space with
// `read {follow: true}` until the head, reads every pinned profile space, and replays them.
import fs from "node:fs";
import { HanClient } from "../cleanroom-ts/dist/client.js";
import { importKey } from "../cleanroom-ts/dist/keys.js";
import { verifySpace } from "../cleanroom-ts/dist/verify.js";

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i < 0 ? d : process.argv[i + 1]; };
const URL_ = arg("url"), SERVER = arg("server"), ACTOR = arg("actor"), KEYS = arg("keys"), SPACE = arg("space"), OUT = arg("out");
let SK = { publicKey: arg("server-key"), keyId: arg("server-key-id", "h1"), from: "1970-01-01T00:00:00Z" };
const WK = arg("wellknown", null);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const j = JSON.parse(fs.readFileSync(`${KEYS}/${ACTOR}.json`, "utf8"));
const out = {};
try {
  let keyList = null;
  if (WK) { const doc = await (await fetch(WK)).json(); keyList = doc.keys; out.keysFrom = "wellknown"; out.wellknownKeys = keyList.length; }
  const c = await HanClient.connect(URL_, SERVER, `@${ACTOR}:${SERVER}`, importKey(j.keyId, j.public, j.private));
  const st = (await c.state(SPACE)).state; out.head = st.head;
  const recs = [];
  const s = c.stream("read", { space: SPACE, after: 0, follow: true }, (i) => { if (i.type === "record") recs.push(i.record); });
  const t = Date.now(); while (recs.length < st.head && Date.now() - t < 15000) await sleep(50);
  await s.cancel(); out.followed = recs.length;
  const profiles = new Map();
  const profileRecords = async (actor) => {
    if (!profiles.has(actor)) profiles.set(actor, c.records(await c.read("profile:" + actor, 0)));
    return profiles.get(actor);
  };
  const deps = { serverKeys: (server, keyId) => server !== SERVER ? undefined : keyList ? keyList.find((k) => k.keyId === keyId) : keyId === SK.keyId ? SK : undefined, profileRecords };
  const v = await verifySpace(SPACE, recs, deps);
  out.verdict = { ok: v.ok, through: v.through, faults: v.faults };
  // the verifier must have teeth: a flipped receipt signature must be a fault
  const bad = JSON.parse(JSON.stringify(recs)); const ti = Math.min(2, bad.length - 1); const hs = bad[ti].receipt.homeSig; bad[ti].receipt.homeSig = hs.slice(0, 10) + (hs[10] === "A" ? "B" : "A") + hs.slice(11);
  const v2 = await verifySpace(SPACE, bad, deps);
  out.tamper = { ok: v2.ok, through: v2.through, faults: v2.faults };
  c.close();
} catch (e) { out.error = String(e?.message ?? e); }
fs.writeFileSync(OUT, JSON.stringify(out, null, 1));
setTimeout(() => process.exit(0), 50);
