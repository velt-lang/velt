# velt:collections/btree_map

`import { BTreeMap } from "velt:collections/btree_map"`. A map with its keys kept in ascending
order (`K extends Comparable<K>`: numbers, strings, bool, and user types with a `compareTo`).
It is a B+tree: lookups, inserts and deletes are O(log n), and ordered walks follow the linked
leaves. Prefer it to `SortedMap` once a map has more than a few hundred keys or changes often:
`SortedMap` shifts its arrays on every insert and delete.

A node holds up to 31 keys, and next to them a 64-byte line with two bytes of each key past the
prefix all of the node's keys share, which a search compares eight at a time (`u16x8`); it
reads a key only when those bytes tie (strings keep two more bytes in a second line). So a
lookup reads about one cache line per level. A leaf keeps its entries in the order they came and
their rank order apart, so an insert or a delete moves no other entry; leaves that fill up hand
entries to a neighbor before they split, which keeps them about 85% full. Numbers, `bool` and
strings have such an encoding (`InlineKey`); keys of other types are compared in full, and get
the fast search by implementing `InlineKey` (below). A map of 1M `u64` keys and values takes
about 27 MB, and string keys of up to 23 bytes are kept in the map, not on the heap.

- `new BTreeMap<K, V>(entries?)`, `size`, `isEmpty()`, `clear()`.
- `set(k, v)`, `get(k): V | undefined`, `has(k)`, `delete(k): bool`. Like `Map`, the map shares
  the values it stores: `get` returns the stored object, not a copy.
- `getMany(keys): (V | undefined)[]`: `get` of each key, in order. The lookups of a batch run
  interleaved, each one's next node loading while the others search theirs, so in a map larger
  than the CPU's caches a batch costs about half as much as the same `get`s.
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
- A deleted key or value, or a replaced value, is released: the map keeps no reference to it,
  so a string or an object is freed with its entry once nothing else holds it. Unused slots
  hold a blank value (velt:mem `Blank`: `""` for strings), or for a type without one a share
  of a live entry, which moves to another entry when that one goes.
- The map is written in Velt on public APIs only. Its parts live in `velt:collections/btree/*`
  modules, which are internal: they can be imported, but they are not API and change without
  notice. Use `velt:collections/btree_map`.

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

## `InlineKey`

`import { InlineKey } from "velt:collections/btree_map"`. A key type gives the map its
order-preserving bytes: `hasInlineKey()` is `true`, and `inlineKey(offset)` returns bytes
`offset .. offset + 8` of an encoding that orders as the keys do (for keys `a < b`, `a`'s bytes
are at most `b`'s in byte order), as a big-endian `u64`, zero past its end. Numbers encode
big-endian (signed ones with the sign bit flipped), `bool` as one byte, strings as their CESU-8
bytes (UTF-16 code units written as UTF-8 would, the order of `<`, read in place for ASCII);
floats have no encoding yet. `inlineWord`, `inlineQuad` (2 and 4 bytes), `inlineWindow()` (the
first 16, which an operation computes once and reads each node's word from) and `inlineWide()`
(whether nodes keep 4 bytes per key, `false` by default; strings say `true`) have defaults from
`inlineKey`. Every other comparable type has the default, `hasInlineKey()` `false`, through
`extend<T extends Comparable<T>> T implements InlineKey`, and an `extend` block for one type
replaces it for that type ([classes](../../reference/classes.md#extend--implements)):

```ts
import { BTreeMap, InlineKey } from "velt:collections/btree_map";

struct OrderKey {
  customer: u32;
  order: u32;
}

// Ordered by customer, then order: the encoding is both fields' bytes, big-endian.
extend OrderKey implements Comparable<OrderKey>, InlineKey {
  compareTo(other: OrderKey): i64 {
    return this.customer != other.customer
      ? (this.customer as i64) - (other.customer as i64)
      : (this.order as i64) - (other.order as i64);
  }
  hasInlineKey(): bool {
    return true;
  }
  inlineKey(offset: usize): u64 {
    const both = ((this.customer as u64) << 32) | (this.order as u64);
    return offset >= 8 ? 0 : both << ((offset * 8) as u64);
  }
}

function main() {
  const orders = new BTreeMap<OrderKey, string>();
  orders.set({ customer: 2, order: 1 }, "b1");
  orders.set({ customer: 1, order: 7 }, "a7");
  orders.set({ customer: 1, order: 3 }, "a3");
  console.log(orders.values()); // [ 'a3', 'a7', 'b1' ]
  console.log(orders.getMany([{ customer: 1, order: 7 }, { customer: 9, order: 9 }])); // [ 'a7', undefined ]
}
```

An encoding that does not order as `compareTo` does gives wrong answers, as a `compareTo` that is
not an order does.
