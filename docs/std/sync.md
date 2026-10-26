# Atomics

Lock-free shared state: the prelude's `Atomic<T>`. It is for code that needs to be as fast as
threads allow: counters, flags, statistics, and data structures that many threads read and
change at once without a lock, such as a concurrent tree with a latch in every node. For ordinary shared state, a
[`Mutex` or `RwLock`](../reference/async.md#thread-safety) is simpler. Design:
[atomics.md](../internals/design/atomics.md).

## `Atomic<T>`

`new Atomic<T>(v)` is a number or `bool` that threads read and change at once: `T` is an
integer type (`i8` … `i64`, `u8` … `u64`, `isize`, `usize`) or `bool`. It is stored in place, as
a plain `T`, in the variable, object or array that holds it, and it is the one part of a
[`shared<T>`](../reference/async.md#thread-safety) that changes: `s.hits.add(1)` changes the word
inside the shared value that every thread reads. Each method is one CPU instruction (or a short
loop on a CPU without one), the same as Rust's `std::sync::atomic`.

| Method | Does | Returns |
|---|---|---|
| `load(order?)` | reads the value | the value |
| `store(v, order?)` | makes `v` the value | |
| `exchange(v, order?)` | makes `v` the value | the previous value |
| `compareExchange(expected, v, order?, failure?)` | makes `v` the value if it is `expected` | the previous value (`== expected` when it swapped) |
| `compareExchangeWeak(expected, v, order?, failure?)` | the same, but may fail although the value is `expected` (call it in a loop); cheaper on some CPUs | whether it swapped |
| `add(v)`, `sub(v)` | adds or subtracts `v`, wrapping at the type's bounds | the previous value |
| `and(v)`, `or(v)`, `xor(v)` | bitwise (logical for `bool`) | the previous value |
| `max(v)`, `min(v)` | the larger or smaller of the value and `v` (signed for a signed `T`) | the previous value |
| `Atomic.fence(order?)` | a fence: orders this thread's reads and writes around it | |
| `Atomic.spinHint()` | tells the CPU the thread is waiting in a loop (`pause`, `isb`) | |

The names and results are those of JavaScript's `Atomics` (`Atomics.add` returns the previous
value too). `add`, `sub`, `max` and `min` are for integers only.

**Orderings.** Every method takes a memory ordering, `"seqcst"` when left out, as `Atomics`
always is. A weaker one is faster where it is enough:

- `"relaxed"`: only this word is atomic; nothing else is ordered by it (counters, statistics).
- `"acquire"` (a read): the reads and writes after it see everything the thread that made the
  value saw before its `"release"`.
- `"release"` (a write): the reads and writes before it are seen by a thread that reads the
  value with `"acquire"`.
- `"acqrel"`: both, for a read-modify-write.
- `"seqcst"`: also one order of all `"seqcst"` operations that every thread agrees on.

An ordering must be a string literal (the compiler picks the instruction for it), and one an
operation cannot have is a type error: `load("release")`. A compare-and-swap's `failure`
ordering, for the read when it does not swap, is by default `order` without its release part.

```ts
class Stats {
  requests: Atomic<u64> = new Atomic<u64>(0);
  largest: Atomic<u64> = new Atomic<u64>(0);
}

async function handle(s: shared<Stats>, size: u64) {
  s.requests.add(1, "relaxed");
  s.largest.max(size, "relaxed");
}

async function main() {
  const s = shared(new Stats());
  const ps: Promise<void>[] = [];
  for (let i: u64 = 1; i <= 100; i++) {
    ps.push(spawn(handle(s, i)));
  }
  await Promise.all(ps);
  console.log(s.requests.load(), s.largest.load()); // 100 100
}
```

**Used where it is.** An `Atomic` cannot be copied or shared: `const a = s.hits` and storing it
somewhere else are errors (``an `Atomic<u64>` cannot be copied or shared``). Read it with
`.load()`, or pass the object that holds it. A function may take one as a parameter and call
its methods: the call passes the atomic itself, in place, also one inside a `shared` value
(`bump(s.hits)`). Binding a part of a `shared` value that holds an `Atomic` to a variable is an
error too, since that makes a copy and the changes would go to the copy: use the part in place
(`s.items[i].count.add(1)`, `for (const x of s.items) x.count.add(1)`).

```ts
function bump(counter: Atomic<u32>): u32 {
  return counter.add(1);
}

class Slots {
  used: Atomic<u32>[] = [];
}

function main() {
  const slots = new Slots();
  for (let i = 0; i < 4; i++) {
    slots.used.push(new Atomic<u32>(0));
  }
  const s = shared(slots);
  bump(s.used[2]);
  bump(s.used[2]);
  for (const u of s.used) {
    u.or(8);
  }
  console.log(s.used[0].load(), s.used[2].load()); // 8 10
}
```

An array of atomics inside a `shared` value never moves or shrinks while the value lives, so its
words are memory every thread may read and write in place, which no thread can free under
another: what a lock-free structure needs.
