# Design: weak references without a collector

Status: `WeakMap`, `WeakSet` and `WeakRef` are in the language (#823 step B); `weak T` fields
(#11) are not yet. The core is in `velt_rt` (`crates/velt_rt/src/weak/`) with unit tests and a
leak soak test; the ABI is in [rt_abi.md](../contracts/rt_abi.md#weak-references). The compiler's
side is `std/prelude/weak.vlt` (the classes), `velt_vir` `lower/boxing/weak.rs` (which types are
weak-capable), `lower/weak.rs` (the intrinsics and map value words) and `lower/glue/trace.rs`
(trace, retain and release glue).

`WeakMap`, `WeakSet`, `WeakRef` (#823) and `weak T` fields (#11) all need the same thing: a
reference that does not keep its target alive and can tell whether the target is gone. This
note describes that core, how it fits the counted-object layout, what it costs, and the
ephemeron rule that keeps `raw -> proxy` caches from leaking.

## Representation

A counted object is `[count: u64][value]`; the value pointer is the block address + 8
([semantics-stage2.md](semantics-stage2.md) §3). The core adds:

- **A flag in the count word.** Bit 63 (`RC_WEAK`, the sign bit) is set while the object is
  weakly held: a weak-map key, a `WeakRef` target, or a member of an ephemeron cycle (below).
  Counts never get near 2^63, so the bit costs no range, and retain stays `count += 1`.
- **One side table for the process**, behind a lock, from object address to a record: the
  maps the object is a key of, the entries whose value graph refers to it, its `WeakRef` slots,
  and two small counts used by the ephemeron rule (`hint`, `observed`). A weakly held object is
  used by one task at a time (its count is not atomic), but not on one thread: an async task
  resumes on any worker after an `await`, and a `shared` `Mutex` hands its value to whichever
  task locks it (a `with` callback may key a map of its own on the value and drop it there). So
  map ids, `WeakRef` slots and records mean the same on every thread. The
  lock is taken only on the cold paths (weak map and `WeakRef` operations, and releases of
  objects with `RC_WEAK` set); a program without weak collections never takes it.
- **Weak maps** are runtime-owned hash tables keyed by object address (the key is not counted),
  with the value's retain, release and trace glue given at creation. A value word of a map with
  release glue is 0 (`null`, `undefined`) or a counted object pointer: the compiler boxes strings
  and unions stored as such values. A `WeakSet` is a weak map whose values are plain words. A **`WeakRef`** is a slot holding the target's address, cleared when
  the target is freed.

## Release paths and their cost

The compiler decides program-wide which types are **weak-capable**: the types used as weak-map
keys, `WeakRef` targets or `weak T` fields, plus every type reachable from a weak map's key and
value types (the objects an ephemeron trial may meet). For those types only, release changes its
first test from `c == 1` to a signed compare, which works because `RC_WEAK` is the sign bit:

```text
c = *rc
if c as i64 > 1 { *rc = c - 1 }                                              // shared path
else if c == 1  { drop the fields; free }                                    // unique path
else            { if velt_rt_weak_release(obj) { drop the fields; free } }  // RC_WEAK: cold
```

A weakly held object's count word is negative as a signed number, so it leaves the shared path
with the unique case and goes to the cold call, which removes the object from every map and
`WeakRef` before the generated code frees it. Every other type keeps today's two-way release,
and a program without `WeakMap`, `WeakSet`, `WeakRef` or `weak` has no weak-capable types: it
compiles exactly as today.

| Object | Cost |
|---|---|
| Type never weakly held | none: same code as today |
| Weak-capable type, object not weakly held | the signed compare: nothing on a shared release, two instructions per free (measured below) |
| Object weakly held | a call and a side-table probe on each release |
| Freeing a weakly held object | the above, plus removing its entries and clearing its `WeakRef`s |

Measured with valgrind (`crates/velt_rt/scripts/weak_rc_cost.sh`, callgrind's per-function
counts, loops in `src/weak/tests/bench.rs` inlining each release sequence, release build, Linux
x86_64, 10^6 iterations), instructions per iteration of the loop:

| Loop | Today's release | Weak-capable, signed compare | Weak-capable, separate bit test | Weakly held object |
|---|---|---|---|---|
| retain + release (shared path) | 10.00 | 10.00 | 14.00 | 88.00 |
| allocate + release (drop, free) | 93.69 | 95.69 | 93.69 | — |

The signed compare leaves the shared release as it is and costs two instructions per free (+2.1%
on the allocate/free loop, which is mostly mimalloc); a separate bit test after `c == 1` keeps the
free path unchanged but costs four per shared release, so the signed compare is the proposal.
Neither touches types that are not weak-capable, which is every type of every program today:
their code is unchanged, so `bench/` is unaffected. A release of a weakly held object is a call
and a hash probe, 78 instructions more than today's.

## Rules for the compiler

The runtime relies on these; the compiler must keep them when it lowers weak references
(#823 step B):

- **Uniqueness tests compare the whole word.** A test of whether a weak-capable object is unique
  (copy-on-write, in-place transfer, `release` itself) compares the whole count word with 1. A
  weakly held object's word is never exactly 1 (`RC_WEAK` is set), so it is never taken as
  unique; masking the flag off first would be wrong.
- **Every raw decrement site uses the weak-capable sequence.** Code that decrements a count
  without the release sequence (`c - 1` where it knows the count is above 1) would leave
  `RC_WEAK | 0` behind for a weakly held object. For weak-capable types each such site emits the
  sequence above instead: today's sites are `velt_vir/src/lower/rc.rs`,
  `lower/glue/drop_chain.rs` (`take`, which decrements when the count is not 1),
  `lower/glue/transfer.rs` (releasing the sender's reference after a deep copy) and
  `lower/glue/transfer_env.rs` (the same for a closure environment).
- **Weak handles belong to one task.** A `WeakMap`, `WeakSet` or `WeakRef` (a class instance
  with a plain count) stays with the task that made it: sema rejects them in transfers (spawn
  arguments and captures, channel sends, promise results), or the transfer glue panics. The
  task itself may run on several threads in turn, which the process-wide table allows. A
  weak-capable object that is weakly held is never transferred in place either (its count word
  is never 1, so the transfer deep-copies it, and the copy is not weakly held). An object with
  `RC_WEAK` set and no record is an `ICE:` that `velt_rt_weak_release` reports before it touches
  the count word.
- **Weak-capable types are never atomic.** The flag lives in a plain count word, which only the
  task using the object changes; the table's lock protects the table, not the objects.
- **Lock discipline.** No generated code that could reach the table runs while it is locked:
  value retains and releases run after the lock is released (the cascade below), and trace glue,
  which runs under the lock during a trial, only reads fields and records them. A re-entry would
  deadlock, so the runtime reports it as an `ICE:` instead. Cascades and the trials they defer
  (`CASCADE`, `PENDING`) are per thread: a cascade is synchronous drop glue, which never awaits,
  so it starts and ends on one thread; two threads may run cascades at once over objects of
  different tasks.

### What the compiler does

- **Weak-capable types** are found with the counted types (`lower_program`'s fixed point): the
  key types of `WeakMap`s and `WeakSet`s and the target types of `WeakRef`s are counted (an
  object type used as one is counted for its identity), and they, the counted types a weak
  map's value refers to, and everything the trace glue reaches from those through fields,
  elements and payloads stored inline are weak-capable. Classes are weak-capable per hierarchy
  (the root class), since a release through a base class type releases a subclass object. A
  program that never names a weak collection has none, and its VIR is unchanged
  (`crates/veltc/tests/bench_programs.rs` checks every benchmark for it).
- **Release.** `release_as` in `lower/rc.rs` takes the three-way sequence above for those
  types, at every release (drop glue, boxed values, half-built objects, the queued drops of
  `drop_depth.rs`), at `drop_chain.rs`'s `take` and at `transfer.rs`'s release of the sender's
  reference after a deep copy (a full release there). `transfer_env.rs` decrements closure
  environments, which are never weak-capable: function values are opaque to the trace glue.
- **Trace glue** (`glue/trace.rs`) visits the counted objects an object refers to through its
  fields, elements and payloads stored inline; uncounted objects, function and interface values
  and `shared` values are not looked into, and classes in a hierarchy with a vtable are opaque
  (null glue), so a glue never reads a field the dynamic type does not have.
- **Threads and `shared`.** Sema rejects a weak collection (or a value holding one) as a `spawn`
  argument (each branch's call of `spawn(c ? f(x) : g(y))`, `tasks.rs` `spawned_calls`), the
  result of a spawned task or the error it throws (both copied for the awaiting task's thread,
  checked by `weak_threads.rs` with the hidden forms below), a channel message, a capture of an async closure that may run on another thread,
  the value of `shared(...)` and the receiver of `clone()`. A weak-capable object that is not
  weakly held may still cross threads (it is copied when shared, and the copy is not weakly
  held). A `Mutex`'s `with` callback cannot use a weak collection from outside the lock: a
  capture holding one is an error (`body/expr/sync.rs` `no_weak_in_lock`), and so is one
  captured by a closure the callback calls, found through the function values `with` resolves
  (`ownership/locked` `weak_across`; a function value it cannot resolve is already an error
  there). Nor can the callback's result hold one, or be a function value whose closure made under
  the lock captures one. The table's lock protects the table, not the objects: a key from the
  locked value in a map of the caller's would be released by the next task to replace it, on its
  thread, together with the entry's value that the caller still uses
  (`tests/golden/std/weakmap_mutex_value.vlt`), and a `WeakRef` taken out of `with` would hand
  out a strong reference to an object the Mutex owns (`weakref_mutex.vlt`). A weak collection
  made under the lock and dropped there is allowed (`weakmap_mutex_local.vlt`). Interface calls
  under the lock already get a copy of a part of the value (#398).
  These checks see the written type (`Ctx::holds_weak`). What it hides is found after all bodies
  by `weak_threads.rs`, over the whole program, for the same crossings (`with`'s result and the
  callback's captures, `shared(...)` values, `spawn` arguments, channel messages, async-closure
  captures): an interface value holds a weak collection when some class converted to the
  interface anywhere (`ToDyn`) holds one, a class value when a subclass upcast to it does (a
  fixed point, since a class can hold an interface value); a crossing whose type mentions a type
  parameter is checked at every instantiation, propagated to the callers with the calls' type
  arguments like `promise_copies.rs`'s copies (`weak_hidden_generic_*.vlt`), and so are
  conversions to an interface in generic code. A callback not written at the `with` (a generic
  helper's `m.with(f)`) has its result checked by `weak_across` too
  (`weakref_mutex_generic.vlt`). A closure literal passed to another task, or stored into the
  locked value (`ownership/locked/stores.rs`), is checked by its captures: at run time either
  would panic (a function value's environment is opaque to the transfer). Plain interface use
  stays legal: an interface none of whose converted classes holds a weak collection is not
  affected (`weak_hidden_ok_iface_plain.vlt`). The other way round, the rule is program-wide:
  once any weak-holding class is converted to an interface, even one used on a single task,
  every value of that interface counts as holding a weak collection where it crosses threads
  (the diagnostic's note names the class).
  **The run-time backstop (a panic, not a compile error):** a function value whose closure
  captured a weak collection but which is not a closure literal where it crosses (made by a
  factory, held in a field or a variable: `spawn(run(mk(keep)))`, `setTimeout(task(keep), 0)`,
  `ch.send(mk(keep))`) has captures its type does not show. Its transfer reaches the captured
  `WeakMap`, `WeakSet` or `WeakRef`, whose transfer glue panics ("a `WeakRef` cannot go to another
  thread …", velt_vir `glue/transfer.rs` `transfer_expand`) whether the environment is moved (the
  only reference) or copied: the weak classes are not counted, but `Cx::holds_counted` counts
  them as having something to transfer so the glue is reached. Values holding no weak class get
  the same transfer glue as before (`bench/vir_diff.sh`: the benchmarks' VIR is unchanged). So
  the panic happens before anything crosses: no race
  (`tests/golden/std/weakref_transfer_{spawn,field,timer,channel}.vlt`). The transfer goes by the
  object's own class, not the written one: a class in a hierarchy with a vtable may be any
  subclass, so `holds_counted` takes it as having something to transfer, like an interface
  value, and the glue dispatches through the vtable to the subclass's fields
  (`weakref_transfer_base_{class,result}.vlt`). A class that extends a weak collection
  (`class Cache extends WeakMap<K, V>`) is one in every check: sema's `Ctx::is_weak_class` and
  lowering's `Cx::weak_class` go by the root class (`weak_subclass*.vlt`). A function value made
  shared between threads (`shared(...)`, a `Mutex`'s value, an HTTP handler) panics in the
  many-threads check the same way. `async main` keeps its maps across `await`s and spawned tasks
  use maps of their own at once (`weakmap_async.vlt`).
- **User code a release runs** (#876, `crates/velt_sema/src/replaced_holders/implicit.rs` entry
  8): releasing a weak map's key drops its entries' values, and dropping a map drops all of them,
  running their `[Symbol.dispose]()` without a call in the program. The analysis counts a value
  of a key type (found from the program's `new WeakMap<K, V>()`) as reaching what its maps'
  values reach, and a `WeakMap<K, V>` as reaching what a `V` does
  (`tests/golden/lang/errors/assign_target_replaced_holder_weak.vlt`).

## Weak maps and `WeakRef`

- `set(k, v)` records `k` in the side table (setting its flag), stores `v` (the map owns one
  reference), and runs the insert-time ephemeron analysis below when the map's values can refer
  to objects.
- `get`, `has` and `delete` are hash probes. `get` returns the value counted (retained with the
  map's value glue; the caller releases it), like `deref()`: any later release of a weakly held
  object may run a trial that deletes the entry and frees its value, so a borrowed result would
  not survive `const p = cache.get(raw); raw = null; use(p)`.
- When a key's last reference goes, the cold release removes its entries from every map and
  releases their values, then the object is freed as usual. Dropping a map releases its values
  and unmarks keys held by no other map.
- `deref()` returns the target counted (or null once freed). Liveness is exact: a `WeakRef` whose
  target has no other reference is cleared at once, earlier than Node, which clears it at some
  later collection (owner decision on #823). Node's guarantee that a target derefed in a job
  stays alive until the job ends holds because `deref()` returns a counted reference, which the
  compiler keeps until the end of the job.

## The ephemeron rule

sigx's `rawToProxy.set(raw, proxy)` (and Vue's `reactiveMap`) maps an object to its own proxy,
and the proxy holds the object strongly. JavaScript frees both once nothing else refers to
either. With counts alone, the key's count never reaches zero while the proxy lives, and the
map keeps the proxy alive: every reactive object would leak. Owner decision 3 on #823: a
back-reference count plus a bounded cycle check on release.

**At insert,** `set(k, v)` traces `v`'s graph through the compiler's trace glue (each counted
type's strong references; up to 256 objects) and finds every object on a path from `v` back to
`k`. For each, the entry records how many references to it the entry holds: the map's reference
to `v`, and the references from inside `v`'s graph. Their sum over all entries is the object's
`hint`, and the objects get the flag.

**On a release** that leaves a recorded object's count at or below `max(hint, observed)`, every
remaining reference may come from the cycle, so the cold path runs a **trial deletion**
(Bacon and Rajan's synchronous cycle collection, restricted to one small subgraph):

1. Collect the subgraph: the released object, everything reachable from it through strong
   references, the value of every entry whose key is in it (a key leads to its value), and the
   key of every entry an object in it belongs to. Stop and keep everything past 256 objects.
2. Count the references among them, the map's reference to each value counting as coming from
   its key.
3. An object whose count is higher is referenced from outside: it is live, and so is everything
   it reaches, including the values of its entries (the ephemeron rule: a value is reachable
   if its key is).
4. Delete the entries of every key left dead. Releasing their values frees the cycle through the
   ordinary release paths.

When the trial finds the cycle live, it records for each live object on a path back to a key the
references it counted (`observed`), so the next release to that level tries again even when the
value graph changed after insert. Trials that releases cause while values are being released
wait until the outermost release ends, so a trial never sees a cycle half freed.

**Safety.** A trial only ever deletes entries of keys it found dead, and deleting an entry only
gives up the map's own reference. A wrong answer can therefore leak or drop an entry early, never
free memory that is still in use. Trace glue must report exactly the counted references an object
owns; one it misses makes the target look referenced from outside (a leak).

### What it handles

The unit tests (`src/weak/tests/ephemeron.rs`) release the outside references in every order:

- a raw object and its proxy: both freed;
- the key kept alive outside: the proxy stays (`signal(raw) === signal(raw)`), both freed with it;
- the value kept alive outside: the key stays, both freed with it;
- chains of entries (`A -> B` with B holding A, `B -> C` with C holding B), holding any link;
- nested reactive objects (`raw1.child = raw2`, both proxied in the same cache);
- a handler whose closure captures the target (an object on the path back to the key);
- a shared handler outside the cycles, a key mapped to itself, 10,000 entries;
- a value that gains a second path to its key after insert (found by the next trial);
- null values and values of maps without trace glue next to a cycle (`src/weak/tests/values.rs`);
- a `get` result held across the release of its key's last outside reference.

`src/weak/tests/threads.rs` releases weakly held objects and an ephemeron cycle on another
thread than the one that made them, runs eight threads with maps of their own at once, releases
values that release other maps' keys or drop other maps (nothing re-enters the lock), and checks
that an object marked without a record is an ICE reported before the count changes. The goldens
`weakmap_async` and `weakmap_nested` do the same from Velt programs; `weakmap_mutex`,
`weakmap_mutex_call`, `weakmap_mutex_value`, `weakref_mutex`, `weakref_mutex_closure` and
`weakref_mutex_result` check that a weak collection cannot cross a `with` lock.

The soak test creates and drops reactive objects (raw, handler capturing it, proxy; a nested
proxied child every third time) through a window of live ones, and checks after every step that
the live objects, side-table records and map entries are exactly the window's. The long run
(`soak_long`, ignored by default) passed 10^6 reactive objects (4,000,002 objects) through a
window of 1,000: at most 4,002 objects were alive and recorded at any step, and none were left at
the end. CI runs 20,000 through a window of 64.

### What it does not handle

- **A value graph past 256 objects** at insert or during a trial: nothing is recorded or freed;
  the pair leaks until the entry is deleted or the map dropped (test
  `a_value_graph_past_the_limit_is_kept`). The bound keeps every release cheap; the proxy cells
  this is for are a handful of objects.
- **References to the key added after the last trial through it**, when the final outside
  release lands on an object whose count is still above what the table knows: no trial runs and
  the pair is kept until a release that reaches the known level, or the entry goes (test
  `a_path_added_after_the_last_trial_delays_the_free`). sigx's proxies never change their
  target, so its caches do not hit this.
- **Strong cycles through the key** (`raw.self = proxy(raw)`): the trial frees the entry, but raw
  and the proxy still hold each other. That is an ordinary strong cycle, the problem `weak T`
  (#11) solves; the core does not collect cycles that no weak map takes part in.
- **Ephemeron cycles across maps.** `m1.set(a, b); m2.set(b, a)` with no other references
  between `a` and `b`: each key is kept alive only by the other map's value, so a garbage
  collector frees both at its next collection. The insert-time analysis records a path from a
  value back to its own entry's key only, so neither entry has a hint and no trial runs; the
  pair is kept until an entry is deleted or a map dropped. A trial through either object would
  find the pair dead (it follows entries in every map), but nothing starts one.
- **A weak map held only by its own cycle** (sigx's `nestedCache` lives in the handler's
  closure): the trial treats every map as live, so entries whose keys are dead are still deleted,
  but a map that is itself garbage is freed only when its holder is.

### Open cost question

A recorded object whose count sits at its hint because the outside reference is elsewhere in the
cycle (a raw object held only by its proxy, while the program holds the proxy) runs a trial on
every release back to that level, such as the release of a temporary reference in a trap. Each
trial is bounded (a handful of objects), but it is a hash-table walk on a path that is a
decrement today. If sigx's traps show it, a failed trial can record the count it saw and skip
the next trial until a release elsewhere in the cycle changes the answer.

## Next steps

1. Done (#823 step B): `WeakMap`, `WeakSet` and `WeakRef` in the language.
   Left: trace glue through function values' environments (a handler closure capturing the
   target is opaque today, so such a cycle is kept until its map entry goes) and through class
   hierarchies (dispatching on the dynamic class), keys of other object kinds (unions, tuples,
   function and interface values), and ephemeron cycles that span several maps (above).
2. `weak T` fields (#11): a `weak` field is a `WeakRef` slot inline in the object; reading it is
   a liveness check where it is used.
3. Store the records inline (small vectors) and measure `set` and the cold release against
   Node's `WeakMap` on sigx's reactivity tests.
