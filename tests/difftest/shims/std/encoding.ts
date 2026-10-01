// Node twin of std/encoding for the differential harness, on `Buffer` and `TextDecoder`. The
// strict checks that Node's decoders skip (Base64 alphabet and padding, hex digits, UTF-8
// validity) are written out from the std/encoding docs, independently of its code.

export class EncodingError extends Error {}

const STD = /^[A-Za-z0-9+/]*$/;
const URL_SAFE = /^[A-Za-z0-9_-]*$/;

function checked64(s: string, alphabet: RegExp): string {
  let n = s.length;
  let pads = 0;
  while (n > 0 && pads < 2 && s[n - 1] === "=") {
    n--;
    pads++;
  }
  const body = s.slice(0, n);
  if (!alphabet.test(body)) {
    const bad = [...body].findIndex((c) => !alphabet.test(c));
    throw new EncodingError(`invalid base64 character at offset ${bad}`);
  }
  if (n % 4 === 1 || (pads > 0 && (n + pads) % 4 !== 0)) {
    throw new EncodingError("invalid base64: bad length or padding");
  }
  return body;
}

const bytes = (b: Uint8Array): number[] => [...b];

export function base64Encode(data: number[]): string {
  return Buffer.from(data).toString("base64");
}
export function base64Decode(s: string): number[] {
  return bytes(Buffer.from(checked64(s, STD), "base64"));
}
export function base64UrlEncode(data: number[]): string {
  return Buffer.from(data).toString("base64url");
}
export function base64UrlDecode(s: string): number[] {
  return bytes(Buffer.from(checked64(s, URL_SAFE), "base64url"));
}
export function base64EncodeString(s: string): string {
  return Buffer.from(s, "utf8").toString("base64");
}
export function base64DecodeString(s: string): string {
  return utf8Decode(base64Decode(s));
}
export function hexEncode(data: number[]): string {
  return Buffer.from(data).toString("hex");
}
export function hexDecode(s: string): number[] {
  if (s.length % 2 !== 0) {
    throw new EncodingError("invalid hex: odd length");
  }
  if (!/^[0-9a-fA-F]*$/.test(s)) {
    throw new EncodingError("invalid hex digit");
  }
  return bytes(Buffer.from(s, "hex"));
}
export function utf8Encode(s: string): number[] {
  return bytes(Buffer.from(s, "utf8"));
}
export function utf8Decode(data: number[]): string {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(data));
  } catch {
    throw new EncodingError("invalid utf-8");
  }
}
export function utf8DecodeLossy(data: number[]): string {
  return new TextDecoder("utf-8").decode(new Uint8Array(data));
}
export function utf8Valid(data: number[]): boolean {
  try {
    utf8Decode(data);
    return true;
  } catch {
    return false;
  }
}
