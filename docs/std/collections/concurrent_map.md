# velt:collections/concurrent_map

`import { ConcurrentMap } from "velt:collections/concurrent_map"`. A map from `u64` keys to `u64`
values that many threads read and write at once: create it, share it
(`shared(ConcurrentMap.create(n))`), and call its methods from any task. A lookup writes no
shared memory, so readers on many cores never slow each other, and writers take a latch per
node, never a lock for the whole map. For a map one task owns, [`BTreeMap`](btree_map.md) is
simpler, holds any keys and values, and walks them in order.

It is a B+ tree with optimistic lock coupling, as [bbtree](https://github.com/XiangpengHao/bbtree)'s
concurrent map: a reader reads a node, then checks the node's version
([`VersionLatch`](../sync.md#versionlatch)) and starts over in the rare case a writer changed
it meanwhile. It is written in Velt on the public API alone: its nodes are 1 KB blocks of one
`Atomic<u64>[]`, read several words at a time with velt:simd's
[`loadOptimistic`](../simd.md). Its keys are kept in key ranges, but a leaf's keys are unsorted
and leaves are not linked, so it has no ordered walk, iteration or `size` yet (#1688).

On 10M random keys, on 1 to 8 threads (`bench/concurrent_map`, against bbtree; the M4 column
was measured on a slightly earlier version of the map, before its leaf splits got cheaper):

| workload | Apple M4 | Intel i9-12900HK |
|---|---|---|
| insert | 19–23% faster | 32–48% faster |
| lookup (half the keys present) | 6–19% faster | 24–77% faster |
| batched lookup (`getManyInto`, 64 keys) | 30–40% faster | 56–94% faster |
| update (`add`) | 6–9% slower | 13–58% faster |
| 90% lookups, 10% updates | 3–12% faster | 27–67% faster |

- `ConcurrentMap.create(capacity)`: an empty map with room for `capacity` entries, about 40
  bytes an entry, taken when it is created. A `set` past its room panics with a RangeError.
  Room is not given back: a deleted entry frees a slot in its own leaf only, and leaves are
  never merged or reused (#1689). So keys that keep moving to new ranges (ids or timestamps
  that only grow, old ones deleted) use up the capacity over the map's life, even with few
  entries present at once. A `set` of a key the map holds can need room too, when its leaf is full and splits
  first.
- `get(key): u64 | undefined`, `has(key)`.
- `set(key, value)`: adds `key` or replaces its value. The key `2^64 - 1` cannot be held (a
  RangeError); every lookup of it answers absent.
- `add(key, delta): u64 | undefined`: adds `delta` to the value of `key` (wrapping) atomically
  and returns the value before, or `undefined` if the map has no `key` (then nothing is added).
- `delete(key): bool`, whether the map had it.
- `getManyInto(keys, values): u64`: looks up at most 64 keys together into `values`; where bit
  `i` of the result is set, `values[i]` is the value of `keys[i]` (`values` grows to
  `keys.length`). Unlike `BTreeMap.getMany`, which returns a new array, it fills the caller's, so
  a loop of batches allocates nothing. In a map larger than the caches it costs about half as
  much as the same `get`s: the keys descend the tree together, each one's next node loading while
  the others search theirs.

Each call is atomic on its own: a `get` sees a `set` of the same key either wholly or not at
all. A sequence of calls is not one step: `m.set(k, (m.get(k) ?? 0) + 1)` can lose another
thread's increment, where `m.add(k, 1)` cannot. Lookups are not lock-free: a lookup waits
(spins) while a writer holds its node, so a writer preempted in the middle of a change stalls
that node's readers until it runs again.

```ts
import { ConcurrentMap } from "velt:collections/concurrent_map";

async function count(m: shared<ConcurrentMap>, from: u64) {
  for (let i: u64 = from; i < from + 1000; i++) {
    m.set(i, i * 2);
  }
}

async function main() {
  const m = shared(ConcurrentMap.create(10000));
  await Promise.all([spawn(count(m, 0)), spawn(count(m, 1000)), spawn(count(m, 2000))]);
  console.log(m.get(1500), m.has(3000), m.delete(10), m.get(10)); // 3000 false true undefined
  const values: u64[] = [];
  const found = m.getManyInto([1, 2, 9999], values);
  console.log(found, values[0], values[1]); // 3 2 4
}
```
