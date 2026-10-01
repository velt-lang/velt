# velt:collections/deque

`import { Deque } from "velt:collections/deque"`. A double-ended queue on a growable ring buffer.
Pushing and popping at either end is amortized O(1), and indexing is O(1).

- `new Deque<T>()`, `Deque.from(xs)`, `length` / `size`, `isEmpty()`, `clear()`.
- `pushBack(v)`, `pushFront(v)`.
- `popBack()`, `popFront()`: return `T | null`.
- `peekFront()`, `peekBack()`, `at(i: i64)`: return clones or null; a negative `i` counts from
  the back.
- `toArray()`: clones, front to back. `forEach(f)`.

```ts
import { Deque } from "velt:collections/deque";

function main() {
  const q = Deque.from([2, 3]);
  q.pushFront(1);
  q.pushBack(4);
  console.log(q.popFront(), q.popBack(), q.at(-1), q.length, q.toArray()); // 1 4 3 2 [ 2, 3 ]
}
```

Notes: don't store a nullable `T` (`Deque<i64 | null>`), because a stored null reads as an empty
slot.
