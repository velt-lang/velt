# velt:collections/btree_map

`import { BTreeMap } from "velt:collections/btree_map"`. A map with its keys kept in ascending
order (`K extends Comparable<K>`: numbers, strings, bool, and user types with a `compareTo`).
It is a B+tree: lookups, inserts and deletes are O(log n), and each level of the tree is a few
cache lines (nodes of 32 keys, linked by index in flat pools). Ordered walks follow the linked
leaves. Prefer it to `SortedMap` once a map has more than a few hundred keys or changes often:
`SortedMap` shifts its arrays on every insert and delete.

- `new BTreeMap<K, V>(entries?)`, `size`, `isEmpty()`, `clear()`.
- `set(k, v)`, `get(k): V | undefined`, `has(k)`, `delete(k): bool`. Like `Map`, the map shares
  the values it stores: `get` returns the stored object, not a copy.
- `keys()`, `values()`, `entries(): [K, V][]`, all in key order; `for (const [k, v] of map)`
  walks the entries as they are when the loop starts.
- `forEach((v, k) => …)` visits the entries in key order. When the callback changes the map
  through another reference, the walk goes on from the first key after the one it visited
  last: keys added past it are visited, keys deleted before their turn are not.
- Ordered queries: `first()`, `last()`, `floorKey(k)` (the largest key `<= k`), `ceilingKey(k)`
  (the smallest key `>= k`), `undefined` when there is no such key, and `range(from, to)`, the
  entries with `from <= key < to`.
- Keys are ordered by `compareTo`. For floats that is `<` with `NaN` after every other number
  and equal to itself, so, as in `Map`, `NaN` is one key of its own (the last one), and `0` and
  `-0` are one key.
- A deleted key is released: the map keeps no reference to it, so a string or an object used as
  a key is freed with its entry once nothing else holds it.

```ts
import { BTreeMap } from "velt:collections/btree_map";

function main() {
  const prices = new BTreeMap<i64, string>();
  prices.set(300, "c");
  prices.set(100, "a");
  prices.set(200, "b");
  console.log(prices.keys(), prices.first(), prices.last()); // [ 100, 200, 300 ] 100 300
  console.log(prices.floorKey(250), prices.ceilingKey(250)); // 200 300
  console.log(prices.range(100, 300)); // [ [ 100, 'a' ], [ 200, 'b' ] ]
  for (const [price, name] of prices) {
    console.log(price, name);
  }
}
```
