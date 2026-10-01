# velt:arena

`import { Arena } from "velt:arena"`. A typed memory pool (a region). Values are appended to one
growing array and named by their `u32` index, and the whole pool is released at once with
`reset()`, which keeps the memory for the next round: a tree rebuilt on every iteration costs no
allocation after the first one.

It works best with Copy structs that link to each other by index
(`struct Node { left: u32; right: u32 }`), the shape of Rust's typed-arena programs: `u32`
indices are half the size of pointers, so twice as many nodes fit in a cache line. Indices stay
valid until `reset()`.

- `new Arena<T>(capacity: usize = 0)`: `capacity` values fit before the pool first grows.
- `alloc(value): u32` stores a value and returns its index.
- `get(index): T` (a copy for Copy types, else a clone), `set(index, value)`.
- `length`: values allocated since the last `reset()`.
- `reset()` drops every value and keeps the memory.

```ts
import { Arena } from "velt:arena";

struct Node {
  left: u32;
  right: u32;
}

const NONE: u32 = 4294967295;

function build(pool: Arena<Node>, depth: i64): u32 {
  if (depth == 0) {
    return pool.alloc(Node { left: NONE, right: NONE });
  }
  const l = build(pool, depth - 1);
  const r = build(pool, depth - 1);
  return pool.alloc(Node { left: l, right: r });
}

function count(pool: Arena<Node>, i: u32): i64 {
  const n = pool.get(i);
  return n.left == NONE ? 1 : 1 + count(pool, n.left) + count(pool, n.right);
}

function main() {
  const pool = new Arena<Node>(1024);
  for (let round = 0; round < 3; round++) {
    const root = build(pool, 6);
    console.log(count(pool, root), pool.length); // 127 127
    pool.reset();
  }
}
```
