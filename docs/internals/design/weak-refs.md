# Weak references without a tracing collector

Status: implemented. Issue: [#823](https://github.com/velt-lang/velt/issues/823).

## Summary

`WeakMap`, `WeakSet` and `WeakRef` need a reference that does not keep its target alive and can
tell whether the target is gone. Velt has counted objects and no tracing collector, so it builds
that from a flag in the count word, one process-wide side table, and an **ephemeron rule**: a
bounded trial deletion on release that frees an object and a map value referring back to it,
the `raw -> proxy` caches of reactive frameworks. A program that never names a weak collection
compiles exactly as it would without them.

The core is std's, written in Velt (`std/lang/hooks/weak*.velt`), with white-box tests and a
leak soak test (`weak_*.test.velt`, run by `velt test std/`); the ABI is in
[rt_abi.md](../contracts/rt_abi.md#weak-references). The classes are
`std/prelude/weak.velt`, `weak_set.velt` and `weak_ref.velt`. `weak T` fields and variables, and
the collector for strong cycles, are designed in [cycles.md](cycles.md).

## Design

### Representation

A counted object is `[count: u64][value]`; the value pointer is the block address + 8
([semantics.md](semantics.md) "Which types are counted"). The core adds:

- **A flag in the count word.** Bit 63 (`RC_WEAK`, the sign bit) is set while the object is
  weakly held: a weak-map key, a `WeakRef` target, or a member of an ephemeron cycle (below).
  Counts never get near 2^63, so the bit costs no range, and retain stays `count += 1`.
- **One side table for the process**, behind a lock, from object address to a record: the maps
  the object is a key of, the entries whose value graph refers to it, its `WeakRef` slots, and two
  small counts used by the ephemeron rule (`hint`, `observed`). A weakly held object is used by
  one task at a time (its count is not atomic), but not on one thread: an async task resumes on
  any worker after an `await`, and a `shared` `Mutex` hands its value to whichever task locks it.
  So map ids, `WeakRef` slots and records mean the same on every thread. The lock is taken only
  on the cold paths (weak map and `WeakRef` operations, and releases of objects with `RC_WEAK`
  set); a program without weak collections never takes it.
- **Weak maps** are hash tables of the core's, keyed by object address (the key is not counted),
  with the value's retain, release and trace glue given at creation. A value word of a map with
  release glue is 0 (`null`, `undefined`) or a counted object pointer: the compiler boxes strings
  and unions stored as such values. A `WeakSet` is a weak map whose values are plain words. A
  **`WeakRef`** is a slot holding the target's address, cleared when the target is freed.

### The core

The core is Velt code over raw memory (`std/lang/hooks/weak*.velt`). Its blocks are the C
library's (the `#weak-os` modules, apart from the executor's, so a program without tasks loads
none of them), outside the leak counters. One header for the process, in a static made on first
use, holds the lock word, the records, the tables of map blocks and `WeakRef` slots, and the
trial's graph. The lock is held for a few table operations at a time and nothing waits with it
held, so a thread that finds it taken spins a while, then gives its core away (`sched_yield`,
`SwitchToThread`) until it is free.

- **Records** sit in an open-addressing table keyed by address, in place. Each list a record has
  (the maps keyed by the object, the entries it belongs to, its `WeakRef`s) keeps its first item
  in the record and spills to a vector, so a key in one map with one `WeakRef` allocates nothing
  of its own.
- **A map** is a block with its glue and an open-addressing table of entries: the value word and
  the entry's contributions (a vector of `(object, references, trace)`, or none). A new key's
  entry, and a new object's record, take one probe, which finds the slot to insert at.
- **The values to release** are queued in the header under the lock, the first in two fields and
  the rest in a vector, and taken out before the lock is given back, so the common release of one
  value allocates nothing.
- **The trial's graph** is the process's (it is used only under the lock), with arrays sized for
  the 256-object bound: an object is found by a scan up to 16 objects and through a hash index
  past that, and only the edges grow.

**Compiled once.** The core is a prebuilt hook module ([std-object.md](std-object.md) "Prebuilt
hook modules"): `velt cache warm` compiles it into one object of the build cache, and a native
build that finds the object calls its entry points (`velt_hook_weak_*`) and checks, lowers and
optimizes none of the core. Its state is its own (the header, a static atomic, and three
thread-local values: the lock flag and the cascade's), kept in the object; the program defines the
thread-local values' storage, as for any object linked into it. A program that loads one of the
core's modules itself (its white-box tests) calls no object. An allocation that fails stops the
program, as the allocator of generated code does: there is no panic, so none leaves the lock held,
and no entry point takes its caller's location. WebAssembly, and a native build before the cache
is warm, compile the core with the program.

### Keys

A key, `WeakSet` member or `WeakRef` target is an object: the key type satisfies `object`
(`new WeakMap<string, V>()` is "type `string` does not satisfy the constraint `object`"). A class
instance, an object type's value and an interface value qualify; the key of an interface value is
the object behind it, whichever interface or type it is used through
(`tests/golden/std/weakmap_interface.velt`). Through `object` or `any`, a value that cannot be
weakly held (a primitive, or a tuple or function value the conversion boxed) throws
`TypeError: Invalid value used as weak map key` at run time.

### Weak maps and `WeakRef`

- `set(k, v)` records `k` in the side table (setting its flag), stores `v` (the map owns one
  reference), and marks the entry for the ephemeron examination below when the map's values can
  refer to objects.
- `get`, `has` and `delete` are hash probes. `get` returns the value counted (retained with the
  map's value glue; the caller releases it), like `deref()`: a later release of a weakly held
  object may run a trial that deletes the entry and frees its value, so a borrowed result would
  not survive `const p = cache.get(raw); raw = null; use(p)`.
- When a key's last reference goes, the cold release removes its entries from every map and
  releases their values, then the object is freed as usual. Dropping a map releases its values
  and unmarks keys held by no other map.
- `deref()` returns the target counted, or `undefined` once it is freed. Liveness is exact: a
  `WeakRef` whose target has no other reference is cleared at once, earlier than Node, which
  clears it at some later collection. Node's guarantee that a target derefed in a job stays alive
  until the job ends holds because `deref()` returns a counted reference, which the compiler
  keeps until the end of the job.

### The ephemeron rule

A reactive framework's `rawToProxy.set(raw, proxy)` (Vue's `reactiveMap`) maps an object to its
own proxy, and the proxy holds the object strongly. JavaScript frees both once nothing else
refers to either. With counts alone, the key's count never reaches zero while the proxy lives,
and the map keeps the proxy alive: every reactive object would leak. Velt keeps a back-reference
count per object and runs a bounded cycle check on release.

**On the key's first decrement after an insert,** the entry is examined (`examine`, called from
`weakRelease`): the core traces `v`'s graph through the compiler's trace glue (each counted
type's strong references; up to 256 objects, `LIMIT` in `std/lang/hooks/weak_graph.velt`) and
finds every object on a path from `v` back to `k`. For each, the entry records how many
references to it the entry holds: the map's reference to `v`, and the references from inside
`v`'s graph. Their sum over all entries is the object's `hint`, and the objects get the flag. An
entry whose value has no path back to `k` gets one more look, at the key's next decrement: the
decrement that triggered the examination may be the one that removed a path, which comes back
before the next. After that, or when the graph is past the limit, the entry counts as examined,
so a key's decrements trace each of its entries at most twice. A record counts its entries not
examined yet (`R_UNEXAMINED`) and keeps a threshold (`R_GATE`: at least `max(hint, observed)`,
the largest value while an entry is unexamined), so a decrement that leaves the count above it
goes on after one comparison.

Waiting for a decrement loses no cycle whose path is there at the key's first decrement or comes
back before its next: an entry whose value refers back to its key holds a reference to the key, so
the key's last outside reference goes by a decrement, never by the release that frees it. It saves
the trace of an entry whose key goes by its only release, and the trials that releases of a value
not examined yet would run (the value is not marked until then), and it sees a path the value gains
between the insert and that decrement. It costs a second lookup of the entry at the decrement:
against an examination at insert, `bench/weakmap_handlers_cycle.velt` runs 37.5% fewer instructions
and `bench/weakmap_key_decrements.velt` (keys decremented a thousand times each while their entries
live) 2.9% fewer. In `bench/weakmap_handlers.velt` (handlers over other objects) a key's one
release is its last, so no entry is traced: a call gets an argument where it is when the arguments
after it assign no variable it reads (`new Handler(() => other.n)` creates a closure that assigns
none; sequence.rs `writes_local`).

**On a release** that leaves a recorded object's count at or below `max(hint, observed)`, every
remaining reference may come from the cycle, so the cold path runs a **trial deletion** (Bacon
and Rajan's synchronous cycle collection, restricted to one small subgraph):

1. Collect the subgraph: the released object, everything reachable from it through strong
   references, the value of every entry whose key is in it (a key leads to its value), and the
   key of every entry an object in it belongs to. Stop and keep everything past 256 objects.
2. Count the references among them, the map's reference to each value counting as coming from
   its key.
3. An object whose count is higher is referenced from outside: it is live, and so is everything
   it reaches, including the values of its entries (a value is reachable if its key is).
4. Delete the entries of every key left dead. Releasing their values frees the cycle through the
   ordinary release paths.

When the trial finds the cycle live, it records for each live object on a path back to a key the
references it counted (`observed`), so the next release to that level tries again even when the
value graph changed after insert. Trials that releases cause while values are being released
wait until the outermost release ends, so a trial never sees a cycle half freed.

**Safety.** A trial only deletes entries of keys it found dead, and deleting an entry only gives
up the map's own reference. A wrong answer can therefore leak or drop an entry early, never free
memory that is still in use. Trace glue must report exactly the counted references an object
owns, each once; one it misses makes the target look referenced from outside (a leak), and one
it reports twice can make a live key look dead.

**Typed proxies** (`crates/velt_vir/src/lower/proxy_typed.rs`). A value of a proxiable type may
be its `Proxy` object's address with bit 0 set. The compiler never hands that word to the table
as a key or a released object (the core treats one as an `ICE:`): a map key is the untagged
`Proxy` object, a weak map value of a proxiable type is boxed, and a `WeakRef` slot keeps the
word as given (the record and the count are the untagged object's), so `deref` gives the proxy
back. Trace glue visits a typed proxy as its `Proxy` object, whose own trace glue visits its
target when the target's `any` table is that of a type the program converts proxies to (others
are opaque). When `object` is also a weak key, the field walk already visits the target through
its weak word, so the `Proxy` glue's own visit runs only for a target whose weak word is 0, and
no target is visited twice. A map whose key type is proxiable has no key trace, since a key may be
a `Proxy` object or a key-type object; each key's record gets its trace when the value graph
reaches it, and the trial uses the record's trace for such keys.

## Compilation

### Rules the compiler keeps

- **Uniqueness tests compare the whole word.** A test of whether a weak-capable object is unique
  (copy-on-write, in-place transfer, `release` itself) compares the whole count word with 1. A
  weakly held object's word is never exactly 1 (`RC_WEAK` is set), so it is never taken as
  unique; masking the flag off first would be wrong.
- **Every raw decrement site uses the weak-capable sequence.** Code that decrements a count
  without the release sequence (`c - 1` where it knows the count is above 1) would leave
  `RC_WEAK | 0` behind for a weakly held object. For weak-capable types each such site emits the
  release sequence instead: `crates/velt_vir/src/lower/rc.rs`, `lower/glue/drop_chain.rs`
  (`take`, which decrements when the count is not 1) and `lower/glue/transfer.rs` (releasing the
  sender's reference after a deep copy). Closure environments and captured variables' cells may
  be weakly held in a program with weak references, since the trace glue looks into function
  values: their releases then take the release sequence (`Boxing::weak_envs`), and
  `lower/glue/transfer_env.rs` gives up a shared environment through its own drop entry instead
  of decrementing it.
- **Weak handles belong to one task.** A `WeakMap`, `WeakSet` or `WeakRef` (a class instance with
  a plain count) stays with the task that made it (see "Threads and `shared`"). The task itself
  may run on several threads in turn, which the process-wide table allows. A weakly held object
  is never transferred in place either (its count word is never 1, so the transfer deep-copies
  it, and the copy is not weakly held). An object with `RC_WEAK` set and no record is an `ICE:`
  that `weakRelease` reports before it touches the count word.
- **Weak-capable types are never atomic.** The flag lives in a plain count word, which only the
  task using the object changes; the table's lock protects the table, not the objects.
- **Lock discipline.** No generated code that could reach the table runs while it is locked: value
  retains and releases run after the lock is released, and trace glue, which runs under the lock
  during a trial, only reads fields and records them. A re-entry would deadlock, so the core reports
  it as an `ICE:` instead. Cascades and the trials they defer (`CASCADE`, `PENDING`, thread-local
  values of `std/lang/hooks/weak.velt`) are per thread: a cascade is synchronous drop glue, which
  never awaits, so it starts and ends on one thread; two threads may run cascades at once over
  objects of different tasks.

### Weak-capable types and release

The compiler decides program-wide which types are **weak-capable**
(`crates/velt_vir/src/lower/boxing/weak.rs`), with the counted types (`lower_program`'s fixed
point): the key types of `WeakMap`s and `WeakSet`s and the target types of `WeakRef`s are counted
(an object type used as one is counted for its identity), and they, the counted types a weak
map's value refers to, and everything the trace glue reaches from those through fields, elements
and payloads stored inline are weak-capable. Classes are weak-capable per hierarchy (the root
class), since a release through a base class type releases a subclass object. A program that
never names a weak collection has none, and its VIR is unchanged
(`crates/veltc/tests/bench_programs.rs` checks every benchmark for it).

For weak-capable types only, `release_as` (`lower/rc.rs`) changes the release's first test from
`c == 1` to a signed compare, which works because `RC_WEAK` is the sign bit:

```text
c = *rc
if c as i64 > 1 { *rc = c - 1 }                                              // shared path
else if c == 1  { drop the fields; free }                                    // unique path
else            { if weakRelease(obj) { drop the fields; free } }           // RC_WEAK: cold
```

A weakly held object's count word is negative as a signed number, so it leaves the shared path
with the unique case and goes to the cold call, which removes the object from every map and
`WeakRef` before the generated code frees it. The sequence is used at every release (drop glue,
boxed values, half-built objects, the queued drops of `drop_depth.rs`), at `drop_chain.rs`'s
`take` and at `transfer.rs`'s release of the sender's reference after a deep copy.

**`object` keys** (`lower/weak_object.rs`): once `object` is a weak key or `WeakRef` target,
every concrete type a dynamic value holds (each type an `any` table is built for) is counted and
weak-capable, and every `any` table gets a weak word: 0 when the value cannot be weakly held (a
primitive, `null`, an object the conversion boxed: a tuple, a function value), else 1 or the
type's trace glue. The key is the dynamic value's data pointer; `set` passes the key's trace glue
(`weakMapSetTraced`), the trace glue of a value holding a dynamic value visits its data
pointer through the same word (so `WeakMap<object, object>` caches are ephemerons too), and a
`WeakRef<object>` keeps the target's descriptor with its slot (`weakRefNewWith`) so
`deref()` rebuilds the `object`. Other programs are unchanged.

**Trace glue** (`lower/glue/trace.rs`) visits the counted objects an object refers to through its
fields, elements and payloads stored inline. A function value is visited through its environment
(`lower/glue/trace_env.rs`): the value's glue compares the environment's drop entry with those of
the closures made at the value's function type (`Boxing::env_traced`) and visits the environment
with the matching closure's trace (`Work::EnvTrace`), which walks the same captures as its drop,
captured variables' cells included; an environment that matches none (one the runtime made, or one
whose drop entry a function property replaced) is left out. An uncounted object's fields are walked
as its owner's, since its owner holds the only reference. A class in a hierarchy with a vtable may
be any class of its hierarchy, with fields its static type lacks, so its glue calls the glue its
object's class table holds (`SLOT_TRACE`: `Glue::ObjTrace`, the walk of exactly that class's fields,
`lower/glue/trace_dyn.rs`). An interface value's glue compares its table with those the program
built for a class (`Boxing::iface_traced`) and visits the object with that class's glue; a table
that matches none (a boxed value's, such as a struct's) is left out. A generator object's glue
traces its state through `f$trace` in the object's table (`lower/async_fn/gen_trace.rs`): the poll
function has a case `TRACE_BIT | k` (`0x2000_0000`) for each `yield` `k` and one for the state
before the start, which visits what the close path of `k` would drop (the same drop entries and
flags), with `cx` pointing to the glue's `(visit, ctx)`; `f$trace` skips a running, finished or
closing generator (one whose tag has `DROP_BIT` or `CLOSE_BIT`). `shared` values are not looked
into. So a glue never reads a field the dynamic type does not have. `lower/weak.rs` lowers the weak
intrinsics and map value words.

### Threads and `shared`

Sema rejects a weak collection, or a value holding one, where it would cross to another thread:

- a `spawn` argument (each branch's call of `spawn(c ? f(x) : g(y))`,
  `crates/velt_sema/src/body/expr/tasks.rs` `spawned_calls`), the result of a spawned task or the
  error it throws, a channel message, a capture of an async closure that may run on another
  thread, the value of `shared(...)` and the receiver of `clone()`;
- inside a `Mutex`'s `with` callback, a weak collection from outside the lock: a capture holding
  one (`body/expr/sync.rs` `no_weak_in_lock`), one captured by a closure the callback calls,
  found through the function values `with` resolves (`ownership/locked` `weak_across`), and a
  callback result that holds one or is a function value whose closure made under the lock
  captures one. A callback not written at the `with` (a generic helper's `m.with(f)`) has its
  result checked the same way.

The table's lock protects the table, not the objects: a key from the locked value in a map of the
caller's would be released by the next task to replace it, on its thread, together with the
entry's value that the caller still uses (`tests/golden/std/weakmap_mutex_value.velt`), and a
`WeakRef` taken out of `with` would hand out a strong reference to an object the `Mutex` owns
(`weakref_mutex.velt`). A weak collection made under the lock and dropped there is allowed
(`weakmap_mutex_local.velt`). A weak-capable object that is not weakly held may cross threads (it
is copied when shared, and the copy is not weakly held).

These checks see the written type (`Ctx::holds_weak`). What it hides is found after all bodies
by `crates/velt_sema/src/weak_threads.rs`, over the whole program, for the same crossings: an
interface value holds a weak collection when some class converted to the interface anywhere
(`ToDyn`) holds one, a class value when a subclass upcast to it does (a fixed point, since a
class can hold an interface value). A crossing whose type mentions a type parameter is checked at
every instantiation, propagated to the callers with the calls' type arguments like
`promise_copies.rs`'s copies (`tests/golden/std/weak_hidden_generic_*.velt`), and so are
conversions to an interface in generic code. A closure literal passed to another task, or stored
into the locked value (`ownership/locked/stores.rs`), is checked by its captures. An interface
none of whose converted classes holds a weak collection is not affected
(`weak_hidden_ok_iface_plain.velt`). The rule is program-wide: once any weak-holding class is
converted to an interface, every value of that interface counts as holding a weak collection
where it crosses threads, and the diagnostic's note names the class.

**The run-time backstop.** A function value whose closure captured a weak collection but which
is not a closure literal where it crosses (made by a factory, held in a field or a variable:
`spawn(run(mk(keep)))`, `setTimeout(task(keep), 0)`, `ch.send(mk(keep))`) has captures its type
does not show. Its transfer reaches the captured `WeakMap`, `WeakSet` or `WeakRef`, whose transfer
glue panics before anything crosses (`crates/velt_vir/src/lower/glue/transfer.rs`
`transfer_expand`):

```text
a `WeakRef` cannot go to another thread (`spawn`, a channel, `shared(...)` or a `Mutex`'s value),
also inside a function value that captured it: it belongs to the task that made it; make it on
the task that uses it
```

The weak classes are not counted, but `Cx::holds_counted` counts them as having something to
transfer so the glue is reached; values holding no weak class get the same transfer glue as
without them (`bench/vir_diff.sh`). The transfer goes by the object's own class: a class in a
hierarchy with a vtable may be any subclass, so `holds_counted` takes it as having something to
transfer, and the glue dispatches through the vtable to the subclass's fields
(`tests/golden/std/weakref_transfer_*.velt`). A class that extends a weak collection
(`class Cache extends WeakMap<K, V>`) is one in every check: sema's `Ctx::is_weak_class` and
lowering's `Cx::weak_class` go by the root class (`weak_subclass*.velt`). A function value made
shared between threads (`shared(...)`, a `Mutex`'s value, an HTTP handler) panics the same way.
`async main` keeps its maps across `await`s, and spawned tasks use maps of their own
(`weakmap_async.velt`).

### User code a release runs

Releasing a weak map's key drops its entries' values, and dropping a map drops all of them,
running their `[Symbol.dispose]()` without a call in the program. The replaced-holder analysis
(`crates/velt_sema/src/replaced_holders/implicit.rs`, entry 8) counts a value of a key type (found
from the program's `new WeakMap<K, V>()`) as reaching what its maps' values reach, and a
`WeakMap<K, V>` as reaching what a `V` does
(`tests/golden/lang/errors/assign_target_replaced_holder_weak.velt`).

## Runtime cost

| Object | Cost |
|---|---|
| Type never weakly held | none: the same code as without weak collections |
| Weak-capable type, object not weakly held | the signed compare: nothing on a shared release, two instructions per free |
| Object weakly held | a call and a side-table probe on each release |
| Freeing a weakly held object | the above, plus removing its entries and clearing its `WeakRef`s |

Measured with valgrind (callgrind's per-function counts, loops inlining each release sequence,
release build, Linux x86_64, 10^6 iterations), instructions per iteration of the loop:

| Loop | Plain release | Weak-capable, signed compare | Weak-capable, separate bit test |
|---|---|---|---|
| retain + release (shared path) | 10.00 | 10.00 | 14.00 |
| allocate + release (drop, free) | 93.69 | 95.69 | 93.69 |

The signed compare leaves the shared release as it is and costs two instructions per free (+2.1%
on the allocate/free loop, which is mostly mimalloc); a separate bit test after `c == 1` would
keep the free path unchanged but cost four per shared release. A release of a weakly held object
is a call of `weakRelease` and a probe of the records' table.

A recorded object whose count sits at its hint because the outside reference is elsewhere in the
cycle (a raw object held only by its proxy, while the program holds the proxy) runs a trial on
every release back to that level, such as the release of a temporary reference in a proxy trap.
Each trial is bounded (a handful of objects), but it is a hash-table walk on a path that is
otherwise a decrement.

### Tests

The white-box tests (`std/lang/hooks/weak_ephemeron.test.velt`, on hand-built counted objects
whose glue is written in Velt, `weak_test_objects.velt`) release the outside references in every
order:

- a raw object and its proxy: both freed;
- the key kept alive outside: the proxy stays (`signal(raw) === signal(raw)`), both freed with it;
- the value kept alive outside: the key stays, both freed with it;
- chains of entries (`A -> B` with B holding A, `B -> C` with C holding B), holding any link;
- nested reactive objects (`raw1.child = raw2`, both proxied in the same cache);
- a handler whose closure captures the target (an object on the path back to the key);
- a shared handler outside the cycles, a key mapped to itself, 10,000 entries;
- a value that gains a second path to its key after insert (found by the next trial);
- null values and values of maps without trace glue next to a cycle (`weak_ops.test.velt`);
- a `get` result held across the release of its key's last outside reference.

`weak_threads.test.velt` releases weakly held objects and an ephemeron cycle in spawned tasks (on
any of the executor's workers), runs eight tasks with maps of their own at once, releases values
that release other maps' keys or drop other maps (nothing re-enters the lock), and checks that an
object marked without a record is an `ICE:` reported before the count changes. The goldens
`weakmap_async` and `weakmap_nested` do the same from Velt programs; `weakmap_mutex`,
`weakmap_mutex_call`, `weakmap_mutex_value`, `weakref_mutex`, `weakref_mutex_closure` and
`weakref_mutex_result` check that a weak collection cannot cross a `with` lock.

Its soak test creates and drops reactive objects (raw, handler capturing it, proxy; a
nested proxied child every third time) through a window of live ones, and checks after every step
that the live objects, side-table records and map entries are exactly the window's, and that the
side table never holds more records than there are live objects. It passes 20,000 reactive
objects through a window of 64.

## Compatibility

A `WeakRef` is cleared as soon as its target has no other reference, and a weak map entry goes
when its key does; Node clears both at some later garbage collection. A program cannot tell the
difference except by observing that a `deref()` returns `undefined` earlier. Weak collections
cannot cross threads; Node has one thread per isolate.

## Limitations

- **A value graph past 256 objects** at its examination or during a trial: nothing is recorded
  or freed; the pair leaks until the entry is deleted or the map dropped (test
  "keeps a pair whose value graph is past the limit"). The bound keeps every release cheap.
- **A path that comes back after the second look.** A value that loses its path back to the key
  before the key's first decrement, and gets it back only after the key's next one, is not seen
  by either examination; the pair is kept until the entry is deleted or the map dropped (test
  "keeps a pair whose path came back after the second look"). The bound keeps a key that is
  passed around while its entry lives from tracing its value at every decrement.
- **References to the key added after the last trial through it**, when the final outside
  release lands on an object whose count is still above what the table knows: no trial runs and
  the pair is kept until a release that reaches the known level, or the entry goes (test
  "delays the free of a path added after the last trial").
- **Strong cycles through the key** (`raw.self = proxy(raw)`): the trial frees the entry, but raw
  and the proxy still hold each other. That is an ordinary strong cycle, which
  [cycles.md](cycles.md) covers; the core does not collect cycles no weak map takes part in.
- **Ephemeron cycles across maps.** `m1.set(a, b); m2.set(b, a)` with no other references
  between `a` and `b`: each key is kept alive only by the other map's value. The examination
  records a path from a value back to its own entry's key only, so neither entry has a
  hint and no trial runs; the pair is kept until an entry is deleted or a map dropped.
- **Opaque trace glue.** A cycle through an interface value holding a boxed value (a struct
  implementing the interface), an async generator suspended at an `await`, a running generator,
  or a drop entry the generator's trace does not look into (a value a pattern took parts of, a
  generator held inline in another's state, an array a `for...of` consumes) is not seen by a
  trial; it is kept until its map entry goes, or until a later release finds the path.
- **A weak map held only by its own cycle** (a cache kept in a proxy handler's closure): the
  trial treats every map as live, so entries whose keys are dead are still deleted, but a map
  that is itself garbage is freed only when its holder is.
