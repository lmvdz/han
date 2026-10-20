// Clean-room HAN v1 Policy (§6): decide, fold, canRead, relevantActors, sessionAccess (+ guardStatus).
import type { ActorId, Decision, Entry, EntryId, GuardInput, LogRecord, PolicyCtx, PolicyDoc, SpaceState, Seq } from "./han.js";
import {
  isActorId, isAction, isHex, isInt, isKeyId, isKind, isNonce, isPublicKey, isServerName, isSig, isSpaceId, isTime, isUnit,
  isProfileSpace, profileActorOf, serverOf, spaceServer, jcs, utf8len, jsonInts, isExtensionKind, sha256,
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
  let e = closed(p, ["visibility", "join", "membersMayInvite", "allowAgents", "approvals", "separateApprover", "budget", "maxDelegationDepth",
    "minDurability", "launchWindow", "extensionsMinRole", "sessions", "servers"], ["gates"]);
  if (e) return "policy: " + e;
  if (!["members", "public"].includes(p.visibility)) return "policy.visibility";
  if (!["invite", "open", "closed"].includes(p.join)) return "policy.join";
  if (typeof p.membersMayInvite !== "boolean") return "policy.membersMayInvite";
  if (!["any", "with-operator", "none"].includes(p.allowAgents)) return "policy.allowAgents";
  if (!Array.isArray(p.approvals) || p.approvals.length > 64) return "policy.approvals";
  for (const r of p.approvals) {
    const e2 = closed(r, ["action", "approvers"], ["repo", "minTier", "maxTier", "humans", "evidence", "needs", "revert", "proposer"]); if (e2) return "approval rule: " + e2;
    if ("repo" in r && !(typeof r.repo === "string" && /^[!-~]{1,128}$/.test(r.repo))) return "approval rule repo";
    if (!isAction(r.action, true)) return "approval rule action";
    if (!Array.isArray(r.approvers) || r.approvers.length === 0) return "approvers";
    for (const a of r.approvers) if (!(["owner", "member", "guest", PATH_OWNER].includes(a) || isActorId(a))) return "approver";
    for (const k of ["minTier", "maxTier"]) if (k in r && !(isInt(r[k]) && r[k] <= 3)) return "approval rule " + k;
    if ("minTier" in r && "maxTier" in r && r.minTier > r.maxTier) return "approval rule minTier above maxTier";
    if ("humans" in r && typeof r.humans !== "boolean") return "approval rule humans";
    if ("evidence" in r && !(typeof r.evidence === "string" && reEvidenceKind.test(r.evidence))) return "approval rule evidence";
    if ("needs" in r && !(isInt(r.needs) && r.needs >= 1 && r.needs <= 3)) return "approval rule needs";
    if ("revert" in r && r.revert !== true) return "approval rule revert";   // A2: `true` only
    if ("proposer" in r) {   // P-17 (round 33): `true` only, on exactly "vcs.try", with no member but action, approvers and proposer
      if (r.proposer !== true) return "approval rule proposer";
      const check = r.action === "vcs.try" && Object.keys(r).every((k) => ["action", "approvers", "proposer"].includes(k));
      const land = r.action === "vcs.land" && r.repo !== undefined && r.maxTier !== undefined && r.maxTier <= 1
        && Object.keys(r).every((k) => ["action", "approvers", "proposer", "repo", "minTier", "maxTier"].includes(k));
      if (!check && !land) return "approval rule proposer scope";
    }
  }
  if (typeof p.separateApprover !== "boolean") return "policy.separateApprover";
  if (p.budget !== null) { const e3 = badBudget(p.budget); if (e3) return e3; }
  if (!isInt(p.maxDelegationDepth) || p.maxDelegationDepth > 16) return "policy.maxDelegationDepth";
  if (!["local", "replicated"].includes(p.minDurability)) return "policy.minDurability";
  if (!isInt(p.launchWindow) || p.launchWindow < 15 || p.launchWindow > 3600) return "policy.launchWindow";
  if (!["guest", "member", "owner"].includes(p.extensionsMinRole)) return "policy.extensionsMinRole";
  const se = closed(p.sessions, ["watch", "read", "steer", "modes"], ["forceGuarded"]); if (se) return "policy.sessions: " + se;
  for (const k of ["watch", "read", "steer"]) if (!LEVELS.includes(p.sessions[k])) return "policy.sessions." + k;
  if (strict(p.sessions.read) < strict(p.sessions.watch) || strict(p.sessions.steer) < strict(p.sessions.watch)) return "read/steer wider than watch";
  if (!Array.isArray(p.sessions.modes) || p.sessions.modes.length === 0 || !p.sessions.modes.every((m: A) => m === "auto" || m === "guarded")) return "policy.sessions.modes";
  if ("forceGuarded" in p.sessions && typeof p.sessions.forceGuarded !== "boolean") return "policy.sessions.forceGuarded";
  const sv = closed(p.servers, ["deny"]); if (sv) return "policy.servers: " + sv;
  if (!Array.isArray(p.servers.deny) || p.servers.deny.length > 256 || !p.servers.deny.every(isServerName)) return "policy.servers.deny";
  if ("gates" in p && (e = badGates(p.gates))) return e;
  return null;
}

// ───────────────────────── P-17 gates: schemas, globs, tiers ─────────────────────────
const PATH_OWNER = "@path-owner";
const reEvidenceKind = /^[a-z0-9][a-z0-9./_-]{0,63}$/;
const reGlobChars = /^[A-Za-z0-9._/*?@+=,~:-]+$/;
const rePrintable = /^[!-~]{1,128}$/;
const MAX_CHANGE_FILES = 64, MAX_CHANGE_BYTES = 12288, MAX_GATE_RULES = 64, MAX_PATH_OWNERS = 16;   // han.ts PolicyConstants
const PATH_CLASSES = ["docs", "tests", "generated", "code", "sensitive", "critical"];
const CLASS_TIER: Record<string, number> = { docs: 0, tests: 0, generated: 0, code: 1, sensitive: 2, critical: 3 };
function badGlob(g: A): string | null {
  if (typeof g !== "string" || g.length < 1 || g.length > 256 || !reGlobChars.test(g)) return "glob";
  let stars = 0;
  for (const seg of g.split("/")) {
    if (seg === "" || seg === "." || seg === "..") return "glob segment";
    if (seg === "**") { if (++stars > 4) return "glob has more than four **"; }
    else if (seg.includes("**")) return "glob ** inside a segment";
  }
  return null;
}
function badGates(g: A): string | null {
  let e = closed(g, ["small", "large", "paths", "owners"]); if (e) return "gates: " + e;
  if (!isInt(g.small) || !isInt(g.large) || g.small > g.large) return "gates.small/large";
  if (!Array.isArray(g.paths) || g.paths.length > MAX_GATE_RULES) return "gates.paths";
  for (const r of g.paths) {
    if ((e = closed(r, ["glob", "class"], ["repo"]))) return "path rule: " + e;
    if ((e = badGlob(r.glob))) return e;
    if (!PATH_CLASSES.includes(r.class)) return "path rule class";
    if ("repo" in r && !(typeof r.repo === "string" && rePrintable.test(r.repo))) return "path rule repo";
  }
  if (!Array.isArray(g.owners) || g.owners.length > MAX_GATE_RULES) return "gates.owners";
  for (const r of g.owners) {
    if ((e = closed(r, ["glob", "owners"], ["repo"]))) return "owner rule: " + e;
    if ((e = badGlob(r.glob))) return e;
    if (!Array.isArray(r.owners) || r.owners.length < 1 || r.owners.length > 8 || !r.owners.every(isActorId) || new Set(r.owners).size !== r.owners.length) return "owner rule owners";
    if ("repo" in r && !(typeof r.repo === "string" && rePrintable.test(r.repo))) return "owner rule repo";
  }
  return null;
}
/** One glob segment against one path segment: `*` is a run of characters (maybe none), `?` one character, over Unicode scalar values. */
function segMatch(pat: string[], s: string[]): boolean {
  const m = pat.length, n = s.length;
  const t: boolean[][] = Array.from({ length: m + 1 }, () => new Array<boolean>(n + 1).fill(false));
  t[0][0] = true;
  for (let i = 1; i <= m; i++) {
    for (let j = 0; j <= n; j++) {
      if (pat[i - 1] === "*") t[i][j] = t[i - 1][j] || (j > 0 && t[i][j - 1]);
      else t[i][j] = j > 0 && t[i - 1][j - 1] && (pat[i - 1] === "?" || pat[i - 1] === s[j - 1]);
    }
  }
  return t[m][n];
}
/** A glob matches a whole path, anchored and case-sensitive; a `**` segment is zero or more whole path segments. */
function globMatch(glob: string, path: string): boolean {
  const gs = glob.split("/"), ps = path.split("/");
  const memo = new Map<number, boolean>();
  const go = (i: number, j: number): boolean => {
    const k = i * (ps.length + 1) + j;
    const hit = memo.get(k); if (hit !== undefined) return hit;
    let r: boolean;
    if (i === gs.length) r = j === ps.length;
    else if (gs[i] === "**") { r = false; for (let x = j; x <= ps.length && !r; x++) r = go(i + 1, x); }
    else r = j < ps.length && segMatch(Array.from(gs[i]), Array.from(ps[j])) && go(i + 1, j + 1);
    memo.set(k, r); return r;
  };
  return go(0, 0);
}
/** The built-in never-auto list (P-17): a path matching one is at least tier 2 whatever the rules say. */
const NEVER_AUTO = [
  ".github/**", ".gitlab/**", ".gitlab-ci.yml", ".circleci/**", "**/Jenkinsfile", "azure-pipelines.yml",
  ".han/**",
  "**/Cargo.toml", "**/Cargo.lock", "**/package.json", "**/package-lock.json", "**/pnpm-lock.yaml", "**/yarn.lock", "**/bun.lock", "**/bun.lockb", "**/go.mod", "**/go.sum",
  "**/requirements*.txt", "**/pyproject.toml", "**/poetry.lock", "**/uv.lock", "**/Pipfile", "**/Pipfile.lock", "**/Gemfile", "**/Gemfile.lock", "**/pom.xml",
  "**/build.gradle", "**/build.gradle.kts", "**/deny.toml", "**/rust-toolchain.toml",
  "**/.env", "**/.env.*", "**/*.pem", "**/*.key", "**/*.p12", "**/*.pfx", "**/*.keystore", "**/id_rsa*", "**/id_ed25519*", "**/.secrets/**", "**/secrets/**",
  "**/credentials", "**/credentials.*", "**/.netrc", "**/.npmrc", "**/.pypirc",
  "**/CODEOWNERS", "**/sudoers", "**/sudoers.d/**", "**/.ssh/**",
];
function badChangePath(p: A): boolean {
  if (typeof p !== "string") return true;
  const n = utf8len(p);
  if (n < 1 || n > 255 || /[\u0000-\u001f\u007f-\u009f]/.test(p)) return true;
  return p.split("/").some((seg) => seg === "" || seg === "." || seg === "..");
}
const byteCmp = (a: string, b: string) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));
/** The shape of a change summary (P-17); null when it is one. */
function badChange(c: A): string | null {
  let e = closed(c, ["repo", "deltaDigest", "total", "files"], ["truncated", "revertOf"]); if (e) return "change: " + e;
  if ("revertOf" in c && !isHex(c.revertOf)) return "change.revertOf";   // A2: an EntryId
  if (typeof c.repo !== "string" || !rePrintable.test(c.repo)) return "change.repo";
  if (!isHex(c.deltaDigest)) return "change.deltaDigest";
  if (!isInt(c.total)) return "change.total";
  if (!Array.isArray(c.files) || c.files.length > MAX_CHANGE_FILES) return "change.files";
  let prev: string | undefined;
  for (const f of c.files) {
    if ((e = closed(f, ["path", "kind", "lines"], ["fromPath"]))) return "change file: " + e;
    if (badChangePath(f.path)) return "change file path";
    if (!["add", "modify", "delete", "rename"].includes(f.kind)) return "change file kind";
    if (!isInt(f.lines)) return "change file lines";
    if (f.kind === "rename") {
      if (!("fromPath" in f) || badChangePath(f.fromPath) || f.fromPath === f.path) return "change file fromPath";
    } else if ("fromPath" in f) return "change file fromPath on a " + f.kind;
    if (prev !== undefined && byteCmp(prev, f.path) >= 0) return "change files not in ascending order of path";
    prev = f.path;
  }
  if (c.total < c.files.length) return "change.total below the files listed";
  if ("truncated" in c ? c.truncated !== true || c.total === c.files.length : c.total > c.files.length) return "change.truncated";
  if (utf8len(jcs(c)) > MAX_CHANGE_BYTES) return "change too large";
  return null;
}
const inRepo = (rule: A, repo: string) => rule.repo === undefined || rule.repo === repo;
const pathsOf = (f: A): string[] => (f.fromPath !== undefined ? [f.path, f.fromPath] : [f.path]);
/** P-17, the tier of a landing: the highest of the floor of every path (a rename's fromPath too), the size, and 3 for a summary that is missing or truncated. */
function deriveTier(gates: A, change: A | undefined): number {
  if (!change || change.truncated) return 3;
  let tier = 0, lines = 0;
  for (const f of change.files as A[]) {
    lines += f.lines;
    for (const path of pathsOf(f)) {
      const rule = gates.paths.find((r: A) => inRepo(r, change.repo) && globMatch(r.glob, path));
      let floor = rule ? CLASS_TIER[rule.class] : 1;
      if (NEVER_AUTO.some((g) => globMatch(g, path))) floor = Math.max(floor, 2);
      tier = Math.max(tier, floor);
    }
  }
  if (lines > gates.large) tier = Math.max(tier, 2);
  else if (lines > gates.small) tier = Math.max(tier, 1);
  return tier;
}
/** P-17, the gate an effect.propose records, fixed then: only for a vcs.land in a policy with gates. */
function deriveGate(s: SpaceState, ctx: PolicyCtx, proposer: string, b: A): A | undefined {
  const gates = (s.policy as A).gates;
  if (!gates || b.action !== "vcs.land") return undefined;
  const change: A | undefined = b.change;
  const gate: A = { tier: deriveTier(gates, change) };
  if (change) { gate.repo = change.repo; gate.deltaDigest = change.deltaDigest; gate.changeDigest = sha256(jcs(change)); }
  if (change?.revertOf !== undefined) {   // A2: never a lower tier than the change it undoes; 3 when that effect had no gate
    const reverted: A | undefined = (s.effects as A)[change.revertOf];
    gate.tier = Math.max(gate.tier, reverted?.gate ? reverted.gate.tier : 3);
    gate.revertOf = change.revertOf;
    if (reverted?.gate?.deltaDigest !== undefined) gate.revertedDigest = reverted.gate.deltaDigest;
  }
  const owners: string[] = [];
  for (const f of (change?.files ?? []) as A[]) {
    for (const path of pathsOf(f)) {
      const rule = gates.owners.find((r: A) => inRepo(r, change.repo) && globMatch(r.glob, path));
      if (rule) for (const o of rule.owners as string[]) if (!owners.includes(o)) owners.push(o);
    }
  }
  if (owners.length === 0) { const g = guardStatus(s, ctx, proposer).guard; if (g !== undefined) owners.push(g); }
  if (owners.length) gate.owners = owners.slice(0, MAX_PATH_OWNERS);
  return gate;
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
      if ((e = c(["to"], ["budget", "parent", "deadline", "external"]))) return e;
      if (!Array.isArray(b.to) || b.to.length === 0 || b.to.length > 64 || !b.to.every(isActorId)) return "to";
      if ("budget" in b && (e = badBudget(b.budget))) return e;
      if ("parent" in b && !isHex(b.parent)) return "parent";
      if ("deadline" in b && !isTime(b.deadline)) return "deadline";
      if ("external" in b && !(typeof b.external === "string" && /^[!-~]{1,1024}$/.test(b.external))) return "external";
      return null;
    case "task.update":
      if ((e = c(["task", "state"]))) return e;
      return isHex(b.task) && TASK_STATES.includes(b.state) ? null : "task.update fields";
    case "task.cancel": return (e = c(["task"])) ? e : isHex(b.task) ? null : "task";
    case "effect.propose":
      if ((e = c(["executor", "action", "planDigest", "repeatable"], ["cost", "task", "expiresAt", "change"]))) return e;
      if (!isActorId(b.executor) || !isAction(b.action) || !isHex(b.planDigest) || typeof b.repeatable !== "boolean") return "propose fields";
      if ("cost" in b && (e = badBudget(b.cost))) return e;
      if ("task" in b && !isHex(b.task)) return "task";
      if ("expiresAt" in b && !isTime(b.expiresAt)) return "expiresAt";
      if ("change" in b && (e = badChange(b.change))) return e;
      return null;
    case "effect.approve":
      if ((e = c(["proposal", "planDigest"], ["expiresAt", "evidence"]))) return e;
      if (!isHex(b.proposal) || !isHex(b.planDigest)) return "approve fields";
      if ("evidence" in b) {
        if ((e = closed(b.evidence, ["kind", "over"]))) return "evidence: " + e;
        if (typeof b.evidence.kind !== "string" || !reEvidenceKind.test(b.evidence.kind) || !isHex(b.evidence.over)) return "evidence fields";
      }
      return !("expiresAt" in b) || isTime(b.expiresAt) ? null : "expiresAt";
    case "effect.claim":
      if ((e = c(["proposal", "attempt", "instance", "window"]))) return e;
      return isHex(b.proposal) && isInt(b.attempt) && isKeyId(b.instance) && isInt(b.window) ? null : "claim fields";
    case "effect.cancel": return (e = c(["proposal"])) ? e : isHex(b.proposal) ? null : "proposal";
    case "effect.outcome": {
      if ((e = c(["claim", "result", "started"], ["cost"]))) return e;
      if (!isHex(b.claim) || !RESULTS.includes(b.result) || !STARTED.includes(b.started)) return "outcome fields";
      if ("cost" in b && (e = badBudget(b.cost))) return e;
      return null; // pair and cost/started rules are in the kind list (P-16)
    }
    case "effect.resolve":
      if ((e = c(["proposal", "result"], ["cost"]))) return e;
      if (!isHex(b.proposal) || !["succeeded", "failed"].includes(b.result)) return "resolve fields";
      if ("cost" in b && (e = badBudget(b.cost))) return e;
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
    case "session.input": return (e = c(["session"])) ? e : isHex(b.session) ? null : "session";
    case "agent.unguard":
      if ((e = c(["agent"]))) return e;
      return isActorId(b.agent) ? null : "agent";
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
/** R-25: decide checks only the header fields it reads: v, space, kind, author, keyId, refs, seen. Everything else is the Log's. */
function badHeader(e: A): string | null {
  if (e.v !== 1) return "v";
  if (!isSpaceId(e.space)) return "space";
  if (!isKind(e.kind)) return "kind";
  if (!isActorId(e.author)) return "author";
  if (!isKeyId(e.keyId)) return "keyId";
  if (!idList(e.refs)) return "refs";
  if (!isInt(e.seen)) return "seen";
  if (!isObj(e.body)) return "body";
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
/** P-17: a rule with minTier or maxTier applies only to an effect with a gate whose tier is within them. */
const tierWithin = (r: A, gate: A | undefined): boolean =>
  (r.minTier === undefined && r.maxTier === undefined) || (gate !== undefined && gate.tier >= (r.minTier ?? 0) && gate.tier <= (r.maxTier ?? 3));
const OWNER_RULE = { action: "*", approvers: ["owner"] };
/** A2: a rule with `revert: true` applies only to an effect whose gate has `revertOf`. */
const revertWithin = (r: A, gate: A | undefined): boolean => r.revert !== true || gate?.revertOf !== undefined;
/** P-08, P-17: the first rule whose action matches and whose tier and revert predicates hold; a gated effect of tier 1 or more that none covers needs an owner. */
const ruleFor = (p: PolicyDoc, ef: A): A | undefined =>
  p.approvals.find((r: A) => (r.action === ef.action || r.action === "*") && (r.repo === undefined || r.repo === ef.gate?.repo) && tierWithin(r, ef.gate) && revertWithin(r, ef.gate)) ?? (ef.gate && ef.gate.tier >= 1 ? OWNER_RULE : undefined);
/** P-17, who counts: a role now that the rule names (a role, an ID, or @path-owner: an author in the effect's gate.owners) and, for a non-human author (by the kind
 *  the approval recorded), a tier below 2, no `humans`, and the evidence the rule asks for, of that kind and over the effect's gate.deltaDigest. */
function approvalCounts(s: SpaceState, ef: A, rule: A, author: string, kind: string, evidence: A | undefined): boolean {
  if (rule.proposer === true) return false;   // P-17 (round 33): the proposal itself suffices, no approval is asked
  if (!holds(s, author)) return false;
  const r = roleOf(s, author);
  if (!rule.approvers.some((x: string) => x === author || x === r || (x === PATH_OWNER && (ef.gate?.owners ?? []).includes(author)))) return false;
  if (kind !== "human") {
    if ((ef.gate?.tier ?? 0) >= 2 || rule.humans === true) return false;
    const over = rule.revert === true ? ef.gate?.revertedDigest : ef.gate?.deltaDigest;   // A2: a revert rule's evidence is over the reverted delta
    if (rule.evidence !== undefined && !(evidence !== undefined && evidence.kind === rule.evidence && over !== undefined && evidence.over === over)) return false;
  }
  return true;
}
/** A2: with `separateApprover` the proposer cannot approve, except the lane approving its own revert: a non-human that is both proposer and executor, under a rule with `revert: true`. */
const bars = (s: SpaceState, ef: A, rule: A, author: string, kind: string): boolean =>
  s.policy.separateApprover && author === ef.proposer && !(rule.revert === true && kind !== "human" && author === ef.executor);
/** P-17, how many approvals a claim consumes (E3): the rule's `needs`, at least 2 at tier 3; none if no approval is needed. */
function needsOf(s: SpaceState, ef: A): number {
  const rule = ruleFor(s.policy, ef);
  return rule && rule.proposer !== true ? Math.max(rule.needs ?? 1, ef.gate?.tier === 3 ? 2 : 1) : 0;
}
const secs = (t: string) => Date.UTC(+t.slice(0, 4), +t.slice(5, 7) - 1, +t.slice(8, 10), +t.slice(11, 13), +t.slice(14, 16), +t.slice(17, 19)) / 1000;
const abandonAfter = (window: number) => Math.floor((window * 101 + 98) / 99) + 2; // ⌈window·1.01/0.99⌉ + 2
const FINAL_PHASES = ["succeeded", "failed", "cancelled"];
const isFinalPhase = (p: string) => FINAL_PHASES.includes(p);

// ───────────────────────── P-15 derived ─────────────────────────
const MAX_GUARDED_FOR = 16 + 3, MAX_OPEN_APPROVALS = 16, MAX_OPEN_SESSIONS = 64, MAX_ATTEMPTS = 64, ROLE_INTERVALS = 64;   // han.ts PolicyConstants
/** P-15: a session is live while it is open and its actor holds a role. */
function live(s: SpaceState, sess: A): boolean {
  return holds(s, sess.actor);
}
/** P-15 guardStatus(state, ctx, agent): guard = the operator in the profile pinned for this entry if confirmed and the agent is not human; forced = forceGuarded on and
 *  the pinned profile not human; guards (round 32, Z1) = the agent itself if it chose guarding (the state's set) or is forced, then carried(agent), each once;
 *  guarded = guards not empty. `state` needs only `policy`, `guarded` and the tasks whose `accepted` lists the agent (a `state {agent}` reply serves). */
export function guardStatus(s: GuardInput, ctx: PolicyCtx, x: string): { guard?: ActorId; guarded: boolean; forced: boolean; guards: ActorId[] } {
  const v = (ctx.profiles as A)?.[x];
  const agent = !!v && v.actorKind !== "human";
  const forced = agent && s.policy.sessions.forceGuarded === true;
  const own: string[] = forced || (s.guarded as A)[x] === true ? [x] : [];
  const gs = [...own, ...carried(s, x).filter((y) => !own.includes(y))] as ActorId[];
  const r: { guard?: ActorId; guarded: boolean; forced: boolean; guards: ActorId[] } = { guarded: gs.length > 0, forced, guards: gs };
  if (agent && v.operatorConfirmed && v.operator) r.guard = v.operator as ActorId;
  return r;
}
const guards = (s: GuardInput, ctx: PolicyCtx, x: string): string[] => guardStatus(s, ctx, x).guards;
/** P-15: the guarded agents whose guarding x carries: the union of the guardedFor of the tasks whose `accepted` lists x, in the order the tasks were created
 *  (an assignee leaves `accepted` with its own final update or by leaving the space; a final update by an owner who isn't an assignee empties it; round 32, Z2). */
function carried(s: GuardInput, x: string): string[] {
  const out: string[] = [];
  const mine = (Object.values(s.tasks) as A[]).filter((t) => t.accepted.includes(x)).sort((p, q) => p.seq - q.seq);
  for (const t of mine) for (const y of t.guardedFor as string[]) if (!out.includes(y)) out.push(y);
  return out;
}
/** P-15, a task's guardedFor at its creation: the parent's list, then guards(author); each once (Z1). */
function taskGuardedFor(s: SpaceState, ctx: PolicyCtx, author: string, parentId: string | undefined): string[] {
  const out: string[] = parentId !== undefined ? [...((s.tasks as A)[parentId].guardedFor as string[])] : [];
  for (const x of guards(s, ctx, author)) if (!out.includes(x)) out.push(x);
  return out;
}
/** P-15, an effect's guardedFor at its proposal: the named task's list, then guards(proposer), then guards(executor); each once (Z1). */
function effectGuardedFor(s: SpaceState, ctx: PolicyCtx, proposer: string, executor: string, taskId: string | undefined): string[] {
  const out: string[] = taskId !== undefined ? [...((s.tasks as A)[taskId].guardedFor as string[])] : [];
  for (const p of [proposer, executor]) for (const x of guards(s, ctx, p)) if (!out.includes(x)) out.push(x);
  return out;
}
/** E3 (Z1): guards(proposer) then guards(executor) at this entry, each once. */
function claimGuards(s: SpaceState, ctx: PolicyCtx, ef: A): string[] {
  const out: string[] = [];
  for (const x of [...guards(s, ctx, ef.proposer), ...guards(s, ctx, ef.executor)]) if (!out.includes(x)) out.push(x);
  return out;
}
/** E3 (Z1): the agents of claimGuards the proposal never counted (neither in guardedFor nor the proposer or executor): a claim needing one is denied. */
const unboundedGuards = (s: SpaceState, ctx: PolicyCtx, ef: A): string[] =>
  claimGuards(s, ctx, ef).filter((x) => !ef.guardedFor.includes(x) && x !== ef.proposer && x !== ef.executor);
/** P-15: a proposal's bound counts its proposer and executor whether guarded or not. (A task's is on its own list: MAX_GUARDED_FOR - 2, X6.) */
const overBound = (recorded: string[], ...actors: string[]): boolean => new Set([...recorded, ...actors]).size > MAX_GUARDED_FOR;
/** P-08: whether an open approval can never count again: its author is guarded now (judged from the state and the actorKind it recorded) or its server is denied (X7). */
const approvalDead = (s: SpaceState, x: A): boolean =>
  (s.guarded as A)[x.author] === true || (s.policy.sessions.forceGuarded === true && x.actorKind !== "human") || s.policy.servers.deny.includes(serverOf(x.author));
/** P-15: whose own approval a claim needs besides any rule: the guard of each agent in guardedFor, then of each agent of claimGuards the proposal counted (the
 *  proposer or executor); each once. null: such an agent has no confirmed operator. */
function guardsOf(s: SpaceState, ctx: PolicyCtx, ef: A): (string | null)[] {
  const need: string[] = [...ef.guardedFor, ...claimGuards(s, ctx, ef).filter((x) => (x === ef.proposer || x === ef.executor) && !ef.guardedFor.includes(x))];
  const out: (string | null)[] = [];
  for (const x of need) { const v = guardStatus(s, ctx, x).guard ?? null; if (!out.includes(v)) out.push(v); }
  return out;
}
const openApproval = (a: A, time: string) => !a.consumedBy && unexpired(a.expiresAt, time);
const openApprovals = (ef: A, time: string): A[] => ef.approvals.filter((a: A) => openApproval(a, time));

/** P-15, the Limits (Z1, Z3): what a guarded author may not do. A task or session that isn't there is left to the kind's checks; an approve of a missing proposal is denied here. */
function governsOthers(s: SpaceState, ctx: PolicyCtx, e: A): boolean {
  const b = e.body as A, a = e.author as string;
  switch (e.kind) {
    case "member.set": return !(b.actor === a && b.role === "none" && Object.keys(b).length === 2);   // leaving is not governing
    case "policy.set": case "space.move": return true;
    case "effect.approve": { const ef = (s.effects as A)[b.proposal]; return !ef || !guardsOf(s, ctx, ef).includes(a); }
    case "session.input": { const se = (s.sessions as A)[b.session]; return se !== undefined && se.actor !== a; }
    case "task.update": { const t = (s.tasks as A)[b.task]; return !!t && FINAL_TASK.includes(b.state) && !t.to.includes(a); }   // a force-close
    case "task.cancel": { const t = (s.tasks as A)[b.task]; return !!t && t.author !== a; }
    default: return false;
  }
}

// ───────────────────────── P-02 read ─────────────────────────
export function canRead(state: SpaceState, reader: string, seq: Seq): boolean {
  const s = state;
  if (s.policy.visibility === "public") return true;
  const readableAt = (a: string): boolean => {
    if (READABLE.includes((s.roles as A)[a] ?? "none")) return true;
    return ((s.roleHistory as A)[a] ?? []).some((iv: A) => iv.from <= seq && (iv.to === undefined || seq < iv.to));
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
export function sessionAccess(state: SpaceState, actor: string, session: EntryId) {
  const none = { watch: false, read: false, steer: false };
  const sess = (state.sessions as A)[session];
  if (!sess || !live(state, sess)) return none;
  if (actor === sess.actor) return { watch: true, read: true, steer: true };
  const r = (state.roles as A)[actor];
  const right = (lvl: string) => lvl !== "nobody" && RANK[r] !== undefined && READABLE.includes(r) && RANK[r] >= RANK[lvl];
  const p = state.policy.sessions;
  return { watch: right(p.watch), read: right(p.read), steer: right(p.steer) };
}

// ───────────────────────── P-01 relevant actors ─────────────────────────
function candidates(s: SpaceState, e: A, time: string): A[] {
  const rule = ruleFor(s.policy, e);
  if (!rule) return [];
  return e.approvals.filter((a: A) => openApproval(a, time) && approvalCounts(s, e, rule, a.author, a.actorKind, a.evidence) && !bars(s, e, rule, a.author, a.actorKind));
}
/** P-01, the actors an entry names, which `decide` needs pinned. */
function namedActors(state: SpaceState | undefined, e: A): string[] {
  const out: string[] = [e.author];
  const add = (a: string | undefined) => { if (a !== undefined && !out.includes(a)) out.push(a); };
  if (e.kind === "member.set") add(e.body?.actor);
  if (e.kind === "task" && Array.isArray(e.body?.to)) for (const x of e.body.to) if (typeof x === "string") add(x);
  if (e.kind === "effect.propose") add(e.body?.executor);
  if (e.kind === "session.input" && state) add((state.sessions as A)[e.body?.session]?.actor);
  if (e.kind === "agent.unguard") add(e.body?.agent);
  if ((e.kind === "effect.approve" || e.kind === "effect.cancel" || e.kind === "effect.claim") && state) {
    const ef = (state.effects as A)[e.body?.proposal];
    if (ef) { add(ef.proposer); add(ef.executor); for (const x of ef.guardedFor ?? []) add(x); }
  }
  return out;
}
/** P-01, the complete pin list given the profiles pinned so far (`ctx.profiles`): the named actors; for a claim then the guards (the operators in `ctx` of the
 *  proposer and executor) and the E3 candidates in order up to and including the n-th usable by the profiles in `ctx` (n = the approvals the claim consumes, P-17): a valid key and an author not guarded at the claim (every one before it included, guarded ones too).
 *  The sequencer calls it again with what it fetched until it names nothing new: three calls at most. */
/** E3: a candidate counts if its key is valid and its author is not guarded at the claim (a guarded agent's approval never counts, P-15). */
function usable(s: SpaceState, ctx: PolicyCtx, x: A): boolean { return keyValid(ctx, x.author, x.keyId); }

export function relevantActors(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): ActorId[] {
  const e = entry as A;
  const out = namedActors(state, e);
  const add = (a: string | undefined | null) => { if (a && !out.includes(a)) out.push(a); };
  const ef = e.kind === "effect.claim" && state ? (state.effects as A)[e.body?.proposal] : undefined;
  if (ef) {
    for (const g of guardsOf(state!, ctx, ef)) add(g);
    const need = needsOf(state!, ef);
    let got = 0;
    for (const a of candidates(state!, ef, ctx.time)) { add(a.author); if (usable(state!, ctx, a) && ++got >= need) break; }
  }
  return out as ActorId[];
}

// ───────────────────────── decide ─────────────────────────
export function decide(state: SpaceState | undefined, entry: Entry, ctx: PolicyCtx): Decision {
  const e = entry as A;
  // P-16 common step 1: shape of the header fields Policy reads, and of the body
  if (!isObj(e)) return inv("entry not an object");
  const h = badHeader(e); if (h) return inv(h);
  const bb = badBody(e.kind, e.body); if (bb) return inv(bb);
  const b = e.body as A, a = e.author as string, time = ctx.time;
  // step 2: pins of the actors the entry names
  for (const x of namedActors(state, e)) {
    if (state?.profile !== undefined && x === profileActorOf(state.space) && x === a) continue; // own view is the space's state (§4.3)
    if (!(ctx.profiles as A)?.[x]) return inv("no pinned profile for " + x);
  }
  // step 3: the author's key (R-12)
  if (!authorKeyValid(state, ctx, e)) return deny("author key not valid");
  // step 4: no state yet
  if (!state) {
    if (e.kind !== "space.create") return inv("first entry must be space.create");
    return decideCreate(e, ctx);
  }
  const s = state;
  // step 5: the space exists
  if (e.kind === "space.create") return inv("space.create on an existing space");
  if (e.space !== s.space) return inv("entry names another space");
  if (e.seen > s.head) return inv("seen beyond head");
  for (const r of e.refs) if (!(r in s.entries)) return inv("ref does not resolve");
  // step 6: denied server
  const factReport = e.kind === "effect.outcome" || e.kind === "effect.resolve";
  if (!factReport && s.policy.servers.deny.includes(serverOf(a))) return deny("author on a denied server");
  // step 7: profile spaces
  if (s.profile !== undefined) {
    if (e.kind === "key.add" || e.kind === "key.revoke") return decideKey(s, e);
    if (e.kind === "member.set") return decideProfileMember(s, e, ctx);
    return deny("kind not allowed in a profile space");
  }
  // step 8: a guarded author (guards not empty, Z1) may not approve (unless it is one of the effect's guards, Z3), govern or steer others (P-15)
  if (guardStatus(s, ctx, a).guarded && governsOthers(s, ctx, e)) return deny("a guarded author may not approve, govern or steer others");
  // step 9: extension kinds
  if (isExtensionKind(e.kind)) {
    const r = roleOf(s, a);
    return READABLE.includes(r) && RANK[r] >= RANK[s.policy.extensionsMinRole] ? ok : deny("role below extensionsMinRole");
  }
  // step 10: the kind's own list
  switch (e.kind) {
    case "member.set": return decideMember(s, e, ctx);
    case "policy.set": return decidePolicySet(s, e);
    case "key.add": case "key.revoke": return deny("key entries are for profile spaces only");
    case "message": return holds(s, a) ? ok : deny("no role");
    case "task": return decideTask(s, e, ctx);
    case "task.update": {
      const t = (s.tasks as A)[b.task];
      if (!t) return inv("not a task");
      const final = FINAL_TASK.includes(b.state);
      if (!t.open && !final) return deny("task closed");   // Z2: a final update still ends carries on a closed task
      if (!holds(s, a)) return deny("no role");
      const assignee = t.to.includes(a);
      if (isOwner(s, a) && !assignee && final) return ok;
      if (assignee && final) {
        for (const ef of Object.values(s.effects) as A[]) if (ef.task === b.task && (ef.phase === "claimed" || ef.phase === "unknown")) return deny("effect running on the task");
      }
      return assignee ? ok : deny("not an assignee");
    }
    case "task.cancel": {
      const t = (s.tasks as A)[b.task];
      if (!t) return inv("not a task");
      if (!t.open) return deny("task closed");
      if (!holds(s, a)) return deny("no role");
      return t.author === a || isOwner(s, a) ? ok : deny("not the task's author or an owner");
    }
    case "effect.propose": return decidePropose(s, e, ctx);
    case "effect.approve": return decideApprove(s, e, ctx);
    case "effect.cancel": {
      const ef = (s.effects as A)[b.proposal];
      if (!ef) return inv("not a proposal");
      if (!holds(s, a)) return deny("no role");
      const may = ef.proposer === a || isOwner(s, a) || guardsOf(s, ctx, ef).includes(a)
        || (!guardStatus(s, ctx, a).guarded && ef.approvals.some((x: A) => x.author === a && (x.consumedBy || openApproval(x, ctx.time))))   // a guarded author's approval grants no cancel right (P-04)
        || (ef.phase === "open" && ef.executor === a);   // P-07a: the executor, only while the effect is open
      if (!may) return deny("not allowed to cancel");
      if (ef.phase === "open") return ok;
      if (ef.phase === "claimed" || ef.phase === "unknown") return ef.cancelRequested ? deny("cancel already requested") : ok;
      return deny("effect is final");
    }
    case "effect.claim": return decideClaim(s, e, ctx);
    case "effect.outcome": return decideOutcome(s, e, ctx);
    case "effect.resolve": return decideResolve(s, e, ctx);
    case "checkpoint":
      if (!isMemberOrOwner(s, a)) return deny("not owner/member");
      return b.through <= s.head ? ok : inv("through beyond head");
    case "redact": {
      const target = (s.entries as A)[b.target];
      if (!target) return inv("target not in the space");
      if (target.kind === "redact") return deny("cannot redact a redact");
      if (target.kind === "effect.propose") {
        const ef = (s.effects as A)[b.target];
        if (ef && !isFinalPhase(ef.phase)) return deny("plan of an unfinished effect");
      }
      return target.author === a || isOwner(s, a) ? ok : deny("not the target's author or an owner");
    }
    case "space.move": {
      if (!isOwner(s, a)) return deny("not an owner");
      for (const ef of Object.values(s.effects) as A[]) if (ef.phase === "claimed") return deny("a claim is open");
      return ok;
    }
    case "session.open": return decideSessionOpen(s, e, ctx);
    case "session.close": {
      const sess = (s.sessions as A)[b.session];
      if (!sess) return inv("no such session");
      return sess.actor === a || isOwner(s, a) ? ok : deny("not the session's actor or an owner");
    }
    case "session.input": return decideInput(s, e, ctx);
    case "agent.unguard": return decideUnguard(s, e, ctx);
    default: return inv("unknown kind");
  }
}

function decideKey(s: SpaceState, e: A): Decision {
  const b = e.body as A, a = e.author as string;
  if (a !== profileActorOf(s.space) && a !== "@:" + spaceServer(s.space)) return deny("only the actor or its server");
  if (e.kind === "key.add") return b.keyId in s.profile!.keys ? inv("keyId is single-use") : ok;
  return s.profile!.keys[b.keyId]?.status === "valid" ? ok : inv("key is not currently valid");
}

function decideProfileMember(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string, op = s.profile!.operator;
  if (!op || b.actor !== op) return deny("only the operator may be set in a profile space");
  if (b.role === "owner") {
    if (a !== op) return deny("only the operator confirms");
    return kindOf(ctx, op) === "human" ? ok : deny("operator must be human");
  }
  if (b.role === "none") return a === op || a === "@:" + spaceServer(s.space) ? ok : deny("not allowed to withdraw");
  return deny("only owner or none in a profile space");
}

function decideCreate(e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const home = spaceServer(e.space);
  if (e.seen !== 0) return inv("seen beyond head");
  if (e.refs.length) return inv("ref does not resolve");
  if (isProfileSpace(e.space)) {
    if (a !== "@:" + home) return deny("profile space is created by the server actor");
    if (!b.profile) return inv("profile space needs body.profile");
    if (b.profile.actorKind !== "human" && !("operator" in b.profile)) return inv("a non-human profile names its operator");
    if (b.profile.operator !== undefined && b.profile.operator === profileActorOf(e.space)) return inv("actor names itself as operator");
  } else if (b.profile) return inv("body.profile only in profile spaces");
  if (b.policy.servers.deny.includes(home)) return inv("policy denies its own home");
  if (!isProfileSpace(e.space) && kindOf(ctx, a) !== "human") return deny("owners are human");
  return ok;
}

function decideMember(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string, t = b.actor as string, role = b.role as string;
  const cur = roleOf(s, t);
  const granting = READABLE.includes(role);
  const aRole = roleOf(s, a);
  const owners = Object.keys(s.roles).filter((k) => (s.roles as A)[k] === "owner");
  if (cur === "owner" && role !== "owner" && owners.length === 1) return deny("would leave zero owners");
  if (granting && s.policy.servers.deny.includes(serverOf(t))) return deny("target on a denied server");
  if (role === "owner" && kindOf(ctx, t) !== "human") return deny("owners are human");
  if (granting && (cur === "none" || cur === "banned") && kindOf(ctx, t) !== "human") {
    if (s.policy.allowAgents === "none") return deny("agents not allowed");
    if (s.policy.allowAgents === "with-operator" && !(ctx.profiles as A)?.[t]?.operatorConfirmed) return deny("operator not confirmed");
  }
  if (aRole === "owner") return ok;
  if (a === t && role === "none" && READABLE.includes(aRole)) return ok;
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
  const owners = Object.keys(s.roles).filter((k) => (s.roles as A)[k] === "owner");
  if (owners.length > 0 && owners.every((k) => deny_.includes(serverOf(k)))) return deny("cannot deny every owner's server");
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
  if (!holds(s, a)) return deny("no role");
  for (const x of b.to) if (!holds(s, x)) return deny("assignee holds no role");
  let parent: A;
  if (b.parent !== undefined) {
    parent = (s.tasks as A)[b.parent];
    if (!parent) return inv("parent is not a task");
    if (!parent.open) return deny("parent closed");
    if (!parent.to.includes(a)) return deny("author not an assignee of parent");
    if (parent.depth + 1 > s.policy.maxDelegationDepth) return deny("delegation too deep");
    if (parent.budget && b.budget === undefined) return deny("child of a budgeted parent must carry a budget");
  } else if (roleOf(s, a) === "guest") return deny("guests cannot create top-level tasks");
  if (b.budget !== undefined) {
    if (!accountable(s, ctx, a)) return deny("not accountable");
    if (kindOf(ctx, a) !== "human" && !(parent && parent.budget)) return deny("agents' budgeted tasks need a budgeted parent");
    if (!s.policy.budget) return deny("the space has no budget");
    if (s.policy.budget.unit !== b.budget.unit) return deny("budget unit differs");
    const avail = parent && parent.left ? parent.left.amount : (s.budgetLeft as A).amount;
    if (b.budget.amount > avail) return deny("budget does not fit");
  }
  if (taskGuardedFor(s, ctx, a, b.parent).length > MAX_GUARDED_FOR - 2) return deny("too many guarded tasks; close some");   // P-15, X6
  return ok;
}

/** P-06 (round 33): a revert proposal names exactly the task the reverted vcs.land named, which is open and budgeted, the reverted effect having succeeded. */
function revertsTaskOf(s: SpaceState, b: A): boolean {
  const reverted: A | undefined = b.change?.revertOf !== undefined ? (s.effects as A)[b.change.revertOf] : undefined;
  if (!reverted || reverted.action !== "vcs.land" || reverted.phase !== "succeeded" || reverted.task === undefined || reverted.task !== b.task) return false;
  const t: A | undefined = (s.tasks as A)[b.task];
  return !!t && t.open && !!t.budget;
}

function decidePropose(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  if (!isMemberOrOwner(s, a)) return deny("not owner/member");
  let task: A;
  if (b.task !== undefined) {
    task = (s.tasks as A)[b.task];
    if (!task) return inv("not a task");
    if (!task.open) return deny("task closed");
    if (!task.to.includes(a) && !isOwner(s, a) && !(s.policy.budget && kindOf(ctx, a) !== "human" && revertsTaskOf(s, b))) return deny("neither an assignee nor an owner");
  }
  if (s.policy.budget && (!b.cost || b.cost.unit !== s.policy.budget.unit)) return deny("budgeted space needs a cost in its unit");
  if (!accountable(s, ctx, a)) return deny("not accountable");
  if (s.policy.budget && kindOf(ctx, a) !== "human") {
    if (!task || !task.open || !task.budget || !(task.to.includes(a) || revertsTaskOf(s, b))) return deny("agents must name an open budgeted task assigned to them");
  }
  if (overBound(effectGuardedFor(s, ctx, a, b.executor, b.task), a, b.executor)) return deny("too many guarded tasks; close some");   // P-15: the bound is kept, never exceeded
  if (b.change !== undefined && b.action !== "vcs.land") return inv("change on an action other than vcs.land");   // P-17
  if (b.change?.revertOf !== undefined) {   // A2
    const reverted = (s.effects as A)[b.change.revertOf];
    if (!reverted || reverted.action !== "vcs.land") return inv("revertOf names no vcs.land of this space");
    if (reverted.phase !== "succeeded") return deny("revertOf names a vcs.land that has not succeeded");
  }
  return ok;
}

function decideApprove(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const ef = (s.effects as A)[b.proposal];
  if (!ef) return inv("not a proposal");
  if (b.planDigest !== ef.planDigest) return inv("planDigest does not match");
  if (ef.phase !== "open" && ef.phase !== "claimed") return deny("phase does not take approvals");
  if (b.expiresAt !== undefined && !(b.expiresAt > ctx.time)) return deny("approval already expired");
  const rule = ruleFor(s.policy, ef);
  const guards = guardsOf(s, ctx, ef);
  if (!(guards.includes(a) && holds(s, a))) {   // a guard approves as a guard (P-15): no rule, no cap
    if (!rule || !approvalCounts(s, ef, rule, a, kindOf(ctx, a), b.evidence)) return deny("not an approver, or the approval could not count under the rule (P-17)");
    if (bars(s, ef, rule, a, kindOf(ctx, a))) return deny("proposer cannot approve");
    if (candidates(s, ef, ctx.time).filter((x: A) => x.author !== a && !guards.includes(x.author)).length >= MAX_OPEN_APPROVALS) return deny("too many open approvals");
  }
  return ok;
}

type E3 = { ok: true; approval?: string; coApprovals: string[]; guardApprovals: string[] } | { ok: false; code: "invalid" | "denied"; why: string };
function e3(s: SpaceState, ef: A, ctx: PolicyCtx): E3 {
  const rule = ruleFor(s.policy, ef);
  const picked: A[] = [], need = needsOf(s, ef);   // P-17: the first `need` usable candidates, by distinct authors
  if (rule && rule.proposer !== true) {   // P-17 (round 33): under a `proposer: true` rule the proposal suffices, no candidate is walked
    for (const x of candidates(s, ef, ctx.time)) {
      if (!(ctx.profiles as A)?.[x.author]) return { ok: false, code: "invalid", why: "candidate has no pinned profile" };
      if (usable(s, ctx, x) && !picked.some((y) => y.author === x.author)) { picked.push(x); if (picked.length >= need) break; }
    }
    if (picked.length < need) return { ok: false, code: "denied", why: picked.length ? "fewer usable approvals than the rule needs" : "no valid approval" };
  }
  if (unboundedGuards(s, ctx, ef).length > 0) return { ok: false, code: "denied", why: "the proposer or executor took on guarding the proposal never counted: propose again" };   // Z1
  const guardApprovals: string[] = [];
  for (const g of guardsOf(s, ctx, ef)) {
    if (g === null) return { ok: false, code: "denied", why: "a guarded agent has no confirmed operator" };
    let found: A;
    if (holds(s, g)) {
      for (const x of ef.approvals) {
        if (x.author !== g || !openApproval(x, ctx.time)) continue;
        if (!(ctx.profiles as A)?.[g]) return { ok: false, code: "invalid", why: "guard has no pinned profile" };
        if (keyValid(ctx, g, x.keyId)) { found = x; break; }
      }
    }
    if (!found) return { ok: false, code: "denied", why: "a guard has not approved" };
    guardApprovals.push(found.id);
  }
  return { ok: true, approval: picked[0]?.id, coApprovals: picked.slice(1).map((x) => x.id), guardApprovals };
}

function decideClaim(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const ef = (s.effects as A)[b.proposal];
  if (!ef) return inv("not a proposal");
  // E1
  if (a !== ef.executor || b.instance !== e.keyId) return deny("E1: not the executor, or instance is not the signing key");
  if (!isMemberOrOwner(s, a)) return deny("E1: executor holds no role");
  if (!accountable(s, ctx, a)) return deny("E1: executor not accountable");
  if (ef.expiresAt !== undefined && !(ef.expiresAt > ctx.time)) return deny("E5: proposal expired");
  // E2
  if (!isMemberOrOwner(s, ef.proposer) || !keyValid(ctx, ef.proposer, ef.proposerKey)) return deny("E2: proposer holds no role or its key is revoked");
  if (!accountable(s, ctx, ef.proposer)) return deny("E2: proposer not accountable");
  if (b.window !== s.policy.launchWindow) return deny("E5: window differs from launchWindow");
  // E3
  const r = e3(s, ef, ctx); if (!r.ok) return { allow: false, code: r.code, reason: "E3: " + r.why };
  // E4
  if (ef.cancelRequested) return deny("E4: cancel requested");
  // E5
  if (ef.phase !== "open") return deny("E5: phase is " + ef.phase);
  if (ef.task !== undefined && !(s.tasks as A)[ef.task]?.open) return deny("E5: task closed");
  if (b.attempt !== 1 + ef.claims.length) return deny("E5: wrong attempt");
  if (ef.claims.length >= MAX_ATTEMPTS) return deny("E5: attempt limit: propose again");
  // E6
  if (s.policy.budget) {
    if (!ef.cost || ef.cost.unit !== s.policy.budget.unit) return deny("E6: no cost in the space's unit");
    const task = ef.task !== undefined ? (s.tasks as A)[ef.task] : undefined;
    const charged = task && task.budget ? task : undefined;
    const left = charged ? charged.left.amount : (s.budgetLeft as A).amount;
    if (ef.cost.amount > left) return deny("E6: cost does not fit");
    if (!charged && kindOf(ctx, ef.proposer) !== "human") return deny("E6: agent proposer cannot draw on the space budget");
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
  if (ef.phase !== "claimed" || ef.claims[ef.claims.length - 1] !== claim || claim.outcome !== undefined) return deny("not the latest open claim");
  if (!validPair(b.result, b.started)) return inv("(result, started) pair outside the table");
  if (b.cost !== undefined && b.started !== "yes") return inv("cost only with started yes");
  if (!costWithin(s, claim, b.cost)) return inv("cost exceeds the reservation");
  if (a === ef.executor && e.keyId === claim.instance) return ok;
  if (isOwner(s, a) && b.result === "unknown" && b.started === "unknown" && secs(ctx.time) >= secs(claim.time) + abandonAfter(claim.window)) return ok;
  return deny("not the claim's instance, and not a timely abandon by an owner");
}
function decideResolve(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const ef = (s.effects as A)[b.proposal];
  if (!ef) return inv("not a proposal");
  if (ef.phase !== "unknown") return deny("effect not unknown");
  if (a !== ef.executor && !isOwner(s, a)) return deny("not the executor or an owner");
  if (b.cost !== undefined && b.result === "failed") return inv("cost with failed");
  const claim = ef.claims[ef.claims.length - 1];
  if (!costWithin(s, claim, b.cost)) return inv("cost exceeds the reservation");
  return ok;
}

function decideSessionOpen(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  if (!holds(s, a)) return deny("no role");
  if (!accountable(s, ctx, a)) return deny("not accountable");
  if (!s.policy.sessions.modes.includes(b.steerMode)) return deny("steerMode not allowed");
  if (b.steerMode !== "auto" && kindOf(ctx, a) !== "human") {
    if (!holds(s, guardStatus(s, ctx, a).guard)) return deny("guarded session needs a confirmed operator with a role");
  }
  if (b.task !== undefined) {
    const t = (s.tasks as A)[b.task];
    if (!t) return inv("not a task");
    if (!t.to.includes(a) && !isOwner(s, a)) return deny("not assigned to the task");
    if (!t.open) return deny("task closed");
  }
  const open = (Object.values(s.sessions) as A[]).filter((x) => x.actor === a).length;
  if (open >= MAX_OPEN_SESSIONS) return deny("too many open sessions");
  return ok;
}

function decideInput(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string;
  const sess = (s.sessions as A)[b.session];
  if (!sess) return inv("no such session");
  if (!live(s, sess)) return deny("session not live");
  if (!accountable(s, ctx, a)) return deny("not accountable");
  if (!sessionAccess(s, a as ActorId, b.session).steer) return deny("no steer right");
  return ok;
}

function decideUnguard(s: SpaceState, e: A, ctx: PolicyCtx): Decision {
  const b = e.body as A, a = e.author as string, x = b.agent as string;
  if (!(s.guarded as A)[x] || guardStatus(s, ctx, x).guard !== a) return deny("only the guard of an agent that chose guarding unguards it");
  return holds(s, a) ? ok : deny("guard holds no role");
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
/** The phase after an attempt ends without success (§6.4): `cancelled` if a cancel was requested, else `open` again (E3 decides at claim time). */
const reopen = (ef: A): string => (ef.cancelRequested ? "cancelled" : "open");
/** F3 (a): a consumed approval leaves the state once the claim that consumed it has settled. */
function pruneSettled(ef: A) {
  ef.approvals = ef.approvals.filter((x: A) => !(x.consumedBy && ef.claims.find((c: A) => c.id === x.consumedBy)?.settled));
}
/** P-08 (K4): whenever an entry touches an effect, its unconsumed approvals that have expired leave the state. */
function dropOpenApprovals(s: SpaceState, actor: string) {
  for (const ef of Object.values(s.effects) as A[]) ef.approvals = ef.approvals.filter((x: A) => x.author !== actor || x.consumedBy);
}
function dropExpired(ef: A, time: string) {
  ef.approvals = ef.approvals.filter((x: A) => x.consumedBy || unexpired(x.expiresAt, time));
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
      entries: {}, sessions: {}, guarded: {},
    } as SpaceState;
    if (isProfileSpace(e.space)) {
      s.profile = { actorKind: b.profile.actorKind, operatorConfirmed: false, keys: { [b.profile.key.keyId]: { publicKey: b.profile.key.publicKey, status: "valid" } } };
      if (b.profile.operator !== undefined) s.profile.operator = b.profile.operator;
      (s.roles as A)[profileActorOf(e.space)] = "owner";
      (s.roles as A)["@:" + spaceServer(e.space)] = "owner";
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
      if (!was && now) { hist.push({ from: seq }); while (hist.length > ROLE_INTERVALS) hist.shift(); }
      else if (was && !now) { const last = hist[hist.length - 1]; if (last && last.to === undefined) last.to = seq; }
      if (!now) {
        dropOpenApprovals(s, t);   // P-08: they could never count again
        for (const tk of Object.values(s.tasks) as A[]) if (tk.accepted.includes(t)) tk.accepted = tk.accepted.filter((x: string) => x !== t);   // Z2: leaving ends every carry
      }
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
      // P-08: forcing on drops the open approvals of the authors it guards, judged from the recorded kind
      for (const ef of Object.values(s.effects) as A[]) ef.approvals = ef.approvals.filter((x: A) => x.consumedBy || !approvalDead(s, x));
      break;
    }
    case "key.add": s.profile!.keys[b.keyId] = { publicKey: b.publicKey, status: "valid" }; break;
    case "key.revoke": s.profile!.keys[b.keyId].status = "revoked"; break;
    case "task": {
      const parent = b.parent !== undefined ? (s.tasks as A)[b.parent] : undefined;
      const t: A = { author: a, to: [...b.to], open: true, state: "pending", cancelRequested: false, depth: parent ? parent.depth + 1 : 0, seq,
        accepted: b.to.includes(a) ? [a] : [] };   // P-15: an author assigning itself accepts at once
      if (b.parent !== undefined) t.parent = b.parent;
      if (b.budget) {
        t.budget = { unit: b.budget.unit, amount: b.budget.amount };
        t.left = { unit: b.budget.unit, amount: b.budget.amount };
        if (parent && parent.left) parent.left.amount -= b.budget.amount; else s.budgetLeft!.amount -= b.budget.amount;
      }
      t.guardedFor = taskGuardedFor(s, ctx, a, b.parent);   // P-15: fixed now, never shrinks
      (s.tasks as A)[id] = t;
      break;
    }
    case "task.update": {
      const t = (s.tasks as A)[b.task];
      const wasOpen = t.open;
      if (wasOpen) t.state = b.state;   // a closed task only loses acceptances (Z2)
      if (b.state === "accepted" && !t.accepted.includes(a)) t.accepted.push(a);   // P-15, X4: only an explicit accept accepts
      if (FINAL_TASK.includes(b.state)) t.accepted = t.to.includes(a) ? t.accepted.filter((x: string) => x !== a) : [];   // X1: an assignee's own final update ends what it carries; an owner's force-close, all
      if (FINAL_TASK.includes(b.state) && wasOpen) {
        t.open = false;
        if (t.left) { const amt = t.left.amount; t.left.amount = 0; refundFrom(s, t.parent, amt, true); }
      }
      break;
    }
    case "task.cancel": {
      const t = (s.tasks as A)[b.task];
      t.cancelRequested = true;
      break;
    }
    case "effect.propose": {
      const ef: A = {
        proposer: a, proposerKey: e.keyId, executor: b.executor, action: b.action, planDigest: b.planDigest, seq, repeatable: b.repeatable,
        phase: "open", cancelRequested: false, approvals: [], claims: [],
      };
      if (b.cost) ef.cost = { unit: b.cost.unit, amount: b.cost.amount };
      if (b.task !== undefined) ef.task = b.task;
      if (b.expiresAt !== undefined) ef.expiresAt = b.expiresAt;
      ef.guardedFor = effectGuardedFor(s, ctx, a, b.executor, b.task);   // P-15
      const gate = deriveGate(s, ctx, a, b);   // P-17: fixed now, like guardedFor
      if (gate) ef.gate = gate;
      (s.effects as A)[id] = ef;
      break;
    }
    case "effect.approve": {
      const ef = (s.effects as A)[b.proposal];
      const guards = guardsOf(s, ctx, ef);
      const keep: A[] = [...candidates(s, ef, time), ...ef.approvals.filter((x: A) => guards.includes(x.author))];
      // supersedes the author's open ones, and drops the open ones that don't count unless a guard's (P-08)
      ef.approvals = ef.approvals.filter((x: A) => x.consumedBy || (x.author !== a && keep.includes(x)));
      const ap: A = { id, author: a, keyId: e.keyId, actorKind: kindOf(ctx, a) };
      if (b.expiresAt !== undefined) ap.expiresAt = b.expiresAt;
      if (b.evidence !== undefined) ap.evidence = { kind: b.evidence.kind, over: b.evidence.over };
      ef.approvals.push(ap);
      dropExpired(ef, time);
      break;
    }
    case "effect.cancel": {
      const ef = (s.effects as A)[b.proposal];
      if (ef.phase === "open") ef.phase = "cancelled";
      ef.cancelRequested = true;  // §6.4: set in every phase that accepts a cancel
      dropExpired(ef, time);
      break;
    }
    case "effect.claim": {
      const ef = (s.effects as A)[b.proposal];
      const r = e3(s, ef, ctx);
      const claim: A = { id, attempt: b.attempt, instance: b.instance, time, window: b.window, settled: false };
      if (r.ok) {
        if (r.approval) claim.approval = r.approval;
        if (r.coApprovals.length) claim.coApprovals = r.coApprovals;
        if (r.guardApprovals.length) claim.guardApprovals = r.guardApprovals;
        const used = new Set([r.approval, ...r.coApprovals, ...r.guardApprovals]);
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
      dropExpired(ef, time);
      break;
    }
    case "effect.outcome": {
      const { ef, claim } = findClaim(s, b.claim)!;
      const own = a === ef.executor && e.keyId === claim.instance;
      claim.outcome = b.result; claim.started = b.started;
      if (!own) { ef.phase = "unknown"; break; } // abandon: always unknown, reservation stays charged
      const giveBack = () => {
        const returned = ef.approvals.filter((x: A) => x.consumedBy === claim.id);
        for (const x of returned) delete x.consumedBy;
        // a returned approval stays superseded if its author approved again meanwhile (A5): one open approval per author
        ef.approvals = ef.approvals.filter((x: A) => !(returned.includes(x) && (ef.approvals.some((y: A) => y !== x && y.author === x.author && !y.consumedBy)
          || !holds(s, x.author) || approvalDead(s, x))));   // or its author lost its role / is guarded or forced meanwhile (P-08)
      };
      const settle = (cost: A | undefined) => {
        if (!claim.reserved) { claim.settled = true; return; }
        const spent = cost ? cost.amount : claim.reserved.amount;
        refundClaim(s, claim, claim.reserved.amount - spent);
        claim.settled = true;
      };
      if (b.started === "no") {
        giveBack();
        if (claim.reserved) refundClaim(s, claim, claim.reserved.amount);
        claim.settled = true;  // D14: a claim without a reservation is settled by the same events
        ef.phase = reopen(ef);
      } else if (b.started === "yes" && b.result !== "unknown") {
        settle(b.cost);
        if (b.result === "succeeded") ef.phase = "succeeded";
        else ef.phase = ef.repeatable && !ef.cancelRequested ? "open" : "failed";
      } else {
        // the executor's own unknown
        if (ef.repeatable && !ef.cancelRequested) { claim.settled = true; ef.phase = "open"; }
        else ef.phase = "unknown";
      }
      pruneSettled(ef);
      dropExpired(ef, time);
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
        ef.approvals = ef.approvals.filter((x: A) => x.consumedBy);
        ef.phase = reopen(ef);
      }
      pruneSettled(ef);
      dropExpired(ef, time);
      break;
    }
    case "space.move": s.home = b.to; break;
    case "session.open": {
      const sess: A = { actor: a, opened: time, steerMode: b.steerMode, steers: 0 };
      if (b.task !== undefined) sess.task = b.task;
      (s.sessions as A)[id] = sess;
      if (b.steerMode === "guarded" && kindOf(ctx, a) !== "human") {
        (s.guarded as A)[a] = true;   // sticky: only its guard unguards it
        dropOpenApprovals(s, a);      // P-08: a guarded author's approval never counts
      }
      break;
    }
    case "session.close": {
      delete (s.sessions as A)[b.session];
      break;
    }
    case "session.input": {
      const sess = (s.sessions as A)[b.session];
      sess.steers += 1;
      break;
    }
    case "agent.unguard": delete (s.guarded as A)[b.agent]; break;
    default: break;
  }
  return s;
}

/** ProfileView of a profile space's state (§4.3). */
export function profileViewOf(actor: string, seq: number, s: SpaceState): import("./han.js").ProfileView {
  const p = s.profile!;
  const v: any = { actor, seq, actorKind: p.actorKind, operatorConfirmed: p.operatorConfirmed, keys: JSON.parse(JSON.stringify(p.keys)) };
  if (p.operator) v.operator = p.operator;
  return v;
}
