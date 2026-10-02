// Node twin of std/crypto for the differential harness, on `node:crypto`. Random functions can
// only be checked for their contracts (length, range), not their values.

import * as nc from "node:crypto";

export class CryptoError extends Error {}

const digest = (alg: string, data: number[]): number[] => [
  ...nc.createHash(alg).update(new Uint8Array(data)).digest(),
];
const hmac = (alg: string, key: number[], data: number[]): number[] => [
  ...nc.createHmac(alg, new Uint8Array(key)).update(new Uint8Array(data)).digest(),
];

export const sha256 = (data: number[]): number[] => digest("sha256", data);
export const sha1 = (data: number[]): number[] => digest("sha1", data);
export const sha256Hex = (s: string): string => nc.createHash("sha256").update(s).digest("hex");
export const sha1Hex = (s: string): string => nc.createHash("sha1").update(s).digest("hex");
export const hmacSha256 = (key: number[], data: number[]): number[] => hmac("sha256", key, data);
export const hmacSha1 = (key: number[], data: number[]): number[] => hmac("sha1", key, data);
export const randomBytes = (n: number): number[] => [...nc.randomBytes(n)];

// Node's own range check and message, thrown as the `CryptoError` Velt throws.
export function randomInt(min: number, max: number): number {
  try {
    return nc.randomInt(min, max);
  } catch (e) {
    throw new CryptoError((e as Error).message);
  }
}

export function timingSafeEqual(a: number[], b: number[]): boolean {
  return a.length === b.length && nc.timingSafeEqual(new Uint8Array(a), new Uint8Array(b));
}
