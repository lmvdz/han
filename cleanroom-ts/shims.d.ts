declare module "node:crypto" {
  export function createHash(alg: string): { update(d: string | Uint8Array): any; digest(enc: "hex"): string };
}
declare module "node:fs" {
  export function readFileSync(p: string, enc: "utf8"): string;
  export function readdirSync(p: string): string[];
}
declare module "node:path" { export function join(...p: string[]): string; }
declare const process: { argv: string[]; exit(n: number): never };
declare const console: { log(...a: any[]): void; error(...a: any[]): void };
declare function structuredClone<T>(x: T): T;
declare class TextEncoder { encode(s: string): Uint8Array }
