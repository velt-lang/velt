# Design: reference cycles (cycle-capable types, a collector for them, and `weak`)

Status: proposed (#3119, for #11). Step 1 (the cycle-capability analysis, below) is
implemented; the rest is planned. This note replaces the "without a
collector" plan in [semantics.md](semantics.md#reference-cycles--without-a-collector), which
needs the owner's sign-off (decision 1).

## The problem

Semantics stage 2 made objects, arrays, maps and closures shared references with a count
([semantics-stage2.md](semantics-stage2.md)). A count never reaches zero inside a cycle, so
every cyclic graph the program lets go of stays allocated until the process exits. Node frees
all of them. Measured on `main` (debug, `VELT_RC_STATS=1`):

```ts
class Button {
  listeners: (() => void)[] = [];
  onClick(f: () => void) { this.listeners.push(f); }
}
class View { clicks = 0; button = new Button(); }
function mount() {
  const v = new View();
  const b = v.button;
  b.onClick(() => { v.clicks += 1; });   // the closure holds the view that holds it
}
// 1,000 mounts: blocks=4000/0, all four blocks of every view leak
```

(`v.button.onClick(() => { v.clicks += 1; })`, the direct form, is refused today by the
exclusivity rule, "this call may modify `v.button` through another argument"; that is a
TypeScript-compatibility gap of its own, not part of this design.)

The cycles the issues report are of a few shapes, and most go through a closure, not a field:

| Shape | Example | Issues |
|---|---|---|
| A closure stored in the object it captures | listeners, callbacks, `onTick` | this note's example, #2688 (event classes) |
| A `const` used by its own closures | `const app: App = { use(p) { p(app); return app; } }` | #1291, #1298, #1576 (one leak per sigx component, app, topic) |
| A function that holds itself | `f.self = f`, `const factory: any = function () { return factory; }` | #1780 (3) |
| A data structure with back-references | parent ↔ child, doubly linked lists, graphs | #11 |
| A copy of a captured cycle per call | an async closure handed to an unknown callee | #2438 (last item), #2726 (1) |
| A strong cycle through a weak key | `raw.self = proxy(raw)` | #955 (1), weak-refs.md |
| An ephemeron cycle across maps | `m1.set(a, b); m2.set(b, a)` | #989 (4), weak-refs.md |
| A cycle behind an opaque trace | a `WeakMap` value whose handler closure captures the key | #989 (2), weak-refs.md |

`weak T` fields alone (the plan in #11 and semantics.md) fix only the fourth row, and only in
code that writes `weak`. Ported TypeScript never does: sigx, Vue's server renderer and any UI
with listeners rely on the collector Node has. Velt's rule is that valid TypeScript runs as it
does in Node, so a program Node runs in bounded memory must run in bounded memory here too.

## Goals

1. **No leak without annotations.** A program that frees a cyclic graph in Node frees it in
   Velt, whatever shape the cycle has (fields, closures, interface values, `any`, weak maps).
2. **Nothing for types that cannot form cycles.** The cost is confined to the counted blocks
   that can be part of a cycle (cycle-capable types, below). A program without them compiles
   to the same VIR as today. The CPU benchmarks of the stage 2 gate are such programs (they
   count nothing, [bench/RESULTS.md](../../../bench/RESULTS.md) "Semantics stage 2"), but
   not all of `bench/`: of its 297 programs, 114 count something and 60 have cycle-capable
   types under step 1's analysis (the HTTP servers and `fetch`, `any` and proxies, JSX, timers,
   the database clients), 36 of them only through the coarse closure node of section 1. Those
   rows are gated like the others, which is why narrowing the analysis (step 2) comes before
   any code generation.
3. **Determinism stays.** Acyclic garbage is freed at its last reference, and
   `[Symbol.dispose]()` runs there, as today. Cyclic garbage is freed at a collection point of
   the task that owns it. Collection points depend only on what the task did, never on time or
   on other threads, so a program prints the same output on every run.
4. **No pause for other tasks.** Each task collects its own objects on its own thread, between
   two of its own operations. Nothing stops the world.
5. **`weak` as the explicit tool.** A `weak` field or variable keeps a type out of the collector
   (no cost at all) and reads as `null` once its target is gone, like Swift's `weak`.

Not goals: a tracing collector; collecting cycles that go through `shared` values (each side
is a separate copy with an atomic count; documented below); `FinalizationRegistry`.

## 1. Cycle-capable types (step 1, implemented)

A cycle needs an object with two owners, one inside the cycle and one that let go, so every
cycle contains at least one **counted block**: an object of a counted type, or one of the
blocks that are counted whatever their type: closure environments on the heap, captured
variables' cells, and the boxes interface values share their data in (semantics-stage2.md
§3.1, §5). The last kind matters: `const app: App = { use(p) { p(app); return app; } }` counts
no type at all, yet leaks (`blocks=8/4`), because the method's environment holds the cell of
`app`, which holds the literal. Uniquely owned values can never be in a cycle on their own, but
one can sit on a cycle's path (a counted `A` owns an uncounted `U` that refers back to `A`),
so the analysis walks through uncounted types without stopping there.

**The graph.** Its nodes are the types of the program after the counted types settle
(`lower_program`'s fixed point). An edge `A -> B` means "a value of `A` may own a strong
reference to a value of `B`" (`velt_vir` `lower/boxing/cycles.rs`):

| Type | Edges to |
|---|---|
| class | the types of its fields; and, for a class in a hierarchy with a vtable, every concrete class of that hierarchy a vtable was built for (its dynamic class may have more fields) |
| object type, tuple, struct, `T \| null`, union, `Result` | their parts |
| `T[]`, `Map<K, V>`, `Set<T>`, other std containers | their elements, keys and values (a std container's type arguments) |
| function value (`FnPtr`, `Closure`) | the *closures* node: the environment of any closure of the program, a counted block, with edges to every type a closure environment captures (a captured variable's cell is counted too) |
| interface value, `any`, `unknown`, `object` | the *dynamics* node: the value of any of them, possibly a counted box, with edges to every concrete type a vtable or `any` table was built for |
| `Promise<T, E>` | `T`, `E` |
| `shared<T>` | none: a shared value is a copy with an atomic count; cycles through it are out of scope |
| `WeakMap<K, V>`, `WeakSet<T>`, `WeakRef<T>` | none (their entries are the runtime's); a weak map adds `K -> V` (an entry keeps its value while the key lives: the ephemeron edge) |
| `weak T` field or variable (step 6) | none |

**The set.** A counted type is **cycle-capable** when it lies on a cycle of this graph: in a
strongly connected component with more than one node, or with an edge to itself. So are
closure environments (with cells) and dynamic values' boxes when their node does. Tarjan's
algorithm finds the components in time linear in the graph, which is linear in the program's
types. Classes are decided per hierarchy (by root class), as counting is, since a release
through a base class type may release a subclass object.

The two summary nodes are coarse: a function value may be any closure of the program, and an
interface value any type a table was built for. That is sound (a block that cannot be in a
cycle is never in the set) but puts blocks in the set that a finer analysis would leave out,
and the collector's cost goes with the set. Step 2 splits the closures node by function type
(each environment under the function types its closure converts to, `fresh_fns.rs`'s `FnKey`
edges) and the dynamics node by interface (the types converted to each, `ToDyn`).

`VELT_DEBUG_COUNTED=1` prints the set (`velt: cycle-capable types: …`) next to the counted and
weak-capable types; `crates/veltc/tests/cycle_capable.rs` checks it on the shapes above and on
programs that must have none, and each edge kind has a test that fails without it. Step 1
changes no code generation and computes the set only when asked; step 5 moves it into the
counted-types fixed point (`close_boxing`), once code generation depends on it.

## 2. Trace glue for every cycle-capable type (step 3)

The collector and the weak core find cycles the same way: by asking each object which counted
objects it owns, through compiler-generated trace glue (`rt_abi.md`, "Weak references"). Today
the glue is null (opaque) for classes with a vtable, interface values and function values'
environments, and uncounted classes stop the walk. A leak at worst for the weak core; for the
collector, every opaque object on a cycle is a cycle never found. So step 3 makes the glue
complete for every type a cycle-capable type reaches:

- **Closure environments.** An environment's trace walks the same captures as its drop
  (`closure.rs` `build_env_drop`: owned value captures and cell captures; a cell is a counted
  child whose trace is the variable type's). The environment header gets a trace entry, only
  in a program with cycle-capable types (as `WRAPPED`'s header word is conditional today), so
  other programs' environment layouts do not change.
- **Class hierarchies.** A `SLOT_TRACE` vtable slot (next to `SLOT_DROP`, `SLOT_TRANSFER`)
  holds each concrete class's glue, demanded like `SLOT_FORMAT`; a field of a class type with
  a vtable dispatches through it, as drop and transfer already do.
- **Interface values and `any`.** The interface table gets the same slot; `any` tables already
  carry a trace word when `object` is a weak key (`weak_object.rs`), and get it whenever a
  dynamic value can reach a cycle-capable type.
- **Uncounted classes** are traced inline: their fields' references are reported as the
  owner's, since the owner holds the only reference.

The rule for every glue stays the one the weak core states: never report a word that is not a
counted reference the object owns, nor one twice: either makes the collector free a live
object, while a reference left out only keeps garbage. So step 3's tests check each glue
against the drop glue's releases
(the same objects, the same number of times) on every shape of section 1. With complete glue,
weak maps also find cycles through handler closures and class hierarchies (#989 (2)).

## 3. The collector (steps 4 and 5)

Synchronous cycle collection by trial deletion (Bacon and Rajan 2001; the scheme of Nim's ORC),
restricted to cycle-capable types. The weak core's ephemeron trial (`velt_rt` `weak/trial.rs`)
is already this algorithm over one small subgraph; the collector generalizes it to a root set
and shares its graph code.

### 3.1 Possible roots and the count word

Only a release that leaves a count above zero can leave a cycle behind: that object is a
**possible root**. A release of a cycle-capable type records it, once per collection, and so
does a release of a closure environment, a cell or an interface value's box when section 1
puts their kind in the set (their drop entries take the same sequence, `closure.rs`,
`cells.rs`, `glue/drop.rs`):

```text
c = *rc
if c as i64 > BUFFERED + 1 { *rc = c - 1 }                       // already a possible root
else if c == 1             { drop the fields; free }             // unique: as today
else                       { if velt_rt_cycle_release(obj, desc) { drop the fields; free } }
```

`BUFFERED` is bit 60 of the count word (the count proper keeps bits 0..59; bits 61 and 62 are
the lock set's, which cycle-capable types make exclusive as weak-capable ones do, `locks.rs`;
bit 63 is `RC_WEAK`). The sequence has the weak-capable release's shape and cost: one signed
compare on the shared path, one more compare before a free. The cold call handles a first
release to a nonzero count (decrement, set `BUFFERED`, append the object and its descriptor to
the root set), the last release of a buffered object (drop its fields now, so dispose runs at
the last reference as always, and leave the block marked dead in the root set, freed at the
collection), and weakly held objects (the weak core's path, unchanged). Retain stays
`count += 1`. A collection clears `BUFFERED` on the roots it looked at, so a long-lived shared
object pays one cold call per collection, not per release.

The compiler rules of the weak core extend to `BUFFERED`: uniqueness tests compare the whole
word with 1 (a buffered object is never taken as unique, so copy-on-write copies it and a
transfer deep-copies it; the copy is unbuffered), and every raw decrement site uses the
sequence above.

**Descriptors.** The root set needs to know how to trace, dispose and drop an object, and the
header has only the count. Each cycle-capable type gets a static descriptor
`{ trace, dispose, drop_fields }` (the drop glue split at the point where `obj_drop_body`
already separates the dispose call from the field drops), passed to `velt_rt_cycle_release`
and reported by `visit` for children. The weak core's `TraceFn` is the descriptor's first
field, so weak maps keep working with either.

### 3.2 The root set belongs to the task

Counted objects are used by one task at a time with plain counts; a task may resume on another
worker after an `await`, but never runs on two at once. So the root set is per task: it lives
in the task's context (`velt_rt` `task/local`, `TaskCx`; the same field in the node of the
executor in Velt, #2649), and the cold call finds it through the current-task pointer the
executor already sets. Code outside any task (synchronous `main`, runtime clean-up) uses a
per-thread set. No lock is taken.

Two places hand objects between tasks without a transfer, and both close their roots first:

- **`Mutex.with`** opens a nested root set for the callback and collects it before the unlock
  (the walk at the end of `with` already runs there), so no root of the locked value is left
  in a task's set after another task can lock it.
- **`RwLock` read mode** shares objects between readers with the lock-set bits; a cycle-capable
  type makes its lock exclusive (as a weak-capable one does today), so read mode never sees one.

### 3.3 A collection

It runs when the root set reaches a threshold, when the task ends, when a `with` scope closes,
and before the program exits (so `VELT_RC_STATS` balances). The threshold adapts as Nim's
does: it starts at 4,096 roots and doubles while collections free less than a quarter of what
they traverse, so a program with many long-lived shared objects is not traversed over and over.

1. Drop the dead entries (blocks whose last reference went while buffered) and free them.
2. Build the graph from the remaining roots through the trace glue and the weak core's
   ephemeron edges (a key leads to its entries' values; a value's entry to its key), counting
   the references among its objects.
3. An object whose count exceeds its internal references is referenced from outside: it is
   live, with everything it reaches (a weak map value is reachable when its key is).
4. Clear `BUFFERED` on every root. The objects left are cyclic garbage.
5. Free them in three passes, so a cycle is never seen half freed:
   1. **Pin** each (count + 1), and clear the weak references to them (their `WeakRef`s and
      `weak` fields read `null` from here on, and their weak map entries are deleted, as on a
      last release).
   2. **Dispose**: run each one's `[Symbol.dispose]()`, in the order the graph found them
      (deterministic, unspecified). A dispose sees the other members alive and not yet dropped.
      If a dispose stores a member somewhere outside the garbage (resurrection), its count is
      then above its pin and internal references: that is a panic naming the class ("a
      `[Symbol.dispose]()` kept an object of an unreachable reference cycle alive"), the way
      Swift traps an object retained during `deinit` (decision 3).
   3. **Drop the fields** of each (without dispose): references to other members only
      decrement (they are pinned); references out of the cycle are ordinary releases, which
      free acyclic garbage at once. Then free every block.

Releases that a collection's own disposes and drops cause append to the root set for the next
collection; a collection never starts inside another. Deep garbage drops through the existing
drop-depth queue (`drop_depth.rs`), and a panic out of a dispose leaves the remaining garbage
pinned and in the root set, so the next collection (at the latest, the one at exit) finishes it.

### 3.4 What it costs

| Object | Cost |
|---|---|
| Uncounted, or counted but not cycle-capable (every object of the stage 2 CPU benchmarks) | none: same code as today |
| Cycle-capable, shared release of a buffered object | the weak-capable sequence (one signed compare) |
| Cycle-capable, first release to a nonzero count since the last collection | a cold call and an append |
| Cycle-capable, collected | its share of the traversal: trace, a hash-map entry, and the frees |

A collection's pause is proportional to the graph reachable from its roots, not to the heap,
and only the task that collects waits; it is not bounded by a constant. A
large long-lived structure of a cycle-capable type (a tree with parent pointers) is traversed
whenever one of its objects is a root; `weak` parent pointers take the type out of the set,
and the lint of section 5 says where (decision 2). Steps 4 and 5 measure with
`bench/sigx`, the 60 `bench/` programs with cycle-capable types (above), `bench/counted_*.velt`
and new rows for the shapes of the problem table, against the 3% gate, and report cachegrind
counts for the cold call and a collection per object.

## 4. `weak T` (step 6)

For the cycles a program knows about, `weak` is cheaper than the collector and says what the
code means.

```ts
class TreeNode {
  parent: weak TreeNode | null = null;
  children: TreeNode[] = [];
}
const self: weak View | null = this;            // a closure capturing `self` holds it weakly
button.onClick(() => self?.refresh());
```

- **Syntax.** `weak` is a type operator, a contextual word parsed like `keyof` (not a keyword:
  `weak` stays a valid name), binding tighter than `|`: `weak Node | null` is a weak reference
  to a `Node`, or `null`. It is allowed in the type of a class field, an object type or
  interface member, and a `let` or `const`; anywhere else (parameters, results, elements, type
  arguments) it is an error naming the places it may go. The AST gets `TypeExprKind::Weak`.
- **Typing.** The target must be a type whose values are objects (a class, an object type, an
  array, a map or set; interfaces and function values later). The written type must include
  `null`: a weak reference reads as `null` once its target is freed, and `parent: weak Node`
  is an error with a fix-it to `weak Node | null`. A read has the type `T | null` and yields a
  counted reference (or `null`); a write takes `T | null`. A read is not narrowed across
  another read (`if (n.parent) n.parent.x` reads twice, and the target may be gone in
  between), so sema narrows nothing through a weak field: the second read is still
  `T | null`, and the "possibly null" error gets a note suggesting `const p = n.parent`.
  `n.parent?.x` works as written.
- **Representation.** A weak field holds a pointer to its target's **slot**, a counted block
  `[count][target]` shared by every weak reference to that target (Swift's side table). The
  target's weak-core record holds one reference to its slot, and each weak field or variable
  one more; freeing the target (a last release or a collection) sets `slot.target = 0` and
  gives up the record's reference, so the slot lives as long as a weak reference reads it. A
  read is inline and takes no lock:
  `s = field; p = s ? s.target : 0; if p { retain p }`. A write releases the old slot and gets
  the new target's through `velt_rt_weak_slot(obj)` (the cold, locked path). The slot is used
  only by the task that owns the target, like every counted object.
- **The rest of the language.** A `weak` field is not an edge of section 1's graph and is
  skipped by trace glue, so `TreeNode` above is not cycle-capable. Its drop releases the slot.
  A closure captures a `weak` variable as its slot, so the capture is weak too, and the
  environment has no edge to the target.
  A transfer to another task (and `clone()`, step 7) maps a weak field to the copy of its
  target when the target is copied along (the transfer's identity map, `transfers.rs`), and to
  `null` otherwise, so a class with a `weak` field may cross threads, unlike `WeakRef`. A store
  into a weak field of a value locked by `Mutex.with` of a target from outside the lock is an
  error, as weak collections are there (`ownership/locked/stores.rs`).
- **TypeScript.** `weak` parses in `.ts` files as all Velt syntax does, and
  `velt check --ts-compat` reports it (a new `velt_tscompat` rule). `velt doc`'s TypeScript
  output writes the read type, `T | null`.

`WeakRef<T>` stays as it is (a `u32` slot id under the table's lock); it can move onto slots
later for a faster `deref`.

## 5. The lint (step 8)

With the collector, a cycle-capable type is a cost note, not a bug. A `velt_lint` rule,
`perf/cycle-capable`, at the `hint` level, reports each cycle-capable type once at the field
or capture that closes its cycle, with a fix-it to `weak` where the field is nullable and of
an object type. It is a lint and not a sema warning so that `// velt-allow(perf/cycle-capable):
reason` acknowledges an intended cycle (sema warnings cannot be allowed today).

## 6. `clone()` and transfers of cyclic graphs (step 7)

Transfers already copy a graph with its sharing and cycles (`transfers.rs`, the identity map),
but `x.clone()` of a cyclic graph recurses until the stack overflows (#508 (3)). With cycles a
supported shape, `clone()` of a cycle-capable type uses the same identity map, at no cost for
other types, and fixes up `weak` fields as transfers do.

## Decisions for review

1. **A collector at all.** semantics.md says "No cycle collector by default. If one is ever
   added, it must be opt-in per type, incremental and bounded (no pause), and off in
   benchmarks." This design keeps three of those four: it is per type (decided by the
   compiler; `weak` opts a type out), it never pauses another task, and a benchmark without
   cycle-capable types pays nothing (those with some are held to the 3% gate).
   It is not opt-in: an opt-in collector leaves every ported TypeScript program leaking
   (goal 1). **Recommendation: automatic for cycle-capable types.**
2. **Bounded pauses.** A collection traverses what is reachable from its roots. An
   incremental collector needs a write barrier on every pointer store of a cycle-capable type,
   a cost on the hot path of every such program instead of only at collections, and a far
   larger runtime. **Recommendation: synchronous, with the adaptive threshold and the lint;
   revisit if step 5's measurements on sigx show pauses that matter.**
3. **Resurrection in a dispose** of cyclic garbage. (a) A panic, like Swift; (b) CPython's
   PEP 442: run the disposes, look again, keep the resurrected objects with a "disposed" mark
   so their dispose never runs twice (a fourth count-word bit, and the mark on every dispose).
   **Recommendation: (a)**: no program in the repository does it, Node never runs a dispose
   for garbage at all, and a panic can become (b) later without breaking a working program.
4. **`weak` locals**, not only fields: they are how a closure captures weakly without a
   capture-list syntax TypeScript lacks. **Recommendation: yes.**
5. **The warning #11 asked for** becomes the `hint`-level lint of section 5, since a
   cycle-capable type no longer leaks. **Recommendation: yes.**

## Steps

Each step is one pull request with its own tests, gated like any other.

1. This note and the cycle-capability analysis (section 1): no code generation change.
2. A finer analysis: the closures node split by function type and the dynamics node by
   interface, measured by how many of `bench/`'s 60 programs with cycle-capable types keep
   them. Before any code generation, since the set decides who pays.
3. Complete trace glue (section 2) for the types the analysis finds; `rt_abi.md`'s
   environment header and vtable slot. Weak maps use it at once (#989 (2)).
4. The collector in `velt_rt` (section 3) with unit tests and a soak test in the style of the
   weak core's: every shape of the problem table, released in every order, `WeakRef`s and weak
   map entries into collected cycles, resurrection, a panicking dispose, tasks on several
   threads. The ephemeron trial moves onto the shared graph code.
5. Lowering: the release sequence, descriptors and collection points for cycle-capable types;
   goldens with `// check: no leaks` for each shape (including the existing
   `lang/const_used_by_its_closures`, which gets the leak check it could not have), and the
   benchmark gate (`bench_programs.rs` asserts no `velt_rt_cycle_*` call in a program without
   cycle-capable types).
6. `weak T` (section 4): syntax, typing, slots, transfers, ts-compat rule, formatter, LSP,
   VS Code grammar.
7. `clone()` of cyclic graphs (section 6).
8. The lint (section 5), and the docs-sync issue for the website's memory reference.

## What this fixes

| Issue | Fixed by |
|---|---|
| #11 `weak` references, cycle-capability analysis and a warning | steps 1, 2, 6, 8 |
| #1291 a `const` used by its own closures | step 5 |
| #1298 closure-only self cycles | step 5 |
| #1576 an object literal whose method reads the literal | step 5 |
| #1780 (3) `f.self = f`, self-capturing function expressions | step 5 |
| #2438 (last item), #2726 (1) a captured cycle copied per call | step 5 (the copies are freed; #2726 removes the copies) |
| #955 (1) a strong cycle through a weak key | step 5 (the collection clears the `WeakRef`) |
| #989 (2) tracing through closures and class hierarchies | step 3 |
| #989 (4) ephemeron cycles across maps | step 5 |
| #508 (3) `clone()` of a cyclic graph | step 7 |

Related but not covered: #3014 (`WeakRef<shared<…>>`), the rest of #955 and #989 (weak-core
costs), and #91's other stage 2 follow-ups.
