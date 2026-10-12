// Vector runner, following ref/README.md "Vector format".
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { decide, fold, canRead, relevantActors, sessionAccess } from "./policy.js";
import { jcs, entryId } from "./wire.js";

const dir = process.argv[2] ?? "H:/HAN/v1/ref/vectors";
const only = process.argv[3];
const verbose = process.argv.includes("-v");
const A: any = null;
const clone = <T>(x: T): T => (x === undefined ? x : JSON.parse(JSON.stringify(x)));
const same = (a: unknown, b: unknown) => jcsLoose(a) === jcsLoose(b);
function jcsLoose(x: any): string { try { return jcs(x === undefined ? null : x); } catch { return JSON.stringify(x); } }

function resolve(obj: any, path: string): { found: boolean; value?: any } {
  if (path === "") return { found: true, value: obj };
  if (obj === null || typeof obj !== "object") return { found: false };
  const idxs: number[] = [];
  for (let i = 0; i < path.length; i++) if (path[i] === ".") idxs.push(i);
  idxs.push(path.length);
  for (const i of idxs.reverse()) { // greedy: longest key first
    const key = path.slice(0, i);
    if (Object.prototype.hasOwnProperty.call(obj, key)) {
      const r = resolve(obj[key], path.slice(i + 1));
      if (r.found) return r;
    }
  }
  return { found: false };
}

let pass = 0, fail = 0, vpass = 0, vfail = 0;
const failures: string[] = [];
for (const f of readdirSync(dir).filter((x) => x.endsWith(".json")).sort()) {
  if (only && !f.includes(only)) continue;
  const v: any = JSON.parse(readFileSync(join(dir, f), "utf8"));
  if (!v.steps) {
    // 00-*: id derivation
    const got = "han-entry-v1\n" + jcs((({ sig, ...r }) => r)(v.entry));
    const okp = got === v.preimage;
    if (okp) { pass++; vpass++; } else { fail++; vfail++; failures.push(`${f}: preimage mismatch`); }
    continue;
  }
  const states: Record<string, any> = {};
  const profiles: Record<string, any> = clone(v.profiles ?? {});
  let lastTime = "2026-10-10T12:00:00Z";
  let vok = true;
  v.steps.forEach((st: any, i: number) => {
    const tag = `${f}#${i}`;
    const bad = (m: string) => { fail++; vok = false; failures.push(`${tag}: ${m}`); };
    try {
      if (st.profiles) Object.assign(profiles, clone(st.profiles));
      if (st.entry) {
        const { id, ...entry } = clone(st.entry);
        lastTime = st.time;
        const ctx: any = { time: st.time, profiles: clone(profiles) };
        if (st.predecessor) ctx.predecessor = clone(states[st.predecessor]);
        const prev = states[entry.space];
        const before = JSON.stringify([prev, entry, ctx]);
        const d1: any = decide(prev && clone(prev), clone(entry), clone(ctx));
        const d2: any = decide(prev && clone(prev), clone(entry), clone(ctx));
        if (JSON.stringify([prev, entry, ctx]) !== before) return bad("decide mutated input");
        if (!same(d1, d2)) return bad("decide nondeterministic");
        const got = d1.allow ? "allow" : d1.code;
        if (got !== st.expect) return bad(`expected ${st.expect}, got ${got} (${d1.reason ?? ""})`);
        if (d1.allow) {
          const rec: any = { entry, content: null, redacted: false, receipt: { space: entry.space, seq: (prev?.head ?? 0) + 1, id, prev: "", home: "", homeKey: "k", time: st.time, durability: "local", profiles: {}, homeSig: "" } };
          const b4 = JSON.stringify([prev, rec, ctx]);
          const s1 = fold(prev && clone(prev), clone(rec), clone(ctx));
          const s2 = fold(prev && clone(prev), clone(rec), clone(ctx));
          if (JSON.stringify([prev, rec, ctx]) !== b4) return bad("fold mutated input");
          if (!same(s1, s2)) return bad("fold nondeterministic");
          states[entry.space] = s1;
          for (const [p, want] of Object.entries(st.check ?? {})) {
            const r = resolve(s1, p);
            const have = r.found ? r.value : "<missing>";
            if (!same(have, want)) return bad(`check ${p}: want ${JSON.stringify(want)} have ${JSON.stringify(r.value)}`);
          }
        } else if (st.check) {
          for (const [p, want] of Object.entries(st.check)) {
            const r = resolve(prev, p); if (!same(r.found ? r.value : "<missing>", want)) return bad(`check (after refusal) ${p}`);
          }
        }
        pass++;
      } else if (st.read) {
        const s = states[st.read.space];
        const got = canRead(clone(s), st.read.actor, st.read.seq);
        if (got !== st.expect) return bad(`read: expected ${st.expect}, got ${got}`);
        pass++;
      } else if (st.relevantActors) {
        const { id, ...entry } = clone(st.relevantActors);
        const s = states[entry.space];
        const got = relevantActors(s && clone(s), entry, lastTime);
        if (!same(got, st.expect)) return bad(`relevantActors: expected ${JSON.stringify(st.expect)}, got ${JSON.stringify(got)}`);
        pass++;
      } else if (st.sessionAccess) {
        const q = st.sessionAccess;
        const got = sessionAccess(clone(states[q.space]), q.actor, q.session);
        if (!same(got, st.expect)) return bad(`sessionAccess: expected ${JSON.stringify(st.expect)}, got ${JSON.stringify(got)}`);
        pass++;
      } else bad("unknown step form");
    } catch (e: any) {
      bad("exception " + (e?.stack ?? e).split("\n").slice(0, 3).join(" | "));
    }
  });
  if (vok) vpass++; else vfail++;
}
console.log(failures.join("\n"));
console.log(`steps pass ${pass} fail ${fail}; vectors pass ${vpass} fail ${vfail}`);
if (verbose) console.log("done");
