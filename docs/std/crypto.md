# velt:crypto

`import { sha256Hex, randomBytes } from "velt:crypto"`. SHA-256 and SHA-1, HMAC, secure
randomness and constant-time comparison. The hashes are pure Velt (SHA-256 runs at about
360 MB/s in release builds); the randomness comes from the OS generator.

- `sha256(data: u8[]): u8[]` (32 bytes), `sha256Hex(s: string)`.
- `sha1(data)` (20 bytes), `sha1Hex(s)`: SHA-1 is for legacy protocols only.
- `hmacSha256(key, data)`, `hmacSha1(key, data)`.
- `randomBytes(n: usize): u8[]`.
- `randomInt(min, max): i64`: uniform in `[min, max)`; throws `CryptoError` if `max <= min`.
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
