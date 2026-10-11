// A minimal R-14 verifier for the interop test: chain, receipt signatures, entry signatures, content hashes and a Policy replay.
import type { LogRecord, PolicyCtx, ProfileView, SpaceState } from "./han.js";
import { decide, fold, profileViewOf } from "./policy.js";
import { entryId, jcs, sha256, profileActorOf, isProfileSpace } from "./wire.js";
import { homeSigMessage, receiptHash, sigMessage, verifyBytes, parseTimeMs } from "./keys.js";

type A = any;
export interface ServerKey { publicKey: string; from: string; until?: string }
export interface VerifyDeps {
  serverKeys: (server: string, keyId: string) => ServerKey | undefined;
  /** the profile space's records for an actor (public, §4.3) */
  profileRecords: (actor: string) => Promise<LogRecord[]>;
}
export interface Verdict { ok: boolean; through: number; faults: string[]; state?: SpaceState }

/** views[seq] of a profile space, by replaying it with Policy */
export function profileViews(actor: string, recs: LogRecord[]): ProfileView[] {
  const views: ProfileView[] = []; let st: SpaceState | undefined;
  for (const r of recs) {
    st = fold(st, r, { time: r.receipt.time, profiles: {} });
    views[r.receipt.seq] = profileViewOf(actor, r.receipt.seq, st);
  }
  return views;
}

export async function verifySpace(space: string, recs: LogRecord[], deps: VerifyDeps): Promise<Verdict> {
  const faults: string[] = []; let st: SpaceState | undefined; let through = 0;
  const cache = new Map<string, ProfileView[]>();
  const viewOf = async (actor: string, seq: number): Promise<ProfileView | undefined> => {
    if (actor.startsWith("@:")) {
      const server = actor.slice(2);
      const keys: A = {};
      // synthetic view: the server's keys valid at the receipt time (§4.3); here every key id the checker knows
      for (const kid of ["srv1"]) { const k = deps.serverKeys(server, kid); if (k) keys[kid] = { publicKey: k.publicKey, status: "valid" }; }
      return { actor: actor as A, seq: 0, actorKind: "service", operatorConfirmed: false, keys };
    }
    if (!cache.has(actor)) cache.set(actor, profileViews(actor, await deps.profileRecords(actor)));
    return cache.get(actor)![seq];
  };
  let prevHash = "", lastTime = "";
  for (let i = 0; i < recs.length; i++) {
    const r = recs[i], e = r.entry as A, rc = r.receipt as A, at = `seq ${i + 1}`;
    const bad = (m: string) => { faults.push(`${at}: ${m}`); };
    if (rc.seq !== i + 1) { bad("seq gap"); break; }
    if (rc.prev !== prevHash) { bad("prev does not link"); break; }
    if (rc.space !== space) { bad("receipt names another space"); break; }
    const id = entryId(e);
    if (rc.id !== id) { bad("receipt id is not the entry id"); break; }
    const home = st ? st.home : (isProfileSpace(space) ? space.slice(space.indexOf("@") + 1).split(":")[1] : space.split(":")[1]);
    if (rc.home !== home) { bad(`wrong home: receipt by ${rc.home}, home was ${home}`); break; }
    const sk = deps.serverKeys(rc.home, rc.homeKey);
    if (!sk) { bad("unknown server key"); break; }
    const t = parseTimeMs(rc.time);
    if (t < parseTimeMs(sk.from) || (sk.until && t >= parseTimeMs(sk.until))) { bad("receipt time outside the server key's window"); break; }
    if (!verifyBytes(sk.publicKey, homeSigMessage(receiptHash(rc)), rc.homeSig)) { bad("homeSig does not verify"); break; }
    if (lastTime && rc.time < lastTime) { bad("receipt time decreases (R-03a)"); break; }
    // content binding
    if (r.redacted) {
      const rb = (r as A).redactedBy;
      if (!rb || rb.entry.kind !== "redact" || rb.entry.body.target !== id || rb.receipt.seq <= rc.seq) { bad("bad redaction proof"); break; }
    } else if (e.contentHash === "" ? r.content !== null : sha256(jcs(r.content)) !== e.contentHash) { bad("content does not match contentHash"); break; }
    // pinned profiles
    const profiles: A = {};
    for (const [actor, seq] of Object.entries(rc.profiles)) {
      const own = st?.profile !== undefined && actor === profileActorOf(space);
      const v = await viewOf(actor, seq as number);
      if (!v) { bad(`cannot reconstruct pinned profile ${actor}@${seq}`); break; }
      if (!own) profiles[actor] = v;
    }
    if (faults.length) break;
    // entry signature against the pinned view
    const authorKeys = st?.profile !== undefined && e.author === profileActorOf(space) ? st.profile.keys : profiles[e.author]?.keys;
    const k = authorKeys?.[e.keyId];
    if (!k || !verifyBytes(k.publicKey, sigMessage(id), e.sig)) { bad("entry signature does not verify against the pinned key"); break; }
    // Policy replay
    const ctx: PolicyCtx = { time: rc.time, profiles };
    const d = decide(st, e, ctx);
    if (!d.allow) { bad("Policy.decide replay says " + d.code + ": " + d.reason); break; }
    st = fold(st, r, ctx);
    prevHash = receiptHash(rc); lastTime = rc.time; through = rc.seq;
  }
  return { ok: faults.length === 0, through, faults, state: st };
}
