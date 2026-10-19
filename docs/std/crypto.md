# velt:crypto

`import { sha256Hex, randomBytes } from "velt:crypto"`. SHA-256 and SHA-1, HMAC, secure
randomness and constant-time comparison. The hashes are Velt code: in release builds on a CPU
with SHA instructions (arm64, and x86-64 with SHA-NI) they run on them through
[`velt:simd`](simd.md#sha-instructions), about 3.0 GB/s for SHA-1 and 2.7 GB/s for SHA-256 on an
Apple M4, within 2% of Rust's `sha1`/`sha2` crates for large inputs. Elsewhere (other CPUs,
WebAssembly, debug builds, or with `VELT_SIMD_DISABLE=sha`) they use portable rounds, about
360 MB/s for SHA-256. The randomness comes from the OS generator.

- `sha256(data: u8[]): u8[]` (32 bytes), `sha256Hex(s: string)`.
- `sha1(data)` (20 bytes), `sha1Hex(s)`: SHA-1 is for legacy protocols only.
- `hmacSha256(key, data)`, `hmacSha1(key, data)`.
- `randomBytes(n: usize): u8[]`.
- `randomInt(min, max): i64`: uniform in `[min, max)`. Like Node, it throws `CryptoError` (with
  Node's message) when `min` or `max` is not a safe integer, `max <= min`, or `max - min` is 2^48 or
  more.
- `timingSafeEqual(a, b): bool`.

```ts
import { sha256Hex, hmacSha256, randomBytes, randomInt, timingSafeEqual } from "velt:crypto";
import { utf8Encode, hexEncode } from "velt:encoding";

function main() {
  console.log(sha256Hex("abc"));
  const mac = hmacSha256(utf8Encode("key"), utf8Encode("The quick brown fox jumps over the lazy dog"));
  console.log(hexEncode(mac));
  const die = randomInt(1, 7);
  console.log(randomBytes(16).length, die >= 1 && die < 7, timingSafeEqual(mac, mac.clone()));
}
```
