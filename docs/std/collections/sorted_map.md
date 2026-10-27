# velt:collections/sorted_map

`import { SortedMap } from "velt:collections/sorted_map"`. A map with its keys kept in ascending
order (`K extends Comparable<K>`). It uses two sorted arrays: lookups are O(log n), and inserts
and deletes are O(n). For maps of more than a few hundred keys, or that change often, use
[`BTreeMap`](btree_map.md), a B+tree with O(log n) inserts and deletes.

- `new SortedMap<K, V>()`, `size`, `isEmpty()`, `clear()`.
- `set(k, v)`, `get(k): V | undefined` (a clone), `has(k)`, `delete(k): bool`.
- `keys()`, `values()`, `entries(): [K, V][]`, all in key order. `forEach((v, k) => …)` visits
  the entries by position in key order and reads the size again after each call: when the
  callback sets or deletes keys through another reference, entries added after the current
  position are visited, and a key added or deleted before it shifts the entries still to come.
- Ordered queries: `first()`, `last()`, `floorKey(k)`, `ceilingKey(k)` (`undefined` when there
  is no such key), and `range(from, to)`,
  which returns the half-open range `from <= key < to`.
- Keys are ordered by `compareTo`. For floats that is `<` with `NaN` after every other number
  and equal to itself, so `NaN` is one key of its own, the last one.

```ts
import { SortedMap } from "velt:collections/sorted_map";

function main() {
  const scores = new SortedMap<string, i64>();
  scores.set("carol", 7);
  scores.set("alice", 9);
  scores.set("bob", 4);
  console.log(scores.keys(), scores.first(), scores.last());
  console.log(scores.floorKey("bz"), scores.ceilingKey("bz"), scores.range("b", "d"));
}
```
