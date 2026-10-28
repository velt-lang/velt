# Atomics and velt:sync

Lock-free shared state: the prelude's `Atomic<T>`, the primitive, and `velt:sync`
(`import { Backoff, VersionLatch } from "velt:sync"`), building blocks written on it. They are
for code that needs to be as fast as threads allow: counters, flags, statistics, and data
structures that many threads read and change at once without a lock, such as a concurrent tree
with a latch in every node. For ordinary shared state, a
[`Mutex` or `RwLock`](../reference/async.md#thread-safety) is simpler. Design:
[atomics.md](../internals/design/atomics.md).

## `Atomic<T>`

`new Atomic<T>(v)` is a number or `bool` that threads read and change at once: `T` is an
integer type (`i8` … `i64`, `u8` … `u64`, `isize`, `usize`), `f64` (`number`), `f32` or `bool`
(`new Atomic(0)` is an `Atomic<f64>`, as `0` is a number). It is stored in place, as
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
| `add(v)`, `sub(v)` | adds or subtracts `v`, wrapping at an integer type's bounds | the previous value |
| `and(v)`, `or(v)`, `xor(v)` | bitwise (logical for `bool`) | the previous value |
| `max(v)`, `min(v)` | the larger or smaller of the value and `v` (signed for a signed `T`; `Math.max` and `Math.min` for a float) | the previous value |
| `Atomic.fence(order?)` | a fence: orders this thread's reads and writes around it | |
| `Atomic.spinHint()` | tells the CPU the thread is waiting in a loop (`pause`, `isb`) | |

The names and results are those of JavaScript's `Atomics` (`Atomics.add` returns the previous
value too). `add`, `sub`, `max` and `min` are for numbers, `and`, `or` and `xor` for integers
and `bool`.

**Floats.** An `Atomic<f64>` or `Atomic<f32>` holds the float's bits in a word of its size.
`load`, `store` and `exchange` are the word's instructions. `compareExchange` compares bits, not
values: `-0` does not match `0`, and a NaN matches a NaN with the same bits (one read back from
the atomic; NaNs made by different operations can differ). No CPU adds floats in memory, so
`add`, `sub`, `max` and `min` are a loop: read the value, compute, and swap it in with
`compareExchange` if no other thread changed it meanwhile, else again. Like Rust's atomic float
crates, the swap that succeeds has the method's ordering. `max` and `min` follow `Math.max` and
`Math.min`: a NaN on either side makes the value NaN, and `0` is larger than `-0`.

```ts
async function addTo(total: shared<Atomic<f64>>, x: f64) {
  total.add(x, "relaxed");
}

async function main() {
  const total = shared(new Atomic(0));
  const ps: Promise<void>[] = [];
  for (let i = 1; i <= 100; i++) {
    ps.push(spawn(addTo(total, i / 2)));
  }
  await Promise.all(ps);
  console.log(total.load()); // 2525
}
```

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
(`s.items[i].count.add(1)`, `for (const x of s.items) x.count.add(1)`). So is destructuring
one (`const { hits } = c`), and generic code that would copy or share atomics (a
`Map<string, Atomic<u64>>`, `slice` or `filter` of a `shared` value's parts holding them),
reported at the call.

**Across tasks.** An object holding an `Atomic` that is not in a `shared` value stays on its
task: awaiting a call that changes it is fine, but passing it to `spawn` (or capturing it in a
spawned closure) while the program still uses it is an error, since the task would get a copy
whose atomics take its changes. So is capturing it in any async closure whose calls may run on
another thread (`const go = async () => …` with `spawn(go())`): such a closure copies what it
captured for each call. The same holds for a plain closure whose promise goes to another task
(`const go = () => work(c)` with `spawn(go())`). Share it (`const c = shared(new Hits())`), and every task
changes the same atomics. A module-level `const` holding one is one atomic, made once, that
every task changes: `const REQUESTS = new Atomic<u64>(0)`. Made from a literal like that, it
is a word stored with the program, which threads change at once with no other cost; made from
a computed value or inside an object, it is kept with the module's other state, which tasks
reach one at a time.

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

### An `Atomic` of a `shared` value

An `Atomic<shared<T>>` (or `Atomic<shared<T> | null>`) holds one reference to an immutable
value, which threads read and replace at once without a lock: a configuration every request
reads while an admin task swaps in a new one, or the root of a structure that writers rebuild.
- `load()` gives the current value. Reading it costs one load and no count, also through a
  `const` (`const c = CONFIG.load()`) or a function it is passed to; what keeps it (a return, a
  store, a closure, a task) takes a reference of its own, which keeps seeing that value.
- `store(v)` makes `v` the value.
- `exchange(v)` makes `v` the value and gives the one before.
- `compareExchange(expected, v)` makes `v` the value only if the `Atomic` still holds
  `expected` itself (the same reference, not an equal value), and returns whether it did.
  When it did not, `v` is dropped. `compareExchangeWeak` may fail even when it holds
  `expected` (call it in a loop).

An `Atomic<string>` works the same way: `load()`, `store(v)` and `exchange(v)` read and
replace the string, and `compareExchange(expected, v)` compares contents (strings are values)
and returns whether it made `v` the value. Each `store` boxes the string, so a store costs one
allocation; a read costs a load. A module-level `const` `Atomic<string>` is kept with the
module's other state (its box is made at run time), which tasks reach one at a time: keep a
busy one in a field of a `shared` object instead.

It has no `add`, `and` and the like. A value it replaces is not freed at once, since another
thread may be reading it: it is freed once no thread can be (`velt:sync/epoch` below). A
function that calls `load()` runs in a section for that, which the compiler opens and closes:
a few nanoseconds per call, so a loop of loads costs one. The section lasts the whole call, so
values replaced while a long-running plain function loops are freed only once it returns
(memory, not speed): keep such loops in a function of their own that returns now and then. An `async` function's `load()` counts
the value at once, which keeps it valid across its `await`s: read many values in a plain
function it calls.

```ts
interface Config {
  name: string;
  limit: u64;
}

const CONFIG = new Atomic<shared<Config> | null>(null);

function main() {
  CONFIG.store(shared({ name: "default", limit: 10 }));
  const before = CONFIG.load();
  CONFIG.store(shared({ name: "strict", limit: 2 }));
  const now = CONFIG.load();
  if (before != null && now != null) {
    console.log(before.name, now.name, now.limit); // default strict 2
  }
}
```

### `Atomic` in generic code

A type parameter bounded by `AtomicValue` can be an `Atomic`'s `T`: `class Cell<T extends
AtomicValue> { word: Atomic<T>; … }`. The prelude's `AtomicValue` is implemented by the integer
types, `f32`, `f64`, `bool`, `string` and `shared<U>`, which the compiler knows (declaring it
with `implements` or `extends` is an error, and a type parameter needs the bound itself), and
the bound is checked where the generic is used (`new Cell<Obj>(…)` with a plain object is an
error). Each use gets its type's instructions. Generic code has the operations every `AtomicValue` has alike: `load()`,
`store(v)`, `exchange(v)` and `compareExchangeWeak(expected, v)` (in a loop: it returns whether
it swapped for every `T`, where `compareExchange` returns the value before for a number).

```ts
class Cell<T extends AtomicValue> {
  word: Atomic<T>;

  constructor(v: T) {
    this.word = new Atomic<T>(v);
  }

  replace(v: T): T {
    return this.word.exchange(v);
  }
}

function main() {
  const c = new Cell<string>("a");
  console.log(c.replace("b"), c.word.load()); // a b
}
```

### `shared` values in generic code

A `shared<T>` is read like its `T` (in place, no lock, no copy) only when nothing in `T` could
change it or run code on it. Generic code does not know what a type parameter will be, so it
needs the prelude's `Frozen` bound to read a `shared<V>` as a `V`: `class Store<V extends
Frozen>`. Like `AtomicValue`, the compiler knows which types are `Frozen` (declaring it is an
error, and a type parameter needs the bound itself), and checks the bound where the generic is used: a type holding a `Mutex`, a
function or interface value, a weak reference or a promise is not. A callback still gets the
`shared<V>` itself (`read(f: (v: shared<V>) => R)`): a function value might change what it is
given.

```ts
class Store<V extends Frozen> {
  cell: Atomic<shared<V> | null> = new Atomic<shared<V> | null>(null);

  put(v: V) {
    this.cell.store(shared(v));
  }

  get(): V | undefined {
    const s = this.cell.load("acquire");
    if (s == null) {
      return undefined;
    }
    return s;
  }
}

function main() {
  const s = new Store<string>();
  s.put("hello");
  console.log(s.get()); // hello
}
```

## `Atom<T>`

A `shared` value that threads read at once without a lock and replace whole, like Rust's
`arc-swap`: `Atom.create(shared(config))`, kept in a `shared` object. Readers never wait and
never write; a value it replaces is dropped once no thread can still be reading it.

- `read((v) => …)` runs the callback on the current value, borrowed: reading it there costs no
  count, the cheapest read, and whatever the callback keeps of it takes a reference of its own.
- `load()` returns the current value as a reference of its own, to keep.
- `store(v)` makes `v` the value, and `swap(v)` returns the one before.
- `update((v) => …)` replaces the value with what the callback makes of it. Updates take turns, so
  the callback runs once per update, unless a `store` or `swap` came in between (then it runs
  again on the newer value). If it throws, nothing changes. The callback must not call `update`
  on the same `Atom`: updates take turns, so it would wait for itself forever. While an update
  waits for its turn and runs its callback, values replaced meanwhile stay allocated (memory,
  not speed).

At module level, use an `Atomic<shared<T> | null>` (`null` to start), which takes no lock.

```ts
import { Atom } from "velt:sync";

interface Limits {
  rps: u64;
}

class Gateway {
  limits: Atom<Limits> = Atom.create(shared({ rps: 100 }));
}

function main() {
  const g = shared(new Gateway());
  g.limits.update((l) => shared({ rps: l.rps * 2 }));
  console.log(g.limits.read((l) => l.rps)); // 200
}
```

## `ThreadLocal<T>`

`const depth = new ThreadLocal<u32>(0)` is one value per thread: every thread starts with the
value given (a literal), `get()` reads this thread's and `set(v)` changes this thread's. `T` is a
number type or `bool`. It is for code that keeps per-thread state, such as which slot of a
shared table is this thread's, or a counter a thread updates without sharing its cache line.

A task moves to another thread only at an `await`, so between two `await`s it reads its own
writes. After an `await` it may be on another thread with another value: a value a task needs
across an `await` belongs in a variable.

```ts
const calls = new ThreadLocal<u64>(0);

function work(n: u64): u64 {
  calls.set(calls.get() + 1);
  return n * 2;
}

async function job(n: u64): Promise<u64> {
  // No `await` between these: the task stays on one thread.
  const before = calls.get();
  work(n);
  work(n);
  return calls.get() - before;
}

async function main() {
  const ps: Promise<u64>[] = [];
  for (let i: u64 = 0; i < 4; i++) {
    ps.push(spawn(job(i)));
  }
  console.log(await Promise.all(ps)); // [ 2, 2, 2, 2 ]
}
```

Only a module `const` holds one, and `get` and `set` are called on that constant: a local, a
parameter or a copy of it is an error (``a `ThreadLocal` is read and written through the module
constant that holds it``). A thread's value holds no object, since nothing drops it when the
thread ends: keep objects in a structure indexed by a per-thread number. Programs reach the
value through the platform's thread-local storage (a few instructions; a call on macOS and in
Linux debug builds);
`velt dev` and arm64 Windows debug builds use the operating system's thread keys (a call).

## `Backoff`

Waiting for another thread in a loop: `Backoff.create()`, then `snooze()` each time the loop
finds it must wait again. The first rounds spin `2^round` CPU spin hints, twice as long each
round, up to `2^SPIN_ROUNDS`; after that each round yields the thread to the OS. `spin()` only
spins, and `completed` tells whether spinning stopped growing (a loop that waits longer should
block instead).

**Never across `await`.** A `Backoff` loop never gives its worker thread back to the scheduler,
so it can wait for another *thread*, not for a task: what a spin lock or latch guards must be
taken and let go between two `await`s. A task holding one across an `await` can hang the program,
when the task that waits for it runs on the same worker (and on wasm, where yielding does
nothing). To wait for another task, use a `Mutex` (its callback cannot be async either) or a
channel.

```ts
import { Backoff } from "velt:sync";

class Lock {
  held: Atomic<bool> = new Atomic<bool>(false);
}

function withLock(l: Lock, f: () => void) {
  const b = Backoff.create();
  while (!l.held.compareExchangeWeak(false, true, "acquire", "relaxed")) {
    b.snooze();
  }
  f();
  l.held.store(false, "release");
}

function main() {
  const l = new Lock();
  withLock(l, () => console.log("held")); // held
}
```

## `VersionLatch`

The latch word of optimistic lock coupling, 8 bytes, kept in place in each node of a concurrent
data structure (a field, or an element of a `VersionLatch[]`), as in
[bbtree](https://github.com/XiangpengHao/bbtree)'s concurrent map. From the low bit: an exclusive
bit, a count of shared holders, and a version that only grows.

- **Optimistic reads:** `readVersion()` (it waits while a writer holds the node), read the node,
  then `isUnchanged(v)`: when it is `false`, a writer changed the node meanwhile and the reader
  starts over. Until then the values read may be torn (some from before a write, some from
  after): code that could fail on such a value, like indexing an array with an index read under
  the latch, runs only after the check (or clamps the value first). A reader writes no shared
  memory, so readers on many cores never slow each other. Words of an `Atomic<u64>[]` read under
  the latch can be read several at a time with velt:simd's `loadOptimistic` (`u64x2`, two words
  in one load; [SIMD](simd.md)), whose lanes are trusted after the same check.
- **Exclusive:** `tryLockAt(v)` takes the node at the version the thread read, failing at once
  if anything changed it (then start over); `lockExclusive()` waits for it and returns the
  version; `tryLockExclusive()` takes it only if nothing holds it. `unlockExclusive(v)` lets go
  and moves the node to the next version, which fails every reader that read it meanwhile.
- **Shared:** `lockShared()` / `unlockShared()` keep writers out without changing the version.
- `tryReadVersion()`: the version, or `null` while a writer holds the node.

As with `Backoff`, hold a latch only between `await`s: `readVersion()` and `lockExclusive()`
wait by spinning, so a task waiting on a latch another task holds across an `await` can hang.

```ts
import { VersionLatch } from "velt:sync";

class Point {
  latch: VersionLatch = VersionLatch.create();
  x: Atomic<i64> = new Atomic<i64>(0);
  y: Atomic<i64> = new Atomic<i64>(0);
}

/** Moves the point; readers never see `x` and `y` from two different moves. */
function move(p: Point, dx: i64, dy: i64) {
  const v = p.latch.lockExclusive();
  p.x.store(p.x.load("relaxed") + dx, "relaxed");
  p.y.store(p.y.load("relaxed") + dy, "relaxed");
  p.latch.unlockExclusive(v);
}

/** `x + y` of one consistent position, without taking the latch. */
function sum(p: Point): i64 {
  while (true) {
    const v = p.latch.readVersion();
    const s = p.x.load("relaxed") + p.y.load("relaxed");
    if (p.latch.isUnchanged(v)) {
      return s;
    }
  }
}

function main() {
  const p = new Point();
  move(p, 2, 3);
  console.log(sum(p)); // 5
}
```

## `velt:sync/epoch`

Epoch-based reclamation, written in Velt on `Atomic`, `ThreadLocal` and `Mutex`: a way to drop
a `shared` value that other threads may still be reading, once none can be. The compiler calls
it for the values an `Atomic` of a `shared` value replaces, and opens a section around a
function that loads one. Lock-free code of your own can call it too.

- `pin()` enters a *section*, and `unpin()` leaves it. Sections nest; an `unpin()` without a
  `pin()` before it on the thread is an error. While a thread is in one,
  no value retired since it entered, nor one retired before that it can still reach, is
  dropped. Entering costs a store and a fence, and leaving a store. Keep a section between two
  `await`s: a section belongs to the thread it was entered on, and a task may resume on another
  thread after an `await`, where its `unpin()` is an error (and the first thread stays pinned).
- `retire(v)` hands over a `shared` value just taken out of where other threads find it. It is
  dropped once every thread that was in a section then has left it.
- `collect()` moves the global epoch on if every thread in a section has seen it, then drops
  what was retired two epochs or more before, on every thread's list. It returns how many it
  dropped. `retire` calls it every 64 retirements.
- `pending()` counts this thread's values not dropped yet, `outstanding()` every thread's, and
  `dropped()` all values dropped so far.

```ts
import { pin, unpin, retire, collect, pending } from "velt:sync/epoch";

interface Item {
  n: u64;
}

function main() {
  retire(shared({ n: 1 }));
  pin();
  console.log(collect(), pending()); // 0 1: this thread may still read it
  unpin();
  console.log(collect(), pending()); // 1 0
}
```
