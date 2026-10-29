# velt:collections/concurrent_map

`import { ConcurrentMap } from "velt:collections/concurrent_map"`. A map from `u64` or `string`
keys to values that many threads read and write at once, `ConcurrentMap<K, V>` (`K` and `V` are
`u64` when not written): create it, share it (`shared(ConcurrentMap.create(n))`), and call its
methods from any task. A lookup writes no shared memory, so readers on many cores never slow
each other, and writers take a latch per node, never a lock for the whole map. For a map one
task owns, [`BTreeMap`](btree_map.md) is simpler, holds any keys and values, and walks them in
order.

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
  bytes an entry (17 more for string keys), taken when it is created. A `set` past its room
  panics with a RangeError. A deleted entry frees a slot in its leaf, and a leaf whose last
  entry is deleted is reused. So keys that keep moving to new ranges (ids or timestamps that
  only grow, old ones deleted) fit for as long as the entries present at once do. Leaves are
  not merged, though: entries spread thinly over many leaves (most of a large map's keys deleted
  at random) can hold a node each and need more room than their number. A `set` of a key the map
  holds can need room too, when its leaf is full and splits first. `create` panics with a
  RangeError above 10^11 entries, and for keys other than `u64` and `string`.
- `get(key): V | undefined`, `has(key)`.
- `read(key, (v) => …)`: runs the callback on the value of `key`, the map's own (borrowed, not
  copied), and returns what it returns, or `undefined` if the map has no `key`. For 64-bit
  integer values (`u64`, `i64`, `usize`, `isize`) the callback gets a `shared` integer, an atomic
  one read with `v.get()`, in a box of its own for the call: changing it does not change the map,
  and `get` is the cheaper read there.
- `set(key, value)`: adds `key` or replaces its value. The `u64` key `2^64 - 1` cannot be held
  (a RangeError); every lookup of it answers absent.
- `add(key, delta): u64 | undefined`: for `u64` values, adds `delta` to the value of `key`
  (wrapping) atomically and returns the value before, or `undefined` if the map has no `key`
  (then nothing is added).
- `delete(key): bool`, whether the map had it.
- `getManyInto(keys, values): u64`: for `u64` keys and values (a RangeError otherwise), looks
  up at most 64 keys together into `values`; where bit `i` of the result is set, `values[i]` is
  the value of `keys[i]` (`values` grows to `keys.length`). Unlike `BTreeMap.getMany`, which
  returns a new array, it fills the caller's, so a loop of batches allocates nothing. In a map
  larger than the caches it costs about half as much as the same `get`s: the keys descend the
  tree together, each one's next node loading while the others search theirs.

A `u64` value sits in the tree, next to its key. Any other `V` (it must be `Frozen`, see
[`shared` values in generic code](../sync.md#shared-values-in-generic-code): a string, an array,
an object of plain fields) is boxed once by `set`, as a `shared<V>` beside the leaf: `get`
copies it, `read` reads it in place, and one that `set` replaces or `delete` removes is dropped
once no reader can still be reading it.

A `string` key is ordered in the tree by its first 8 bytes (its UTF-16 code units as CESU-8,
the order of `<`), a word that sits where a `u64` key would, and kept whole beside its leaf:
`set` copies a new key there once, and `delete` drops it once no reader can still be reading
it. Keys whose first 8 bytes are the same (`"user:000123"` and `"user:000456"`) tie on that
word, and the map tells them apart by comparing the strings: in a leaf, the slot whose tag (a
hash of the whole string, so keys with a common prefix do not share tags) and word match is
compared in full; in an inner node, a binary search over the strings of the separators that tie
with the key finds its child. So a lookup of a string key costs a hash of the key, its first 8
bytes and one string compare in the leaf, which reads the kept string (a few cache lines more
than a `u64` key's lookup), and inner nodes read strings only where words tie: keys that share
their first 8 bytes (URLs that all start `https://`) cost up to 6 string compares more per inner
node, as in any ordered map.

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
  const names: ConcurrentMap<u64, string> = ConcurrentMap.create(100);
  names.set(7, "seven");
  console.log(names.get(7), names.read(7, (s) => s.length)); // seven 5
  const ids: ConcurrentMap<string, u64> = ConcurrentMap.create(100);
  ids.set("user:000123", 123);
  ids.set("user:000124", 124);
  ids.add("user:000123", 1);
  console.log(ids.get("user:000123"), ids.has("user:0001"), ids.delete("user:000124"));
  // 124 false true
}
```
