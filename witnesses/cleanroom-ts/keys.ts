// Ed25519 over Node's built-in crypto: raw 32-byte public keys and 64-byte signatures, base64url without padding (§2).
import { createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes, sign, verify, type KeyObject } from "node:crypto";
import { jcs, sha256, entryId } from "./wire.js";

export const b64u = (b: Uint8Array): string => Buffer.from(b).toString("base64url");
export const unb64u = (s: string): Buffer => Buffer.from(s, "base64url");

export interface KeyPair { keyId: string; publicKey: string; priv: KeyObject }
export function newKey(keyId: string): KeyPair {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  return { keyId, publicKey: String(publicKey.export({ format: "jwk" }).x), priv: privateKey };
}
export function exportPrivate(k: KeyPair): string { return String(k.priv.export({ format: "jwk" }).d); }
export function importKey(keyId: string, publicKey: string, d: string): KeyPair {
  return { keyId, publicKey, priv: createPrivateKey({ key: { kty: "OKP", crv: "Ed25519", x: publicKey, d }, format: "jwk" }) };
}
export function pubKeyObject(publicKey: string): KeyObject | null {
  try {
    if (unb64u(publicKey).length !== 32) return null;
    return createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: publicKey }, format: "jwk" });
  } catch { return null; }
}
export const signBytes = (k: KeyPair, msg: string): string => b64u(sign(null, Buffer.from(msg, "utf8"), k.priv));
export function verifyBytes(publicKey: string, msg: string, sig: string): boolean {
  const pk = pubKeyObject(publicKey); if (!pk) return false;
  try {
    const raw = unb64u(sig);
    if (raw.length !== 64 || b64u(raw) !== sig) return false;
    return verify(null, Buffer.from(msg, "utf8"), pk, raw);
  } catch { return false; }
}

// ── protocol messages (§2, §3, §7.3) ──
export const sigMessage = (id: string) => "han-sig-v1\n" + id;
export const helloMessage = (challenge: string, dialed: string) => "han-hello-v1\n" + challenge + "\n" + dialed;
export const acceptMessage = (space: string, headReceiptHash: string) => "han-accept-move-v1\n" + space + "\n" + headReceiptHash;
export function receiptHash(r: Record<string, unknown>): string {
  const { homeSig, ...rest } = r;
  return sha256("han-receipt-v1\n" + jcs(rest));
}
export const homeSigMessage = (hash: string) => "han-home-sig-v1\n" + hash;

const B32 = "abcdefghijklmnopqrstuvwxyz234567";
export function newNonce(): string {
  const b = randomBytes(26); let s = "";
  for (let i = 0; i < 26; i++) s += B32[b[i] & 31];
  return s;
}
export const timeString = (ms: number): string => new Date(Math.floor(ms / 1000) * 1000).toISOString().replace(".000Z", "Z");
export const parseTimeMs = (t: string): number => Date.parse(t);

/** Build and sign an entry. `content` (if any) sets contentHash. */
export function makeEntry(key: KeyPair, f: { space: string; kind: string; author: string; body: unknown; content?: unknown; refs?: string[]; seen?: number; time?: string }): { entry: any; content?: any; id: string } {
  const e: any = {
    v: 1, space: f.space, kind: f.kind, author: f.author, keyId: key.keyId, body: f.body,
    contentHash: f.content === undefined ? "" : sha256(jcs(f.content)), refs: f.refs ?? [], seen: f.seen ?? 0, nonce: newNonce(),
    time: f.time ?? timeString(Date.now()),
  };
  const id = entryId(e);
  e.sig = signBytes(key, sigMessage(id));
  return f.content === undefined ? { entry: e, id } : { entry: e, content: f.content, id };
}
