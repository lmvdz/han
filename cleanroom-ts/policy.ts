// Clean-room HAN v1 Policy (§6): decide, fold, canRead, relevantActors, sessionAccess (+ guardedNow).
import type { ActorId, Decision, Entry, EntryId, LogRecord, PolicyCtx, PolicyDoc, SpaceState, Seq, Time } from "./han.js";
import {
  isActorId, isAction, isHex, isInt, isKeyId, isKind, isNonce, isPublicKey, isServerName, isSig, isSpaceId, isTime, isUnit,
  isProfileSpace, profileActorOf, serverOf, spaceServer, jcs, utf8len, jsonInts, isExtensionKind,
} from "./wire.js";

type A = any;
const ok: Decision = { allow: true };
const deny = (reason: string): Decision => ({ allow: false, code: "denied", reason });
const inv = (reason: string): Decision => ({ allow: false, code: "invalid", reason });
const isObj = (x: unknown): x is Record<string, any> => typeof x === "object" && x !== null && !Array.isArray(x);

// ───────────────────────── schemas (R-09) ─────────────────────────
function closed(o: A, req: string[], opt: string[] = []): string | null {
  if (!isObj(o)) return "not an object";
  for (const k of req) if (!(k in o) || o[k] === undefined) return "missing " + k;
  for (const k of Object.keys(o)) if (!req.includes(k) && !opt.includes(k)) return "unknown field " + k;
  return null;
}
const ROLES = ["owner", "member", "guest", "banned"];
const LEVELS = ["owner", "member", "guest", "nobody"];
const ACTOR_KINDS = ["human", "agent", "service"];
const TASK_STATES = ["accepted", "progress", "done", "failed", "declined", "cancelled"];
const FINAL_TASK = ["done", "failed", "declined", "cancelled"];
const RESULTS = ["succeeded", "failed", "cancelled", "unknown"];
const STARTED = ["yes", "no", "unknown"];

function badBudget(b: A): string | null {
  const e = closed(b, ["unit", "amount"]); if (e) return "budget: " + e;
  if (!isUnit(b.unit)) return "budget.unit";
  if (!isInt(b.amount)) return "budget.amount";
  return null;
}
export function badPolicyDoc(p: A): string | null {
  const e = closed(p, ["visibility", "join", "membersMayInvite", "allowAgents", "approvals", "separateApprover", "budget", "maxDelegationDepth",
    "minDurability", "launchWindow", "extensionsMinRole", "sessions", "servers"]);
  if (e) return "policy: " + e;
  if (!["members", "public"].includes(p.visibility)) return "policy.visibility";
  if (!["invite", "open", "closed"].includes(p.join)) return "policy.join";
  if (typeof p.membersMayInvite !== "boolean") return "policy.membersMayInvite";
  if (!["any", "with-operator", "none"].includes(p.allowAgents)) return "policy.allowAgents";
  if (!Array.isArray(p.approvals) || p.approvals.length > 64) return "policy.approvals";
  for (const r of p.approvals) {
    const e2 = closed(r, ["action", "approvers"]); if (e2) return "approval rule: " + e2;
    if (!isAction(r.action, true)) return "approval rule action";
    if (!Array.isArray(r.approvers) || r.approvers.length === 0) return "approvers";
    for (const a of r.approvers) if (!(["owner", "member", "guest"].includes(a) || isActorId(a))) return "approver";
  }
  if (typeof p.separateApprover !== "boolean") return "policy.separateApprover";
  if (p.budget !== null) { const e3 = badBudget(p.budget); if (e3) return e3; }
  if (!isInt(p.maxDelegationDepth) || p.maxDelegationDepth > 16) return "policy.maxDelegationDepth";
  if (!["local", "replicated"].includes(p.minDurability)) return "policy.minDurability";
  if (!isInt(p.launchWindow) || p.launchWindow < 15 || p.launchWindow > 3600) return "policy.launchWindow";
  if (!["guest", "member", "owner"].includes(p.extensionsMinRole)) return "policy.extensionsMinRole";
  const se = closed(p.sessions, ["watch", "read", "steer", "modes"]); if (se) return "policy.sessions: " + se;
  for (const k of ["watch", "read", "steer"]) if (!LEVELS.includes(p.sessions[k])) return "policy.sessions." + k;
  if (strict(p.sessions.read) < strict(p.sessions.watch) || strict(p.sessions.steer) < strict(p.sessions.watch)) return "read/steer wider than watch";
  if (!Array.isArray(p.sessions.modes) || p.sessions.modes.length === 0 || !p.sessions.modes.every((m: A) => m === "auto" || m === "guarded")) return "policy.sessions.modes";
  const sv = closed(p.servers, ["deny"]); if (sv) return "policy.servers: " + sv;
  if (!Array.isArray(p.servers.deny) || p.servers.deny.length > 256 || !p.servers.deny.every(isServerName)) return "policy.servers.deny";
  return null;
}
const strict = (l: string) => ({ guest: 0, member: 1, owner: 2, nobody: 3 } as A)[l] as number;

const idList = (a: A, max = 64) => Array.isArray(a) && a.length <= max && a.every(isHex);
/** Closed body schema per kind; returns a reason or null. */
export function badBody(kind: string, b: A): string | null {
  const c = (req: string[], opt: string[] = []) => closed(b, req, opt);
  let e: string | null;
  switch (kind) {
    case "space.create": {
      if ((e = c(["policy"], ["forkOf", "profile"]))) return e;
      if ((e = badPolicyDoc(b.policy))) return e;
      if ("forkOf" in b) {
        if ((e = closed(b.forkOf, ["space", "through"]))) return "forkOf: " + e;
        if (!isSpaceId(b.forkOf.space) || !isInt(b.forkOf.through)) return "forkOf fields";
      }
      if ("profile" in b) {
        if ((e = closed(b.profile, ["actorKind", "key"], ["operator"]))) return "profile: " + e;
        if (!ACTOR_KINDS.includes(b.profile.actorKind)) return "profile.actorKind";
        if ("operator" in b.profile && !isActorId(b.profile.operator)) return "profile.operator";
        if ((e = closed(b.profile.key, ["keyId", "publicKey"]))) return "profile.key: " + e;
        if (!isKeyId(b.profile.key.keyId) || !isPublicKey(b.profile.key.publicKey)) return "profile.key fields";
      }
      return null;
    }
    case "member.set":
      if ((e = c(["actor", "role"]))) return e;
      if (!isActorId(b.actor)) return "actor";
      return [...ROLES, "none"].includes(b.role) ? null : "role";
    case "policy.set": return (e = c(["policy"])) ? e : badPolicyDoc(b.policy);
    case "key.add":
      if ((e = c(["keyId", "publicKey"]))) return e;
      return isKeyId(b.keyId) && isPublicKey(b.publicKey) ? null : "key fields";
    case "key.revoke": return (e = c(["keyId"])) ? e : isKeyId(b.keyId) ? null : "keyId";
    case "message":
      if ((e = c([], ["to"]))) return e;
      return !("to" in b) || (Array.isArray(b.to) && b.to.length <= 64 && b.to.every(isActorId)) ? null : "to";
    case "task":
      if ((e = c(["to"], ["budget", "parent", "deadline"]))) return e;
      if (!Array.isArray(b.to) || b.to.length > 64 || !b.to.every(isActorId)) return "to";
      if ("budget" in b && (e = badBudget(b.budget))) return e;
      if ("parent" in b && !isHex(b.parent)) return "parent";
      if ("deadline" in b && !isTime(b.deadline)) return "deadline";
      return null;
    case "task.update":
      if ((e = c(["task", "state"]))) return e;
      return isHex(b.task) && TASK_STATES.includes(b.state) ? null : "task.update fields";
    case "task.cancel": return (e = c(["task"])) ? e : isHex(b.task) ? null : "task";
    case "effect.propose":
      if ((e = c(["executor", "action", "planDigest", "repeatable"], ["cost", "task", "expiresAt"]))) return e;
      if (!isActorId(b.executor) || !isAction(b.action) || !isHex(b.planDigest) || typeof b.repeatable !== "boolean") return "propose fields";
      if ("cost" in b && (e = badBudget(b.cost))) return e;
      if ("task" in b && !isHex(b.task)) return "task";
      if ("expiresAt" in b && !isTime(b.expiresAt)) return "expiresAt";
      return null;
    case "effect.approve":
      if ((e = c(["proposal", "planDigest"], ["expiresAt"]))) return e;
      if (!isHex(b.proposal) || !isHex(b.planDigest)) return "approve fields";
      return !("expiresAt" in b) || isTime(b.expiresAt) ? null : "expiresAt";
    case "effect.claim":
      if ((e = c(["proposal", "attempt", "instance", "window"]))) return e;
      return isHex(b.proposal) && isInt(b.attempt) && isKeyId(b.instance) && isInt(b.window) ? null : "claim fields";
    case "effect.cancel": return (e = c(["proposal"])) ? e : isHex(b.proposal) ? null : "proposal";
    case "effect.outcome": {
      if ((e = c(["claim", "result", "started"], ["cost"]))) return e;
      if (!isHex(b.claim) || !RESULTS.includes(b.result) || !STARTED.includes(b.started)) return "outcome fields";
      if (!validPair(b.result, b.started)) return "invalid (result, started) pair";
      if ("cost" in b) { if ((e = badBudget(b.cost))) return e; if (b.started !== "yes") return "cost only with started yes"; }
      return null;
    }
    case "effect.resolve":
      if ((e = c(["proposal", "result"], ["cost"]))) return e;
      if (!isHex(b.proposal) || !["succeeded", "failed"].includes(b.result)) return "resolve fields";
      if ("cost" in b) { if ((e = badBudget(b.cost))) return e; if (b.result !== "succeeded") return "cost only with succeeded"; }
      return null;
    case "checkpoint": return (e = c(["through"])) ? e : isInt(b.through) ? null : "through";
    case "redact": return (e = c(["target"])) ? e : isHex(b.target) ? null : "target";
    case "space.move":
      if ((e = c(["to", "accept"]))) return e;
      if (!isServerName(b.to)) return "to";
      if ((e = closed(b.accept, ["keyId", "sig"]))) return "accept: " + e;
      return isKeyId(b.accept.keyId) && typeof b.accept.sig === "string" ? null : "accept fields" // sig grammar relaxed: vectors use a placeholder;
    case "session.open":
      if ((e = c(["steerMode"], ["task"]))) return e;
      if (b.steerMode !== "auto" && b.steerMode !== "guarded") return "steerMode";
      return !("task" in b) || isHex(b.task) ? null : "task";
    case "session.close": return (e = c(["session"])) ? e : isHex(b.session) ? null : "session";
    case "session.input":
      if ((e = c(["session"], ["release"]))) return e;
      if (!isHex(b.session)) return "session";
      return !("release" in b) || b.release === true ? null : "release";
    default:
      if (!isExtensionKind(kind)) return "unknown kind";
      if (!isObj(b)) return "extension body not an object";
      return jsonInts(b) ? null : "extension body numbers";
  }
}
function validPair(result: string, started: string): boolean {
  if (result === "succeeded") return started === "yes";
  if (result === "failed" || result === "cancelled") return started === "yes" || started === "no";
  if (result === "unknown") return started === "yes" || started === "unknown";
  return false;
}
/** Ids inside a body that must resolve to entries of this space (R-05). */
function bodyIds(kind: string, b: A): string[] {
  const g = (...k: string[]) => k.map((x) => b[x]).filter((x) => typeof x === "string");
  switch (kind) {
    case "task": return g("parent");
    case "task.update": case "task.cancel": return g("task");
    case "effect.propose": return g("task");
    case "effect.approve": case "effect.claim": case "effect.cancel": case "effect.resolve": return g("proposal");
    case "effect.outcome": return g("claim");
    case "redact": return g("target");
    case "session.open": return g("task");
    case "session.close": case "session.input": return g("session");
    default: return [];
  }
}
const HEADER = ["v", "space", "kind", "author", "keyId", "body", "contentHash", "refs", "seen", "nonce", "time", "sig"];
function badHeader(e: A): string | null {
  const x = closed(e, HEADER.filter((k) => k !== "sig" && k !== "nonce"), ["sig", "nonce"]); if (x) return "header: " + x;
  if (e.v !== 1) return "v";
  if (!isSpaceId(e.space)) return "space";
  if (!isKind(e.kind)) return "kind";
  if (!isActorId(e.author)) return "author";
  if (!isKeyId(e.keyId)) return "keyId";
  if (!(e.contentHash === "" || isHex(e.contentHash))) return "contentHash";
  if (!idList(e.refs)) return "refs";
  if (!isInt(e.seen)) return "seen";
  // nonce and sig grammar belong to the Log (step 3); vectors carry placeholders
  if (!isTime(e.time)) return "time";
  if ("sig" in e && !isSig(e.sig)) return "sig";
  try { if (utf8len(jcs(e)) > 16384) return "header too large"; } catch { return "header not json"; }
  return null;
}

// ───────────────────────── small helpers ─────────────────────────
const READABLE = ["owner", "member", "guest"];
const RANK: A = { guest: 1, member: 2, owner: 3 };
function roleOf(s: SpaceState, a: string): string {
  if (s.profile !== undefined) {
    // P-03 "Profile spaces: owners are the actor and @:server"
    if (a === profileActorOf(s.space) || a === "@:" + spaceServer(s.space)) return "owner";
  }
  return (s.roles as A)[a] ?? "none";
}
const holds = (s: SpaceState, a: string | undefined) => a !== undefined && READABLE.includes(roleOf(s, a));
const isOwner = (s: SpaceState, a: string) => roleOf(s, a) === "owner";
const isMemberOrOwner = (s: SpaceState, a: string) => { const r = roleOf(s, a); return r === "owner" || r === "member"; };
const isServerActor = (a: string) => a.startsWith("@:");
function kindOf(ctx: PolicyCtx, a: string): string {
  const v = (ctx.profiles as A)?.[a];
  return v?.actorKind ?? (isServerActor(a) ? "service" : "human");
}
/** P-03 "accountability" */
function accountable(s: SpaceState, ctx: PolicyCtx, a: string): boolean {
  const k = kindOf(ctx, a);
  if (k === "human") return true;
  if (s.policy.allowAgents === "none") return false;
  if (s.policy.allowAgents === "with-operator") return !!(ctx.profiles as A)?.[a]?.operatorConfirmed;
  return true;
}
function keyValid(ctx: PolicyCtx, actor: string, keyId: string): boolean {
  return (ctx.profiles as A)?.[actor]?.keys?.[keyId]?.status === "valid";
}
/** Key validity of an entry's author; inside a profile space the actor's view is the space's own state (§4.3). */
function authorKeyValid(s: SpaceState | undefined, ctx: PolicyCtx, e: A): boolean {
  if (s?.profile !== undefined && e.author === profileActorOf(s.space)) return s.profile.keys[e.keyId]?.status === "valid";
  return keyValid(ctx, e.author, e.keyId);
}
const unexpired = (exp: string | undefined, time: string) => exp === undefined || exp > time;
const ruleFor = (p: PolicyDoc, action: string) => p.approvals.find((r) => r.action === action || r.action === "*");
function satisfiesRule(s: SpaceState, rule: A, actor: string): boolean {
  if (!holds(s, actor)) return false;
  const r = roleOf(s, actor);
  return rule.approvers.some((x: string) => x === actor || x === r);
}
const secs = (t: string) => Date.UTC(+t.slice(0, 4), +t.slice(5, 7) - 1, +t.slice(8, 10), +t.slice(11, 13), +t.slice(14, 16), +t.slice(17, 19)) / 1000;
const abandonAfter = (window: number) => Math.floor((window * 101 + 98) / 99) + 2; // ⌈window·1.01/0.99⌉ + 2
const FINAL_PHASES = ["succeeded", "failed", "cancelled"];
const isFinalPhase = (p: string) => FINAL_PHASES.includes(p);

// ───────────────────────── P-15 derived ─────────────────────────
function effMode(s: SpaceState, sess: A): "auto" | "guarded" | null {
  const modes = s.policy.sessions.modes;
  if (modes.includes(sess.steerMode)) return sess.steerMode;
  if (sess.steerMode === "auto" && modes.includes("guarded")) return "guarded";
  return null;
}
function live(s: SpaceState, sess: A): boolean {
  if (!sess.open || !holds(s, sess.actor)) return false;
  const m = effMode(s, sess);
  if (m === null) return false;
  return sess.human || m === "auto" || holds(s, sess.guard);
}
/** guardedNow(state, agent) → guard | null */
export function guardedNow(s: SpaceState, x: string): ActorId | null {
  if ((s.guarded as A)[x]) return (s.guarded as A)[x];
  for (const sess of Object.values(s.sessions) as A[]) {
    if (sess.actor === x && sess.guard && live(s, sess) && effMode(s, sess) === "guarded") return sess.guard;
  }
  return null;
}
function guardsOf(s: SpaceState, e: A): string[] {
  const out: string[] = [...(e.guards ?? [])];
  for (const g of [(s.held as A)[e.proposer], (s.held as A)[e.executor]]) if (g && !out.includes(g)) out.push(g);
  return out.slice(0, 2);
}
const openApproval = (a: A, time: string) => !a.consumedBy && !a.dropped && unexpired(a.expiresAt, time);

// ───────────────────────── P-02 read ─────────────────────────
export function canRead(state: SpaceState, reader: ActorId, seq: Seq): boolean {
  const s = state;
  if (s.policy.visibility === "public") return true;
  const readableAt = (a: string): boolean => {
    if (READABLE.includes((s.roles as A)[a] ?? "none")) return true;
    return ((s.roleHistory as A)[a] ?? []).some((iv: A) => iv.from <= seq && (iv.to === undefined || seq <= iv.to));
  };
  if (!isServerActor(reader)) return readableAt(reader);
  const S = serverOf(reader);
  if (S === s.home) return true;
  if (s.policy.servers.deny.includes(S)) return false;
  const actors = new Set([...Object.keys(s.roles), ...Object.keys(s.roleHistory)]);
  for (const a of actors) if (serverOf(a) === S && readableAt(a)) return true;
  return false;
}

// ───────────────────────── P-15 session access ─────────────────────────
export function sessionAccess(state: SpaceState, actor: ActorId, session: EntryId) {
  const none = { watch: false, read: false, steer: false };
  const sess = (state.sessions as A)[session];
  if (!sess || !live(state, sess)) return none;
  if (actor === sess.actor) return { watch: true, read: true, steer: true };
  if (sess.guard === actor && holds(state, actor)) return { watch: true, read: true, steer: true };
  const r = (state.roles as A)[actor];
  const right = (lvl: string) => lvl !== "nobody" && RANK[r] !== undefined && READABLE.includes(r) && RANK[r] >= RANK[lvl];
  const p = state.policy.sessions;
  return { watch: right(p.watch), read: right(p.read), steer: right(p.steer) };
}

// ───────────────────────── P-01 relevant actors ─────────────────────────
function candidates(s: SpaceState, e: A, time: string): A[] {
  const rule = ruleFor(s.policy, e.action);
  if (!rule) return [];
  return e.approvals.filter((a: A) => openApproval(a, time) && satisfiesRule(s, rule, a.author) && !(s.policy.separateApprover && a.author === e.proposer));
}
export function relevantActors(state: SpaceState | undefined, entry: Entry, time: Time): ActorId[] {
  const e = entry as A;
  const out: string[] = [e.author];
  const add = (a: string | undefined) => { if (a !== undefined && !out.includes(a)) out.push(a); };
  if (e.kind === "member.set") add(e.body?.actor);
  if (e.kind === "effect.claim" && state) {
    const ef = (state.effects as A)[e.body?.proposal];
    if (ef) {
      add(ef.proposer);
      for (const a of candidates(state, ef, time)) add(a.author);
      for (const g of guardsOf(state, ef)) add(g);
    }
  }
  return out as ActorId[];
}

// ───────────────────────── decide ─────────────────────────
const HELD_OK = ["message", "session.open", "session.close", "session.input", "task.update", "effect.outcome", "effect.resolve", "effect.claim",
  "effect.cancel", "task.cancel", "effect.propose"];

export function decide(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): Decision {
  const e = entry as A;
  if (!isObj(e)) return inv("entry not an object");
  const h = badHeader(e); if (h) return inv(h);
  const bb = badBody(e.kind, e.body); if (bb) return inv(bb);
  if (e.kind === "space.create") return decideCreate(state, e, ctx);
  if (!state) return deny("no such space");
  const s = state;
  if (e.space !== s.space) return inv("entry names another space");
  if (e.seen > s.head) return inv("seen beyond head");
  for (const r of e.refs) if (!(r in s.entries)) return inv("ref does not resolve");
  for (const i of bodyIds(e.kind, e.body)) if (!(i in s.entries)) return inv("body id does not resolve");
  const b = e.body as A, a = e.author as string, time = ctx.time;

  // R-12 key validity
  if (!authorKeyValid(s, ctx, e)) return deny("author key not valid");
  // P-04: denied servers may append only fact reports
  const factReport = e.kind === "effect.outcome" || e.kind === "effect.resolve";
  if (!factReport && s.policy.servers.deny.includes(serverOf(a))) return deny("author on a denied server");
  // P-15 hold
  if ((s.held as A)[a] !== undefined) {
    if (!HELD_OK.includes(e.kind)) return deny("agent is on hold");
    if (e.kind === "session.input") {
      const ts = (s.sessions as A)[b.session];
      if (!ts || ts.actor !== a) return deny("held agent may only steer its own sessions");
    }
  }

  switch (e.kind) {
    case "member.set": return decideMember(s, e, ctx);
    case "policy.set": return decidePolicySet(s, e);
    case "key.add": case "key.revoke": {
      if (s.profile === undefined) return deny("key entries are for profile spaces only");
      if (a !== profileActorOf(s.space) && a !== "@:" + spaceServer(s.space)) return deny("only the actor or its server");
      if (e.kind === "key.add") return b.keyId in s.profile.keys ? inv("keyId is single-use") : ok; // vector: invalid
      const k = s.profile.keys[b.keyId];
      if (!k) return deny("unknown key");
      return k.status === "revoked" ? inv("already revoked") : ok; // vector: invalid
    }
    case "message": {
      return holds(s, a) ? ok : deny("no role");
    }
    case "task": return decideTask(s, e, ctx);
    case "task.update": {
      const t = (s.tasks as A)[b.task];
      if (!t) return deny("not a task");
      if (!t.open) return deny("task closed");
      const final = FINAL_TASK.includes(b.state);
      if (isOwner(s, a) && final) return ok;
      if (!t.to.includes(a) || !holds(s, a)) return deny("not an assignee");
      if (final) {
        for (const ef of Object.values(s.effects) as A[]) if (ef.task === b.task && (ef.phase === "claimed" || ef.phase === "unknown")) return deny("effect running on the task");
      }
      return ok;
    }
    case "task.cancel": {
      const t = (s.tasks as A)[b.task];
      if (!t) return deny("not a task");
      if (!t.open) return deny("task closed");
      if (!holds(s, a)) return deny("no role");
      return t.author === a || isOwner(s, a) ? ok : deny("not the task's author or an owner");
    }
    case "effect.propose": return decidePropose(s, e, ctx);
    case "effect.approve": return decideApprove(s, e, ctx);
    case "effect.cancel": {
      const ef = (s.effects as A)[b.proposal];
      if (!ef) return deny("not a proposal");
      if (!holds(s, a)) return deny("no role");
      const guards = guardsOf(s, ef);
      const may = ef.proposer === a || ef.approvals.some((x: A) => x.author === a) || guards.includes(a) || isOwner(s, a);
      if (!may) return deny("not allowed to cancel");
      if (ef.phase === "awaiting-approval" || ef.phase === "ready") return ok;
      if (ef.phase === "claimed" || ef.phase === "unknown") return ef.cancelRequested ? deny("cancel already requested") : ok;
      return deny("effect is final");
    }
    case "effect.claim": return decideClaim(s, e, ctx);
    case "effect.outcome": return decideOutcome(s, e, ctx);
    case "effect.resolve": return decideResolve(s, e, ctx);
    case "checkpoint":
      if (!isMemberOrOwner(s, a)) return deny("not owner/member");
      return b.through <= s.head ? ok : inv("through beyond head"); // vector: invalid
    case "redact": {
      const target = (s.entries as A)[b.target];
      if (target.kind === "redact") return deny("cannot redact a redact");
      if (target.author !== a && !isOwner(s, a)) return deny("not the target's author or an owner");
      if (target.kind === "effect.propose") {
        const ef = (s.effects as A)[b.target];
        if (ef && !isFinalPhase(ef.phase)) return deny("plan of an unfinished effect");
      }
      return ok;
    }
    case "space.move": {
      if (!isOwner(s, a)) return deny("not an owner");
      for (const ef of Object.values(s.effects) as A[]) if (ef.phase === "claimed") return deny("a claim is open");
      return ok;
    }
    case "session.open": return decideSessionOpen(s, e, ctx);
    case "session.close": {
      const sess = (s.sessions as A)[b.session];
      if (!sess) return deny("not a session");
      return sess.actor === a || isOwner(s, a) ? ok : deny("not the session's actor or an owner");
    }
    case "session.input": return decideInput(s, e, ctx);
    default: {
      // extension kind
      const min = s.policy.extensionsMinRole;
      const r = roleOf(s, a);
      return READABLE.includes(r) && RANK[r] >= RANK[min] ? ok : deny("role below extensionsMinRole");
    }
  }
}

function decideCreate(state: SpaceState | undefined, e: A, ctx: PolicyCtx): Decision {
  if (state) return inv("space.create only at seq 1"); // vector: invalid (P-04 table suggests denied)
  if (e.seen !== 0) return inv("seen beyond head");
  if (e.refs.length) return inv("ref does not resolve");
  const b = e.body as A;
  const a = e.author as string;
  const home = spaceServer(e.space);
  const profileSpace = isProfileSpace(e.space);
  if (profileSpace) {
    if (!b.profile) return inv("profile space needs body.profile");
    if (a !== "@:" + home) return deny("profile space is created by the server actor");
    if (b.profile.operator !== undefined && b.profile.operator === profileActorOf(e.space)) return inv("actor names itself as operator");
    if (!keyValid(ctx, a, e.keyId)) return deny("author key not valid");
  } else {
    if (b.profile) return inv("body.profile only in profile spaces");
    if (kindOf(ctx, a) !== "human") return deny("owners are human");
    if (!keyValid(ctx, a, e.keyId)) return deny("author key not valid");
  }
  if (b.policy.servers.deny.includes(home) || b.policy.servers.deny.includes(serverOf(a))) return deny("policy denies the home or the creator's server");
  return ok;
}

function decideMember(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string, t = b.actor as string, role = b.role as string;
  if (s.profile !== undefined) {
    // §4.3: operator confirmation / withdrawal
    const op = s.profile.operator;
    if (!op || t !== op) return deny("only the operator may be set in a profile space");
    if (role === "owner") {
      if (a !== op) return deny("only the operator confirms");
      if (kindOf(ctx, op) !== "human") return deny("operator must be human");
      return ok;
    }
    if (role === "none") return a === op || a === profileActorOf(s.space) || a === "@:" + spaceServer(s.space) ? ok : deny("not allowed to withdraw");
    return deny("only owner or none in a profile space");
  }
  const cur = roleOf(s, t);
  const granting = READABLE.includes(role);
  const aRole = roleOf(s, a);
  if (granting && s.policy.servers.deny.includes(serverOf(t))) return deny("target on a denied server");
  if (granting && kindOf(ctx, t) !== "human") {
    if (s.policy.allowAgents === "none") return deny("agents not allowed");
    if (s.policy.allowAgents === "with-operator" && !(ctx.profiles as A)?.[t]?.operatorConfirmed) return deny("operator not confirmed");
  }
  const owners = Object.keys(s.roles).filter((k) => (s.roles as A)[k] === "owner");
  const lastOwner = (x: string) => cur === "owner" && x === t && owners.length === 1 && owners[0] === t;
  if (aRole === "owner") {
    if (role === "owner" && kindOf(ctx, t) !== "human") return deny("owners are human");
    if (cur === "owner" && role !== "owner" && owners.length === 1) return deny("would leave zero owners");
    return ok;
  }
  if (a === t && role === "none" && aRole !== "banned") return lastOwner(a) ? deny("last owner") : ok;
  if (s.policy.join === "open" && a === t && cur === "none" && role === "member") return ok;
  if (s.policy.membersMayInvite && aRole === "member" && cur === "none" && (role === "guest" || role === "member")) return ok;
  return deny("not allowed to set this role");
}

function decidePolicySet(s: SpaceState, e: A): Decision {
  const b = e.body as A, a = e.author as string;
  if (!isOwner(s, a)) return deny("not an owner");
  const np: PolicyDoc = b.policy;
  const deny_ = np.servers.deny;
  if (deny_.includes(s.home)) return deny("cannot deny the current home");
  for (const [k, r] of Object.entries(s.roles)) if (r === "owner" && deny_.includes(serverOf(k))) return deny("cannot deny an owner's server");
  const ob = s.policy.budget, nb = np.budget;
  const same = ob && nb && ob.unit === nb.unit;
  if (!same && (ob || nb) && reservedAnything(s)) return deny("budget unit/presence change while reserved");
  return ok;
}
function reservedAnything(s: SpaceState): boolean {
  for (const t of Object.values(s.tasks) as A[]) if (t.open && t.budget) return true;
  for (const ef of Object.values(s.effects) as A[]) for (const c of ef.claims) if (c.reserved && !c.settled) return true;
  return false;
}

function decideTask(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  if (b.to.length === 0) return deny("to is empty");
  for (const x of b.to) if (!holds(s, x)) return deny("assignee holds no role");
  let parent: A;
  if (b.parent !== undefined) {
    parent = (s.tasks as A)[b.parent];
    if (!parent) return deny("parent is not a task");
    if (!parent.open) return deny("parent closed");
    if (!parent.to.includes(a)) return deny("author not an assignee of parent");
    if (!holds(s, a)) return deny("no role");
    if (parent.depth + 1 > s.policy.maxDelegationDepth) return deny("delegation too deep");
  } else if (!isMemberOrOwner(s, a)) return deny("not owner/member");
  if (b.budget !== undefined) {
    if (!s.policy.budget || s.policy.budget.unit !== b.budget.unit) return deny("budget unit differs or space unbudgeted");
    if (!accountable(s, ctx, a)) return deny("not accountable");
    if (kindOf(ctx, a) !== "human" && !(parent && parent.budget)) return deny("agents' budgeted tasks need a budgeted parent");
    const avail = parent && parent.left ? parent.left.amount : (s.budgetLeft as A)?.amount;
    if (avail === undefined || b.budget.amount > avail) return deny("budget does not fit");
  } else if (parent && parent.budget) return deny("child of a budgeted parent must carry a budget");
  return ok;
}

function decidePropose(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  if (!isMemberOrOwner(s, a)) return deny("not owner/member");
  if (!accountable(s, ctx, a)) return deny("not accountable");
  let task: A;
  if (b.task !== undefined) {
    task = (s.tasks as A)[b.task];
    if (!task) return deny("not a task");
    if (!task.open) return deny("task closed");
    if (task.budget && !task.to.includes(a)) return deny("cannot charge a budgeted task one is not assigned to"); // vector 35#8
  }
  if (s.policy.budget) {
    if (!b.cost || b.cost.unit !== s.policy.budget.unit) return deny("budgeted space needs a cost in its unit");
    if (kindOf(ctx, a) !== "human") {
      if (!task || !task.budget || !task.to.includes(a)) return deny("agents must name an open budgeted task assigned to them");
    }
  }
  return ok;
}

function decideApprove(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const ef = (s.effects as A)[b.proposal];
  if (!ef) return deny("not a proposal");
  if (b.planDigest !== ef.planDigest) return inv("planDigest does not match");
  if (!["awaiting-approval", "ready", "claimed"].includes(ef.phase)) return deny("phase does not take approvals");
  if (b.expiresAt !== undefined && !(b.expiresAt > ctx.time)) return deny("approval already expired");
  if (!holds(s, a)) return deny("no role");
  const guards = guardsOf(s, ef);
  const rule = ruleFor(s.policy, ef.action);
  const asGuard = guards.includes(a);
  if (!asGuard && !(rule && satisfiesRule(s, rule, a))) return deny("not an approver");
  if (s.policy.separateApprover && a === ef.proposer) return deny("proposer cannot approve");
  if (!asGuard) {
    const open = ef.approvals.filter((x: A) => openApproval(x, ctx.time) && x.author !== a && !guards.includes(x.author)).length;
    if (open >= 16) return deny("too many open approvals");
  }
  return ok;
}

function e3(s: SpaceState, ef: A, ctx: PolicyCtx): { ok: true; approval?: string; guardApprovals: string[] } | { ok: false; why: string } {
  const rule = ruleFor(s.policy, ef.action);
  let approval: A;
  if (rule) {
    for (const x of candidates(s, ef, ctx.time)) if (keyValid(ctx, x.author, x.keyId)) { approval = x; break; }
    if (!approval) return { ok: false, why: "no valid approval" };
  }
  const guardApprovals: string[] = [];
  for (const g of guardsOf(s, ef)) {
    if (!holds(s, g)) return { ok: false, why: "guard holds no role" };
    const x = ef.approvals.find((y: A) => y.author === g && openApproval(y, ctx.time));
    if (!x || !keyValid(ctx, g, x.keyId)) return { ok: false, why: "guard has not approved" };
    guardApprovals.push(x.id);
  }
  return { ok: true, approval: approval?.id, guardApprovals };
}

function decideClaim(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const ef = (s.effects as A)[b.proposal];
  if (!ef) return deny("not a proposal");
  // E1
  if (a !== ef.executor) return deny("E1: not the executor");
  if (!isMemberOrOwner(s, a)) return deny("E1: executor holds no role");
  if (!accountable(s, ctx, a)) return deny("E1: executor not accountable");
  if (b.instance !== e.keyId || !keyValid(ctx, a, b.instance)) return deny("E1: instance is not the valid signing key");
  // E2
  if (!isMemberOrOwner(s, ef.proposer)) return deny("E2: proposer holds no role");
  if (!accountable(s, ctx, ef.proposer)) return deny("E2: proposer not accountable");
  if (!keyValid(ctx, ef.proposer, ef.proposerKey)) return deny("E2: proposer key revoked");
  // E4
  if (ef.cancelRequested) return deny("E4: cancel requested");
  // E5
  if (ef.phase !== "ready") return deny("E5: not ready");
  if (b.attempt !== 1 + ef.claims.length) return deny("E5: wrong attempt");
  if (b.window !== s.policy.launchWindow) return deny("E5: window differs from launchWindow");
  if (ef.expiresAt !== undefined && !(ef.expiresAt > ctx.time)) return deny("E5: proposal expired");
  if (ef.task !== undefined && !(s.tasks as A)[ef.task]?.open) return deny("E5: task closed");
  // E3
  const r = e3(s, ef, ctx); if (!r.ok) return deny("E3: " + r.why);
  // E6
  if (s.policy.budget) {
    if (!ef.cost || ef.cost.unit !== s.policy.budget.unit) return deny("E6: no cost in the space's unit");
    const task = ef.task !== undefined ? (s.tasks as A)[ef.task] : undefined;
    if (kindOf(ctx, ef.proposer) !== "human" && !(task && task.open && task.budget && task.to.includes(ef.proposer))) return deny("E6: agent proposer needs an open budgeted task");
    const left = task && task.budget ? task.left.amount : (s.budgetLeft as A).amount;
    if (ef.cost.amount > left) return deny("E6: cost does not fit");
  }
  return ok;
}

function findClaim(s: SpaceState, id: string): { ef: A; claim: A } | undefined {
  for (const ef of Object.values(s.effects) as A[]) for (const c of ef.claims) if (c.id === id) return { ef, claim: c };
  return undefined;
}
function costWithin(s: SpaceState, claim: A, cost: A | undefined): boolean {
  if (!cost) return true;
  if (claim.reserved) return cost.unit === claim.reserved.unit && cost.amount <= claim.reserved.amount;
  return s.policy.budget === null;
}
function decideOutcome(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const f = findClaim(s, b.claim);
  if (!f) return inv("claim does not resolve");
  const { ef, claim } = f;
  if (ef.phase !== "claimed") return deny("effect not claimed");
  if (ef.claims[ef.claims.length - 1] !== claim) return deny("not the latest claim");
  if (claim.outcome !== undefined) return deny("claim already has an outcome");
  const own = a === ef.executor && e.keyId === claim.instance;
  if (!own) {
    // abandon
    if (!isOwner(s, a)) return deny("abandon needs an owner");
    if (b.result !== "unknown" || b.started !== "unknown") return deny("abandon must be unknown/unknown");
    if (!(secs(ctx.time) >= secs(claim.time) + abandonAfter(claim.window))) return deny("abandon too early");
    return ok;
  }
  if (!costWithin(s, claim, b.cost)) return inv("cost exceeds the reservation"); // vector: invalid
  return ok;
}
function decideResolve(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const ef = (s.effects as A)[b.proposal];
  if (!ef) return deny("not a proposal");
  if (ef.phase !== "unknown") return deny("effect not unknown");
  if (a !== ef.executor && !isOwner(s, a)) return deny("not the executor or an owner");
  const claim = ef.claims[ef.claims.length - 1];
  if (!costWithin(s, claim, b.cost)) return inv("cost exceeds the reservation"); // vector: invalid
  return ok;
}

function decideSessionOpen(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  if (!holds(s, a)) return deny("no role");
  if (!accountable(s, ctx, a)) return deny("not accountable");
  if (!s.policy.sessions.modes.includes(b.steerMode)) return deny("steerMode not allowed");
  const open = (Object.values(s.sessions) as A[]).filter((x) => x.actor === a && x.open).length;
  if (open >= 64) return deny("too many open sessions");
  if (b.steerMode === "guarded" && kindOf(ctx, a) !== "human") {
    const v = (ctx.profiles as A)?.[a];
    if (!v?.operatorConfirmed || !v.operator || !holds(s, v.operator)) return deny("guarded session needs a confirmed operator with a role");
  }
  if (b.task !== undefined) {
    const t = (s.tasks as A)[b.task];
    if (!t) return deny("not a task");
    if (!t.to.includes(a) && !isOwner(s, a)) return deny("not assigned to the task");
  }
  return ok;
}

function decideInput(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const sess = (s.sessions as A)[b.session];
  if (!sess) return deny("not a session");
  if (b.release === true) {
    const x = sess.actor;
    if ((s.held as A)[x] !== a) return deny("not the guard of a held agent");
    if (!holds(s, a)) return deny("guard holds no role");
    if (((s.outside as A)[x] ?? 0) > e.seen) return deny("an outside instruction arrived after seen");
    return ok;
  }
  if (!sessionAccess(s, a as ActorId, b.session).steer) return deny("no steer right");
  if (!accountable(s, ctx, a)) return deny("not accountable");
  return ok;
}

// ───────────────────────── fold ─────────────────────────
function refundFrom(s: SpaceState, start: string | undefined, amount: number, includeStart: boolean) {
  if (amount === 0) return;
  let id = start;
  let first = true;
  while (id !== undefined) {
    const t = (s.tasks as A)[id];
    if (!t) break;
    if ((includeStart || !first) && t.open && t.left) { t.left.amount += amount; return; }
    first = false;
    id = t.parent;
  }
  if (s.budgetLeft) s.budgetLeft.amount += amount;
}
function refundClaim(s: SpaceState, claim: A, amount: number) {
  if (!claim.reserved) return;
  if (claim.source && claim.source[0] === "task") refundFrom(s, claim.source[1], amount, true);
  else if (s.budgetLeft) s.budgetLeft.amount += amount;
}
function freshPhase(s: SpaceState, ef: A): string {
  return ruleFor(s.policy, ef.action) || (s.held as A)[ef.proposer] ? "awaiting-approval" : "ready";
}
function satisfied(s: SpaceState, ef: A, time: string): boolean {
  const rule = ruleFor(s.policy, ef.action);
  if (rule && candidates(s, ef, time).length === 0) return false;
  for (const g of guardsOf(s, ef)) if (!ef.approvals.some((x: A) => x.author === g && openApproval(x, time))) return false;
  return true;
}
function instruct(s: SpaceState, x: string, author: string, seq: number) {
  const g = guardedNow(s, x);
  if (g && author !== x && author !== g) { (s.held as A)[x] = g; (s.outside as A)[x] = seq; }
}

export function fold(state: SpaceState | undefined, record: LogRecord, ctx: PolicyCtx): SpaceState {
  const e = record.entry as A;
  const rc = record.receipt as A;
  const id: string = rc?.id ?? e.id;
  const seq: number = rc.seq;
  const time = ctx.time;
  const b = e.body as A, a = e.author as string;
  let s: SpaceState;
  if (e.kind === "space.create") {
    if (state) throw new Error("space.create on an existing space");
    s = {
      space: e.space, head: 0, home: spaceServer(e.space), policy: structuredClone(b.policy), roles: {}, roleHistory: {}, tasks: {}, effects: {},
      budgetLeft: b.policy.budget ? { unit: b.policy.budget.unit, amount: b.policy.budget.amount } : null,
      entries: {}, sessions: {}, held: {}, cleared: {}, outside: {}, guarded: {},
    } as SpaceState;
    if (isProfileSpace(e.space)) {
      s.profile = { actorKind: b.profile.actorKind, operatorConfirmed: false, keys: { [b.profile.key.keyId]: { publicKey: b.profile.key.publicKey, status: "valid" } } };
      if (b.profile.operator !== undefined) s.profile.operator = b.profile.operator;
    } else {
      (s.roles as A)[a] = "owner";
      (s.roleHistory as A)[a] = [{ from: seq }];
    }
  } else {
    if (!state) throw new Error("no state");
    s = structuredClone(state);
  }
  s.head = seq;
  (s.entries as A)[id] = { author: a, kind: e.kind };

  switch (e.kind) {
    case "member.set": {
      if (s.profile !== undefined) { s.profile.operatorConfirmed = b.role === "owner"; break; }
      const t = b.actor as string;
      const was = READABLE.includes((s.roles as A)[t] ?? "none");
      if (b.role === "none") delete (s.roles as A)[t]; else (s.roles as A)[t] = b.role;
      const now = READABLE.includes(b.role);
      const hist = ((s.roleHistory as A)[t] ??= []);
      if (!was && now) hist.push({ from: seq });
      else if (was && !now) { const last = hist[hist.length - 1]; if (last && last.to === undefined) last.to = seq; }
      break;
    }
    case "policy.set": {
      const ob = s.policy.budget, nb = b.policy.budget;
      if (ob && nb && ob.unit === nb.unit) {
        const committed = ob.amount - s.budgetLeft!.amount;
        s.budgetLeft = { unit: nb.unit, amount: nb.amount - committed };
      } else if (ob || nb) {
        s.budgetLeft = nb ? { unit: nb.unit, amount: nb.amount } : null;
      }
      s.policy = structuredClone(b.policy);
      for (const ef of Object.values(s.effects) as A[]) {
        if (ef.phase === "awaiting-approval" && !ruleFor(s.policy, ef.action) && satisfied(s, ef, time)) ef.phase = "ready";
      }
      break;
    }
    case "key.add": s.profile!.keys[b.keyId] = { publicKey: b.publicKey, status: "valid" }; break;
    case "key.revoke": s.profile!.keys[b.keyId].status = "revoked"; break;
    case "task": {
      const parent = b.parent !== undefined ? (s.tasks as A)[b.parent] : undefined;
      const t: A = { author: a, to: [...b.to], open: true, state: "pending", cancelRequested: false, depth: parent ? parent.depth + 1 : 0, seq };
      if (b.parent !== undefined) t.parent = b.parent;
      if (b.budget) {
        t.budget = { unit: b.budget.unit, amount: b.budget.amount };
        t.left = { unit: b.budget.unit, amount: b.budget.amount };
        if (parent && parent.left) parent.left.amount -= b.budget.amount; else s.budgetLeft!.amount -= b.budget.amount;
      }
      (s.tasks as A)[id] = t;
      for (const x of b.to) instruct(s, x, a, seq);
      break;
    }
    case "task.update": {
      const t = (s.tasks as A)[b.task];
      t.state = b.state;
      if (FINAL_TASK.includes(b.state)) {
        t.open = false;
        if (t.left) { const amt = t.left.amount; t.left.amount = 0; refundFrom(s, t.parent, amt, true); }
      }
      break;
    }
    case "task.cancel": (s.tasks as A)[b.task].cancelRequested = true; break;
    case "effect.propose": {
      const ef: A = {
        proposer: a, proposerKey: e.keyId, executor: b.executor, action: b.action, planDigest: b.planDigest, seq, repeatable: b.repeatable,
        phase: "ready", cancelRequested: false, approvals: [], claims: [],
      };
      if (b.cost) ef.cost = { unit: b.cost.unit, amount: b.cost.amount };
      if (b.task !== undefined) ef.task = b.task;
      if (b.expiresAt !== undefined) ef.expiresAt = b.expiresAt;
      const hg = (s.held as A)[a];
      if (hg) ef.guards = [hg];
      ef.phase = freshPhase(s, ef);
      (s.effects as A)[id] = ef;
      instruct(s, b.executor, a, seq);
      break;
    }
    case "effect.approve": {
      const ef = (s.effects as A)[b.proposal];
      for (const x of ef.approvals) if (x.author === a && !x.consumedBy && !x.dropped) x.dropped = true;
      const ap: A = { id, author: a, keyId: e.keyId };
      if (b.expiresAt !== undefined) ap.expiresAt = b.expiresAt;
      ef.approvals.push(ap);
      if (ef.phase === "awaiting-approval" && satisfied(s, ef, time)) ef.phase = "ready";
      break;
    }
    case "effect.cancel": {
      const ef = (s.effects as A)[b.proposal];
      if (ef.phase === "awaiting-approval" || ef.phase === "ready") ef.phase = "cancelled";
      else ef.cancelRequested = true;
      break;
    }
    case "effect.claim": {
      const ef = (s.effects as A)[b.proposal];
      const r = e3(s, ef, ctx);
      const claim: A = { id, attempt: b.attempt, instance: b.instance, time, window: b.window };
      if (r.ok) {
        if (r.approval) claim.approval = r.approval;
        if (r.guardApprovals.length) claim.guardApprovals = r.guardApprovals;
        const used = new Set([r.approval, ...r.guardApprovals]);
        for (const x of ef.approvals) if (used.has(x.id)) x.consumedBy = id;
      }
      if (s.policy.budget && ef.cost) {
        claim.reserved = { unit: ef.cost.unit, amount: ef.cost.amount };
        const task = ef.task !== undefined ? (s.tasks as A)[ef.task] : undefined;
        if (task && task.left) { claim.source = ["task", ef.task]; task.left.amount -= ef.cost.amount; }
        else { claim.source = ["space", null]; s.budgetLeft!.amount -= ef.cost.amount; }
      }
      ef.claims.push(claim);
      ef.phase = "claimed";
      break;
    }
    case "effect.outcome": {
      const { ef, claim } = findClaim(s, b.claim)!;
      const own = a === ef.executor && e.keyId === claim.instance;
      claim.outcome = b.result; claim.started = b.started;
      if (!own) { ef.phase = "unknown"; break; } // abandon: always unknown, reservation stays charged
      const giveBack = () => {
        for (const x of ef.approvals) if (x.consumedBy === claim.id) {
          delete x.consumedBy;
          if (ef.approvals.some((y: A) => y !== x && y.author === x.author && !y.dropped && !y.consumedBy)) x.dropped = true; // superseded meanwhile (vector 94)
        }
      };
      const settle = (cost: A | undefined) => {
        if (!claim.reserved) { claim.settled = true; return; }
        const spent = cost ? cost.amount : claim.reserved.amount;
        refundClaim(s, claim, claim.reserved.amount - spent);
        claim.settled = true;
      };
      if (b.started === "no") {
        giveBack();
        if (claim.reserved) { refundClaim(s, claim, claim.reserved.amount); claim.settled = true; }
        ef.phase = ef.cancelRequested ? "cancelled" : "ready";
      } else if (b.started === "yes" && b.result !== "unknown") {
        settle(b.cost);
        if (b.result === "succeeded") ef.phase = "succeeded";
        else ef.phase = ef.repeatable && !ef.cancelRequested ? "ready" : "failed";
      } else {
        // the executor's own unknown
        if (ef.repeatable && !ef.cancelRequested) { claim.settled = true; ef.phase = "ready"; }
        else ef.phase = "unknown";
      }
      break;
    }
    case "effect.resolve": {
      const ef = (s.effects as A)[b.proposal];
      const claim = ef.claims[ef.claims.length - 1];
      if (b.result === "succeeded") {
        if (claim.reserved) { const spent = b.cost ? b.cost.amount : claim.reserved.amount; refundClaim(s, claim, claim.reserved.amount - spent); }
        claim.settled = true;
        ef.phase = "succeeded";
      } else {
        if (claim.reserved) refundClaim(s, claim, claim.reserved.amount);
        claim.settled = true;
        for (const x of ef.approvals) x.dropped = true;
        ef.phase = ef.cancelRequested ? "cancelled" : freshPhase(s, ef);
      }
      break;
    }
    case "space.move": s.home = b.to; break;
    case "session.open": {
      const v = (ctx.profiles as A)?.[a];
      const human = kindOf(ctx, a) === "human";
      const sess: A = { actor: a, open: true, opened: time, steerMode: b.steerMode, steers: 0, human };
      if (b.task !== undefined) sess.task = b.task;
      if (!human && v?.operatorConfirmed && v.operator) sess.guard = v.operator;
      (s.sessions as A)[id] = sess;
      if (b.steerMode === "guarded" && sess.guard) (s.guarded as A)[a] = sess.guard;
      break;
    }
    case "session.close": {
      const sess = (s.sessions as A)[b.session];
      sess.open = false;
      if (a === sess.actor && sess.steerMode === "guarded" && !(s.held as A)[a] && (s.guarded as A)[a] !== undefined) {
        const others = (Object.values(s.sessions) as A[]).some((x) => x.actor === a && x.open && x.steerMode === "guarded");
        if (!others) delete (s.guarded as A)[a];
      }
      break;
    }
    case "session.input": {
      const sess = (s.sessions as A)[b.session];
      if (b.release === true) {
        delete (s.held as A)[sess.actor];
        (s.cleared as A)[sess.actor] = e.seen;
      } else {
        sess.steers += 1;
        if (!sess.human) instruct(s, sess.actor, a, seq);
      }
      break;
    }
    default: break;
  }
  settle(s, id, time);
  return s;
}

/** P-15 "after every entry Policy also settles holds in one place". */
function settle(s: SpaceState, createdId: string, time: string) {
  const agents = new Set<string>([...Object.keys(s.guarded), ...(Object.values(s.sessions) as A[]).map((x) => x.actor)]);
  for (const x of agents) {
    const g = guardedNow(s, x);
    if (!g) continue;
    const since = (s.cleared as A)[x] ?? 0;
    let pending = false;
    for (const t of Object.values(s.tasks) as A[]) if (t.open && t.to.includes(x) && t.author !== x && t.author !== g && t.seq > since) pending = true;
    for (const ef of Object.values(s.effects) as A[]) {
      if (isFinalPhase(ef.phase) || ef.executor !== x || ef.proposer === x || ef.proposer === g || ef.seq <= since) continue;
      pending = true;
      ef.guards ??= [];
      if (!ef.guards.includes(g)) {
        ef.guards.push(g);
        if (ef.phase === "ready" && !ef.approvals.some((y: A) => y.author === g && openApproval(y, time))) ef.phase = "awaiting-approval";
      }
    }
    if (pending) (s.held as A)[x] = g;
  }
}
