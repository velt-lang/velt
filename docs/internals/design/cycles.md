# Reference cycles: cycle-capable types, a collector for them, and `weak`

Status: partly implemented. Issue: [#3119](https://github.com/velt-lang/velt/issues/3119).

## Summary

Objects, arrays, maps and closures are shared references with a count
([semantics.md](semantics.md) "Compilation"). A count never reaches zero inside a cycle, so a
cyclic graph the program lets go of stays allocated, where Node frees it. Velt closes that gap
in three parts:

- the compiler finds the **cycle-capable types**: the counted types that can lie on a cycle of
  the program's ownership graph (implemented);
- a **synchronous cycle collector**, per task and written in std, looks only at objects of those
  types; every other object is freed at its last reference as before, and a program without
  cycle-capable types compiles to the same code as without the collector;
- **`weak T`** fields and variables take a type out of the collector's set for the cycles a
  program knows about, and a `hint`-level lint says where.

## Motivation

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
```

Each `mount()` leaves the view, its button, the listener array and the closure's environment
allocated: each is referenced from inside the cycle. Node frees all of them. (The direct form
`v.button.onClick(() => { v.clicks += 1; })` is refused by the exclusivity rule, "this call may
modify `v.button` through another argument"; that is a separate TypeScript-compatibility gap.)

The cycles real programs make are of a few shapes, and most go through a closure, not a field:

| Shape | Example | Issues |
|---|---|---|
| A closure stored in the object it captures | listeners, callbacks, `onTick` | [#2688](https://github.com/velt-lang/velt/issues/2688) |
| A `const` used by its own closures | `const app: App = { use(p) { p(app); return app; } }` | [#1291](https://github.com/velt-lang/velt/issues/1291), [#1298](https://github.com/velt-lang/velt/issues/1298), [#1576](https://github.com/velt-lang/velt/issues/1576) |
| A function that holds itself | `f.self = f`, `const factory: any = function () { return factory; }` | [#1780](https://github.com/velt-lang/velt/issues/1780) |
| A data structure with back-references | parent and child, doubly linked lists, graphs | [#11](https://github.com/velt-lang/velt/issues/11) |
| A copy of a captured cycle per call | an async closure handed to an unknown callee | [#2438](https://github.com/velt-lang/velt/issues/2438), [#2726](https://github.com/velt-lang/velt/issues/2726) |
| A strong cycle through a weak key | `raw.self = proxy(raw)` | [#955](https://github.com/velt-lang/velt/issues/955), [weak-refs.md](weak-refs.md) |
| An ephemeron cycle across maps | `m1.set(a, b); m2.set(b, a)` | [#989](https://github.com/velt-lang/velt/issues/989), [weak-refs.md](weak-refs.md) |
| A cycle behind an opaque trace | a `WeakMap` value whose handler closure captures the key | [#989](https://github.com/velt-lang/velt/issues/989), [weak-refs.md](weak-refs.md) |

`weak` fields alone fix only the fourth row, and only in code that writes `weak`. Ported
TypeScript never does: UI frameworks with listeners and server renderers rely on the collector
Node has. Valid TypeScript runs as it does in Node, so a program Node runs in bounded memory must
run in bounded memory in Velt too.

## Design

### Goals

1. **No leak without annotations.** A program that frees a cyclic graph in Node frees it in
   Velt, whatever shape the cycle has (fields, closures, interface values, `any`, weak maps).
2. **Nothing for types that cannot form cycles.** The cost is confined to the counted blocks
   that can be part of a cycle. A program without them compiles to the same VIR as without the
   collector. Many programs have cycle-capable types only through dynamic values and closures
   (HTTP servers, `fetch`, `any` and proxies, JSX, timers, database clients), which is why the
   analysis splits closures and interface values as finely as soundness allows.
3. **Determinism.** Acyclic garbage is freed at its last reference, and `[Symbol.dispose]()`
   runs there. Cyclic garbage is freed at a collection point of the task that owns it.
   Collection points depend only on what the task did, never on time or on other threads, so a
   program prints the same output on every run.
4. **No pause for other tasks.** Each task collects its own objects on its own thread, between
   two of its own operations. Nothing stops the world.
5. **`weak` as the explicit tool.** A `weak` field or variable keeps a type out of the collector
   and reads as `null` once its target is gone, like Swift's `weak`.

Not goals: a tracing collector; collecting cycles that go through `shared` values (each side is
a separate copy with an atomic count); `FinalizationRegistry`.

### Cycle-capable types

A cycle needs an object with two owners, one inside the cycle and one that let go, so every
cycle contains at least one **counted block**: an object of a counted type, or one of the
blocks that are counted whatever their type: closure environments on the heap, captured
variables' cells, and the boxes interface values share their data in ([semantics.md](semantics.md)
"Which types are counted", "Closures and captured variables"). The last kind matters:
`const app: App = { use(p) { p(app); return app; } }` counts no type at all, yet forms a cycle,
because the method's environment holds the cell of `app`, which holds the literal. Uniquely
owned values are never in a cycle on their own, but one can sit on a cycle's path (a counted `A`
owns an uncounted `U` that refers back to `A`), so the analysis walks through uncounted types
without stopping there.

**The graph.** Its nodes are the types of the program after the counted types settle
(`lower_program`'s fixed point). An edge `A -> B` means "a value of `A` may own a strong
reference to a value of `B`":

| Type | Edges to |
|---|---|
| class | the types of its fields; for a class with subclasses, every built subclass (an object of that type may be one, with more fields); never the class itself or a base class, since "is a" is not "owns" |
| object type, tuple, struct, `T \| null`, union, `Result` | their parts |
| `T[]`, `Map<K, V>`, `Set<T>`, other std containers | their elements, keys and values (a std container's type arguments) |
| function value of type `F` | the *closures of `F`'s key* (below) and the *generic closures*: environments, counted blocks, with edges to what they capture (a captured variable's cell is counted too) |
| generic function value | the closures of its interface's key, the generic closures, and its interface's node |
| interface value of interface `I` | the *`I` node*: its value, possibly a counted box, with edges to every concrete type an `I` table was built for and to the nodes of the interfaces converted to `I` |
| `any`, `unknown`, `object` | the *dynamics* node: edges to every type an `any` table was built for, the nodes of the interfaces converted to `any`, and the generic closures |
| closure environment, in a program that writes properties of functions through `any` | the dynamics node: its properties are `any` values it owns (`f.self = f`; velt_rt `fn_props.rs`) |
| `Promise<T, E>` | `T`, `E` |
| generator object (`Generator`, `AsyncGenerator`) | its fields, and every local of the generator functions made at that class type (the state of the function it runs) |
| `shared<T>` | none: a shared value is a copy with an atomic count |
| `WeakMap<K, V>`, `WeakSet<T>`, `WeakRef<T>` | their own fields and a subclass's, not their entries (the runtime's); a weak map adds `K -> V` from every type a key may be (the class `K` and its built subclasses, or the concrete types behind an interface, `object` or `any`): an entry keeps its value while the key object lives, the ephemeron edge |

**A pending promise** also owns the state of the async call that settles it and its reactions
(`.then` callbacks, the awaiter's waker). It is not garbage while it is pending: the task that
drives it holds it, so it is freed when it settles or its task ends. The graph has no edge for
that state.

**The set.** A counted type is **cycle-capable** when it lies on a cycle of this graph: in a
strongly connected component with more than one node, or with an edge to itself. So are the
closure environments of a key (with their cells) and an interface's or `any`'s boxes when their
node is. Classes are decided per hierarchy (by root class), as counting is, since a release
through a base class type may release a subclass object.

**Closures by key.** Each closure environment is filed under the key of the function type its
closure was made at (the closure expression's type, substituted). The key is sound because sema
never uses a function value as one of another type as it is
(`crates/velt_sema/src/body/expr/fn_widen.rs`): every conversion that changes what a function
takes, returns or throws makes a wrapper, a closure of the target's key capturing the source. The
few retypes sema makes without a wrapper keep the key equal by its definition: error-set
variables (`TyKind::Op`) are one marker, a `never` result is `void`, a generic function value's
key is its interface alone, and readonly twins, `unknown` and `any`, brands and the two forms of
an object type are one type. Closures whose own type is not a function type of fixed types
(generic closures) have no key, and every function value and dynamic value may lead to them.
Without keys, sema's wrappers (a closure that does not throw, converted to a function type that
may) would make every program with a wrapper cycle-capable, since "any closure" would capture
"any closure".

**Interfaces by interface.** An interface value leads to the types its own tables were built for
and to the interfaces converted to it (`Upcast`), not to every implementation of every
interface.

## Compilation

The analysis is `crates/velt_vir/src/lower/boxing/cycles.rs` (the graph and the set),
`cycles_keys.rs` (closure keys), `cycles_scc.rs` (Tarjan's strongly connected components, linear
in the graph) and `cycles_sites.rs` (where each type's values are made). It changes no code
generation, and both the facts it reads and the analysis itself are computed only when
`VELT_DEBUG_COUNTED` is set (`Cx::cycle_facts`), so other compiles do none of it. With it,
`velt` prints, next to the counted and weak-capable types:

```text
velt: cycle-capable types: …
velt: cycle-capable sites: class View: src/view.velt:12:13, …
velt: cycle-capable cycle: <entry>: <a shortest cycle through it, node by node>
```

`crates/veltc/tests/cycle_capable.rs` checks the set on the shapes of the motivation table, on
programs that must have none, and on each edge kind, split and wrapper.

**Trace glue for closures.** The collector and the weak core find cycles the same way: by asking
each object which counted objects it owns, through trace glue ([rt_abi.md](../contracts/rt_abi.md)
"Weak references"). A function value's glue visits its environment
(`crates/velt_vir/src/lower/glue/trace_env.rs`). An environment's trace (`Work::EnvTrace`) walks
the same captures as its drop (`closure.rs` `build_env_drop`: owned value captures, and captured
variables' cells, counted blocks traced like a weak map's boxed value). The environment header
has no trace entry, which would change every environment's layout and the runtime's own
three-word environments (`fn_props.rs`); the glue dispatches on the drop entry instead, one
function per closure instance (`Work::EnvDrop`). It compares it with the drop entries of the
closures made at the value's function type (`Boxing::env_traced`, by key) and visits the
environment with the matching closure's trace; an environment that matches none (made by the
runtime, or a drop entry a function property replaced) is left out, a leak at worst. Inside a
shape-shared instance the drop entry comes from the dictionary, per concrete type, so it is the
one the comparison names. Environments and cells on a path back to a weak key get `RC_WEAK`, so
in a program with such glue their releases take the weak-capable sequence (`Boxing::weak_envs`,
program-wide, so no shape question depends on it), and the transfer glue gives up a shared
environment through its drop entry instead of a raw decrement. An uncounted object (a class
instance with one owner, which the compiler does not count) is walked inline: its fields'
references are reported as its owner's, since the owner holds the only reference, so a closure
that reaches a key through an uncounted object holding a function value is traced too.

**Trace glue for class hierarchies and interface values**
(`crates/velt_vir/src/lower/glue/trace_dyn.rs`). An object of a class with a vtable may be any class
of its hierarchy, so the walk follows the fields of every built class of it
(`Boxing::built_classes`), and its glue calls the glue its class table holds in `SLOT_TRACE` (slot
−8, the walk of exactly that class's fields, `Glue::ObjTrace`). Every hidden slot of a table is
taken and the runtime reads some at fixed offsets, so the trace entry uses the one slot class tables
leave empty (interface tables of value structs keep their `BitsEq` there). An interface table has no
free slot, so an interface value's glue compares its table with those built for classes implementing
the interface (`Boxing::iface_traced`, from the tables lowering builds) and visits the object with
that class's glue, as a function value's glue compares drop entries.

**Trace glue for generator states** (`crates/velt_vir/src/lower/async_fn/gen_trace.rs`). A
generator's state holds what its function had live at its suspension, in places spill.rs chooses
after the poll function is built. So the trace runs in the poll function, as the close path does:
the case `TRACE_BIT | k` of each `yield` (and of the state before the start) visits the drop
entries the close path of `k` would drop, with their flags, and `f$trace` in the generator's
table runs it for a suspended generator. The weak-capable walk takes every local of the generator
functions made at a generator class type (`Boxing::gen_types`, from the `gen_frames` fact,
recorded in programs with weak references). Weak maps find cycles through handler closures,
class hierarchies, interface values and generators with it ([weak-refs.md](weak-refs.md)).

## Alternatives considered

**`weak` only, no collector.** Ported TypeScript never writes `weak`, so every program with
listeners or self-referencing constants would leak, against the rule that a program Node runs in
bounded memory runs in bounded memory in Velt.

**An opt-in collector, off in benchmarks.** An opt-in collector leaves the same programs leaking,
and benchmarks with the collector off would not measure what users run. The collector is per type
already (the compiler decides, `weak` opts a type out), and the 3% gate covers its cost.

**An incremental collector with bounded pauses.** It needs a write barrier on every pointer store
of a cycle-capable type, a cost on the hot path of every such program instead of only at
collections, and a far larger runtime. The synchronous collector with an adaptive threshold and
the lint covers the programs that matter.

**Keeping resurrected objects** (CPython's PEP 442: run the disposes, look again, keep the
resurrected objects with a "disposed" mark). It costs a fourth count-word bit and a mark check on
every dispose, for a pattern no known program uses; Node never runs a dispose for garbage at all.
A panic can become this later without breaking a working program.

**A tracing collector.** It would give up deterministic freeing of acyclic garbage and add a cost
to programs that have no cycles.

## Limitations

- Cycles through `shared` values are not collected: each side is a separate copy with an atomic
  count.
- The analysis is conservative where static types say little: a function value leads to every
  generic closure, and an `any` to every type an `any` table was built for.

## Not yet implemented

The cycle-capability analysis and the trace glue above are built, and so are `weak T`'s syntax and
checks (`velt_sema` `weak_types`: where it may go, its target, its `null`), its formatting, its
`velt doc` TypeScript output, its `--ts-compat` rule (`weak-type`), its slots in the weak core and
the lowering of a class's `weak` field and of a function's `weak` variable whose target is a class
(`velt_vir` `weak_fields`). Other `weak` references (the closures capturing a variable, a module's
variable that functions use, object type and interface members, interface, array and map targets,
a field declared with `?`) are reported as not implemented yet,
and so are a transfer or a `clone()` of a value with a `weak` field and a destructuring of one.
They, the collector, the lint and `clone()` of cyclic graphs are accepted design, described here.

### The collector

Synchronous cycle collection by trial deletion (Bacon and Rajan 2001; the scheme of Nim's ORC),
restricted to cycle-capable types. The weak core's ephemeron trial ([weak-refs.md](weak-refs.md))
is this algorithm over one small subgraph; the collector generalizes it to a root set and shares
its graph code.

**Possible roots.** Only a release that leaves a count above zero can leave a cycle behind: that
object is a **possible root**. A release of a cycle-capable block records it, once per
collection:

```text
c = *rc
if c as i64 > BUFFERED + 1 { *rc = c - 1 }                       // already a possible root
else if c == 1             { drop the fields; free }             // unique
else                       { if cycleRelease(obj, desc) { drop the fields; free } }  // std hook
```

`BUFFERED` is bit 60 of the count word (the count proper keeps bits 0..59; bits 61 and 62 are the
lock set's, which cycle-capable types make exclusive as weak-capable ones do; bit 63 is
`RC_WEAK`). The sequence has the weak-capable release's shape and cost: one signed compare on the
shared path, one more compare before a free. The cold call handles a first release to a nonzero
count (decrement, set `BUFFERED`, append the object and its descriptor to the root set), the last
release of a buffered object (drop its fields at once, so dispose runs at the last reference, and
leave the block marked dead in the root set, freed at the collection), and weakly held objects
(the weak core's path). Retain stays `count += 1`. A collection clears `BUFFERED` on the roots it
looked at, so a long-lived shared object pays one cold call per collection, not per release.

The weak core's compiler rules extend to `BUFFERED`: uniqueness tests compare the whole word with
1 (a buffered object is never taken as unique, so copy-on-write copies it and a transfer
deep-copies it; the copy is unbuffered), and every raw decrement site uses the sequence above.

**Descriptors.** Each cycle-capable type gets a static descriptor
`{ trace, dispose, drop_fields, free }`: the drop glue split where `obj_drop_body`
(`crates/velt_vir/src/lower/glue/drop.rs`) separates the dispose call from the field drops, and
`free` knowing the block's size. The weak core's trace function is the descriptor's first field,
so weak maps work with either.

**Which blocks a collection may touch.** A trial reads every count in its graph and frees what it
finds dead, so it is sound only while no other thread reads or writes those counts. The rule: *a
collection runs on the task that owns its root set, inside one of that task's polls, and
traverses only blocks with plain counts that no other task can reach while it runs. Every other
block it meets is external: never entered, never counted as internal, never decremented, never
freed.* A wrong "external" only keeps garbage; it never frees a live block.

- **`shared<T>` and atomically counted values** (strings, values that crossed threads): trace
  glue never reports them, so no graph contains them.
- **Frozen read-only shared state** (module state and handler captures only read after
  initialization): freezing marks a block in its header before another task can see it (an
  immortal count), and the trial reads the header with an atomic load and stops at a marked
  block.
- **Written shared state behind a lock** (the module lock of rt_abi.md "Module state",
  `Mutex.with`): taking such a lock opens a **lock scope**. A root recorded while it is held goes
  to the scope's root set, and that set is collected while the lock is still held, just before
  it is released. A collection outside the scope stops at a marked block.
- **Blocks moved to another task** (spawn arguments, channel messages, promise results): a
  transfer moves a block in place only when its whole count word is 1, and a buffered block's
  word never is, so a buffered block never moves: the transfer deep-copies it, and the sender's
  reference is released on the sender.
- **Tasks that move between workers**: the root set is a word of the task block, found through
  the worker's current task (`E_CURRENT`, std/lang/hooks/sched.velt) at each cold call and each
  collection, never cached in a thread-local across polls, so it moves with the task. Code
  outside any task uses its worker block's set. On WebAssembly there is one set for the process.
- **`RwLock` read mode** shares objects between readers with the lock-set bits; a cycle-capable
  type makes its lock exclusive, so read mode never sees one.

**A collection** runs when the root set reaches a threshold, when the task ends, and when a lock
scope closes. The threshold adapts as Nim's does: it starts at 4,096 roots and doubles while
collections free less than a quarter of what they traverse. At exit, the process collects only
when `VELT_RC_STATS` is set (leak-checked tests must balance) or when a cycle-capable type has a
`[Symbol.dispose]()` the program can observe; otherwise it exits with its root sets as they are,
as Node runs no finalizers at exit.

1. Drop the dead entries (blocks whose last reference went while buffered) and free them.
2. Build the graph from the remaining roots through the trace glue and the weak core's ephemeron
   edges, counting the references among its objects.
3. An object whose count exceeds its internal references is referenced from outside: it is
   live, with everything it reaches (a weak map value is reachable when its key is).
4. Clear `BUFFERED` on every root. The objects left are cyclic garbage.
5. Free them in three passes, so a cycle is never seen half freed:
   1. **Pin** each (count + 1), and clear the weak references to them (their `WeakRef`s and
      `weak` fields read `null`, their weak map entries are deleted).
   2. **Dispose**: run each one's `[Symbol.dispose]()`, in the order the graph found them
      (deterministic, unspecified). A dispose sees the other members alive. If a dispose stores
      a member outside the garbage (resurrection), the program panics, naming the class, the way
      Swift traps an object retained during `deinit`.
   3. **Drop the fields** of each without dispose: references to other members only decrement
      (they are pinned); references out of the cycle are ordinary releases. Then free every
      block.

Releases caused by a collection's own disposes and drops go to the root set for the next
collection; a collection never starts inside another. Deep garbage drops through the drop-depth
queue (`crates/velt_rt/src/drop_depth.rs`), and a panic out of a dispose leaves the remaining
garbage pinned and in the root set for the next collection.

**In std.** The collector is written in std next to the weak core it generalizes
(`std/lang/hooks/weak*.velt`), not in `velt_rt`. Lowering reaches it through std hooks, as it
reaches the weak core (`crates/velt_common/src/std_hooks/weak_hooks.rs`): `cycleRelease(obj,
desc)`, `cycleScopeOpen()` and `cycleScopeClose()` around a lock, and `cycleCollect()` at a task's
end and at exit. Like the weak core, it uses the std-only intrinsics for memory at an address
(`addressLoad`, `addressStore`, `addressAtomic*`) and calls trace glue through `addressCall3`
([intrinsics.md](intrinsics.md)).

### `weak T`

```ts
class TreeNode {
  parent: weak TreeNode | null = null;
  children: TreeNode[] = [];
}
const self: weak View | null = this;            // a closure capturing `self` holds it weakly
button.onClick(() => self?.refresh());
```

- **Syntax.** `weak` is a type operator, a contextual word parsed like `keyof`, binding tighter
  than `|`: `weak Node | null` is a weak reference to a `Node`, or `null`. It is the operator
  only before a type name or `(` on the same line, so TypeScript that names a type `weak` keeps
  parsing, written without semicolons too (`a: weak` then `b: number` on the next line,
  `x as weak as T`). It is allowed in the type of a class field (a constructor's parameter
  property included: its parameter takes the type without `weak`; and an abstract property,
  whose getter and setter do), an object type or interface
  member, and a `let`, `const` or `var` (which Velt reads as `let`); anywhere else (parameters,
  results, elements, type arguments) it is an error naming the places it may go.
- **Typing.** The target is a type whose values are objects with an identity: a class, an object
  type, an interface value (an interface with methods: a reference to an object and its table,
  which the weak field keeps beside the slot), an array, a map or set. A struct, an interface of
  fields only (stored by value), a type parameter, and the dynamic types `any`, `unknown` (which
  may hold a number or a string) and `object` are not. The written type must include
  `null`: `parent: weak Node` is an error with a fix-it to `weak Node | null`. A read has the
  type `T | null` and yields a counted reference; a write takes `T | null`. Sema narrows nothing
  through a weak field, since the target may be gone between two reads: the "possibly null" error
  after `if (n.parent) n.parent.x` gets a note showing `const p = n.parent; if (p) p.x`.
  `n.parent?.x` works as written.
- **Representation.** A weak field holds a pointer to its target's **slot**, a counted block
  `[count][target]` shared by every weak reference to that target (Swift's side table). The
  target's weak-core record holds one reference to its slot and each weak field or variable one
  more, and keeps it until the target is freed, which sets `slot.target = 0`. A read is inline
  and takes no lock: `s = field; p = s ? s.target : 0; if p { retain p }`. A write gets the new
  target's slot through the `weakSlot(obj)` std hook and gives up the old one with
  `weakSlotDrop(s)` (`std/lang/hooks/weak_slots.velt`, rt_abi.md "Weak references"). The target
  is weakly held, but a release of it that leaves its count above 0 stays inline: a slot alone
  gives its record no release gate (`RC_GATED`, rt_abi.md), so a walk up `weak` parents costs a
  slot load per step more than one up strong parents (`bench/weak_parent_walk.velt`: 1.16 times
  the instructions of `bench/weak_parent_walk_strong.velt`).
- **The rest of the language.** A `weak` field is not an edge of the ownership graph and is
  skipped by trace glue, so a cycle that only a back reference closes (a connection holding its
  streams, each referring back to it) is gone once that reference is `weak`. A type that holds
  values of its own type stays cycle-capable: a weak target is counted (its count word carries
  the weak flag), and a counted `TreeNode` can be put among its own `children`, which `weak`
  parents do not change. A closure captures a `weak`
  variable as its slot. A transfer to another task, and `clone()`, map a weak field to the copy
  of its target when the target is part of the copied graph, and to `null` when it is not: the
  copy never refers to an object of another task. Until that is built, a value holding a class
  with a weak field stays on its task as a weak collection does: sema refuses the crossings it
  sees (`velt_sema` `weak_threads`, through interface and base-class values, `unknown` and a
  promise spawned from a variable too), and the transfer glue panics on the rest (a function
  value that captured one). Nor is it copied: sema refuses a `clone()` that reaches one (through
  those types as well), and the clone glue panics on what sema cannot see. A store into a weak field of a value locked by `Mutex.with` of a
  target from outside the lock is an error, as weak collections are there.
- **TypeScript.** `velt check --ts-compat` reports `weak`; `velt doc`'s TypeScript output writes
  the read type, `T | null`.

`WeakRef<T>` keeps its own representation ([weak-refs.md](weak-refs.md)).

### The lint

A `velt_lint` rule, `perf/cycle-capable`, at the `hint` level, reports each cycle-capable type of
the program and its packages (never std's) once, at the field or capture that closes its cycle,
with a fix-it to `weak` where the field is nullable and of an object type. It is a lint rather
than a sema warning so that `// velt-allow(perf/cycle-capable): reason` acknowledges an intended
cycle.

### `clone()` of cyclic graphs

Transfers copy a graph with its sharing and cycles (`crates/velt_vir/src/lower/boxing/transfers.rs`,
an identity map). `clone()` of a cycle-capable type uses the same identity map, at no cost for
other types, and maps `weak` fields as transfers do.

### Runtime cost

| Object | Cost |
|---|---|
| Uncounted, or counted but not cycle-capable | none: the same code as without the collector |
| Cycle-capable, shared release of a buffered object | one signed compare |
| Cycle-capable, first release to a nonzero count since the last collection | a cold call and an append |
| Cycle-capable, collected | its share of the traversal: trace, a hash-map entry, the frees |

A collection's pause is proportional to the graph reachable from its roots, not to the heap, and
only the collecting task waits; it is not bounded by a constant. A large long-lived structure of
a cycle-capable type (a tree with parent pointers) is traversed whenever one of its objects is a
root; a back reference made `weak` takes the type out of the set when no other field closes a
cycle (a stream's connection; a tree's `children` still do), and the lint says where. Every
`bench/` row is held to the 3% rule by instructions with the collector on: benchmarks measure
what users run.

### Compatibility

Node frees cyclic garbage at some later collection and never runs code for it. Velt frees it at
a deterministic collection point and runs its `[Symbol.dispose]()` there, as it does for acyclic
garbage at the last reference. A `[Symbol.dispose]()` that resurrects a member of cyclic garbage
panics. `weak T` is Velt syntax; it parses in `.ts` files, and `--ts-compat` reports it.

### Work items

- the collector in std beside the weak core, with the intrinsic it needs, `cycleDesc<T>()` (the
  address of `T`'s descriptor);
- lowering of the release sequence, descriptors and collection points for cycle-capable types,
  and the leak check on `tests/golden/lang/const_used_by_its_closures.velt`;
- the rest of `weak T`'s lowering (class fields with class targets are done): variables and
  closure captures, object type and interface members, interface, array and map targets,
  destructuring, transfers and `clone()`;
- `clone()` of cyclic graphs, which recurses until the stack overflows on a cycle
  ([#508](https://github.com/velt-lang/velt/issues/508));
- the `perf/cycle-capable` lint.
