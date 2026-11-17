# Design: reference cycles (cycle-capable types, a collector for them, and `weak`)

Status: accepted (#3119, for #11): the design review on #3119 is folded in, and the owner
chose a collector, automatic for cycle-capable types (decision 1). Step 1 (the cycle-capability
analysis, below) is implemented; the rest is planned. This note replaces the "without a
collector" plan [semantics.md](semantics.md#reference-cycles) had.

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

## 1. Cycle-capable types (steps 1 and 2, implemented)

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
| class | the types of its fields; and, for a class with subclasses, every built subclass (an object of that type may be one, with more fields); never the class itself or a base class, since "is a" is not "owns" |
| object type, tuple, struct, `T \| null`, union, `Result` | their parts |
| `T[]`, `Map<K, V>`, `Set<T>`, other std containers | their elements, keys and values (a std container's type arguments) |
| function value of type `F` | the *closures of `F`'s key* (below) and the *generic closures*: environments, counted blocks, with edges to what they capture (a captured variable's cell is counted too) |
| generic function value | the closures of its interface's key, the generic closures, and its interface's node |
| interface value of interface `I` | the *`I` node*: its value, possibly a counted box, with edges to every concrete type an `I` table was built for and to the nodes of the interfaces converted to `I` |
| `any`, `unknown`, `object` | the *dynamics* node: edges to every type an `any` table was built for, the nodes of the interfaces converted to `any`, and the generic closures |
| closure environment, in a program that writes properties of functions through `any` | the dynamics node: its properties are `any` values it owns (`f.self = f`, #1780; velt_rt `fn_props.rs`) |
| `Promise<T, E>` | `T`, `E` (see below for what a pending promise owns) |
| generator object (`Generator`, `AsyncGenerator`) | its fields, and the state of the generator function it runs, which follows them: every local of the generator functions made at that class type |
| `shared<T>` | none: a shared value is a copy with an atomic count; cycles through it are out of scope |
| `WeakMap<K, V>`, `WeakSet<T>`, `WeakRef<T>` | their own fields and a subclass's, not their entries (the runtime's); a weak map adds `K -> V` from every type a key may be (the class `K` and its built subclasses, or the concrete types behind an interface, `object` or `any`): an entry keeps its value while the key object lives, the ephemeron edge |
| `weak T` field or variable (step 6) | none |

**A pending promise** (a started promise's node, or a heap future) also owns the state of the
async call that settles it and its reactions (`.then` callbacks, the awaiter's waker). The
promise is not garbage while it is pending: the task that drives it holds it, so it is freed
when it settles or its task ends, and a cycle through a pending promise's state is one of
pending work, like a timer's. The graph has no edge for it. Step 3 decides whether trace glue
reports those references (a never-settling promise that nothing but its own cycle refers to
is the one case it matters for), with a test either way.

**The set.** A counted type is **cycle-capable** when it lies on a cycle of this graph: in a
strongly connected component with more than one node, or with an edge to itself. So are the
closure environments of a key (with their cells) and an interface's or `any`'s boxes when
their node does. Tarjan's algorithm finds the components in time linear in the graph, which is
linear in the program's types (`cycles_scc.rs`). Classes are decided per hierarchy (by root
class), as counting is, since a release through a base class type may release a subclass
object.

**Closures by key (step 2, `cycles_keys.rs`).** Step 1 had one node for every closure of the
program, which made every program with a function wrapper cycle-capable: sema's wrappers
(#2688: a closure that doesn't throw, converted to a function type that may) are closures
capturing the function they wrap, so "any closure" captured "any closure". Step 2 files each
environment under the key of the function type its closure was made at (the closure
expression's type, substituted). The key is sound because sema never uses a function value
as one of another type as it is (velt_sema `fn_widen.rs`): every conversion that changes what
a function takes, returns or throws makes a wrapper, a closure of the target's key capturing
the source. The few retypes sema makes without a wrapper keep the key equal, by its
definition: error-set variables (`TyKind::Op`) are one marker, a `never` result is `void`, a
generic function value's key is its interface alone, and erasure and `sub` make readonly
twins, `unknown` and `any`, brands and the two forms of an object type one type. Closures
whose own type is not a function type of fixed types (generic closures) have no key, and
every function value and dynamic value may lead to them.

**Interfaces by interface.** An interface value leads to the types its own tables were built
for and to the interfaces converted to it (`Upcast`), not to every implementation of every
interface.

`VELT_DEBUG_COUNTED=1` prints the set (`velt: cycle-capable types: …`) next to the counted and
weak-capable types, then a line per entry with where its values are made
(`velt: cycle-capable sites: class View: src/view.velt:12:13, …`: `new`, literals, closures;
`cycles_sites.rs`). `crates/veltc/tests/cycle_capable.rs` checks the set on the shapes above
and on programs that must have none, and each edge kind, each split and the wrapper edge has a
test that fails without it. The analysis changes no code generation and is computed only when
asked; step 5 moves it into the counted-types fixed point (`close_boxing`), once code
generation depends on it.

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
and shares its graph code. Both are written in std, not in `velt_rt` (section 3.5).

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
else                       { if cycleRelease(obj, desc) { drop the fields; free } }  // std hook
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

**Descriptors.** The root set needs to know how to trace, dispose, drop and free an object,
and the header has only the count. Each cycle-capable type gets a static descriptor
`{ trace, dispose, drop_fields, free }` (the drop glue split at the point where
`obj_drop_body` already separates the dispose call from the field drops; `free` knows the
block's size), passed to the `cycleRelease` hook and reported by `visit` for children. The
weak core's `TraceFn` is the descriptor's first field, so weak maps keep working with either.

### 3.2 Which blocks a collection may touch

A trial reads every count in its graph and frees what it finds dead, so it is sound only while
no other thread reads or writes those counts. The rule:

**A collection runs on the task that owns its root set, inside one of that task's polls, and
traverses only blocks with plain counts that no other task can reach while it runs. Every
other block it meets is external: never entered, never counted as internal, never
decremented, never freed.** A wrong "external" only keeps garbage; the rule never frees a
live block.

Blocks other tasks can reach, by the shared-state model of #2726 (option (c)), and how a
collection keeps out of them:

- **`shared<T>` and atomically counted values** (strings, values that crossed threads): trace
  glue never reports them (section 1 gives `shared<T>` no edges), so no graph contains them.
- **Frozen read-only shared state** (module state and handler captures that are only read
  after initialisation, read on every core with no lock, #2726 (c) step 3): freezing marks a
  block in its header before another task can see it (an immortal count, as the static atomics
  and strings have), and the trial reads the header with an atomic load and stops at a marked
  block.
- **Written shared state behind a lock**: today the process-wide module lock (rt_abi.md
  "Module state": a task that touched module state holds it for every later poll) and
  `Mutex.with`; under #2726 (c) one lock per independent piece, held until the next `await`,
  with objects taken out of it marked at run time (#2730). Taking such a lock opens a **lock
  scope**: a root recorded while it is held goes to the scope's root set, not the task's, and
  the scope's set is collected **while the lock is still held**, just before it is released
  (the end of `with`, the end of the poll for the module lock, before the `await` for a
  per-piece lock). That collection touches the locked state with the lock held, so no other
  task touches it meanwhile. A collection outside the scope stops at a marked block. Until
  #2730's marks exist, the module lock covers the gap: a task that can reach a module-state
  block holds the module lock in every poll, so any collection it runs holds it too.
- **Blocks moved to another task** (spawn arguments, channel messages, promise results): a
  transfer moves a block in place only when its whole count word is 1, and a buffered block's
  word never is (its `BUFFERED` bit is set), so **a buffered block never moves**: the transfer
  deep-copies it (the copy is unbuffered and belongs to the receiver), and the sender's
  reference is released through the sequence of 3.1, on the sender. A block moved in place had
  that one reference only, so no block of the sender reaches it afterwards.
- **Tasks that move between workers** (spawned tasks are stolen; connections stay on their
  worker): the root set is a word of the task block (`T_CYCLES` in the executor in Velt,
  std/lang/hooks/sched_task.velt), found through the worker's current task (`E_CURRENT`) at
  each cold call and each collection, never cached in a thread-local across polls, so it
  moves with the task. Code outside any task (a synchronous `main` before it starts a task, the
  executor's own clean-up) uses its worker block's set (`E_CYCLES`). On WebAssembly (one
  thread, the runtime's executor) there is one set for the process.
- **`RwLock` read mode** shares objects between readers with the lock-set bits; a cycle-capable
  type makes its lock exclusive (as a weak-capable one does today), so read mode never sees one.

Steps 4 and 5 add a soak test per case, run with `VELT_THREADS=4` and the debug allocator:
cycles made and dropped inside `Mutex.with` by 8 tasks; a module-level registry whose cyclic
entries tasks replace; cyclic graphs sent between tasks (spawn arguments, channels) while the
sender holds buffered roots in them; and a spawn storm whose tasks are stolen between workers
with root sets pending. Each checks `blocks=A/F` with A = F and no `ICE:`.

### 3.3 A collection

It runs when the root set reaches a threshold, when the task ends, and when a lock scope
closes (3.2). The threshold adapts as Nim's does: it starts at 4,096 roots and doubles while
collections free less than a quarter of what they traverse, so a program with many long-lived
shared objects is not traversed over and over.

**At exit, only when something can see it.** Node runs no finalizers at exit, and a
short-lived command should not pay a traversal on its way out. So the exit collects only when
`VELT_RC_STATS` is set (the leak-checked goldens, which must balance) or when a cycle-capable
type has a `[Symbol.dispose]()` the program can observe (the compiler records whether one
does). Otherwise the process exits with its root sets as they are. Running the disposes of
cyclic garbage, at a collection or at that exit, is a deliberate difference from Node, which
never runs a dispose for garbage; it is what Velt already does for acyclic garbage at its last
reference, and the docs-sync issue (step 8) says so.

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
pinned and in the root set, so the next collection finishes it.

### 3.4 What it costs

| Object | Cost |
|---|---|
| Uncounted, or counted but not cycle-capable (every object of the stage 2 CPU benchmarks) | none: same code as today |
| Cycle-capable, shared release of a buffered object | the weak-capable sequence (one signed compare) |
| Cycle-capable, first release to a nonzero count since the last collection | a cold call and an append |
| Cycle-capable, collected | its share of the traversal: trace, a hash-map entry, and the frees |

A collection's pause is proportional to the graph reachable from its roots, not to the heap,
and only the task that collects waits; it is not bounded by a constant. A large long-lived
structure of a cycle-capable type (a tree with parent pointers) is traversed whenever one of
its objects is a root; `weak` parent pointers take the type out of the set, and the lint of
section 5 says where (decision 2).

**The gate, per row, decided before code generation.** Step 2 ends with a number: for every
HTTP, `fetch`, JSX and database row of `bench/`, which types are still cycle-capable after the
finer analysis and at which allocation sites (`VELT_DEBUG_COUNTED=1` gains the sites), posted
on #3119. If the request path of `serve` is still cycle-capable then, that is a design point
brought back to #3119 before step 5, not something to tune afterwards. Step 5 then holds
every row of `bench/` to the 3% rule by instructions (cachegrind, `bench/nightly.sh`), with no
accepted exceptions: a row that misses is profiled to its cause and fixed before merging. It
also adds rows for the cold call, a collection per object, `bench/sigx` and the shapes of the
problem table.

### 3.5 In std, not in `velt_rt` (#2196)

#2196 moves the runtime into std: no new Rust or externs go into `velt_rt`, and the weak core
this collector generalizes (`crates/velt_rt/src/weak/`) is on its phase 2 list. So:

- **Step 4 moves the weak core to std first** (std/lang/hooks, its own pull request, deleting
  the Rust and the `velt_rt_weakmap_*`, `velt_rt_weakref_*` and `velt_rt_weak_release` externs,
  measured on `bench/weakmap.velt` and a `bench/stdport` row: no more instructions than the
  Rust path), **then writes the collector next to it**, sharing its graph code. Neither depends
  on Rust.
- **Lowering reaches them through std hooks** (`crates/velt_common/src/std_hooks.rs`, the
  contract table): `cycleRelease(obj, desc)` (the cold path of 3.1), `cycleScopeOpen()` and
  `cycleScopeClose()` (around a lock, 3.2), and `cycleCollect()` (a task's end, and the exit
  when it collects).
- **What they use that exists:** memory and atomics at an address (`addressLoad`,
  `addressStore`, `addressAtomic*`), `rawAlloc` and `rawFree`, `addressCall` (dispose, drop
  and free glue each take one pointer), `fnAddress` (the visit callback trace glue calls),
  `ThreadLocal` (a cascade's depth and the trials it defers), the executor's task and worker
  blocks (`T_CYCLES`, `E_CYCLES`), process-wide statics (#2797) for the weak table, and the
  executor's lock (sched_lock.velt) for it.
- **What they need that does not exist yet**, both in #2196's phase 2 list:
  1. `addressCall3(f, a, b, c)`: a call of a C-ABI function pointer with three pointer-sized
     arguments, for trace glue's `(obj, visit, ctx)` (`addressCall` takes one).
  2. `cycleDesc<T>()`: the address of `T`'s static descriptor (3.1), for std's `WeakMap` to
     trace its keys and values ("function pointers to drop glue"). The release sequence passes
     descriptors itself, and an object's address reaches std as a `usize` from generated code,
     so nothing else is needed.

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
  Parser tests keep `weak` an ordinary name wherever it is not the operator: `type weak = …`,
  `let weak = 1`, a property or method named `weak`, `weak` as a type name (`x: weak`,
  `weak[]`, `Map<weak, T>`).
- **Typing.** The target must be a type whose values are objects (a class, an object type, an
  array, a map or set; interfaces and function values later). The written type must include
  `null`: a weak reference reads as `null` once its target is freed, and `parent: weak Node`
  is an error with a fix-it to `weak Node | null`. A read has the type `T | null` and yields a
  counted reference (or `null`); a write takes `T | null`. A read is not narrowed across
  another read (`if (n.parent) n.parent.x` reads twice, and the target may be gone in
  between), so sema narrows nothing through a weak field: the second read is still
  `T | null`, and the "possibly null" error after `if (n.parent) n.parent.x` gets a note
  showing the form that keeps TypeScript's feel: `const p = n.parent; if (p) p.x`.
  `n.parent?.x` works as written.
- **Representation.** A weak field holds a pointer to its target's **slot**, a counted block
  `[count][target]` shared by every weak reference to that target (Swift's side table). The
  target's weak-core record holds one reference to its slot, and each weak field or variable
  one more; freeing the target (a last release or a collection) sets `slot.target = 0` and
  gives up the record's reference, so the slot lives as long as a weak reference reads it. A
  read is inline and takes no lock:
  `s = field; p = s ? s.target : 0; if p { retain p }`. A write releases the old slot and gets
  the new target's through the `weakSlot(obj)` std hook (the cold, locked path). The slot is used
  only by the task that owns the target, like every counted object.
- **The rest of the language.** A `weak` field is not an edge of section 1's graph and is
  skipped by trace glue, so `TreeNode` above is not cycle-capable. Its drop releases the slot.
  A closure captures a `weak` variable as its slot, so the capture is weak too, and the
  environment has no edge to the target.
  A transfer to another task (and `clone()`, step 7) maps a weak field to the copy of its
  target when the target is part of the copied graph (the transfer's identity map,
  `transfers.rs`), and to `null` when it is not: the copy must not refer to an object of
  another task, even weakly. So a class with a `weak` field may cross threads, unlike
  `WeakRef`. Steps 6 and 7 have a golden for each case (target copied along, target left
  behind) for spawn arguments, channel messages, promise results and `clone()`. A store
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
reason` acknowledges an intended cycle (sema warnings cannot be allowed today). It reports only
types declared in the program and its packages, never std's (std's cycle-capable types, such
as `Error` through a subclass holding `unknown`, would otherwise flood every program's hints);
a test checks that a program using std's HTTP server gets no hint from std.

## 6. `clone()` and transfers of cyclic graphs (step 7)

Transfers already copy a graph with its sharing and cycles (`transfers.rs`, the identity map),
but `x.clone()` of a cyclic graph recurses until the stack overflows (#508 (3)). With cycles a
supported shape, `clone()` of a cycle-capable type uses the same identity map, at no cost for
other types, and fixes up `weak` fields as transfers do.

## Decisions

1. **A collector at all.** Decided (owner, 2026-11-17): automatic for cycle-capable types,
   with "off in benchmarks" dropped; semantics.md is rewritten to match. The case for it,
   since it reverses a written decision:
   semantics.md said "No cycle collector by default. If one is ever added, it must be opt-in
   per type, incremental and bounded (no pause), and off in benchmarks." This design is per
   type (decided by the compiler; `weak` opts a type out) and never pauses another task. It is
   not opt-in, because an opt-in collector leaves every ported TypeScript program leaking
   (goal 1). It is not off in benchmarks either: benchmarks then measure what users run, and
   the 3% gate per row (3.4) covers the cost.
2. **Bounded pauses.** A collection traverses what is reachable from its roots. An
   incremental collector needs a write barrier on every pointer store of a cycle-capable type,
   a cost on the hot path of every such program instead of only at collections, and a far
   larger runtime. **Accepted in review: synchronous, with the adaptive threshold and the lint;
   revisit if step 5's measurements on sigx show pauses that matter.**
3. **Resurrection in a dispose** of cyclic garbage. (a) A panic, like Swift; (b) CPython's
   PEP 442: run the disposes, look again, keep the resurrected objects with a "disposed" mark
   so their dispose never runs twice (a fourth count-word bit, and the mark on every dispose).
   **Accepted in review: (a)**: no program in the repository does it, Node never runs a dispose
   for garbage at all, and a panic can become (b) later without breaking a working program.
4. **`weak` locals**, not only fields: they are how a closure captures weakly without a
   capture-list syntax TypeScript lacks. **Accepted in review: yes.**
5. **The warning #11 asked for** becomes the `hint`-level lint of section 5, since a
   cycle-capable type no longer leaks. **Accepted in review: yes.**

## Steps

Each step is one pull request with its own tests, gated like any other.

1. This note and the cycle-capability analysis (section 1): no code generation change.
2. A finer analysis: the closures node split by function type and the dynamics node by
   interface, and the allocation sites of each cycle-capable type in `VELT_DEBUG_COUNTED=1`.
   It ends with the per-row list of 3.4 posted on #3119 (the HTTP, `fetch`, JSX and database
   rows, with their sites); a `serve` request path still cycle-capable goes back to #3119
   before step 5.
3. Complete trace glue (section 2) for the types the analysis finds; `rt_abi.md`'s
   environment header and vtable slot. Weak maps use it at once (#989 (2)).
4. In std (3.5): first the weak core moved out of `velt_rt` with the two intrinsics, then the
   collector next to it, with unit tests and soak tests in the style of the weak core's: every
   shape of the problem table released in every order, `WeakRef`s and weak map entries into
   collected cycles, resurrection, a panicking dispose, and the four cases of 3.2 with
   `VELT_THREADS=4`. The ephemeron trial moves onto the shared graph code.
5. Lowering: the release sequence, descriptors and collection points for cycle-capable types;
   goldens with `// check: no leaks` for each shape (including the existing
   `lang/const_used_by_its_closures`, which gets the leak check it could not have), and the
   gate of 3.4: every `bench/` row within 3% by instructions, no exceptions
   (`bench_programs.rs` also asserts no call of the cycle hooks in a program without
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
