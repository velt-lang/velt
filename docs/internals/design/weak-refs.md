# Design: weak references without a collector

Status: runtime prototype (issues #823 and #11). The core is in `velt_rt` (`crates/velt_rt/src/weak/`)
with unit tests and a leak soak test; the compiler does not use it yet. The ABI it will be
called through is proposed in [rt_abi.md](../contracts/rt_abi.md#weak-references-proposed).

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
- **A per-thread side table** from object address to a record: the maps the object is a key
  of, the entries whose value graph refers to it, its `WeakRef` slots, and two small counts
  used by the ephemeron rule (`hint`, `observed`). Counted objects never cross threads, so the
  table is thread-local and unlocked.
- **Weak maps** are runtime-owned hash tables keyed by object address (the key is not counted),
  with the value's release and trace glue given at creation. A `WeakSet` is a weak map whose
  values are plain words. A **`WeakRef`** is a slot holding the target's address, cleared when
  the target is freed.

## Release paths and their cost

The compiler decides program-wide which types are **weak-capable**: the types used as weak-map
keys, `WeakRef` targets or `weak T` fields, plus every type reachable from a weak map's key and
value types (the objects an ephemeron trial may meet). For those types only, release changes its first
test from `c == 1` to a signed compare, which works because `RC_WEAK` is the sign bit:

```text
c = *rc
if c as i64 > 1 { *rc = c - 1 }                                                  // shared path
else if c == 1  { drop the fields; free }                                        // unique path
else            { if velt_rt_weak_release(obj) { drop the fields; free } }      // RC_WEAK set: cold call
```

A weakly held object's count word is negative as a signed number, so it leaves the shared path
with the unique case and goes to the cold call, which removes the object from every map and `WeakRef` before
the generated code frees it. Every other type keeps today's two-way release, and a program
without `WeakMap`, `WeakSet`, `WeakRef` or `weak` has no weak-capable types: it compiles exactly
as today.

| Object | Cost |
|---|---|
| Type never weakly held | none: same code as today |
| Weak-capable type, object not weakly held | the signed compare: about one instruction per release, two per free (measured below) |
| Object weakly held | a call and a side-table probe on each release |
| Freeing a weakly held object | the above, plus removing its entries and clearing its `WeakRef`s |

Measured with valgrind (`crates/velt_rt/scripts/weak_rc_cost.sh`, callgrind's per-function
counts, loops in `src/weak/tests/bench.rs` inlining each release sequence, release build, Linux
x86_64, 10^6 iterations), instructions per iteration of the loop:

| Loop | Today's release | Weak-capable, signed compare | Weak-capable, separate bit test | Weakly held object |
|---|---|---|---|---|
| retain + release (shared path) | 10.00 | 11.00 | 14.00 | 94.00 |
| allocate + release (drop, free) | 93.69 | 95.69 | 93.69 | — |

The signed compare costs one instruction per shared release and two per free (+2.1% on the
allocate/free loop, mostly mimalloc); a separate bit test after `c == 1` keeps the free path
unchanged but costs four per shared release, so the signed compare is the proposal. Neither
touches types that are not weak-capable, which is every type of every program today: their code
is unchanged, so `bench/` is unaffected. A release of a weakly held object is a call and a hash
probe, about 84 instructions more than today's.

## Weak maps and `WeakRef`

- `set(k, v)` records `k` in the side table (setting its flag), stores `v` (the map owns one
  reference), and runs the insert-time ephemeron analysis below when the map's values can refer
  to objects.
- `get`, `has` and `delete` are hash probes; `get` returns the value borrowed.
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
- a value that gains a second path to its key after insert (found by the next trial).

The soak test creates and drops reactive objects (raw, handler capturing it, proxy; a nested
proxied child every third time) through a window of live ones, and checks after every step that
the live objects, side-table records and map entries are exactly the window's. The long run (`soak_long`, ignored by default) passed
10^6 reactive objects (4,000,002 objects) through a window of 1,000: at most 4,002 objects were
alive and recorded at any step, and none were left at the end. CI runs 20,000 through a window of
64.

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

1. `WeakMap`/`WeakSet` in the language on this core (#823 step B): the program-wide
   weak-capable analysis, trace glue per type, the release sequence above, and the ABI exported.
2. `weak T` fields (#11): a `weak` field is a `WeakRef` slot inline in the object; reading it is
   a liveness check where it is used.
3. Store the records inline (small vectors) and measure `set` and the cold release against
   Node's `WeakMap` on sigx's reactivity tests.
