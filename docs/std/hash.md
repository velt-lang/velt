# velt:hash

`import { fnv1a64, fnv1a64Bytes } from "velt:hash"`. A stable, non-cryptographic hash for
sharding, bucketing and consistent hashing.

- `fnv1a64(s: string): u64`: the 64-bit [FNV-1a](https://datatracker.ietf.org/doc/html/draft-eastlake-fnv)
  hash of the UTF-8 bytes of `s`.
- `fnv1a64Bytes(data: u8[]): u64`: the same hash of a byte array, so
  `fnv1a64(s) == fnv1a64Bytes(utf8Encode(s))`.
- **Stable**: the algorithm is fixed, and it is not seeded, so a value is the same on every
  platform, in every run and in every Velt version. It is safe to store, or to share between
  processes that must agree on where a key lives.
- **Not DoS-resistant**: anyone can compute colliding keys. Don't use it for a hash table whose
  keys an attacker chooses, and never for security; use [`velt:crypto`](crypto.md) there.
  (The prelude `Map` hashes keys its own way; that hash is not part of any contract.)

```ts
import { fnv1a64 } from "velt:hash";

function shardOf(key: string, shards: u64): u64 {
  return fnv1a64(key) % shards;
}

function main() {
  console.log(fnv1a64("foobar")); // 9625390261332436968
  console.log(shardOf("user:1", 16)); // 11
}
```
