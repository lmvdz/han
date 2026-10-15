// Clean-room HAN v1: grammar (§2), I-JSON strict parsing, JCS (RFC 8785), entry id, content schemas (§5).
import { createHash } from "node:crypto";

// ───────────── grammar ─────────────
const SERVER = String.raw`[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*`;
const SERVER_LEN = String.raw`(?=[a-z0-9.-]{1,253}$)`;   // a ServerName is at most 253 characters (also inside ActorId and SpaceId)
const reServer = new RegExp(`^${SERVER_LEN}${SERVER}$`);
const reActor = new RegExp(`^@[a-z0-9._-]{0,64}:${SERVER_LEN}${SERVER}$`);
const reSpaceNormal = new RegExp(`^([a-z0-9]{1,26}):${SERVER_LEN}${SERVER}$`);
const reSpaceProfile = new RegExp(`^profile:(@[a-z0-9._-]{0,64}:${SERVER_LEN}${SERVER})$`);
const reHex = /^[0-9a-f]{64}$/;
const reNonce = /^[a-z2-7]{26}$/;
const reKeyId = /^[A-Za-z0-9_-]{1,64}$/;
const reAction = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const reUnit = /^[A-Za-z0-9_-]{1,32}$/;
const reB64 = /^[A-Za-z0-9_-]+$/;
const reTimeParts = /^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})Z$/;
const reMediaHalf = /^[a-z0-9][a-z0-9!#$&^_.+-]{0,63}$/;
const rePrefix = /^[a-z][a-z0-9-]{0,31}$/;
const reExtName = /^[a-z0-9][a-z0-9._-]{0,63}$/;
export const CORE_PREFIXES = ["space", "member", "policy", "key", "message", "task", "effect", "checkpoint", "redact", "session", "agent"];
export const CORE_KINDS = ["space.create", "member.set", "policy.set", "key.add", "key.revoke", "message", "task", "task.update", "task.cancel",
  "effect.propose", "effect.approve", "effect.claim", "effect.cancel", "effect.outcome", "effect.resolve", "checkpoint", "redact",
  "space.move", "session.open", "session.close", "session.input", "agent.unguard"];

export const isServerName = (s: unknown): s is string => typeof s === "string" && reServer.test(s);
export const isActorId = (s: unknown): s is string => typeof s === "string" && reActor.test(s);
export const isHex = (s: unknown): s is string => typeof s === "string" && reHex.test(s);
export const isNonce = (s: unknown): s is string => typeof s === "string" && reNonce.test(s);
export const isKeyId = (s: unknown): s is string => typeof s === "string" && reKeyId.test(s);
export const isAction = (s: unknown, star = false): s is string => typeof s === "string" && ((star && s === "*") || reAction.test(s));
export const isUnit = (s: unknown): s is string => typeof s === "string" && reUnit.test(s);
/** D4: base64url is unpadded, URL-safe and canonical (the unused trailing bits are zero): it re-encodes to itself. */
export const isCanonB64u = (s: unknown): s is string => typeof s === "string" && s.length > 0 && reB64.test(s) && Buffer.from(s, "base64url").toString("base64url") === s;
export const isSig = (s: unknown): s is string => typeof s === "string" && /^[A-Za-z0-9_-]{86}$/.test(s) && isCanonB64u(s);
export const isPublicKey = (s: unknown): s is string => typeof s === "string" && s.length >= 1 && s.length <= 128 && reB64.test(s);   // Policy layer: vectors use dummy keys; the Log checks canonical form (isCanonB64u)
export const isInt = (n: unknown): n is number => typeof n === "number" && Number.isSafeInteger(n) && n >= 0;
export function isSpaceId(s: unknown): s is string {
  if (typeof s !== "string") return false;
  const m = reSpaceNormal.exec(s);
  if (m) return m[1] !== "profile";
  return reSpaceProfile.test(s);
}
export const isProfileSpace = (s: string) => reSpaceProfile.test(s);
export const profileActorOf = (s: string): string => reSpaceProfile.exec(s)![1];
export const serverOf = (actor: string): string => actor.slice(actor.indexOf(":") + 1);
export const spaceServer = (space: string): string => isProfileSpace(space) ? serverOf(profileActorOf(space)) : space.slice(space.indexOf(":") + 1);
export function isTime(s: unknown): s is string {
  if (typeof s !== "string") return false;
  const m = reTimeParts.exec(s);
  if (!m) return false;
  const [y, mo, d, h, mi, se] = m.slice(1).map(Number);
  if (y < 1 || mo < 1 || mo > 12 || h > 23 || mi > 59 || se > 59) return false;
  const leap = (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0;
  const dim = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][mo - 1];
  return d >= 1 && d <= dim;
}
export function isExtensionKind(k: unknown): boolean {
  if (typeof k !== "string") return false;
  const i = k.indexOf(".");
  if (i < 0) return false;
  const p = k.slice(0, i), n = k.slice(i + 1);
  return rePrefix.test(p) && !CORE_PREFIXES.includes(p) && reExtName.test(n);
}
export const isKind = (k: unknown) => typeof k === "string" && (CORE_KINDS.includes(k) || isExtensionKind(k));
export const utf8len = (s: string) => new TextEncoder().encode(s).length;
export function isMediaType(s: unknown): boolean {
  if (typeof s !== "string") return false;
  const p = s.split("/");
  return p.length === 2 && reMediaHalf.test(p[0]) && reMediaHalf.test(p[1]);
}

// ───────────── JCS (RFC 8785) ─────────────
export function jcs(v: unknown): string {
  if (v === null) return "null";
  switch (typeof v) {
    case "boolean": return v ? "true" : "false";
    case "number":
      if (!Number.isFinite(v)) throw new Error("non-finite");
      return Object.is(v, -0) ? "0" : String(v);
    case "string": return JSON.stringify(v);
    case "object": {
      if (Array.isArray(v)) return "[" + v.map(jcs).join(",") + "]";
      const o = v as Record<string, unknown>;
      const keys = Object.keys(o).filter((k) => o[k] !== undefined).sort(); // default sort = UTF-16 code units
      return "{" + keys.map((k) => JSON.stringify(k) + ":" + jcs(o[k])).join(",") + "}";
    }
    default: throw new Error("not JSON");
  }
}
export const sha256 = (s: string): string => createHash("sha256").update(s).digest("hex");
export function entryId(entry: Record<string, unknown>): string {
  const { sig, ...rest } = entry;
  return sha256("han-entry-v1\n" + jcs(rest));
}
export const contentHashOf = (content: unknown): string => sha256(jcs(content));

// ───────────── I-JSON strict parse ─────────────
export class JsonError extends Error {}
/** `big`: when given, an integer of 2^53 or more does not fail the parse; it becomes a bigint and `big.found` is set (the caller answers it, §3 frames). */
export function parseStrict(text: string, big?: { found: boolean }): unknown {
  let i = 0;
  const ws = () => { while (i < text.length && " \t\n\r".includes(text[i])) i++; };
  const fail = (m: string): never => { throw new JsonError(m + " at " + i); };
  const str = (): string => {
    if (text[i] !== '"') fail("string expected");
    i++;
    let out = "";
    for (;;) {
      if (i >= text.length) fail("unterminated string");
      const c = text[i++];
      if (c === '"') break;
      if (c === "\\") {
        const e = text[i++];
        const map: Record<string, string> = { '"': '"', "\\": "\\", "/": "/", b: "\b", f: "\f", n: "\n", r: "\r", t: "\t" };
        if (e in map) out += map[e];
        else if (e === "u") {
          const h = text.slice(i, i + 4);
          if (!/^[0-9a-fA-F]{4}$/.test(h)) fail("bad escape");
          i += 4; out += String.fromCharCode(parseInt(h, 16));
        } else fail("bad escape");
      } else {
        if (c.charCodeAt(0) < 0x20) fail("control char");
        out += c;
      }
    }
    // lone surrogates are invalid
    for (let k = 0; k < out.length; k++) {
      const cc = out.charCodeAt(k);
      if (cc >= 0xd800 && cc <= 0xdbff) { const n = out.charCodeAt(k + 1); if (n >= 0xdc00 && n <= 0xdfff) k++; else fail("lone surrogate"); }
      else if (cc >= 0xdc00 && cc <= 0xdfff) fail("lone surrogate");
    }
    return out;
  };
  const val = (): unknown => {
    ws();
    const c = text[i];
    if (c === "{") {
      i++; const o: Record<string, unknown> = {}; const seen = new Set<string>();
      ws(); if (text[i] === "}") { i++; return o; }
      for (;;) {
        ws(); const k = str(); if (seen.has(k)) fail("duplicate key"); seen.add(k);
        ws(); if (text[i++] !== ":") fail(": expected");
        Object.defineProperty(o, k, { value: val(), enumerable: true, writable: true, configurable: true }); ws();
        const d = text[i++]; if (d === "}") break; if (d !== ",") fail(", or } expected");
      }
      return o;
    }
    if (c === "[") {
      i++; const a: unknown[] = []; ws(); if (text[i] === "]") { i++; return a; }
      for (;;) { a.push(val()); ws(); const d = text[i++]; if (d === "]") break; if (d !== ",") fail(", or ] expected"); }
      return a;
    }
    if (c === '"') return str();
    if (text.startsWith("true", i)) { i += 4; return true; }
    if (text.startsWith("false", i)) { i += 5; return false; }
    if (text.startsWith("null", i)) { i += 4; return null; }
    const m = /^-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?/.exec(text.slice(i));
    if (!m) return fail("value expected");
    // integers only: no fraction, no exponent, no leading zeros, no -0; a negative integer parses (G-07 (1a)) and every position that takes a count refuses it (isInt)
    if (!/^(0|-?[1-9][0-9]*)$/.test(m[0])) fail("non-integer number form");
    const n = Number(m[0]);
    if (!Number.isSafeInteger(n)) { if (!big) fail("number out of range"); big!.found = true; i += m[0].length; return BigInt(m[0]); }
    i += m[0].length; return n;
  };
  const v = val(); ws(); if (i !== text.length) fail("trailing data");
  return v;
}

// ───────────── content schemas (§5, han.ts Contents) ─────────────
const isObj = (x: unknown): x is Record<string, any> => typeof x === "object" && x !== null && !Array.isArray(x);
function closed(o: any, req: string[], opt: string[]): string | null {
  if (!isObj(o)) return "not an object";
  for (const k of req) if (!(k in o)) return "missing " + k;
  for (const k of Object.keys(o)) if (!req.includes(k) && !opt.includes(k)) return "unknown field " + k;
  return null;
}
const strMax = (s: unknown, n: number) => typeof s === "string" && utf8len(s) <= n;
export function validBlobRef(b: any): string | null {
  const e = closed(b, ["hash", "size", "type"], ["uri"]); if (e) return "blobref: " + e;
  if (!isHex(b.hash)) return "blobref.hash";
  if (!isInt(b.size) || b.size > 4294967296) return "blobref.size";
  if (!isMediaType(b.type)) return "blobref.type";
  if ("uri" in b && !(typeof b.uri === "string" && utf8len(b.uri) <= 1024)) return "blobref.uri";
  return null;
}
function blobs(a: any): string | null {
  if (!Array.isArray(a) || a.length > 64) return "blobs";
  for (const b of a) { const e = validBlobRef(b); if (e) return e; }
  return null;
}
function links(a: any): string | null {
  if (!Array.isArray(a) || a.length > 64) return "links";
  for (const l of a) {
    const e = closed(l, ["space", "seq", "id"], []); if (e) return "link: " + e;
    if (!isSpaceId(l.space) || !isInt(l.seq) || !isHex(l.id)) return "link fields";
  }
  return null;
}
export function jsonInts(v: any, depth = 0): boolean {
  if (v === null || typeof v === "boolean" || typeof v === "string") return true;
  if (typeof v === "number") return isInt(v);
  if (depth >= 32) return false;   // at most 32 nested arrays and objects, the outermost counted
  if (Array.isArray(v)) return v.every((x) => jsonInts(x, depth + 1));
  if (isObj(v)) return Object.values(v).every((x) => jsonInts(x, depth + 1));
  return false;
}
export function validPlan(p: any): string | null {
  const e = closed(p, ["action", "target"], ["args", "argsBlob"]); if (e) return "plan: " + e;
  if (!isAction(p.action)) return "plan.action";
  if (!strMax(p.target, 1024)) return "plan.target";
  if ("args" in p && !jsonInts(p.args)) return "plan.args";
  if ("argsBlob" in p) { const e2 = validBlobRef(p.argsBlob); if (e2) return e2; }
  return null;
}
/** Returns null if content is acceptable for the kind. content undefined/null means "no content sent". */
export function validateContent(kind: string, content: any, body?: any): string | null {
  const none = content === undefined || content === null;
  const need = (req: string[], opt: string[], fn: (c: any) => string | null): string | null => {
    if (none) return req.length ? "content required" : null;
    const e = closed(content, req, opt); if (e) return e;
    if (utf8len(jcs(content)) > 65536) return "content too large";
    return fn(content);
  };
  switch (kind) {
    case "space.create": return need(["name"], [], (c) => strMax(c.name, 256) ? null : "name");
    case "message": return need(["text"], ["blobs", "links"], (c) => !strMax(c.text, 32768) ? "text" : ("blobs" in c && blobs(c.blobs)) || ("links" in c && links(c.links)) || null);
    case "task": return need(["ask"], ["blobs", "links"], (c) => !strMax(c.ask, 32768) ? "ask" : ("blobs" in c && blobs(c.blobs)) || ("links" in c && links(c.links)) || null);
    case "task.update": return need([], ["text", "blobs"], (c) => ("text" in c && !strMax(c.text, 32768)) ? "text" : ("blobs" in c && blobs(c.blobs)) || null);
    case "effect.propose": return need(["plan"], [], (c) => {
      const e = validPlan(c.plan); if (e) return e;
      if (body && c.plan.action !== body.action) return "plan.action != action";
      if (body && sha256(jcs(c.plan)) !== body.planDigest) return "planDigest mismatch";
      return null;
    });
    case "effect.approve": return need([], ["note"], (c) => ("note" in c && !strMax(c.note, 4096)) ? "note" : null);
    case "effect.cancel": return need([], ["reason", "evidence"], (c) => ("reason" in c && !strMax(c.reason, 1024)) ? "reason" : ("evidence" in c && validBlobRef(c.evidence)) || null);
    case "effect.outcome": return need([], ["evidence"], (c) => ("evidence" in c && validBlobRef(c.evidence)) || null);
    case "effect.resolve": return need(["evidence"], [], (c) => validBlobRef(c.evidence));
    case "checkpoint": return need(["blobs"], [], (c) => blobs(c.blobs));
    case "redact": return need([], ["reason"], (c) => ("reason" in c && !strMax(c.reason, 1024)) ? "reason" : null);
    case "session.open": return need([], ["title"], (c) => ("title" in c && !strMax(c.title, 256)) ? "title" : null);
    case "session.input": return need(["text"], ["blobs"], (c) => !strMax(c.text, 32768) ? "text" : ("blobs" in c && blobs(c.blobs)) || null);
    case "member.set": case "policy.set": case "key.add": case "key.revoke": case "task.cancel": case "effect.claim":
    case "space.move": case "session.close": case "agent.unguard":
      return none ? null : "content not allowed";
    default: // extension: opaque Json with integer numbers
      if (none) return null;
      return jsonInts(content) && utf8len(jcs(content)) <= 65536 ? null : "extension content";
  }
}
