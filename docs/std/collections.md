# velt/collections

`import { Deque, PriorityQueue, SortedMap, BTreeMap, ConcurrentMap, Set } from
"velt/collections"`. Containers beyond the prelude's arrays and `Map`, written in Velt:

| Type | What it is |
|---|---|
| [`Deque<T>`](collections/deque.md) | a double-ended queue on a ring buffer: O(1) at both ends and by index |
| [`PriorityQueue<T>`](collections/priority_queue.md) | a binary heap: `pop` returns what `cmp` orders first |
| [`SortedMap<K, V>`](collections/sorted_map.md) | a map ordered by key, on sorted arrays (small maps, fast iteration) |
| [`BTreeMap<K, V>`](collections/btree_map.md) | a map ordered by key, as a B+ tree (large maps, range queries) |
| [`ConcurrentMap<K, V>`](collections/concurrent_map.md) | a map many threads read and write at once |
| [`Set<T>`](collections/set.md) | an insertion-ordered set of unique values (also a global, as in JavaScript); `ReadonlySet<T>` |
