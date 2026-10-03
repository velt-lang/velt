# Design: semantics stage 2 — objects, arrays, maps and closures as shared references

Status: **implemented** except the removal of the `struct` keyword (§7; checkpoints in §10).
Parent design: [semantics.md](semantics.md) (hard requirements: no GC, no pauses, no performance
loss). Benchmarks: bench/RESULTS.md "Semantics stage 2".

Goal: `const b = a; b.push(1)` changes `a`, like JS, for class instances, arrays, `Map`s,
object types and closures; "use of moved value" and the escaping-closure capture error disappear;
`==` compares objects by identity; `x.clone()` is an explicit deep copy; programs that never
share a value keep exactly today's code.

## 1. Model in one paragraph

Sema keeps its ownership inference (moves at last use, borrows for calls, inferred `Owned`
params) and stops reporting moves: wherever a non-Copy place would be used after a move, or
moved out of something it cannot leave (a borrowed param, an array element, a class field, a
capture), the use becomes an explicit **share** (`Intrinsic::Share`): a second reference to the
same value. Lowering decides, per concrete type and for the whole program, which types are
ever shared; only those get a reference count (an 8-byte word in front of the object). Every
other type keeps today's representation and code — unique ownership, `noalias`, inline storage,
no count traffic. No benchmark shares a value, so every benchmark keeps its representation
(`VELT_DEBUG_COUNTED=1` prints the counted types of a build: empty for all of them).

## 2. Sema: where shares come from

`Intrinsic::Share(place)` (HIR; type `T`, operand borrowed) = "another reference to the same
value". `Intrinsic::Clone` stays and means **deep copy**: `x.clone()`, and async closures'
captures (§6).

- **Shared values** (`Ctx::is_shared_value`): everything that is not Copy and holds no promise
  (strings, objects, arrays, maps, closures, interface values). Structs and object literals are
  objects and never Copy (checkpoint D). A promise has one owner: using a promise variable after
  handing it on stays ``use of moved value``.
- **Soft moves for every shared value** (`ownership/shares.rs`, generalizing stage 1's strings):
  a move stays a move where the place is dead and becomes a share where it is used again
  (`moves` dataflow → `clone_reused`) or cannot be moved from (`validate`: borrowed params,
  elements, class fields, captures). By-value captures of escaping closures become share
  captures (`Capture::share`, formerly `clone`). Exceptions that stay moves: the receiver of an
  explicit `x[Symbol.dispose]()` and a `using` declaration (disposed at the end of its block).
- **Implicit copies are shares**: async-call arguments used again, discriminated-union field
  reads, spread fields, interface-field getters, lowering's "borrowed place used as an owned
  value", and the prelude (`filter`, `slice`, `at`, `find`, `concat`, `Map.get/keys/values/
  entries`, `fill`, `getOrInsert`) via `__intrinsic_share`: `xs.filter(f)` returns the same
  objects.
- **By-reference `const`s** (`const x = node.left`) stay borrows unless the rest of the block
  would invalidate the place; then they become owned shares instead of an error
  (`exclusive/let_borrow.rs`). Modifying through pattern / `for...of` / `const` bindings is
  allowed (mutation inference already attributes it to the place they point into).
- Ownership inference is unchanged: a param that stores or returns its value is `Owned`, so a
  caller whose variable is dead hands it over for free, and only a caller that uses its variable
  again pays one count increment.
- `==` / `!=` on non-primitive types is `Intrinsic::Same` (JS `===`, §7); `__intrinsic_eq` stays
  structural (`deepEqual`, `assertEq`, `Map` keys).

## 3. Lowering: representation

### 3.1 Which types are counted
`boxing/`: lowering records **facts** while it builds functions — every share of a value of type
`T`, every borrow of a place reached through a value of type `C` (§3.3), every identity
comparison — and `lower_program` repeats lowering with the closure of those facts until it is
stable (the type table carries over between passes, so type ids stay valid). Programs without
shares lower once. `ShareKind` (boxing/kinds.rs):

| type | share | when shared |
|---|---|---|
| numbers, bool, literal and C-like enum types | bitwise copy | — |
| `string` | `velt_rt_str_clone` (stage 1) | — |
| class instance | count + 1 | the class hierarchy (by root class) gets a count in front of every object |
| `T[]` | count + 1 | **boxed**: values become a `Ptr` to the `{ data, len, cap }` header in a counted heap block |
| struct / object literal that is assigned somewhere (`AdtDef::assigned`), has `[Symbol.dispose]` or holds a `Mutex` | count + 1 | boxed like arrays (`Ptr` to the fields) |
| other struct / object literal, `T \| null`, unions, tuples | field-wise share (`Glue::Share`) | parts as needed; an object type compared by identity is boxed instead |
| function value | env count + 1 (heap envs are always counted; a frame env is copied to the heap) | — |
| interface value | the data's share entry in the vtable (`SLOT_SHARE`: retain counted data, copy immutable data) | — |
| `shared<T>` | its own atomic count | — |
| `Promise` | never shared (sema) | — |

**Why the count lives at `-8`:** a pointer to a boxed value is exactly the pointer the borrow
ABI passes for an inline value (`&T`). Borrowed params, `this`, glue functions and externs taking
`T*` work unchanged; only owning storage (locals, fields, elements, captures, results) holds a
pointer instead of the inline bytes, and content accesses go through `FnLower::content`.

### 3.2 Operations (rc.rs, boxes.rs, share.rs)
- **share**: `count += 1` (non-atomic, §6) — inline code, no runtime call.
- **release**: `if count == 1 { drop the contents; free } else { count -= 1 }`: the unique case
  reads the count and never writes it.
- `[Symbol.dispose]()` runs as part of "drop the contents": at the last reference.
- **deep clone** (`x.clone()`): a fresh box (count 1) with a deep copy of the contents.
- **move**: copies the pointer.

### 3.3 Borrows through counted objects (stabilize.rs)
In unique code the exclusivity rule guarantees that a value borrowed by a call stays put. A
place reached through a counted object has other owners: the callee may reach the same object
another way and replace or free what the borrow points to. Such borrows are **stabilized**: a
counted value is shared into a statement temporary; anything else is read through containers
retained for the statement (`retained_hop`). This applies to call arguments and receivers (not
runtime externs), by-reference `const`s, `switch`/`match` scrutinees, compound assignments
(the place is formed again after the right-hand side) and assignments (the new value is stored
before the old one is dropped). `for...of` over a boxed array, or one reached through a counted
object, works like JS's array iterator (for_of_shared.rs): the loop holds a reference to the
array, re-reads the length every iteration and shares each element into the binding. Moving a
part out of a counted value shares it instead, and pattern bindings inside a counted value are
shares. Every stabilized borrow is a fact: a container that becomes counted makes the values
borrowed through it counted too, so uncounted values stay reachable only through unique owners.

### 3.4 `noalias`, `readonly` and exclusivity
`noalias` (and, for borrowed params, `readonly`) is set only when `Cx::unique_refs` holds: the
type is not counted and never borrowed inside a counted object. A counted object may be written
through a share of a `Borrow` param (`const o = p; o.x = 1`), so it is not read-only either.
velt_opt's promotions key off the attributes. The static exclusivity errors stay (`append(xs,
xs)` is still rejected — a deviation from JS); aliasing created through shares (`const ys = xs;
append(xs, ys)`) is correct at run time because those types carry no `noalias` and interior
borrows are stabilized.

### 3.5 Extern ABI (foreign.rs)
The runtime sees unboxed layouts. A top-level boxed argument is passed as its pointer (= `&T`);
an argument or result whose layout contains boxed parts crosses as an unboxed *view*
(arguments, owning nothing) or is boxed after the call (results, `adopt_foreign`), also for the
result slot of an awaited runtime leaf future. Arrays of boxed elements in extern signatures are
not supported (none exist).

## 4. `.clone()`
`x.clone()` is a deep copy of any value (structuredClone-like): fresh boxes all the way down
(shared substructures are duplicated rather than preserved — structuredClone keeps the aliasing
within the copy).

## 5. Closures and captured variables (cells.rs)
- Function values share their environment (counted heap envs; `EnvDrop` releases one reference).
- A variable that an **escaping** closure captures by value, and that it or the enclosing
  function assigns while another party still sees it, lives in a **cell**: a counted block
  holding the value (`LocalDef::boxed`, set by `ownership/cells.rs` from the move dataflow,
  which now records instead of reporting "cannot assign … after a stored closure captured it" /
  "use of moved value" into a closure). The enclosing function's local and every capturing
  closure's capture local are indirect through the cell; closures hold one reference each; the
  function releases its own at scope end; reassignment drops the old value in place. A closure
  that is the only remaining user (`makeCounter`) keeps a plain copy. Variables captured by
  async closures keep their own copies (they may run on other threads): assigning one after the
  capture stays an error.

## 6. Threads (transfer.rs)
Counts are not atomic, so no counted object may be reachable from two threads. Values cross
threads at `spawn(f(args))` (owned arguments; for a call through a function value, vtable
or interface, every argument and the receiver) and the HTTP handler environment: a value whose
type can reach a counted object is **deep-copied** for the task and the original reference
released (structured clone at a worker boundary); others move as before. Interface, closure
and function values count as able to reach one (a closure's heap env is itself counted). An
owned closure value still moves when, at run time, its env is null or in a frame, or its count
is 1 and its `reach` header word says no capture can reach a counted object (set where the
closure is created, closure.rs): then it is the only reference to everything it captures, and
a captured unique value is not copied (whose `[Symbol.dispose]()` would run twice, #122). A
borrow-ABI argument of a call through a function value, vtable or interface (the caller keeps
its reference) is always deep-copied. Async closures copy what they capture per call (deep
copies of shared captures, `validate`), since an HTTP handler runs them concurrently. Strings
keep their atomic counts (stage 1); `shared<T>` stays atomic.

## 7. Identity and the `struct` keyword
- `==` / `!=` on objects (classes, arrays, structs, object literals, interface and function
  values) compare **identity** (`Intrinsic::Same`, same.rs): a counted value by pointer, an
  uncounted one by the address of its single home (a unique value lives in exactly one place).
  `T | null`, unions and tuples compare part by part (`Glue::Same`). An object type copied when
  shared would lose its identity, so comparing one makes it counted once it is shared.
- Interface values compare their data pointer; comparing an interface type makes the object
  types converted to it counted (`Boxing::identity_dyns`), so the data pointer is the object,
  not a copy. Function values compare code and env; once a program compares function values
  (`Boxing::fn_identity`), a closure without captures gets an empty env per evaluation (a frame
  env when only borrowed by a call), so each evaluation is a new function as in JS (#365).
- `deepEqual(a, b)` (prelude) is the structural comparison; `assertEq` uses it.
- **Not done:** removing the `struct` keyword and migrating its ~95 declarations (std handles to
  classes with `[Symbol.dispose]`, data structs to `type X = { … }` + `extend X`). Structs
  already behave as objects (references, identity, never Copy), so the remaining step is syntax
  and std API shape; it overlaps the std streams and the Velt rename, so it is left to a
  coordinated follow-up.

## 8. Gates and accounting
- Benchmarks: `bench/compare.ps1` (bench/, bench/async) and `bench/benchmarks-game/compare.ps1`,
  interleaved, LLVM release, ±3%.
- Goldens `tests/golden/lang/share_*.vlt` (aliasing, prelude results, identity, closures cells,
  dispose at the last reference, async/spawn, objects): JS-identical where the program is valid
  TS (checked with node), every debug run under the checking allocator, and `// check: no
  leaks` goldens assert `blocks=A/F` with A = F (`VELT_RC_STATS=1`; the runtime counts every
  `velt_rt_alloc` / `velt_rt_free` block).

## 9. Contract changes
- hir.rs: `Intrinsic::Share`, `Intrinsic::Same`; `Capture::clone` → `Capture::share`;
  `LocalDef::boxed`; `AdtDef::assigned`. hir_encodings.md "Sharing".
- vir.rs invariant 9: `noalias` only for params of types with unique references.
- rt_abi.md: "Counted objects"; `VELT_RC_STATS` reports `blocks=A/F`.

## 10. Checkpoints
| | content | state |
|---|---|---|
| A | `Intrinsic::Share`, counted-type fixpoint, counted classes/arrays/objects/closure envs, share/release/stabilize, `noalias` restricted, implicit copies → shares | done |
| B | soft moves for every shared value: no "use of moved value"; let-borrow conflicts → shares | done |
| C | captured mutable variables in counted cells | done |
| D | identity `==`, `deepEqual`, structs as objects | done; `struct` keyword removal not done (§7) |
| E | RC elision / reuse tuning | not needed for the gate (no benchmark shares); see gaps |

## 11. Known gaps
- Reference cycles leak (stage 3: `weak`).
- Granularity is per type: one share of `T[]` anywhere counts every `T[]` of the program
  (per-allocation-site or per-field decisions would be finer).
- No Perceus-style reuse or elision of retain/release pairs around stabilized calls yet.
- `using x` of a value that was also shared elsewhere disposes at the last reference, not at the
  block end; an explicit `x[Symbol.dispose]()` of a shared object releases that reference only.
- Threads: deep copies at `spawn`/handler boundaries do not preserve aliasing inside the copied
  graph. (A stored promise handed to `spawn` already runs on its creator's task and stays there,
  [Async — tasks](../../reference/async.md#tasks), so it crosses no thread.)
- `Mutex.with` callbacks and `attempt(f)` are not stabilized like ordinary calls.
- `Map` (and `Set`) keys of struct, object-literal and tuple type compare by content
  (`__intrinsic_eq` with the structural hash), not by identity as in JS; class instances
  compare by identity.
